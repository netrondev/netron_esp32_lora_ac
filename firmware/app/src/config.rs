//! Remotely configurable device settings and the downlink command decoder.
//!
//! Commands arrive on FPort 85 in the same `channel | type | value` form as
//! uplinks, on channel `0xff`. There is no length byte on the wire, so each
//! type has an implied length and an unrecognised type ends parsing — nothing
//! from the remainder of a malformed frame is applied.
//!
//! Every accepted command is echoed back in the next uplink. That echo is the
//! acknowledgement: it is the only way the operator learns the value landed.

use crate::packet::TlvBuilder;

const CH_CONFIG: u8 = 0xff;

const TYPE_SAMPLE_INTERVAL: u8 = 0x02;
const TYPE_REPORT_INTERVAL: u8 = 0x03;
const TYPE_JITTER: u8 = 0x04;
const TYPE_REBOOT: u8 = 0x10;
/// Alias accepted for compatibility with platforms built against Milesight's
/// newer sensors, which express the report interval in minutes.
const TYPE_REPORT_INTERVAL_MINUTES: u8 = 0x8e;

/// Seconds between measurements that feed the aggregate.
pub const SAMPLE_INTERVAL_RANGE: (u16, u16) = (1, 3600);
/// Seconds between transmissions.
pub const REPORT_INTERVAL_RANGE: (u16, u16) = (10, u16::MAX);
/// Bound on the random spread applied to each report time, in seconds.
pub const JITTER_RANGE: (u16, u16) = (0, 600);

/// Settings that survive a reboot and can be changed by downlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceConfig {
    /// Seconds between transmissions.
    pub report_interval_s: u16,
    /// Maximum random offset applied to each report time, in seconds.
    ///
    /// Devices powered up together would otherwise transmit in lockstep and
    /// collide on air for as long as they stayed powered.
    pub jitter_s: u16,
    /// Seconds between measurements. Samples are accumulated and summarised
    /// into a single report rather than transmitted individually.
    pub sample_interval_s: u16,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            report_interval_s: 60,
            jitter_s: 30,
            sample_interval_s: 1,
        }
    }
}

/// Serialised size of a `DeviceConfig`.
pub const CONFIG_BYTES: usize = 6;

impl DeviceConfig {
    pub fn to_bytes(&self) -> [u8; CONFIG_BYTES] {
        let mut buf = [0u8; CONFIG_BYTES];
        buf[0..2].copy_from_slice(&self.report_interval_s.to_le_bytes());
        buf[2..4].copy_from_slice(&self.jitter_s.to_le_bytes());
        buf[4..6].copy_from_slice(&self.sample_interval_s.to_le_bytes());
        buf
    }

    /// Read a config back, falling back to defaults for any field that is out
    /// of range — an erased or half-written record must never brick the device.
    pub fn from_bytes(buf: &[u8]) -> Self {
        if buf.len() < CONFIG_BYTES {
            return Self::default();
        }
        let defaults = Self::default();
        let read = |lo: usize| u16::from_le_bytes([buf[lo], buf[lo + 1]]);

        Self {
            report_interval_s: sanitise(read(0), REPORT_INTERVAL_RANGE, defaults.report_interval_s),
            jitter_s: sanitise(read(2), JITTER_RANGE, defaults.jitter_s),
            sample_interval_s: sanitise(read(4), SAMPLE_INTERVAL_RANGE, defaults.sample_interval_s),
        }
    }
}

fn sanitise(value: u16, range: (u16, u16), fallback: u16) -> u16 {
    if value >= range.0 && value <= range.1 {
        value
    } else {
        fallback
    }
}

/// What a downlink asked us to do, beyond the config changes already applied.
pub struct CommandOutcome {
    /// Records to echo in the next uplink, confirming what was applied.
    pub echo: TlvBuilder,
    /// A reboot was requested. Reset only after the echo has been transmitted.
    pub reboot: bool,
    /// Number of commands accepted.
    pub accepted: u8,
    /// Parsing stopped early on an unrecognised or truncated command.
    pub malformed: bool,
}

/// Apply downlink commands to `config`, returning what to echo back.
///
/// Out-of-range values are rejected and not echoed, so a command that comes
/// back is one that actually took effect.
pub fn apply_downlink(config: &mut DeviceConfig, payload: &[u8]) -> CommandOutcome {
    let mut outcome = CommandOutcome {
        echo: TlvBuilder::new(),
        reboot: false,
        accepted: 0,
        malformed: false,
    };

    let mut i = 0usize;
    while i + 1 < payload.len() {
        let channel = payload[i];
        let type_ = payload[i + 1];
        if channel != CH_CONFIG {
            outcome.malformed = true;
            break;
        }
        let value = &payload[i + 2..];

        // Each arm consumes its own implied length and reports whether the
        // value was in range.
        let (consumed, applied) = match type_ {
            TYPE_REBOOT => {
                if value.is_empty() {
                    (0, false)
                } else {
                    outcome.reboot = true;
                    outcome.echo.push_u8(CH_CONFIG, TYPE_REBOOT, 0xff);
                    (1, true)
                }
            }
            TYPE_SAMPLE_INTERVAL => match take_u16(value) {
                Some(seconds) if in_range(seconds, SAMPLE_INTERVAL_RANGE) => {
                    config.sample_interval_s = seconds;
                    outcome.echo.push_u16(CH_CONFIG, TYPE_SAMPLE_INTERVAL, seconds);
                    (2, true)
                }
                Some(_) => (2, false),
                None => (0, false),
            },
            TYPE_REPORT_INTERVAL => match take_u16(value) {
                Some(seconds) if in_range(seconds, REPORT_INTERVAL_RANGE) => {
                    config.report_interval_s = seconds;
                    outcome.echo.push_u16(CH_CONFIG, TYPE_REPORT_INTERVAL, seconds);
                    (2, true)
                }
                Some(_) => (2, false),
                None => (0, false),
            },
            TYPE_JITTER => match take_u16(value) {
                Some(seconds) if in_range(seconds, JITTER_RANGE) => {
                    config.jitter_s = seconds;
                    outcome.echo.push_u16(CH_CONFIG, TYPE_JITTER, seconds);
                    (2, true)
                }
                Some(_) => (2, false),
                None => (0, false),
            },
            TYPE_REPORT_INTERVAL_MINUTES => {
                // Layout is a leading zero byte then the interval in minutes.
                if value.len() < 3 {
                    (0, false)
                } else {
                    let minutes = u16::from_le_bytes([value[1], value[2]]);
                    // Minutes that would overflow the seconds field are refused
                    // rather than silently truncated.
                    match minutes.checked_mul(60) {
                        Some(seconds) if in_range(seconds, REPORT_INTERVAL_RANGE) => {
                            config.report_interval_s = seconds;
                            // Echo the canonical seconds form: it says what the
                            // device is actually doing.
                            outcome.echo.push_u16(CH_CONFIG, TYPE_REPORT_INTERVAL, seconds);
                            (3, true)
                        }
                        _ => (3, false),
                    }
                }
            }
            _ => (0, false),
        };

        if consumed == 0 {
            outcome.malformed = true;
            break;
        }
        if applied {
            outcome.accepted += 1;
        }
        i += 2 + consumed;
    }

    if i < payload.len() && !outcome.malformed {
        // Trailing bytes too short to be a record.
        outcome.malformed = true;
    }

    outcome
}

fn take_u16(value: &[u8]) -> Option<u16> {
    if value.len() < 2 {
        None
    } else {
        Some(u16::from_le_bytes([value[0], value[1]]))
    }
}

fn in_range(value: u16, range: (u16, u16)) -> bool {
    value >= range.0 && value <= range.1
}
