//! Decoder for the device's Milesight-style TLV payloads.
//!
//! Records are `channel(1) | type(1) | value(N)` with little-endian values and
//! no length byte — each channel/type pair has an implied length, so an
//! unknown pair ends the decode. See `docs/PROTOCOL.md`.
//!
//! This is the single Rust implementation of the payload format; the JS
//! decoder in `software/loradecode/` mirrors it for the client's platform.

/// One decoded record.
#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    // Device information, sent after each join
    PowerOn,
    ProtocolVersion(u8),
    HardwareVersion(u8, u8),
    FirmwareVersion(u8, u8),
    DeviceClass(u8),
    SerialNumber([u8; 8]),

    // Measurements
    CurrentAvgMa(u16),
    CurrentMinMa(u16),
    CurrentMaxMa(u16),
    CumulativeMah(u32),
    SampleCount(u16),

    // Echoes of applied configuration commands
    ReportIntervalS(u16),
    SampleIntervalS(u16),
    JitterS(u16),
    Rebooted,
}

/// Result of decoding a payload.
#[derive(Debug, Clone, Default)]
pub struct Decoded {
    pub fields: Vec<Field>,
    /// Offset at which decoding stopped early, if it did.
    pub undecoded_at: Option<usize>,
}

impl Decoded {
    /// Human-readable one-line summary, for logs.
    pub fn summary(&self) -> String {
        let parts: Vec<String> = self
            .fields
            .iter()
            .map(|f| match f {
                Field::PowerOn => "power_on".to_string(),
                Field::ProtocolVersion(v) => format!("protocol=v{}", v),
                Field::HardwareVersion(a, b) => format!("hw=v{}.{}", a, b),
                Field::FirmwareVersion(a, b) => format!("fw=v{}.{}", a, b),
                Field::DeviceClass(c) => format!("class={}", class_name(*c)),
                Field::SerialNumber(sn) => format!(
                    "sn={}",
                    sn.iter().map(|b| format!("{:02X}", b)).collect::<String>()
                ),
                Field::CurrentAvgMa(v) => format!("avg={}mA", v),
                Field::CurrentMinMa(v) => format!("min={}mA", v),
                Field::CurrentMaxMa(v) => format!("max={}mA", v),
                Field::CumulativeMah(v) => format!("total={}mAh", v),
                Field::SampleCount(v) => format!("samples={}", v),
                Field::ReportIntervalS(v) => format!("report_interval={}s", v),
                Field::SampleIntervalS(v) => format!("sample_interval={}s", v),
                Field::JitterS(v) => format!("jitter={}s", v),
                Field::Rebooted => "reboot".to_string(),
            })
            .collect();
        parts.join(" ")
    }
}

fn class_name(c: u8) -> &'static str {
    match c {
        0 => "A",
        1 => "B",
        2 => "C",
        _ => "?",
    }
}

const CH_INFO: u8 = 0xff;
const CH_CURRENT_AVG: u8 = 0x03;
const CH_CURRENT_MIN: u8 = 0x04;
const CH_CURRENT_MAX: u8 = 0x05;
const CH_CHARGE: u8 = 0x06;
const CH_SAMPLE_COUNT: u8 = 0x07;

/// Decode a payload into its records.
///
/// Decoding is best-effort: whatever was understood before an unknown or
/// truncated record is returned, with the offset where it stopped.
pub fn decode(payload: &[u8]) -> Decoded {
    let mut out = Decoded::default();
    let mut i = 0usize;

    while i + 1 < payload.len() {
        let channel = payload[i];
        let type_ = payload[i + 1];
        let value = &payload[i + 2..];

        let consumed = match (channel, type_) {
            (CH_INFO, 0x0b) => take(value, 1, |_| Field::PowerOn),
            (CH_INFO, 0x01) => take(value, 1, |v| Field::ProtocolVersion(v[0])),
            (CH_INFO, 0x09) => take(value, 2, |v| Field::HardwareVersion(v[0], v[1])),
            (CH_INFO, 0x0a) => take(value, 2, |v| Field::FirmwareVersion(v[0], v[1])),
            (CH_INFO, 0x0f) => take(value, 1, |v| Field::DeviceClass(v[0])),
            (CH_INFO, 0x16) => take(value, 8, |v| {
                let mut sn = [0u8; 8];
                sn.copy_from_slice(&v[..8]);
                Field::SerialNumber(sn)
            }),

            (CH_CURRENT_AVG, 0x98) => take(value, 2, |v| Field::CurrentAvgMa(u16_le(v))),
            (CH_CURRENT_MIN, 0x98) => take(value, 2, |v| Field::CurrentMinMa(u16_le(v))),
            (CH_CURRENT_MAX, 0x98) => take(value, 2, |v| Field::CurrentMaxMa(u16_le(v))),
            (CH_CHARGE, 0xc8) => take(value, 4, |v| Field::CumulativeMah(u32_le(v))),
            (CH_SAMPLE_COUNT, 0x04) => take(value, 2, |v| Field::SampleCount(u16_le(v))),

            // Configuration echoes share the info channel with device data;
            // the type byte is what distinguishes them.
            (CH_INFO, 0x02) => take(value, 2, |v| Field::SampleIntervalS(u16_le(v))),
            (CH_INFO, 0x03) => take(value, 2, |v| Field::ReportIntervalS(u16_le(v))),
            (CH_INFO, 0x04) => take(value, 2, |v| Field::JitterS(u16_le(v))),
            (CH_INFO, 0x10) => take(value, 1, |_| Field::Rebooted),

            _ => None,
        };

        match consumed {
            Some((field, len)) => {
                out.fields.push(field);
                i += 2 + len;
            }
            None => {
                out.undecoded_at = Some(i);
                return out;
            }
        }
    }

    if i < payload.len() {
        out.undecoded_at = Some(i);
    }
    out
}

fn take<F>(value: &[u8], len: usize, build: F) -> Option<(Field, usize)>
where
    F: FnOnce(&[u8]) -> Field,
{
    if value.len() < len {
        None
    } else {
        Some((build(&value[..len]), len))
    }
}

fn u16_le(v: &[u8]) -> u16 {
    u16::from_le_bytes([v[0], v[1]])
}

fn u32_le(v: &[u8]) -> u32 {
    u32::from_le_bytes([v[0], v[1], v[2], v[3]])
}

// ── Downlink command encoding ──────────────────────────────────────────────

/// A configuration command to send to a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Reboot,
    SetSampleInterval(u16),
    SetReportInterval(u16),
    SetJitter(u16),
}

impl Command {
    /// Encode to the wire form. Multiple commands may be concatenated.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Command::Reboot => vec![CH_INFO, 0x10, 0xff],
            Command::SetSampleInterval(s) => tlv_u16(0x02, *s),
            Command::SetReportInterval(s) => tlv_u16(0x03, *s),
            Command::SetJitter(s) => tlv_u16(0x04, *s),
        }
    }

    /// Parse a command from CLI-friendly text, e.g. `report=300`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("reboot") {
            return Some(Command::Reboot);
        }
        let (name, value) = text.split_once('=')?;
        let value: u16 = value.trim().parse().ok()?;
        match name.trim().to_ascii_lowercase().as_str() {
            "sample" | "sample_interval" => Some(Command::SetSampleInterval(value)),
            "report" | "report_interval" | "heartbeat" => Some(Command::SetReportInterval(value)),
            "jitter" => Some(Command::SetJitter(value)),
            _ => None,
        }
    }
}

fn tlv_u16(type_: u8, value: u16) -> Vec<u8> {
    let mut out = vec![CH_INFO, type_];
    out.extend_from_slice(&value.to_le_bytes());
    out
}

/// Encode a list of commands into one downlink payload.
pub fn encode_commands(commands: &[Command]) -> Vec<u8> {
    commands.iter().flat_map(|c| c.encode()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_report() {
        // avg 1234 mA, min 1000, max 1500, 42 mAh, 10 samples
        let payload = [
            0x03, 0x98, 0xd2, 0x04, 0x04, 0x98, 0xe8, 0x03, 0x05, 0x98, 0xdc, 0x05, 0x06, 0xc8,
            0x2a, 0x00, 0x00, 0x00, 0x07, 0x04, 0x0a, 0x00,
        ];
        let decoded = decode(&payload);
        assert_eq!(decoded.undecoded_at, None);
        assert_eq!(
            decoded.fields,
            vec![
                Field::CurrentAvgMa(1234),
                Field::CurrentMinMa(1000),
                Field::CurrentMaxMa(1500),
                Field::CumulativeMah(42),
                Field::SampleCount(10),
            ]
        );
    }

    #[test]
    fn decodes_a_config_echo() {
        let decoded = decode(&[0xff, 0x03, 0x2c, 0x01]);
        assert_eq!(decoded.fields, vec![Field::ReportIntervalS(300)]);
    }

    #[test]
    fn stops_on_a_truncated_record() {
        let decoded = decode(&[0x03, 0x98, 0xd2]);
        assert_eq!(decoded.fields, vec![]);
        assert_eq!(decoded.undecoded_at, Some(0));
    }

    #[test]
    fn encodes_report_interval_little_endian() {
        assert_eq!(
            Command::SetReportInterval(300).encode(),
            vec![0xff, 0x03, 0x2c, 0x01]
        );
        assert_eq!(Command::Reboot.encode(), vec![0xff, 0x10, 0xff]);
    }

    #[test]
    fn parses_commands_from_text() {
        assert_eq!(Command::parse("report=300"), Some(Command::SetReportInterval(300)));
        assert_eq!(Command::parse("sample = 5"), Some(Command::SetSampleInterval(5)));
        assert_eq!(Command::parse("REBOOT"), Some(Command::Reboot));
        assert_eq!(Command::parse("nonsense=1"), None);
    }
}
