//! LoRa driver for SX1261/SX1262-based modules (e.g. Ai-Thinker Ra-01SH, 868 MHz)

use embedded_hal::spi::SpiBus;
use esp_hal::gpio::{Input, Output};
use esp_hal::time::{Duration, Instant};

use super::{delay_ms, Bandwidth, LoRaDriver, LoRaStatus};

// SX1262 opcodes
const CMD_SET_STANDBY: u8 = 0x80;
const CMD_SET_TX: u8 = 0x83;
const CMD_SET_RX: u8 = 0x82;
const CMD_SET_PACKET_TYPE: u8 = 0x8A;
const CMD_SET_RF_FREQUENCY: u8 = 0x86;
const CMD_SET_PA_CONFIG: u8 = 0x95;
const CMD_SET_TX_PARAMS: u8 = 0x8E;
const CMD_SET_BUFFER_BASE_ADDRESS: u8 = 0x8F;
const CMD_SET_MODULATION_PARAMS: u8 = 0x8B;
const CMD_SET_PACKET_PARAMS: u8 = 0x8C;
const CMD_SET_DIO_IRQ_PARAMS: u8 = 0x08;
const CMD_GET_STATUS: u8 = 0xC0;
const CMD_GET_IRQ_STATUS: u8 = 0x12;
const CMD_CLEAR_IRQ_STATUS: u8 = 0x02;
const CMD_GET_RX_BUFFER_STATUS: u8 = 0x13;
const CMD_GET_RSSI_INST: u8 = 0x15;
const CMD_READ_REGISTER: u8 = 0x1D;
const CMD_WRITE_REGISTER: u8 = 0x0D;
const CMD_WRITE_BUFFER: u8 = 0x0E;
const CMD_READ_BUFFER: u8 = 0x1E;

// Standby modes
const STDBY_RC: u8 = 0x00;

// Packet type
const PACKET_TYPE_LORA: u8 = 0x01;

// IRQ masks
const IRQ_TX_DONE: u16 = 0x0001;
const IRQ_RX_DONE: u16 = 0x0002;
const IRQ_TIMEOUT: u16 = 0x0200;

// LoRa sync word register
const REG_LORA_SYNC_WORD_MSB: u16 = 0x0740;

/// SX1262 errata 15.4: this register must be adjusted whenever IQ polarity changes.
const REG_IQ_POLARITY_SETUP: u16 = 0x0736;

// Default frequency: 868 MHz
const DEFAULT_FREQ_HZ: u32 = 868_100_000;

/// SetRx/SetTx timeout unit is 15.625 µs, so one millisecond is 64 ticks.
const TIMEOUT_TICKS_PER_MS: u32 = 64;

pub struct Sx1262<'a, SPI> {
    spi: SPI,
    nss: Output<'a>,
    rst: Output<'a>,
    /// The module's DIO1 interrupt line.
    ///
    /// Note this board does not bring BUSY out to a GPIO — the schematic wires
    /// DIO1 here instead. Commands therefore cannot wait on BUSY and use a
    /// fixed settling delay, while TxDone/RxDone/Timeout are detected from this
    /// pin, which is both faster and far more precise than polling over SPI.
    dio1: Input<'a>,
    freq_hz: u32,
    sf: u8,
    bw: Bandwidth,
    iq_inverted: bool,
}

impl<'a, SPI: SpiBus> Sx1262<'a, SPI> {
    pub fn new(spi: SPI, nss: Output<'a>, rst: Output<'a>, dio1: Input<'a>) -> Self {
        Self {
            spi,
            nss,
            rst,
            dio1,
            freq_hz: DEFAULT_FREQ_HZ,
            sf: 7,
            bw: Bandwidth::Bw125,
            iq_inverted: false,
        }
    }

    /// Give the radio time to finish the previous command.
    ///
    /// With no BUSY line to watch, this is a fixed wait. 200 µs comfortably
    /// covers every command we issue; the ones that take longer (reset, and
    /// the standby transition after it) get their own explicit delays.
    fn settle(&self) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_micros(200) {}
    }

    /// Whether DIO1 is asserted, i.e. one of the mapped IRQs has fired.
    ///
    /// Reading a GPIO costs nothing next to an SPI transaction, so completion
    /// is detected here first and the IRQ register is only read once it is
    /// worth reading.
    fn irq_pending(&self) -> bool {
        self.dio1.is_high()
    }

    fn reset_hw(&mut self) {
        self.rst.set_low();
        delay_ms(1);
        self.rst.set_high();
        delay_ms(10);
    }

    fn cmd(&mut self, buf: &[u8]) {
        self.settle();
        self.nss.set_low();
        let mut rx = [0u8; 1];
        for &b in buf {
            let _ = self.spi.transfer(&mut rx, &[b]);
        }
        self.nss.set_high();
    }

    fn cmd_read(&mut self, opcode: u8, params: &[u8], out: &mut [u8]) {
        self.settle();
        self.nss.set_low();
        let mut rx = [0u8; 1];
        let _ = self.spi.transfer(&mut rx, &[opcode]);
        for &b in params {
            let _ = self.spi.transfer(&mut rx, &[b]);
        }
        // NOP for status byte
        let _ = self.spi.transfer(&mut rx, &[0x00]);
        for byte in out.iter_mut() {
            let _ = self.spi.transfer(&mut rx, &[0x00]);
            *byte = rx[0];
        }
        self.nss.set_high();
    }

    fn get_status_byte(&mut self) -> u8 {
        self.settle();
        let tx = [CMD_GET_STATUS, 0x00];
        let mut rx = [0u8; 2];
        self.nss.set_low();
        let _ = self.spi.transfer(&mut rx, &tx);
        self.nss.set_high();
        rx[1]
    }

    fn get_irq_status(&mut self) -> u16 {
        let mut out = [0u8; 2];
        self.cmd_read(CMD_GET_IRQ_STATUS, &[], &mut out);
        ((out[0] as u16) << 8) | out[1] as u16
    }

    fn clear_irq(&mut self, mask: u16) {
        self.cmd(&[
            CMD_CLEAR_IRQ_STATUS,
            (mask >> 8) as u8,
            mask as u8,
        ]);
    }

    fn read_register(&mut self, addr: u16) -> u8 {
        let mut out = [0u8; 1];
        self.cmd_read(
            CMD_READ_REGISTER,
            &[(addr >> 8) as u8, addr as u8],
            &mut out,
        );
        out[0]
    }

    fn write_register(&mut self, addr: u16, value: u8) {
        self.cmd(&[
            CMD_WRITE_REGISTER,
            (addr >> 8) as u8,
            addr as u8,
            value,
        ]);
    }

    fn write_buffer(&mut self, offset: u8, data: &[u8]) {
        self.settle();
        self.nss.set_low();
        let mut rx = [0u8; 1];
        let _ = self.spi.transfer(&mut rx, &[CMD_WRITE_BUFFER]);
        let _ = self.spi.transfer(&mut rx, &[offset]);
        for &b in data {
            let _ = self.spi.transfer(&mut rx, &[b]);
        }
        self.nss.set_high();
    }

    fn read_buffer(&mut self, offset: u8, buf: &mut [u8]) {
        self.settle();
        self.nss.set_low();
        let mut rx = [0u8; 1];
        let _ = self.spi.transfer(&mut rx, &[CMD_READ_BUFFER]);
        let _ = self.spi.transfer(&mut rx, &[offset]);
        // NOP for status
        let _ = self.spi.transfer(&mut rx, &[0x00]);
        for byte in buf.iter_mut() {
            let _ = self.spi.transfer(&mut rx, &[0x00]);
            *byte = rx[0];
        }
        self.nss.set_high();
    }

    /// Write the RF frequency currently held in `self.freq_hz`.
    fn apply_frequency(&mut self) {
        let freq_reg = ((self.freq_hz as u64) << 25) / 32_000_000;
        self.cmd(&[
            CMD_SET_RF_FREQUENCY,
            (freq_reg >> 24) as u8,
            (freq_reg >> 16) as u8,
            (freq_reg >> 8) as u8,
            freq_reg as u8,
        ]);
    }

    /// Write the modulation parameters for the current SF/BW.
    ///
    /// LowDataRateOptimize is mandatory when a symbol lasts longer than 16.38 ms,
    /// which for BW125 means SF11 and SF12 — i.e. the RX2 data rates.
    fn apply_modulation_params(&mut self) {
        let low_dr_opt = match (self.sf, self.bw) {
            (11..=12, Bandwidth::Bw125) => 0x01,
            (12, Bandwidth::Bw250) => 0x01,
            _ => 0x00,
        };
        self.cmd(&[
            CMD_SET_MODULATION_PARAMS,
            self.sf,
            self.bw as u8,
            0x01, // CR 4/5
            low_dr_opt,
        ]);
    }

    /// Write the packet parameters for a given payload length.
    ///
    /// Pass 0xFF for receive, where the length is not known in advance.
    fn apply_packet_params(&mut self, payload_len: u8) {
        self.cmd(&[
            CMD_SET_PACKET_PARAMS,
            0x00,
            0x08, // preamble 8 symbols
            0x00, // explicit header
            payload_len,
            0x01, // CRC on
            if self.iq_inverted { 0x01 } else { 0x00 },
        ]);
    }

    /// Apply the errata 15.4 workaround that must accompany every IQ change.
    fn apply_iq_workaround(&mut self) {
        let value = self.read_register(REG_IQ_POLARITY_SETUP);
        let patched = if self.iq_inverted {
            value & 0xFB
        } else {
            value | 0x04
        };
        self.write_register(REG_IQ_POLARITY_SETUP, patched);
    }

    /// Read a completed packet out of the radio's RX buffer.
    fn read_rx_payload(&mut self, buf: &mut [u8]) -> Option<usize> {
        // GetRxBufferStatus: returns [payloadLen, rxStartBufferPointer]
        let mut rx_info = [0u8; 2];
        self.cmd_read(CMD_GET_RX_BUFFER_STATUS, &[], &mut rx_info);
        let len = (rx_info[0] as usize).min(buf.len());
        let offset = rx_info[1];

        self.read_buffer(offset, &mut buf[..len]);
        self.clear_irq(0xFFFF);
        Some(len)
    }

    fn configure(&mut self) {
        // Standby RC
        self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
        delay_ms(5);

        // Packet type: LoRa
        self.cmd(&[CMD_SET_PACKET_TYPE, PACKET_TYPE_LORA]);

        self.apply_frequency();

        // PA config for SX1262: paDutyCycle=0x04, hpMax=0x07, deviceSel=0x00 (SX1262), paLut=0x01
        self.cmd(&[CMD_SET_PA_CONFIG, 0x04, 0x07, 0x00, 0x01]);

        // TX params: power=+22 dBm, rampTime=200us (0x04)
        self.cmd(&[CMD_SET_TX_PARAMS, 0x16, 0x04]);

        self.apply_modulation_params();
        self.apply_packet_params(0xFF);
        self.apply_iq_workaround();

        // Buffer base addresses: TX=0, RX=128
        self.cmd(&[CMD_SET_BUFFER_BASE_ADDRESS, 0x00, 0x80]);

        // LoRa sync word: 0x3444 (public/LoRaWAN network)
        self.write_register(REG_LORA_SYNC_WORD_MSB, 0x34);
        self.write_register(REG_LORA_SYNC_WORD_MSB + 1, 0x44);

        // DIO1 IRQ: enable TxDone + RxDone + Timeout on DIO1
        let irq_mask = IRQ_TX_DONE | IRQ_RX_DONE | IRQ_TIMEOUT;
        self.cmd(&[
            CMD_SET_DIO_IRQ_PARAMS,
            (irq_mask >> 8) as u8, irq_mask as u8, // IRQ mask
            (irq_mask >> 8) as u8, irq_mask as u8, // DIO1 mask
            0x00, 0x00, // DIO2 mask
            0x00, 0x00, // DIO3 mask
        ]);

        // Clear any pending IRQs
        self.clear_irq(0xFFFF);

        let status = self.get_status_byte();
        let sync_msb = self.read_register(REG_LORA_SYNC_WORD_MSB);
        let sync_lsb = self.read_register(REG_LORA_SYNC_WORD_MSB + 1);
        esp_println::println!(
            "{{\"event\":\"configure_done\",\"chip\":\"sx1262\",\"status\":\"0x{:02x}\",\"sync\":\"0x{:02x}{:02x}\",\"freq_mhz\":{:.1}}}",
            status, sync_msb, sync_lsb, self.freq_hz as f32 / 1_000_000.0
        );
    }
}

impl<'a, SPI: SpiBus> LoRaDriver for Sx1262<'a, SPI> {
    fn init(&mut self) -> LoRaStatus {
        self.reset_hw();
        self.configure();
        self.status()
    }

    fn transmit(&mut self, data: &[u8]) -> Option<Instant> {
        // Ensure standby
        self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
        delay_ms(1);

        self.apply_packet_params(data.len() as u8);

        // Write payload to TX buffer (offset 0)
        self.write_buffer(0x00, data);

        // Clear IRQs
        self.clear_irq(0xFFFF);

        // Start TX (timeout 0 = no timeout, use our own)
        self.cmd(&[CMD_SET_TX, 0x00, 0x00, 0x00]);

        // Wait for TxDone on DIO1. The instant it asserts is the reference for
        // the RX1/RX2 windows, so it is timestamped before anything else
        // happens — every microsecond of slop here has to be paid for with a
        // wider receive window later.
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(3000) {
            if self.irq_pending() {
                let tx_done = Instant::now();
                let irq = self.get_irq_status();
                if irq & IRQ_TX_DONE != 0 {
                    self.clear_irq(0xFFFF);
                    self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
                    return Some(tx_done);
                }
                self.clear_irq(0xFFFF);
            }
        }

        let irq = self.get_irq_status();
        let status = self.get_status_byte();
        esp_println::println!(
            "{{\"event\":\"tx_timeout\",\"chip\":\"sx1262\",\"irq\":\"0x{:04x}\",\"status\":\"0x{:02x}\"}}",
            irq, status
        );
        self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
        None
    }

    fn start_receive(&mut self) {
        self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
        delay_ms(1);

        self.apply_packet_params(0xFF);
        self.clear_irq(0xFFFF);

        // RX continuous (timeout = 0xFFFFFF)
        self.cmd(&[CMD_SET_RX, 0xFF, 0xFF, 0xFF]);
    }

    fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        let irq = self.get_irq_status();
        if irq & IRQ_RX_DONE != 0 {
            self.read_rx_payload(buf)
        } else {
            if irq & IRQ_TIMEOUT != 0 {
                self.clear_irq(IRQ_TIMEOUT);
            }
            None
        }
    }

    fn receive_with_timeout(&mut self, buf: &mut [u8], timeout_ms: u32) -> Option<usize> {
        self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
        self.apply_packet_params(0xFF);
        self.clear_irq(0xFFFF);

        // The radio's own timeout stops it listening; ours is only a backstop
        // in case the IRQ never materialises.
        let ticks = timeout_ms.saturating_mul(TIMEOUT_TICKS_PER_MS).min(0x00FF_FFFF);
        self.cmd(&[
            CMD_SET_RX,
            (ticks >> 16) as u8,
            (ticks >> 8) as u8,
            ticks as u8,
        ]);

        // The radio's own timeout ends the window; ours is only a backstop in
        // case DIO1 never asserts.
        let start = Instant::now();
        // Generous, because the window bounds preamble detection while the
        // packet that follows can take far longer — an SF12 downlink is well
        // over a second on air.
        let backstop = Duration::from_millis(timeout_ms as u64 + 2500);
        while start.elapsed() < backstop {
            if !self.irq_pending() {
                continue;
            }
            let irq = self.get_irq_status();
            if irq & IRQ_RX_DONE != 0 {
                let result = self.read_rx_payload(buf);
                self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
                return result;
            }
            if irq & IRQ_TIMEOUT != 0 {
                self.clear_irq(0xFFFF);
                self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
                return None;
            }
            self.clear_irq(0xFFFF);
        }

        self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
        None
    }

    fn set_frequency(&mut self, hz: u32) {
        self.freq_hz = hz;
        self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
        self.apply_frequency();
    }

    fn set_datarate(&mut self, sf: u8, bw: Bandwidth) {
        self.sf = sf;
        self.bw = bw;
        self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
        self.apply_modulation_params();
    }

    fn set_iq_inverted(&mut self, inverted: bool) {
        if self.iq_inverted == inverted {
            return;
        }
        self.iq_inverted = inverted;
        self.cmd(&[CMD_SET_STANDBY, STDBY_RC]);
        self.apply_packet_params(0xFF);
        self.apply_iq_workaround();
    }

    fn rssi(&mut self) -> i16 {
        let mut out = [0u8; 1];
        self.cmd_read(CMD_GET_RSSI_INST, &[], &mut out);
        -(out[0] as i16) / 2
    }

    fn status(&mut self) -> LoRaStatus {
        let status = self.get_status_byte();
        let chip_mode = (status >> 4) & 0x07;
        let comms_ok = status != 0x00 && status != 0xFF;

        // Verify by reading sync word register
        let sync = self.read_register(REG_LORA_SYNC_WORD_MSB);
        let detected = comms_ok && (sync == 0x14 || sync == 0x34);

        LoRaStatus {
            detected,
            version: 0x62, // SX1262 identifier
            op_mode: chip_mode,
            frequency_mhz: self.freq_hz as f32 / 1_000_000.0,
        }
    }
}
