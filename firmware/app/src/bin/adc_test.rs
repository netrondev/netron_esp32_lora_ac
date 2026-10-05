//! ADC test - Heavy oversampling per sample point
//! Uses 64x oversampling at each sample point to reduce noise

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

const MV_PER_STEP: f32 = 3300.0 / 4095.0;
const SENSITIVITY_MV_PER_A: f32 = 40.0;

// 200 samples per cycle at 50Hz = 10kHz effective rate
// Each sample is 64x oversampled
const SAMPLES_PER_CYCLE: usize = 200;
const OVERSAMPLE: usize = 64;

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    println!("\n\n=== Heavy Oversampling AC Capture ===");
    println!("200 samples/cycle, {}x oversample each", OVERSAMPLE);
    println!("");

    let mut adc_config = AdcConfig::new();
    let mut adc_pin = adc_config.enable_pin(peripherals.GPIO34, Attenuation::_11dB);
    let mut adc = Adc::new(peripherals.ADC1, adc_config);

    let mut samples: [u16; SAMPLES_PER_CYCLE] = [0; SAMPLES_PER_CYCLE];
    let mut iteration = 0u32;

    loop {
        iteration += 1;
        println!("\n--- Iteration {} ---", iteration);

        // Target: 200 samples in 20ms = 100us per sample
        // With 64x oversampling, we need to read fast
        let capture_start = Instant::now();

        for i in 0..SAMPLES_PER_CYCLE {
            // Target time for this sample
            let target_us = (i as u64 * 20000) / SAMPLES_PER_CYCLE as u64;

            // Take 64 readings and average
            let mut sum: u32 = 0;
            for _ in 0..OVERSAMPLE {
                let raw: u16 = block!(adc.read_oneshot(&mut adc_pin)).unwrap_or(0);
                sum += raw as u32;
            }
            samples[i] = (sum / OVERSAMPLE as u32) as u16;

            // Wait until target time if needed
            let target_time = capture_start + Duration::from_micros(target_us);
            while Instant::now() < target_time {}
        }

        let capture_time = capture_start.elapsed().as_micros();
        println!("Captured {} samples in {} us", SAMPLES_PER_CYCLE, capture_time);

        // Find min/max/mean
        let mut min_val = u16::MAX;
        let mut max_val = 0u16;
        let mut sum: u32 = 0;
        for &s in samples.iter() {
            if s < min_val { min_val = s; }
            if s > max_val { max_val = s; }
            sum += s as u32;
        }
        let mean = sum as f32 / SAMPLES_PER_CYCLE as f32;
        let amplitude = (max_val - min_val) as f32 / 2.0;

        println!("ADC: min={}, max={}, span={}, mean={:.1}", min_val, max_val, max_val - min_val, mean);

        // Calculate RMS
        let mut sum_sq: f32 = 0.0;
        for &s in samples.iter() {
            let diff = s as f32 - mean;
            sum_sq += diff * diff;
        }
        let rms_adc = libm::sqrtf(sum_sq / SAMPLES_PER_CYCLE as f32);
        let rms_current = rms_adc * MV_PER_STEP / SENSITIVITY_MV_PER_A;

        println!("RMS: ADC={:.2}, Current={:.3} A, Power={:.1} W", rms_adc, rms_current, rms_current * 230.0);

        // Find zero crossings for frequency
        let mean_u16 = mean as u16;
        let mut crossings = 0u32;
        let mut last_above = samples[0] > mean_u16;
        let mut first_crossing = 0usize;
        let mut last_crossing = 0usize;

        for i in 1..SAMPLES_PER_CYCLE {
            let above = samples[i] > mean_u16;
            if above != last_above {
                if crossings == 0 {
                    first_crossing = i;
                }
                last_crossing = i;
                crossings += 1;
                last_above = above;
            }
        }

        if crossings >= 2 {
            let span_samples = last_crossing - first_crossing;
            let span_us = (span_samples as u64 * 20000) / SAMPLES_PER_CYCLE as u64;
            let half_cycles = crossings - 1;
            let freq = (half_cycles as f32 / 2.0) * 1_000_000.0 / span_us as f32;
            println!("Crossings: {}, span_samples={}, freq={:.2} Hz", crossings, span_samples, freq);
        }

        // Find peaks
        let mut peak_pos = 0;
        let mut trough_pos = 0;
        for i in 0..SAMPLES_PER_CYCLE {
            if samples[i] > samples[peak_pos] { peak_pos = i; }
            if samples[i] < samples[trough_pos] { trough_pos = i; }
        }
        let peak_time_ms = (peak_pos as f32 * 20.0) / SAMPLES_PER_CYCLE as f32;
        let trough_time_ms = (trough_pos as f32 * 20.0) / SAMPLES_PER_CYCLE as f32;
        println!("Peak at {:.1}ms ({}), Trough at {:.1}ms ({})",
                 peak_time_ms, samples[peak_pos], trough_time_ms, samples[trough_pos]);

        // Print waveform
        println!("\nWaveform (time_ms,adc):");
        for i in 0..SAMPLES_PER_CYCLE {
            let time_ms = (i as f32 * 20.0) / SAMPLES_PER_CYCLE as f32;
            println!("{:.2},{}", time_ms, samples[i]);
        }

        // Wait
        let wait_start = Instant::now();
        while wait_start.elapsed() < Duration::from_secs(3) {}
    }
}
