//! # Production Current Sensor + LoRaWAN Firmware
//!
//! A `no_std` bare-metal ESP32 binary that measures AC current via ACS712
//! (with voltage divider) and reports it over LoRaWAN as an OTAA Class A
//! device.
//!
//! Behaviour:
//! - Joins the network by OTAA, retrying with backoff until it succeeds
//! - Measures every `sample_interval_s`, accumulating min/avg/max
//! - Transmits an aggregated report every `report_interval_s` (± jitter),
//!   confirmed, retrying once if the network does not acknowledge
//! - Applies configuration downlinks (intervals, jitter, reboot) received in
//!   the receive windows that follow each report, and echoes them back
//! - Persists cumulative mAh, frame counters, join nonce and configuration
//!   across power cycles

#![no_std]
#![no_main]

use app::config::{apply_downlink, DeviceConfig};
use app::flash_storage::WearLevelingStorage;
use app::lora::sx1262::Sx1262;
use app::lora::LoRaDriver;
use app::lorawan::{LoRaWanSession, UplinkOptions};
use app::mac;
use app::packet::{self, SampleAccumulator, FPORT};
use app::session_store::{SessionRecord, SessionStore};

use esp_hal::{
    analog::adc::{Adc, AdcConfig, Attenuation},
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig},
    spi::master::{Config as SpiConfig, Spi},
    time::{Duration, Instant, Rate},
};
use esp_println::println;
use nb::block;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC: {:?}", info);
    loop {
        core::hint::spin_loop();
    }
}

esp_bootloader_esp_idf::esp_app_desc!();

// --- ACS712 Configuration ---

/// Voltage divider ratio: R2 / (R1 + R2) = 10k / (10k + 10k) = 0.5
const DIVIDER_RATIO: f32 = 0.5;

/// ACS712-20A sensitivity in mV/A (before voltage divider)
const SENSITIVITY_MV_PER_A: f32 = 100.0;

/// ESP32 ADC reference voltage in mV (11dB attenuation)
const ADC_REF_MV: f32 = 3300.0;

/// ESP32 12-bit ADC max value
const ADC_MAX: f32 = 4095.0;

/// mV per ADC step
const MV_PER_STEP: f32 = ADC_REF_MV / ADC_MAX;

/// Effective sensitivity at ADC input (after divider)
const EFFECTIVE_SENSITIVITY: f32 = SENSITIVITY_MV_PER_A * DIVIDER_RATIO;

/// Number of samples per 50Hz cycle (20ms)
const SAMPLES_PER_CYCLE: usize = 200;

/// Oversampling count per sample point
const OVERSAMPLE: usize = 64;

/// Number of cycles for calibration
const CALIBRATION_CYCLES: usize = 10;

// --- LoRaWAN identity ---

/// JoinEUI (AppEUI) shared by the fleet.
///
/// All zeros: this is a private network, and we do not own an IEEE-assigned
/// JoinEUI block. The network server matches on DevEUI regardless.
const JOIN_EUI: [u8; 8] = [0; 8];

// --- Timing ---

/// How long to wait after a failed join before trying again, and the ceiling
/// that backoff grows to. Keeps a device that cannot reach a gateway from
/// saturating the band.
const JOIN_RETRY_BASE_MS: u64 = 10_000;
const JOIN_RETRY_MAX_MS: u64 = 300_000;

/// Cumulative energy is checkpointed on this cadence; the session record is
/// written on every uplink instead, because a stale frame counter gets frames
/// rejected by the network server.
const SAVE_INTERVAL_MS: u64 = 5 * 60 * 1_000;

/// Serial energy log cadence.
const ENERGY_LOG_MS: u64 = 5 * 60 * 1_000;

/// Floor on time between transmissions, whatever the configured interval.
const MIN_TX_INTERVAL_MS: u64 = 5_000;

/// Consecutive unacknowledged reports before the session is abandoned.
///
/// Reports are confirmed, so silence this sustained means the network has
/// stopped answering — the server forgot the session, or was replaced. Without
/// this the device would transmit into the void indefinitely, since nothing
/// can reach a device whose keys the network no longer holds.
const UNACKED_REPORTS_BEFORE_REJOIN: u8 = 4;

// --- MAC address reading ---

fn get_mac_address() -> [u8; 6] {
    let mac_ptr = 0x3FF5_A004 as *const u32;
    let mac_ptr2 = 0x3FF5_A008 as *const u32;

    unsafe {
        let word0 = core::ptr::read_volatile(mac_ptr);
        let word1 = core::ptr::read_volatile(mac_ptr2);

        [
            (word1 >> 8) as u8,
            word1 as u8,
            (word0 >> 24) as u8,
            (word0 >> 16) as u8,
            (word0 >> 8) as u8,
            word0 as u8,
        ]
    }
}

/// Expand a MAC-48 into an EUI-64 the usual way: OUI, `FF FE`, then the rest.
fn dev_eui_from_mac(mac: &[u8; 6]) -> [u8; 8] {
    [
        mac[0], mac[1], mac[2], 0xFF, 0xFE, mac[3], mac[4], mac[5],
    ]
}

/// AppKey is the DevEUI repeated, following the convention Milesight uses for
/// its own sensors. It means a device can be onboarded from nothing but the
/// join request it puts on air.
fn app_key_from_dev_eui(dev_eui: &[u8; 8]) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(dev_eui);
    key[8..].copy_from_slice(dev_eui);
    key
}

/// Simple deterministic PRNG (xorshift32) seeded from MAC.
///
/// Used only for transmit jitter. It must not be used for the join nonce,
/// which is persisted and monotonic — this PRNG produces the same sequence
/// after every reset.
struct Rng {
    state: u32,
}

impl Rng {
    fn from_mac(mac: &[u8; 6]) -> Self {
        let mut seed: u32 = 0x1234_5678;
        for (i, &b) in mac.iter().enumerate() {
            seed ^= (b as u32) << ((i % 4) * 8);
        }
        if seed == 0 {
            seed = 0xDEAD_BEEF;
        }
        Self { state: seed }
    }

    fn next(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Random value in range [0, max)
    fn next_range(&mut self, max: u64) -> u64 {
        if max == 0 {
            return 0;
        }
        (self.next() as u64) % max
    }
}

fn delay_ms(ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {}
}

/// Milliseconds until the next report, given the configured interval and jitter.
fn next_report_delay_ms(config: &DeviceConfig, rng: &mut Rng) -> u64 {
    let base = config.report_interval_s as u64 * 1000;
    if config.jitter_s == 0 {
        return base;
    }
    let spread = config.jitter_s as u64 * 1000;
    // Centred on the configured interval: base ± jitter, never below the floor.
    let offset = rng.next_range(spread * 2);
    (base + offset)
        .saturating_sub(spread)
        .max(MIN_TX_INTERVAL_MS)
}

#[esp_hal::main]
fn main() -> ! {
    let hal_config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(hal_config);

    println!(
        "\n{{\"event\":\"boot\",\"firmware\":\"current_lora_otaa\",\"version\":\"{}\"}}",
        env!("CARGO_PKG_VERSION")
    );

    // 1. Identity
    let mac = get_mac_address();
    let dev_eui = dev_eui_from_mac(&mac);
    let app_key = app_key_from_dev_eui(&dev_eui);
    println!(
        "{{\"event\":\"identity\",\"mac\":\"{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}\",\"dev_eui\":\"{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}\"}}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
        dev_eui[0], dev_eui[1], dev_eui[2], dev_eui[3],
        dev_eui[4], dev_eui[5], dev_eui[6], dev_eui[7]
    );

    let mut rng = Rng::from_mac(&mac);

    // 2. Persistent state
    let mut flash_storage = WearLevelingStorage::new();
    let (mut cumulative_uah, _legacy_fcnt) = flash_storage.load();

    let mut session_store = SessionStore::new();
    let record = session_store.load();
    let mut config = record.config;

    println!(
        "{{\"event\":\"config_loaded\",\"report_s\":{},\"jitter_s\":{},\"sample_s\":{},\"cumulative_mah\":{:.3}}}",
        config.report_interval_s,
        config.jitter_s,
        config.sample_interval_s,
        cumulative_uah as f64 / 1000.0
    );

    // 3. Init LoRa FIRST — the radio powering up affects the ADC baseline
    let rst = Output::new(peripherals.GPIO27, Level::High, OutputConfig::default());
    let nss = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
    let busy = Input::new(peripherals.GPIO26, InputConfig::default());

    let spi_config = SpiConfig::default().with_frequency(Rate::from_mhz(1));
    let spi = Spi::new(peripherals.SPI2, spi_config)
        .unwrap()
        .with_sck(peripherals.GPIO18)
        .with_miso(peripherals.GPIO19)
        .with_mosi(peripherals.GPIO23);

    let mut lora = Sx1262::new(spi, nss, rst, busy);
    let lora_status = lora.init();

    if lora_status.detected {
        println!(
            "{{\"event\":\"lora_init\",\"status\":\"ok\",\"freq_mhz\":{:.1}}}",
            lora_status.frequency_mhz
        );
    } else {
        println!(
            "{{\"event\":\"lora_init\",\"status\":\"error\",\"version\":\"0x{:02x}\"}}",
            lora_status.version
        );
    }

    let mut session = LoRaWanSession::new(dev_eui, JOIN_EUI, app_key, record.dev_nonce);
    record.apply_to(&mut session);
    if session.joined {
        println!(
            "{{\"event\":\"session_restored\",\"dev_addr\":\"{:02X}{:02X}{:02X}{:02X}\",\"fcnt_up\":{}}}",
            session.dev_addr[3], session.dev_addr[2], session.dev_addr[1], session.dev_addr[0],
            session.fcnt_up
        );
    }

    // 4. Status LED on GPIO2
    let mut led = Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default());

    // 5. ADC on GPIO34 (ACS712 via voltage divider)
    let mut adc_config = AdcConfig::new();
    let mut adc_pin = adc_config.enable_pin(peripherals.GPIO34, Attenuation::_11dB);
    let mut adc = Adc::new(peripherals.ADC1, adc_config);

    // Capture one 50Hz cycle (20ms) of oversampled ADC readings
    macro_rules! capture_cycle {
        ($adc:expr, $pin:expr, $samples:expr) => {{
            let capture_start = Instant::now();
            for i in 0..SAMPLES_PER_CYCLE {
                let target_us = (i as u64 * 20_000) / SAMPLES_PER_CYCLE as u64;
                let mut sum: u32 = 0;
                for _ in 0..OVERSAMPLE {
                    let raw: u16 = block!($adc.read_oneshot($pin)).unwrap_or(0);
                    sum += raw as u32;
                }
                $samples[i] = (sum / OVERSAMPLE as u32) as u16;
                let target_time = capture_start + Duration::from_micros(target_us);
                while Instant::now() < target_time {}
            }
        }};
    }

    // 6. Calibrate DC offset (no load) — LoRa is already initialised
    let mut dc_offset: f32 = 0.0;
    let mut cal_samples: [u16; SAMPLES_PER_CYCLE] = [0; SAMPLES_PER_CYCLE];
    for _ in 0..CALIBRATION_CYCLES {
        capture_cycle!(adc, &mut adc_pin, cal_samples);
        let mut sum: u32 = 0;
        for &s in cal_samples.iter() {
            sum += s as u32;
        }
        dc_offset += sum as f32 / SAMPLES_PER_CYCLE as f32;
    }
    dc_offset /= CALIBRATION_CYCLES as f32;
    println!(
        "{{\"event\":\"calibrated\",\"dc_offset_adc\":{:.1},\"dc_offset_mv\":{:.1}}}",
        dc_offset,
        dc_offset * MV_PER_STEP
    );

    // --- Main loop state ---
    let mut samples: [u16; SAMPLES_PER_CYCLE] = [0; SAMPLES_PER_CYCLE];
    let mut accumulator = SampleAccumulator::new();
    let mut iteration: u32 = 0;
    let mut tx_count: u32 = 0;

    let mut last_sample_time = Instant::now();
    let mut last_report_time = Instant::now();
    let mut last_save_time = Instant::now();
    let mut last_energy_log = Instant::now();
    let mut join_backoff_ms = JOIN_RETRY_BASE_MS;
    let mut unacked_reports: u8 = 0;
    let mut last_join_attempt: Option<Instant> = None;
    let mut send_device_info = false;

    // The first report goes out promptly so a freshly powered device is
    // visible without waiting out a full interval.
    let mut report_delay_ms = MIN_TX_INTERVAL_MS;

    println!("{{\"event\":\"ready\"}}");

    loop {
        iteration += 1;

        // --- Join, if we are not on the network ---
        if !session.joined {
            let due = match last_join_attempt {
                None => true,
                Some(previous) => previous.elapsed() >= Duration::from_millis(join_backoff_ms),
            };

            if due && lora_status.detected {
                last_join_attempt = Some(Instant::now());

                // The nonce is spent the moment the request goes on air, so it
                // is incremented and persisted before transmitting. Reusing one
                // gets the join dropped without explanation.
                session.dev_nonce = session.dev_nonce.wrapping_add(1);
                session_store.save(&SessionRecord::from_session(&session, &config));

                if mac::join(&mut lora, &mut session) {
                    join_backoff_ms = JOIN_RETRY_BASE_MS;
                    send_device_info = true;
                    report_delay_ms = MIN_TX_INTERVAL_MS;
                    last_report_time = Instant::now();
                    session_store.save(&SessionRecord::from_session(&session, &config));
                } else {
                    join_backoff_ms = (join_backoff_ms * 2).min(JOIN_RETRY_MAX_MS);
                }
            }

            // Nothing else is meaningful before we have a session.
            blink(&mut led, 10);
            delay_ms(500);
            continue;
        }

        // --- Sample ---
        let sample_due =
            last_sample_time.elapsed() >= Duration::from_secs(config.sample_interval_s as u64);

        if sample_due {
            capture_cycle!(adc, &mut adc_pin, samples);

            let mut sum: u32 = 0;
            let mut min_val = u16::MAX;
            let mut max_val = 0u16;
            for &s in samples.iter() {
                sum += s as u32;
                if s < min_val {
                    min_val = s;
                }
                if s > max_val {
                    max_val = s;
                }
            }
            let cycle_offset = sum as f32 / SAMPLES_PER_CYCLE as f32;

            let mut sum_sq: f32 = 0.0;
            for &s in samples.iter() {
                let diff = (s as f32 - cycle_offset) * MV_PER_STEP;
                sum_sq += diff * diff;
            }
            let rms_mv = libm::sqrtf(sum_sq / SAMPLES_PER_CYCLE as f32);
            let current_ma = ((rms_mv / EFFECTIVE_SENSITIVITY) * 1000.0) as u16;

            accumulator.push(current_ma);

            // Integrate energy over the interval this sample represents.
            let dt_ms = last_sample_time.elapsed().as_millis() as u64;
            last_sample_time = Instant::now();
            cumulative_uah = cumulative_uah.saturating_add((current_ma as u64 * dt_ms) / 3600);

            println!(
                "[{}] {} mA | samples={} | cumulative={:.3} mAh",
                iteration,
                current_ma,
                accumulator.count(),
                cumulative_uah as f64 / 1000.0
            );

            // A saturated or dead ADC is worth surfacing on the LED, since the
            // reported current would otherwise look plausible.
            if min_val >= 4095 || max_val == 0 {
                for _ in 0..3 {
                    blink(&mut led, 50);
                    delay_ms(50);
                }
            } else {
                blink(&mut led, 10);
            }
        }

        // --- Checkpoint cumulative energy ---
        if last_save_time.elapsed() >= Duration::from_millis(SAVE_INTERVAL_MS) {
            last_save_time = Instant::now();
            flash_storage.save(cumulative_uah, session.fcnt_up);
        }

        if last_energy_log.elapsed() >= Duration::from_millis(ENERGY_LOG_MS) {
            last_energy_log = Instant::now();
            println!(
                "{{\"event\":\"energy\",\"cumulative_uah\":{},\"mah\":{:.3}}}",
                cumulative_uah,
                cumulative_uah as f64 / 1000.0
            );
        }

        // --- Report ---
        let report_due = last_report_time.elapsed() >= Duration::from_millis(report_delay_ms);
        if report_due && lora_status.detected {
            let payload = if send_device_info {
                packet::build_device_info(&session.dev_eui)
            } else {
                let cumulative_mah = (cumulative_uah / 1000).min(u32::MAX as u64) as u32;
                packet::build_report(&accumulator.summarise(cumulative_mah))
            };

            let opts = UplinkOptions {
                confirmed: true,
                ack: false,
            };

            let mut downlink_buf = [0u8; 128];
            let mut result = mac::send_uplink(
                &mut lora,
                &mut session,
                opts,
                FPORT,
                payload.as_bytes(),
                &mut downlink_buf,
            );

            // Confirmed uplinks are retried exactly once when unacknowledged,
            // reusing the same frame counter — that is what makes it a
            // retransmission rather than a new frame.
            let acknowledged = result.downlink.map_or(false, |d| d.ack);
            if result.transmitted && !acknowledged {
                println!("{{\"event\":\"uplink_unacked\",\"fcnt\":{}}}", session.fcnt_up);
                delay_ms(1000);
                result = mac::send_uplink(
                    &mut lora,
                    &mut session,
                    opts,
                    FPORT,
                    payload.as_bytes(),
                    &mut downlink_buf,
                );
            }

            tx_count += 1;
            println!(
                "{{\"event\":\"tx\",\"seq\":{},\"kind\":\"{}\",\"ok\":{},\"fcnt\":{},\"len\":{},\"acked\":{},\"rx_delay_s\":{},\"rx2_dr\":{}}}",
                tx_count,
                if send_device_info { "device_info" } else { "report" },
                result.transmitted,
                session.fcnt_up,
                payload.len(),
                result.downlink.map_or(false, |d| d.ack),
                session.rx_delay_s,
                session.rx2_dr
            );

            if result.transmitted {
                // The frame is done with, acknowledged or not; the next one
                // must not reuse this counter.
                session.fcnt_up = session.fcnt_up.wrapping_add(1);

                if send_device_info {
                    send_device_info = false;
                } else {
                    accumulator.reset();
                }
            }

            if result.downlink.is_some() {
                unacked_reports = 0;
            } else if result.transmitted {
                unacked_reports = unacked_reports.saturating_add(1);
                if unacked_reports >= UNACKED_REPORTS_BEFORE_REJOIN {
                    println!(
                        "{{\"event\":\"rejoin\",\"reason\":\"unacked\",\"count\":{}}}",
                        unacked_reports
                    );
                    unacked_reports = 0;
                    session.joined = false;
                    last_join_attempt = None;
                    join_backoff_ms = JOIN_RETRY_BASE_MS;
                    session_store.save(&SessionRecord::from_session(&session, &config));
                    continue;
                }
            }

            // --- Downlink commands ---
            let mut reboot_requested = false;
            if let Some(downlink) = result.downlink {
                if downlink.len > 0 && downlink.port == FPORT {
                    let outcome = apply_downlink(&mut config, &downlink_buf[..downlink.len]);
                    println!(
                        "{{\"event\":\"downlink\",\"port\":{},\"len\":{},\"accepted\":{},\"malformed\":{},\"report_s\":{},\"jitter_s\":{},\"sample_s\":{}}}",
                        downlink.port,
                        downlink.len,
                        outcome.accepted,
                        outcome.malformed,
                        config.report_interval_s,
                        config.jitter_s,
                        config.sample_interval_s
                    );

                    reboot_requested = outcome.reboot;

                    // Echo what was applied. This is the only acknowledgement
                    // the operator gets that a command landed, so it goes out
                    // before anything else — including a reboot.
                    if !outcome.echo.is_empty() {
                        session_store.save(&SessionRecord::from_session(&session, &config));
                        let echo_result = mac::send_uplink(
                            &mut lora,
                            &mut session,
                            UplinkOptions {
                                confirmed: false,
                                ack: true,
                            },
                            FPORT,
                            outcome.echo.as_bytes(),
                            &mut [0u8; 128],
                        );
                        if echo_result.transmitted {
                            session.fcnt_up = session.fcnt_up.wrapping_add(1);
                        }
                    }
                } else if downlink.len > 0 {
                    println!(
                        "{{\"event\":\"downlink_ignored\",\"port\":{},\"len\":{}}}",
                        downlink.port, downlink.len
                    );
                }
            }

            // Persist counters and any config change before scheduling the
            // next report, so a reset here cannot replay a frame counter.
            flash_storage.save(cumulative_uah, session.fcnt_up);
            session_store.save(&SessionRecord::from_session(&session, &config));

            if reboot_requested {
                println!("{{\"event\":\"reboot\",\"reason\":\"downlink\"}}");
                delay_ms(100);
                esp_hal::system::software_reset();
            }

            last_report_time = Instant::now();
            report_delay_ms = next_report_delay_ms(&config, &mut rng);
        }

        // Idle briefly so the loop does not spin the CPU flat out between
        // samples. Kept short so a reduced sample interval takes effect at once.
        delay_ms(50);
    }
}

fn blink(led: &mut Output<'_>, ms: u64) {
    led.set_high();
    delay_ms(ms);
    led.set_low();
}
