//! LoRa radio abstraction layer.
//!
//! Provides a common `LoRaDriver` trait implemented by chip-specific drivers:
//! - `sx1278::Sx1278` — SX1276/SX1278 (e.g. Ra-01, 433 MHz)
//! - `sx1262::Sx1262` — SX1261/SX1262 (e.g. Ra-01SH, 868 MHz)

pub mod sx1262;
pub mod sx1278;

use esp_hal::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct LoRaStatus {
    pub detected: bool,
    pub version: u8,
    pub op_mode: u8,
    pub frequency_mhz: f32,
}

/// LoRa bandwidth, as the SX1262 modulation-parameter encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    Bw125 = 0x04,
    Bw250 = 0x05,
    Bw500 = 0x06,
}

pub trait LoRaDriver {
    /// Reset, configure, and verify the radio. Returns status.
    fn init(&mut self) -> LoRaStatus;

    /// Transmit a packet.
    ///
    /// Returns the instant TxDone was observed, which is the reference point
    /// for LoRaWAN receive windows, or `None` if the transmission failed.
    fn transmit(&mut self, data: &[u8]) -> Option<Instant>;

    /// Enter continuous receive mode.
    fn start_receive(&mut self);

    /// Poll for a received packet. Returns number of bytes read, or None.
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize>;

    /// Read current RSSI in dBm.
    fn rssi(&mut self) -> i16;

    /// Read current radio status.
    fn status(&mut self) -> LoRaStatus;

    /// Retune the radio. Not supported by every driver.
    fn set_frequency(&mut self, _hz: u32) {}

    /// Set spreading factor and bandwidth. Not supported by every driver.
    fn set_datarate(&mut self, _sf: u8, _bw: Bandwidth) {}

    /// Select inverted IQ, as LoRaWAN downlinks use. Not supported by every driver.
    fn set_iq_inverted(&mut self, _inverted: bool) {}

    /// Receive with a bounded listen time. Returns bytes read, or None on timeout.
    ///
    /// Drivers that cannot bound their receive return `None` immediately.
    fn receive_with_timeout(&mut self, _buf: &mut [u8], _timeout_ms: u32) -> Option<usize> {
        None
    }

    /// Open a receive window `delay_ms` after `reference`, listening for `window_ms`.
    ///
    /// This is the LoRaWAN RX1/RX2 primitive: the window has to open on time
    /// relative to the end of the uplink, so the wait is a busy-wait rather
    /// than anything that could overshoot.
    fn receive_window(
        &mut self,
        buf: &mut [u8],
        reference: Instant,
        delay_ms: u64,
        window_ms: u32,
    ) -> Option<usize> {
        let open_at = Duration::from_millis(delay_ms);
        while reference.elapsed() < open_at {}
        self.receive_with_timeout(buf, window_ms)
    }
}

/// Re-export SX1278 as `LoRa` for backward compatibility.
pub use sx1278::Sx1278 as LoRa;

pub(crate) fn delay_ms(ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {}
}
