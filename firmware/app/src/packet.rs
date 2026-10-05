//! Uplink payload construction.
//!
//! Payloads follow the Milesight convention: a flat sequence of
//! `channel(1) | type(1) | value(N)` records with little-endian values and no
//! length byte on the wire — the length of each record is implied by its
//! channel/type pair. See `docs/PROTOCOL.md` for the full channel map.

/// Maximum payload we will ever build.
///
/// 51 bytes is what EU868 permits at the slowest data rate, and keeping to a
/// single budget means a report never becomes untransmittable just because the
/// network moved us down the data rates.
pub const MAX_PAYLOAD: usize = 51;

/// LoRaWAN port used for both uplinks and downlink commands.
pub const FPORT: u8 = 85;

// ----- Device information channels -----
const CH_INFO: u8 = 0xff;
const TYPE_POWER_ON: u8 = 0x0b;
const TYPE_PROTOCOL_VERSION: u8 = 0x01;
const TYPE_HARDWARE_VERSION: u8 = 0x09;
const TYPE_FIRMWARE_VERSION: u8 = 0x0a;
const TYPE_DEVICE_CLASS: u8 = 0x0f;
const TYPE_SERIAL_NUMBER: u8 = 0x16;

// ----- Measurement channels -----
const CH_CURRENT_AVG: u8 = 0x03;
const CH_CURRENT_MIN: u8 = 0x04;
const CH_CURRENT_MAX: u8 = 0x05;
const TYPE_CURRENT: u8 = 0x98;
const CH_CHARGE: u8 = 0x06;
const TYPE_COUNTER: u8 = 0xc8;
const CH_SAMPLE_COUNT: u8 = 0x07;
const TYPE_COUNT: u8 = 0x04;

/// Protocol version reported in `ff 01`.
const PROTOCOL_VERSION: u8 = 0x01;
/// Hardware revision reported in `ff 09`.
const HARDWARE_VERSION: (u8, u8) = (1, 0);
/// Class A, reported in `ff 0f`.
const DEVICE_CLASS_A: u8 = 0x00;

/// Builds a TLV payload into a fixed buffer, silently refusing records that
/// would overflow it.
pub struct TlvBuilder {
    buf: [u8; MAX_PAYLOAD],
    len: usize,
}

impl TlvBuilder {
    pub fn new() -> Self {
        Self {
            buf: [0u8; MAX_PAYLOAD],
            len: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append one record. Returns false if it did not fit.
    pub fn push(&mut self, channel: u8, type_: u8, value: &[u8]) -> bool {
        let needed = 2 + value.len();
        if self.len + needed > MAX_PAYLOAD {
            return false;
        }
        self.buf[self.len] = channel;
        self.buf[self.len + 1] = type_;
        self.buf[self.len + 2..self.len + 2 + value.len()].copy_from_slice(value);
        self.len += needed;
        true
    }

    pub fn push_u8(&mut self, channel: u8, type_: u8, value: u8) -> bool {
        self.push(channel, type_, &[value])
    }

    pub fn push_u16(&mut self, channel: u8, type_: u8, value: u16) -> bool {
        self.push(channel, type_, &value.to_le_bytes())
    }

    pub fn push_u32(&mut self, channel: u8, type_: u8, value: u32) -> bool {
        self.push(channel, type_, &value.to_le_bytes())
    }
}

impl Default for TlvBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Aggregated measurements for one reporting window.
#[derive(Debug, Clone, Copy)]
pub struct Report {
    pub current_avg_ma: u16,
    pub current_min_ma: u16,
    pub current_max_ma: u16,
    pub cumulative_mah: u32,
    pub sample_count: u16,
}

/// Collects samples between transmissions.
///
/// Measuring more often than we transmit is the whole point of the split
/// sample/report intervals: a spike between reports still shows up in the
/// maximum rather than being missed entirely.
#[derive(Debug, Clone, Copy)]
pub struct SampleAccumulator {
    sum_ma: u64,
    count: u16,
    min_ma: u16,
    max_ma: u16,
}

impl SampleAccumulator {
    pub fn new() -> Self {
        Self {
            sum_ma: 0,
            count: 0,
            min_ma: u16::MAX,
            max_ma: 0,
        }
    }

    pub fn push(&mut self, current_ma: u16) {
        self.sum_ma = self.sum_ma.saturating_add(current_ma as u64);
        self.count = self.count.saturating_add(1);
        if current_ma < self.min_ma {
            self.min_ma = current_ma;
        }
        if current_ma > self.max_ma {
            self.max_ma = current_ma;
        }
    }

    pub fn count(&self) -> u16 {
        self.count
    }

    /// Summarise the window. Reports zeroes if no sample was taken, which is
    /// itself diagnostic — it means the sample interval outran the report
    /// interval, or measurement is failing.
    pub fn summarise(&self, cumulative_mah: u32) -> Report {
        if self.count == 0 {
            return Report {
                current_avg_ma: 0,
                current_min_ma: 0,
                current_max_ma: 0,
                cumulative_mah,
                sample_count: 0,
            };
        }
        Report {
            current_avg_ma: (self.sum_ma / self.count as u64) as u16,
            current_min_ma: self.min_ma,
            current_max_ma: self.max_ma,
            cumulative_mah,
            sample_count: self.count,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for SampleAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the periodic report payload.
pub fn build_report(report: &Report) -> TlvBuilder {
    let mut tlv = TlvBuilder::new();
    tlv.push_u16(CH_CURRENT_AVG, TYPE_CURRENT, report.current_avg_ma);
    tlv.push_u16(CH_CURRENT_MIN, TYPE_CURRENT, report.current_min_ma);
    tlv.push_u16(CH_CURRENT_MAX, TYPE_CURRENT, report.current_max_ma);
    tlv.push_u32(CH_CHARGE, TYPE_COUNTER, report.cumulative_mah);
    tlv.push_u16(CH_SAMPLE_COUNT, TYPE_COUNT, report.sample_count);
    tlv
}

/// Build the device information packet sent after every successful join.
pub fn build_device_info(dev_eui: &[u8; 8]) -> TlvBuilder {
    let (fw_major, fw_minor) = firmware_version();

    let mut tlv = TlvBuilder::new();
    tlv.push_u8(CH_INFO, TYPE_POWER_ON, 0xff);
    tlv.push_u8(CH_INFO, TYPE_PROTOCOL_VERSION, PROTOCOL_VERSION);
    tlv.push(
        CH_INFO,
        TYPE_HARDWARE_VERSION,
        &[HARDWARE_VERSION.0, HARDWARE_VERSION.1],
    );
    tlv.push(CH_INFO, TYPE_FIRMWARE_VERSION, &[fw_major, fw_minor]);
    tlv.push_u8(CH_INFO, TYPE_DEVICE_CLASS, DEVICE_CLASS_A);
    tlv.push(CH_INFO, TYPE_SERIAL_NUMBER, dev_eui);
    tlv
}

/// Major and minor firmware version, parsed from the crate version at compile time.
const fn firmware_version() -> (u8, u8) {
    let bytes = env!("CARGO_PKG_VERSION").as_bytes();
    let mut major = 0u8;
    let mut minor = 0u8;
    let mut i = 0;
    let mut seen_dot = false;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'.' {
            if seen_dot {
                break;
            }
            seen_dot = true;
        } else if b >= b'0' && b <= b'9' {
            let digit = b - b'0';
            if seen_dot {
                minor = minor.wrapping_mul(10).wrapping_add(digit);
            } else {
                major = major.wrapping_mul(10).wrapping_add(digit);
            }
        }
        i += 1;
    }

    (major, minor)
}
