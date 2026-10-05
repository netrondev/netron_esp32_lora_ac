//! # ACS712 Current Sensor Test for ESP32
//!
//! Replicates the Arduino ACS758 current measurement test but adapted for
//! ESP32 with a voltage divider to handle the 5V sensor output.
//!
//! ## Wiring
//!
//! ```text
//!                        ACS712 Module
//!                    +------------------+
//!   AC Live (IN) --->| IP+          VCC |---> 5V (from USB or external)
//!   AC Live (OUT) <--| IP-          GND |---> GND (shared with ESP32)
//!                    |             OUT  |---> Voltage Divider Input
//!                    +------------------+
//!
//!   Voltage Divider (5V -> 3.3V safe range):
//!
//!   ACS712 OUT ---+--- R1 (10kΩ) ---+--- ESP32 GPIO34 (ADC1_CH6)
//!                                    |
//!                                R2 (10kΩ)
//!                                    |
//!                                   GND
//!
//!   Divider ratio = R2/(R1+R2) = 10k/(10k+10k) = 0.5
//!   ACS712 quiescent output = 2.5V (no current) -> 1.25V at ESP32
//!   ACS712 max output = 5.0V -> 2.5V at ESP32 (safe for 3.3V ADC)
//!
//!   ESP32 Connections:
//!     GPIO34 (ADC input) <--- voltage divider output
//!     GND                <--- shared GND with ACS712 module
//!     3.3V or 5V         ---> ACS712 VCC (module needs 5V)
//!
//!   IMPORTANT:
//!     - The ACS712 module MUST be powered from 5V for correct operation.
//!     - The voltage divider is REQUIRED because ACS712 outputs 0-5V
//!       but ESP32 ADC max is 3.3V (with 11dB attenuation).
//!     - Use 1% tolerance resistors for the divider for best accuracy.
//!     - Keep wires short between divider output and GPIO34 to reduce noise.
//!     - Place a 100nF capacitor between GPIO34 and GND for noise filtering.
//! ```
//!
//! ## ACS712 Variants
//!
//! | Model       | Range  | Sensitivity | After 1/2 divider |
//! |-------------|--------|-------------|-------------------|
//! | ACS712-5A   | ±5A    | 185 mV/A    | 92.5 mV/A        |
//! | ACS712-20A  | ±20A   | 100 mV/A    | 50.0 mV/A        |
//! | ACS712-30A  | ±30A   | 66 mV/A     | 33.0 mV/A        |
//!
//! Change `SENSITIVITY_MV_PER_A` below to match your module.

#![no_std]
#![no_main]

use esp_hal::{
    analog::adc::{Adc, AdcConfig, Attenuation},
    clock::CpuClock,
    time::{Duration, Instant},
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

// --- Configuration ---

/// Voltage divider ratio: R2 / (R1 + R2)
/// With two 10kΩ resistors: 10k / (10k + 10k) = 0.5
const DIVIDER_RATIO: f32 = 0.5;

/// ACS712 sensitivity in mV/A (before voltage divider).
/// ACS712-5A  = 185 mV/A
/// ACS712-20A = 100 mV/A
/// ACS712-30A = 66 mV/A
const SENSITIVITY_MV_PER_A: f32 = 100.0;

/// ESP32 ADC reference voltage in mV (11dB attenuation, ~0-2.6V effective)
const ADC_REF_MV: f32 = 3300.0;

/// ESP32 12-bit ADC max value
const ADC_MAX: f32 = 4095.0;

/// mV per ADC step
const MV_PER_STEP: f32 = ADC_REF_MV / ADC_MAX;

/// Effective sensitivity at the ADC input (after divider)
const EFFECTIVE_SENSITIVITY: f32 = SENSITIVITY_MV_PER_A * DIVIDER_RATIO;

/// Number of samples per 50Hz cycle (20ms)
const SAMPLES_PER_CYCLE: usize = 200;

/// Oversampling count per sample point
const OVERSAMPLE: usize = 64;

/// Number of cycles to capture for calibration
const CALIBRATION_CYCLES: usize = 10;

// --- Current measurement modes (matching Arduino test) ---
const AC: u8 = 0;
const DC: u8 = 1;
const AC_DC: u8 = 2;

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    println!("\n\n========================================");
    println!("  ACS712 Current Sensor Test (ESP32)");
    println!("========================================");
    println!("Sensor sensitivity: {} mV/A", SENSITIVITY_MV_PER_A);
    println!("Voltage divider ratio: {}", DIVIDER_RATIO);
    println!(
        "Effective sensitivity at ADC: {:.1} mV/A",
        EFFECTIVE_SENSITIVITY
    );
    println!(
        "Samples/cycle: {}, Oversample: {}x",
        SAMPLES_PER_CYCLE, OVERSAMPLE
    );
    println!("");

    // Configure ADC on GPIO34
    let mut adc_config = AdcConfig::new();
    let mut adc_pin = adc_config.enable_pin(peripherals.GPIO34, Attenuation::_11dB);
    let mut adc = Adc::new(peripherals.ADC1, adc_config);

    // Macro to capture one 50Hz cycle (20ms) of oversampled ADC readings
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

    // --- Auto-calibration (no current should flow through sensor) ---
    println!("Calibrating DC offset (ensure NO current through sensor)...");
    let mut dc_offset: f32 = 0.0;
    let mut cal_samples: [u16; SAMPLES_PER_CYCLE] = [0; SAMPLES_PER_CYCLE];

    for cycle in 0..CALIBRATION_CYCLES {
        capture_cycle!(adc, &mut adc_pin, cal_samples);
        let mut sum: u32 = 0;
        for &s in cal_samples.iter() {
            sum += s as u32;
        }
        let cycle_mean = sum as f32 / SAMPLES_PER_CYCLE as f32;
        dc_offset += cycle_mean;
        if cycle == 0 || cycle == CALIBRATION_CYCLES - 1 {
            println!("  Cycle {}: mean ADC = {:.1}", cycle, cycle_mean);
        }
    }
    dc_offset /= CALIBRATION_CYCLES as f32;

    let offset_mv = dc_offset * MV_PER_STEP;
    let offset_sensor_mv = offset_mv / DIVIDER_RATIO;
    println!(
        "DC offset: ADC={:.1}, {:.1} mV at ADC, {:.1} mV at sensor",
        dc_offset, offset_mv, offset_sensor_mv
    );
    println!("Expected ~1250 mV at ADC (2500 mV sensor / 2)");
    println!("Calibration complete.\n");

    // Sample buffer
    let mut samples: [u16; SAMPLES_PER_CYCLE] = [0; SAMPLES_PER_CYCLE];
    let mut iteration = 0u32;
    let current_type: u8 = AC; // Default to AC measurement

    loop {
        iteration += 1;

        // Capture one 50Hz cycle
        capture_cycle!(adc, &mut adc_pin, samples);

        // --- Calculate current based on mode ---
        let current_a: f32;
        let mode_str: &str;

        match current_type {
            AC => {
                // AC mode: calculate dynamic DC offset from this cycle, then RMS
                let mut sum: u32 = 0;
                for &s in samples.iter() {
                    sum += s as u32;
                }
                let cycle_offset = sum as f32 / SAMPLES_PER_CYCLE as f32;

                let mut sum_sq: f32 = 0.0;
                for &s in samples.iter() {
                    let diff = (s as f32 - cycle_offset) * MV_PER_STEP;
                    sum_sq += diff * diff;
                }
                let rms_mv = libm::sqrtf(sum_sq / SAMPLES_PER_CYCLE as f32);
                current_a = rms_mv / EFFECTIVE_SENSITIVITY;
                mode_str = "AC";
            }
            DC => {
                // DC mode: average minus calibrated offset
                let mut sum: f32 = 0.0;
                for &s in samples.iter() {
                    sum += (s as f32 - dc_offset) * MV_PER_STEP;
                }
                let avg_mv = sum / SAMPLES_PER_CYCLE as f32;
                current_a = avg_mv / EFFECTIVE_SENSITIVITY;
                mode_str = "DC";
            }
            AC_DC => {
                // AC+DC mode: RMS using calibrated offset
                let mut sum_sq: f32 = 0.0;
                for &s in samples.iter() {
                    let diff = (s as f32 - dc_offset) * MV_PER_STEP;
                    sum_sq += diff * diff;
                }
                let rms_mv = libm::sqrtf(sum_sq / SAMPLES_PER_CYCLE as f32);
                current_a = rms_mv / EFFECTIVE_SENSITIVITY;
                mode_str = "AC+DC";
            }
            _ => {
                current_a = 0.0;
                mode_str = "???";
            }
        }

        // --- Stats ---
        let mut min_val = u16::MAX;
        let mut max_val = 0u16;
        let mut sum: u32 = 0;
        for &s in samples.iter() {
            if s < min_val {
                min_val = s;
            }
            if s > max_val {
                max_val = s;
            }
            sum += s as u32;
        }
        let mean = sum as f32 / SAMPLES_PER_CYCLE as f32;
        let span = max_val - min_val;

        // Estimated power (assuming 230V mains)
        let power_w = libm::fabsf(current_a) * 230.0;

        // Print results
        println!(
            "[{}] {} Current = {:.2} A | Power ~ {:.1} W | ADC min={} max={} span={} mean={:.0}",
            iteration, mode_str, current_a, power_w, min_val, max_val, span, mean
        );

        // Every 10th iteration, print waveform for debugging
        if iteration % 10 == 1 {
            println!("  Waveform (time_ms, adc_raw, mv_at_adc):");
            // Print every 10th sample to keep output manageable
            for i in (0..SAMPLES_PER_CYCLE).step_by(10) {
                let time_ms = (i as f32 * 20.0) / SAMPLES_PER_CYCLE as f32;
                let mv = samples[i] as f32 * MV_PER_STEP;
                println!("  {:.1}ms: {} ({:.0} mV)", time_ms, samples[i], mv);
            }
        }

        // Wait between readings
        let wait_start = Instant::now();
        while wait_start.elapsed() < Duration::from_secs(1) {}
    }
}
