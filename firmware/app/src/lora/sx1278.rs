//! LoRa driver for SX1276/SX1278-based modules (e.g. HKD Ra-01, 433 MHz)

use embedded_hal::spi::SpiBus;
use esp_hal::gpio::{Input, Output};
use esp_hal::time::{Duration, Instant};

use super::{delay_ms, LoRaDriver, LoRaStatus};

// SX1278 Registers
const REG_FIFO: u8 = 0x00;
const REG_OP_MODE: u8 = 0x01;
const REG_FRF_MSB: u8 = 0x06;
const REG_FRF_MID: u8 = 0x07;
const REG_FRF_LSB: u8 = 0x08;
const REG_PA_CONFIG: u8 = 0x09;
const REG_FIFO_ADDR_PTR: u8 = 0x0D;
const REG_FIFO_TX_BASE_ADDR: u8 = 0x0E;
const REG_FIFO_RX_BASE_ADDR: u8 = 0x0F;
const REG_FIFO_RX_CURRENT_ADDR: u8 = 0x10;
const REG_IRQ_FLAGS: u8 = 0x12;
const REG_RX_NB_BYTES: u8 = 0x13;
const REG_RSSI_VALUE: u8 = 0x1B;
const REG_MODEM_CONFIG_1: u8 = 0x1D;
const REG_MODEM_CONFIG_2: u8 = 0x1E;
const REG_PREAMBLE_MSB: u8 = 0x20;
const REG_PREAMBLE_LSB: u8 = 0x21;
const REG_PAYLOAD_LENGTH: u8 = 0x22;
const REG_MODEM_CONFIG_3: u8 = 0x26;
const REG_DIO_MAPPING_1: u8 = 0x40;
const REG_VERSION: u8 = 0x42;

// Operating modes
const MODE_SLEEP: u8 = 0x00;
const MODE_STDBY: u8 = 0x01;
const MODE_TX: u8 = 0x03;
const MODE_RX_CONTINUOUS: u8 = 0x05;
const MODE_LORA: u8 = 0x80;

// IRQ flags
const IRQ_TX_DONE: u8 = 0x08;
const IRQ_RX_DONE: u8 = 0x40;

const EXPECTED_VERSION: u8 = 0x12;

pub struct Sx1278<'a, SPI> {
    spi: SPI,
    nss: Output<'a>,
    rst: Output<'a>,
    dio0: Input<'a>,
}

impl<'a, SPI: SpiBus> Sx1278<'a, SPI> {
    pub fn new(spi: SPI, nss: Output<'a>, rst: Output<'a>, dio0: Input<'a>) -> Self {
        Self { spi, nss, rst, dio0 }
    }

    fn configure(&mut self) {
        let version = self.read_register(REG_VERSION);
        if version != EXPECTED_VERSION {
            return;
        }

        for attempt in 0..5 {
            self.write_register(REG_OP_MODE, MODE_SLEEP | MODE_LORA);
            delay_ms(15);
            let mode = self.read_register(REG_OP_MODE);
            esp_println::println!(
                "{{\"event\":\"lora_mode_set\",\"attempt\":{},\"wrote\":\"0x{:02x}\",\"readback\":\"0x{:02x}\"}}",
                attempt, MODE_SLEEP | MODE_LORA, mode
            );
            if mode & MODE_LORA != 0 {
                break;
            }
            delay_ms(50);
        }

        // Frequency: 433 MHz
        self.write_register(REG_FRF_MSB, 0x6C);
        self.write_register(REG_FRF_MID, 0x40);
        self.write_register(REG_FRF_LSB, 0x00);

        self.write_register(REG_FIFO_TX_BASE_ADDR, 0x00);
        self.write_register(REG_FIFO_RX_BASE_ADDR, 0x00);

        // PA_BOOST, max power
        self.write_register(REG_PA_CONFIG, 0x8F);

        // BW=125kHz, CR=4/5, Explicit header
        self.write_register(REG_MODEM_CONFIG_1, 0x72);
        // SF=7, CRC on
        self.write_register(REG_MODEM_CONFIG_2, 0x74);
        // LNA gain, auto AGC
        self.write_register(REG_MODEM_CONFIG_3, 0x04);

        // Preamble length = 8
        self.write_register(REG_PREAMBLE_MSB, 0x00);
        self.write_register(REG_PREAMBLE_LSB, 0x08);

        // DIO0 mapping for RxDone
        self.write_register(REG_DIO_MAPPING_1, 0x00);

        // Standby
        self.write_register(REG_OP_MODE, MODE_STDBY | MODE_LORA);
        delay_ms(10);

        let mode_check = self.read_register(REG_OP_MODE);
        let frf_check = self.read_register(REG_FRF_MSB);
        let pa_check = self.read_register(REG_PA_CONFIG);
        esp_println::println!(
            "{{\"event\":\"configure_done\",\"mode\":\"0x{:02x}\",\"frf_msb\":\"0x{:02x}\",\"pa\":\"0x{:02x}\"}}",
            mode_check, frf_check, pa_check
        );
    }

    fn reset_hw(&mut self) {
        self.rst.set_low();
        delay_ms(10);
        self.rst.set_high();
        delay_ms(100);
    }

    fn read_frequency(&mut self) -> f32 {
        let msb = self.read_register(REG_FRF_MSB) as u32;
        let mid = self.read_register(REG_FRF_MID) as u32;
        let lsb = self.read_register(REG_FRF_LSB) as u32;
        let raw = (msb << 16) | (mid << 8) | lsb;
        (raw as f32 * 32.0) / 524288.0
    }

    fn read_register(&mut self, reg: u8) -> u8 {
        let tx_buf = [reg & 0x7F, 0x00];
        let mut rx_buf = [0u8; 2];
        self.nss.set_low();
        delay_ms(1);
        let _ = self.spi.transfer(&mut rx_buf, &tx_buf);
        delay_ms(1);
        self.nss.set_high();
        rx_buf[1]
    }

    fn write_register(&mut self, reg: u8, value: u8) {
        let tx_buf = [reg | 0x80, value];
        let mut rx_buf = [0u8; 2];
        self.nss.set_low();
        delay_ms(1);
        let _ = self.spi.transfer(&mut rx_buf, &tx_buf);
        delay_ms(1);
        self.nss.set_high();
    }
}

impl<'a, SPI: SpiBus> LoRaDriver for Sx1278<'a, SPI> {
    fn init(&mut self) -> LoRaStatus {
        self.reset_hw();
        self.configure();
        self.status()
    }

    fn transmit(&mut self, data: &[u8]) -> Option<Instant> {
        self.reset_hw();
        self.configure();

        self.write_register(REG_DIO_MAPPING_1, 0x40);
        self.write_register(REG_FIFO_ADDR_PTR, 0x00);

        for &byte in data {
            self.write_register(REG_FIFO, byte);
        }

        self.write_register(REG_PAYLOAD_LENGTH, data.len() as u8);
        self.write_register(REG_IRQ_FLAGS, 0xFF);
        self.write_register(REG_OP_MODE, MODE_TX | MODE_LORA);
        delay_ms(10);

        let mode = self.read_register(REG_OP_MODE);
        esp_println::println!(
            "{{\"event\":\"tx_debug\",\"mode\":\"0x{:02x}\",\"len\":{}}}",
            mode, data.len()
        );

        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(3000) {
            let irq = self.read_register(REG_IRQ_FLAGS);
            if irq & IRQ_TX_DONE != 0 {
                let tx_done = Instant::now();
                self.write_register(REG_IRQ_FLAGS, IRQ_TX_DONE);
                self.write_register(REG_OP_MODE, MODE_STDBY | MODE_LORA);
                return Some(tx_done);
            }
            delay_ms(10);
        }

        let irq_timeout = self.read_register(REG_IRQ_FLAGS);
        let mode_timeout = self.read_register(REG_OP_MODE);
        esp_println::println!(
            "{{\"event\":\"tx_timeout\",\"irq\":\"0x{:02x}\",\"mode\":\"0x{:02x}\"}}",
            irq_timeout, mode_timeout
        );
        self.write_register(REG_OP_MODE, MODE_STDBY | MODE_LORA);
        None
    }

    fn start_receive(&mut self) {
        self.write_register(REG_OP_MODE, MODE_STDBY | MODE_LORA);
        delay_ms(1);
        self.write_register(REG_FIFO_ADDR_PTR, 0x00);
        self.write_register(REG_FIFO_RX_BASE_ADDR, 0x00);
        self.write_register(REG_IRQ_FLAGS, 0xFF);
        self.write_register(REG_OP_MODE, MODE_RX_CONTINUOUS | MODE_LORA);
    }

    fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        let irq = self.read_register(REG_IRQ_FLAGS);
        if irq & IRQ_RX_DONE != 0 {
            let len = self.read_register(REG_RX_NB_BYTES) as usize;
            let len = len.min(buf.len());
            let rx_addr = self.read_register(REG_FIFO_RX_CURRENT_ADDR);
            self.write_register(REG_FIFO_ADDR_PTR, rx_addr);
            for i in 0..len {
                buf[i] = self.read_register(REG_FIFO);
            }
            self.write_register(REG_IRQ_FLAGS, 0xFF);
            Some(len)
        } else {
            None
        }
    }

    fn rssi(&mut self) -> i16 {
        let rssi_raw = self.read_register(REG_RSSI_VALUE) as i16;
        rssi_raw - 137
    }

    fn status(&mut self) -> LoRaStatus {
        let version = self.read_register(REG_VERSION);
        let op_mode = self.read_register(REG_OP_MODE);
        let frequency_mhz = self.read_frequency();
        LoRaStatus {
            detected: version == EXPECTED_VERSION,
            version,
            op_mode,
            frequency_mhz,
        }
    }
}
