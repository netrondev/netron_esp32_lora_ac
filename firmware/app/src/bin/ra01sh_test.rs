//! Ra-01SH (SX1262) communication test
//!
//! Verifies SPI communication with the SX1262-based Ra-01SH module at 868MHz.
//! Uses the same ESP32 GPIO pinout as the old SX1278 setup:
//!   GPIO18: SCK
//!   GPIO19: MISO
//!   GPIO23: MOSI
//!   GPIO5:  NSS
//!   GPIO27: NRST
//!   GPIO26: BUSY

#![no_std]
#![no_main]

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

// --- SX1262 opcodes ---
const CMD_GET_STATUS: u8 = 0xC0;
const CMD_SET_STANDBY: u8 = 0x80;
const CMD_SET_PACKET_TYPE: u8 = 0x8A;
const CMD_SET_RF_FREQUENCY: u8 = 0x86;
const CMD_READ_REGISTER: u8 = 0x1D;
const CMD_WRITE_REGISTER: u8 = 0x0D;
const CMD_GET_DEVICE_ERRORS: u8 = 0x17;

// Standby modes
const STDBY_RC: u8 = 0x00;

// Packet types
const PACKET_TYPE_LORA: u8 = 0x01;

// LoRa sync word register (0x0740-0x0741)
const REG_LORA_SYNC_WORD_MSB: u16 = 0x0740;

fn delay_ms(ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {}
}

struct Sx1262<'a, SPI> {
    spi: SPI,
    nss: Output<'a>,
    rst: Output<'a>,
    busy: Input<'a>,
}

impl<'a, SPI: embedded_hal::spi::SpiBus> Sx1262<'a, SPI> {
    fn wait_busy(&self, label: &str) -> bool {
        let start = Instant::now();
        while self.busy.is_high() {
            if start.elapsed() > Duration::from_millis(1000) {
                println!("  BUSY timeout waiting for: {}", label);
                return false;
            }
        }
        true
    }

    fn reset(&mut self) {
        self.rst.set_low();
        delay_ms(1);
        self.rst.set_high();
        delay_ms(10);
    }

    fn get_status(&mut self) -> u8 {
        self.wait_busy("get_status");
        let tx = [CMD_GET_STATUS, 0x00];
        let mut rx = [0u8; 2];
        self.nss.set_low();
        let _ = self.spi.transfer(&mut rx, &tx);
        self.nss.set_high();
        rx[1]
    }

    fn set_standby(&mut self, mode: u8) {
        self.wait_busy("set_standby");
        let tx = [CMD_SET_STANDBY, mode];
        let mut rx = [0u8; 2];
        self.nss.set_low();
        let _ = self.spi.transfer(&mut rx, &tx);
        self.nss.set_high();
    }

    fn set_packet_type(&mut self, ptype: u8) {
        self.wait_busy("set_packet_type");
        let tx = [CMD_SET_PACKET_TYPE, ptype];
        let mut rx = [0u8; 2];
        self.nss.set_low();
        let _ = self.spi.transfer(&mut rx, &tx);
        self.nss.set_high();
    }

    fn set_rf_frequency(&mut self, freq_hz: u32) {
        // freq_reg = freq_hz * 2^25 / 32_000_000
        let freq_reg = ((freq_hz as u64) << 25) / 32_000_000;
        self.wait_busy("set_rf_frequency");
        let tx = [
            CMD_SET_RF_FREQUENCY,
            (freq_reg >> 24) as u8,
            (freq_reg >> 16) as u8,
            (freq_reg >> 8) as u8,
            freq_reg as u8,
        ];
        let mut rx = [0u8; 5];
        self.nss.set_low();
        let _ = self.spi.transfer(&mut rx, &tx);
        self.nss.set_high();
    }

    fn read_register(&mut self, addr: u16) -> u8 {
        self.wait_busy("read_register");
        // Opcode + addr_msb + addr_lsb + NOP(status) + NOP(data)
        let tx = [
            CMD_READ_REGISTER,
            (addr >> 8) as u8,
            addr as u8,
            0x00, // status return
            0x00, // data return
        ];
        let mut rx = [0u8; 5];
        self.nss.set_low();
        let _ = self.spi.transfer(&mut rx, &tx);
        self.nss.set_high();
        rx[4]
    }

    fn write_register(&mut self, addr: u16, value: u8) {
        self.wait_busy("write_register");
        let tx = [
            CMD_WRITE_REGISTER,
            (addr >> 8) as u8,
            addr as u8,
            value,
        ];
        let mut rx = [0u8; 4];
        self.nss.set_low();
        let _ = self.spi.transfer(&mut rx, &tx);
        self.nss.set_high();
    }

    fn get_device_errors(&mut self) -> u16 {
        self.wait_busy("get_device_errors");
        let tx = [CMD_GET_DEVICE_ERRORS, 0x00, 0x00, 0x00];
        let mut rx = [0u8; 4];
        self.nss.set_low();
        let _ = self.spi.transfer(&mut rx, &tx);
        self.nss.set_high();
        ((rx[2] as u16) << 8) | rx[3] as u16
    }
}

fn decode_status(status: u8) {
    let chip_mode = (status >> 4) & 0x07;
    let cmd_status = (status >> 1) & 0x07;
    let mode_str = match chip_mode {
        0x02 => "STDBY_RC",
        0x03 => "STDBY_XOSC",
        0x04 => "FS",
        0x05 => "RX",
        0x06 => "TX",
        _ => "UNKNOWN",
    };
    let cmd_str = match cmd_status {
        0x01 => "data_available",
        0x02 => "cmd_timeout",
        0x03 => "cmd_error",
        0x04 => "exec_failure",
        0x05 => "cmd_tx_done",
        0x06 => "cmd_ok",
        _ => "unknown",
    };
    println!(
        "  chip_mode=0x{:02x} ({}) cmd_status=0x{:02x} ({})",
        chip_mode, mode_str, cmd_status, cmd_str
    );
}

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    println!("\n=== Ra-01SH (SX1262) Communication Test ===");
    println!("Pinout: SCK=18 MISO=19 MOSI=23 NSS=5 RST=27 BUSY=26");

    let rst = Output::new(peripherals.GPIO27, Level::High, OutputConfig::default());
    let nss = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
    let busy = Input::new(peripherals.GPIO26, InputConfig::default());

    let spi_config = SpiConfig::default().with_frequency(Rate::from_mhz(1));
    let spi = Spi::new(peripherals.SPI2, spi_config)
        .unwrap()
        .with_sck(peripherals.GPIO18)
        .with_miso(peripherals.GPIO19)
        .with_mosi(peripherals.GPIO23);

    let mut radio = Sx1262 { spi, nss, rst, busy };

    // --- Step 1: Check BUSY pin state before reset ---
    println!("\n[1] Pre-reset BUSY pin: {}", if radio.busy.is_high() { "HIGH" } else { "LOW" });

    // --- Step 2: Reset ---
    println!("\n[2] Resetting module...");
    radio.reset();
    let busy_after = if radio.busy.is_high() { "HIGH" } else { "LOW" };
    println!("  BUSY after reset: {}", busy_after);

    // Wait for BUSY to go low (module ready)
    let ready = radio.wait_busy("post-reset");
    println!("  Module ready: {}", ready);

    // --- Step 3: GetStatus ---
    println!("\n[3] GetStatus:");
    let status = radio.get_status();
    println!("  Raw status byte: 0x{:02x}", status);
    decode_status(status);

    // --- Step 4: SetStandby(RC) ---
    println!("\n[4] SetStandby(RC)...");
    radio.set_standby(STDBY_RC);
    delay_ms(5);
    let status = radio.get_status();
    println!("  Status after standby: 0x{:02x}", status);
    decode_status(status);

    // --- Step 5: Read LoRa sync word register (default value test) ---
    println!("\n[5] Read LoRa sync word register (0x0740):");
    let sync_msb = radio.read_register(REG_LORA_SYNC_WORD_MSB);
    let sync_lsb = radio.read_register(REG_LORA_SYNC_WORD_MSB + 1);
    println!("  Sync word: 0x{:02x}{:02x}", sync_msb, sync_lsb);

    // --- Step 6: SetPacketType(LoRa) and re-read sync word ---
    println!("\n[6] SetPacketType(LoRa)...");
    radio.set_packet_type(PACKET_TYPE_LORA);
    delay_ms(5);
    let sync_msb = radio.read_register(REG_LORA_SYNC_WORD_MSB);
    let sync_lsb = radio.read_register(REG_LORA_SYNC_WORD_MSB + 1);
    println!("  Sync word after LoRa mode: 0x{:02x}{:02x}", sync_msb, sync_lsb);
    println!("  (expect 0x1424 for private network)");

    // --- Step 7: Register write/readback test ---
    println!("\n[7] Register write/readback test (sync word):");
    radio.write_register(REG_LORA_SYNC_WORD_MSB, 0x34);
    radio.write_register(REG_LORA_SYNC_WORD_MSB + 1, 0x44);
    let rb_msb = radio.read_register(REG_LORA_SYNC_WORD_MSB);
    let rb_lsb = radio.read_register(REG_LORA_SYNC_WORD_MSB + 1);
    println!("  Wrote: 0x3444, Read: 0x{:02x}{:02x}", rb_msb, rb_lsb);
    println!("  Match: {}", rb_msb == 0x34 && rb_lsb == 0x44);
    // Restore private sync word
    radio.write_register(REG_LORA_SYNC_WORD_MSB, 0x14);
    radio.write_register(REG_LORA_SYNC_WORD_MSB + 1, 0x24);

    // --- Step 8: Set frequency to 868 MHz ---
    println!("\n[8] SetRfFrequency(868 MHz)...");
    radio.set_rf_frequency(868_000_000);
    delay_ms(5);
    let status = radio.get_status();
    println!("  Status after freq set: 0x{:02x}", status);
    decode_status(status);

    // --- Step 9: Get device errors ---
    println!("\n[9] GetDeviceErrors:");
    let errors = radio.get_device_errors();
    println!("  Error flags: 0x{:04x}", errors);
    if errors == 0 {
        println!("  No errors");
    } else {
        if errors & 0x01 != 0 { println!("  - RC64K calib error"); }
        if errors & 0x02 != 0 { println!("  - RC13M calib error"); }
        if errors & 0x04 != 0 { println!("  - PLL calib error"); }
        if errors & 0x08 != 0 { println!("  - ADC calib error"); }
        if errors & 0x10 != 0 { println!("  - IMG calib error"); }
        if errors & 0x20 != 0 { println!("  - XOSC start error"); }
        if errors & 0x40 != 0 { println!("  - PLL lock error"); }
        if errors & 0x100 != 0 { println!("  - PA ramp error"); }
    }

    // --- Summary ---
    let comms_ok = status != 0x00 && status != 0xFF;
    println!("\n=== RESULT ===");
    if comms_ok {
        println!("SX1262 communication: OK");
    } else {
        println!("SX1262 communication: FAILED");
        println!("  Status 0x00 or 0xFF suggests no SPI response.");
        println!("  Check wiring and that BUSY pin is correct (GPIO26).");
    }

    println!("\nDone. Halting.");
    loop {
        delay_ms(1000);
    }
}
