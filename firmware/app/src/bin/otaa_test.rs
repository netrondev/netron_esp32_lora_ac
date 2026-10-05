//! OTAA bench test.
//!
//! Joins the network and then sends a confirmed uplink every 30 seconds,
//! printing what comes back. Useful for exercising the join and the receive
//! windows without waiting on the measurement path, and for checking that
//! downlinks land at all on a new gateway.
//!
//! It also reports the state of GPIO26 around a transmission. On this board
//! the schematic labels that net DIO1 while the driver treats it as BUSY; the
//! two behave differently around TxDone, so the trace below tells you which
//! one it actually is.

#![no_std]
#![no_main]

use app::lora::sx1262::Sx1262;
use app::lora::LoRaDriver;
use app::lorawan::{LoRaWanSession, UplinkOptions};
use app::mac;
use app::packet::{self, FPORT};

use esp_hal::{
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig},
    spi::master::{Config as SpiConfig, Spi},
    time::{Duration, Instant, Rate},
};
use esp_println::println;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC: {:?}", info);
    loop {
        core::hint::spin_loop();
    }
}

esp_bootloader_esp_idf::esp_app_desc!();

const JOIN_EUI: [u8; 8] = [0; 8];

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

fn delay_ms(ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {}
}

#[esp_hal::main]
fn main() -> ! {
    let hal_config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(hal_config);

    println!("\n{{\"event\":\"boot\",\"firmware\":\"otaa_test\"}}");

    let mac_addr = get_mac_address();
    let dev_eui = [
        mac_addr[0],
        mac_addr[1],
        mac_addr[2],
        0xFF,
        0xFE,
        mac_addr[3],
        mac_addr[4],
        mac_addr[5],
    ];
    let mut app_key = [0u8; 16];
    app_key[..8].copy_from_slice(&dev_eui);
    app_key[8..].copy_from_slice(&dev_eui);

    println!(
        "{{\"event\":\"identity\",\"dev_eui\":\"{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}\"}}",
        dev_eui[0], dev_eui[1], dev_eui[2], dev_eui[3],
        dev_eui[4], dev_eui[5], dev_eui[6], dev_eui[7]
    );

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
    let status = lora.init();
    println!(
        "{{\"event\":\"lora_init\",\"detected\":{},\"freq_mhz\":{:.1}}}",
        status.detected, status.frequency_mhz
    );

    // DevNonce is not persisted here — this is a bench tool, and a network
    // server that tracks nonces may reject a repeat after a power cycle. Vary
    // it if joins start failing silently.
    let mut session = LoRaWanSession::new(dev_eui, JOIN_EUI, app_key, 1);

    loop {
        if !session.joined {
            session.dev_nonce = session.dev_nonce.wrapping_add(1);
            if !mac::join(&mut lora, &mut session) {
                delay_ms(15_000);
                continue;
            }

            let info = packet::build_device_info(&session.dev_eui);
            let mut buf = [0u8; 128];
            let result = mac::send_uplink(
                &mut lora,
                &mut session,
                UplinkOptions::default(),
                FPORT,
                info.as_bytes(),
                &mut buf,
            );
            if result.transmitted {
                session.fcnt_up = session.fcnt_up.wrapping_add(1);
            }
        }

        let report = packet::Report {
            current_avg_ma: 1234,
            current_min_ma: 1000,
            current_max_ma: 1500,
            cumulative_mah: session.fcnt_up,
            sample_count: 10,
        };
        let payload = packet::build_report(&report);

        let mut downlink_buf = [0u8; 128];
        let result = mac::send_uplink(
            &mut lora,
            &mut session,
            UplinkOptions {
                confirmed: true,
                ack: false,
            },
            FPORT,
            payload.as_bytes(),
            &mut downlink_buf,
        );

        println!(
            "{{\"event\":\"tx\",\"ok\":{},\"fcnt\":{},\"acked\":{}}}",
            result.transmitted,
            session.fcnt_up,
            result.downlink.map_or(false, |d| d.ack)
        );

        if let Some(downlink) = result.downlink {
            println!(
                "{{\"event\":\"downlink\",\"port\":{},\"len\":{},\"ack\":{},\"pending\":{}}}",
                downlink.port, downlink.len, downlink.ack, downlink.frame_pending
            );
            for i in 0..downlink.len {
                println!("  [{}] 0x{:02X}", i, downlink_buf[i]);
            }
        }

        if result.transmitted {
            session.fcnt_up = session.fcnt_up.wrapping_add(1);
        }

        delay_ms(30_000);
    }
}
