//! LoRa RX test - Listens for packets and prints them.
//! Flash to one ESP32, flash tx_test.rs to another, verify comms.

#![no_std]
#![no_main]

use app::lora::sx1262::Sx1262;
use app::lora::LoRaDriver;
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

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    println!("\n=== LoRa RX Test (SX1262 / Ra-01SH 868MHz) ===");

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
        "LoRa: detected={}, version=0x{:02x}, freq={:.1}MHz, mode=0x{:02x}",
        status.detected, status.version, status.frequency_mhz, status.op_mode
    );

    if !status.detected {
        println!("ERROR: SX1262 not detected! Check wiring.");
        loop {
            core::hint::spin_loop();
        }
    }

    println!("Listening for packets...\n");
    lora.start_receive();

    let mut rx_count: u32 = 0;
    let mut last_status = Instant::now();

    loop {
        let mut buf = [0u8; 64];
        if let Some(len) = lora.receive(&mut buf) {
            rx_count += 1;
            let rssi = lora.rssi();

            // Print hex dump
            let mut hex = [0u8; 128];
            let hex_len = len.min(64);
            for i in 0..hex_len {
                let lut = b"0123456789abcdef";
                hex[i * 2] = lut[(buf[i] >> 4) as usize];
                hex[i * 2 + 1] = lut[(buf[i] & 0x0F) as usize];
            }
            let hex_str = core::str::from_utf8(&hex[..hex_len * 2]).unwrap_or("?");

            println!(
                "RX #{}: {} bytes, rssi={}, hex={}",
                rx_count, len, rssi, hex_str
            );

            // If it looks like a tx_test packet ("TX" + 4-byte seq)
            if len >= 6 && buf[0] == b'T' && buf[1] == b'X' {
                let seq = buf[2] as u32
                    | (buf[3] as u32) << 8
                    | (buf[4] as u32) << 16
                    | (buf[5] as u32) << 24;
                println!("  -> tx_test packet, seq={}", seq);
            }

            lora.start_receive();
        }

        // Print a heartbeat every 10 seconds so we know it's alive
        if last_status.elapsed() > Duration::from_secs(10) {
            last_status = Instant::now();
            let mode = lora.status().op_mode;
            println!("[heartbeat] rx_count={}, mode=0x{:02x}", rx_count, mode);

            // Re-enter RX mode in case it dropped out
            lora.start_receive();
        }

        // Small busy-wait to avoid hammering SPI
        let pause = Instant::now();
        while pause.elapsed() < Duration::from_millis(10) {}
    }
}
