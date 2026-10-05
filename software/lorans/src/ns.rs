//! LoRaWAN 1.0.x network server for the D4 power monitors.
//!
//! Handles what the gateway's built-in network server would: authenticating
//! joins, deriving session keys, verifying and decrypting uplinks, and queuing
//! downlinks. Class A devices only listen in the windows following their own
//! uplinks, so a queued downlink is scheduled the moment the device is heard
//! from and not before.
//!
//! Deliberately minimal: one channel plan, RX1 only, no ADR, no MAC commands.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use milesight_d4::lorawan;
use milesight_d4::tlv;
use serde::{Deserialize, Serialize};

/// NetID for a private network that does not participate in roaming.
const NET_ID: [u8; 3] = [0x00, 0x00, 0x00];

/// RX1 delay in seconds, as advertised in the join accept. The device uses
/// this value for data downlinks; join accepts always use 5 s.
pub const RX_DELAY_S: u8 = 1;

/// DLSettings: RX1 data rate offset 0, RX2 data rate 0 (SF12BW125).
const DL_SETTINGS: u8 = 0x00;

/// A device we have seen, and its session if it has joined.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub dev_eui: String,
    pub dev_addr: [u8; 4],
    pub nwk_s_key: [u8; 16],
    pub app_s_key: [u8; 16],
    /// Highest uplink counter accepted so far.
    pub fcnt_up: u32,
    /// Next downlink counter to use.
    pub fcnt_down: u32,
    /// Join nonces already used, so a replayed join is refused.
    pub last_dev_nonce: Option<u16>,
    #[serde(default)]
    pub queue: VecDeque<QueuedDownlink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedDownlink {
    pub port: u8,
    pub payload: Vec<u8>,
}

/// What the caller should transmit, and when.
pub struct Transmit {
    pub frame: Vec<u8>,
    /// Delay from the end of the uplink, in microseconds.
    pub delay_us: u32,
    pub description: String,
}

pub struct NetworkServer {
    devices: HashMap<String, Device>,
    /// DevAddr (big-endian) to DevEUI, for routing uplinks.
    by_dev_addr: HashMap<[u8; 4], String>,
    state_path: PathBuf,
    /// Incremented per join so each one derives distinct session keys.
    app_nonce: u32,
}

impl NetworkServer {
    pub fn load(state_path: impl AsRef<Path>) -> Self {
        let state_path = state_path.as_ref().to_path_buf();
        let devices: HashMap<String, Device> = std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();

        let mut by_dev_addr = HashMap::new();
        for (eui, device) in &devices {
            by_dev_addr.insert(device.dev_addr, eui.clone());
        }

        // Start above every nonce already used, so a restart cannot re-derive
        // a session that a device has already seen.
        let app_nonce = devices.len() as u32 + 1;

        println!("[ns] loaded {} device(s) from {}", devices.len(), state_path.display());
        Self {
            devices,
            by_dev_addr,
            state_path,
            app_nonce,
        }
    }

    /// Persist sessions.
    ///
    /// This matters more than it looks: if the server forgets a session, the
    /// device carries on using keys we no longer hold, every uplink fails its
    /// MIC check, and — having no session — we cannot even tell it to rejoin.
    fn save(&self) {
        match serde_json::to_string_pretty(&self.devices) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&self.state_path, text) {
                    eprintln!("[ns] could not save state: {}", e);
                }
            }
            Err(e) => eprintln!("[ns] could not serialise state: {}", e),
        }
    }

    pub fn devices(&self) -> impl Iterator<Item = &Device> {
        self.devices.values()
    }

    /// Queue a downlink for a device, to go out after its next uplink.
    pub fn queue_downlink(&mut self, dev_eui: &str, port: u8, payload: Vec<u8>) -> Result<(), String> {
        let dev_eui = dev_eui.to_uppercase();
        let device = self
            .devices
            .get_mut(&dev_eui)
            .ok_or_else(|| format!("unknown device {}", dev_eui))?;
        device.queue.push_back(QueuedDownlink { port, payload });
        self.save();
        Ok(())
    }

    /// Handle one received frame, returning anything to transmit back.
    pub fn handle_uplink(&mut self, frame: &[u8]) -> Option<Transmit> {
        if frame.is_empty() {
            return None;
        }
        match frame[0] & 0xE0 {
            0x00 => self.handle_join(frame),
            0x40 | 0x80 => self.handle_data(frame),
            other => {
                println!("[ns] ignoring frame with mtype 0x{:02x}", other);
                None
            }
        }
    }

    fn handle_join(&mut self, frame: &[u8]) -> Option<Transmit> {
        let join = lorawan::parse_join_request(frame)?;
        let dev_eui = join.dev_eui_hex();
        let app_key = join.app_key();

        if !lorawan::verify_join_mic(frame, &app_key) {
            println!(
                "[ns] join from {} rejected: MIC does not match the DevEUI-derived AppKey",
                dev_eui
            );
            return None;
        }

        // A repeated nonce is a replay — or a device whose stored nonce went
        // backwards, which is equally not something to hand a session to.
        if let Some(existing) = self.devices.get(&dev_eui) {
            if let Some(last) = existing.last_dev_nonce {
                if join.dev_nonce <= last {
                    println!(
                        "[ns] join from {} rejected: DevNonce {} already used (last {})",
                        dev_eui, join.dev_nonce, last
                    );
                    return None;
                }
            }
        }

        self.app_nonce = self.app_nonce.wrapping_add(1);
        let app_nonce = [
            self.app_nonce as u8,
            (self.app_nonce >> 8) as u8,
            (self.app_nonce >> 16) as u8,
        ];

        let dev_addr = dev_addr_for(&join.dev_eui);
        let (nwk_s_key, app_s_key) =
            lorawan::derive_session_keys(&app_key, &app_nonce, &NET_ID, join.dev_nonce);

        let frame_out = lorawan::build_join_accept(
            &app_key,
            &app_nonce,
            &NET_ID,
            &dev_addr,
            DL_SETTINGS,
            RX_DELAY_S,
        );

        // Anything queued was for the previous session and cannot be decrypted
        // with the new keys, so it goes.
        let device = Device {
            dev_eui: dev_eui.clone(),
            dev_addr,
            nwk_s_key,
            app_s_key,
            fcnt_up: 0,
            fcnt_down: 0,
            last_dev_nonce: Some(join.dev_nonce),
            queue: VecDeque::new(),
        };

        println!(
            "[ns] join accepted: {} -> DevAddr {} (nonce {})",
            dev_eui,
            hex_upper(&reversed(&dev_addr)),
            join.dev_nonce
        );

        self.by_dev_addr.insert(dev_addr, dev_eui.clone());
        self.devices.insert(dev_eui, device);
        self.save();

        Some(Transmit {
            frame: frame_out,
            // Join accepts use their own delay, not the RxDelay they carry.
            delay_us: 5_000_000,
            description: "join accept".to_string(),
        })
    }

    fn handle_data(&mut self, frame: &[u8]) -> Option<Transmit> {
        let uplink = lorawan::parse_uplink(frame)?;
        let confirmed = frame[0] & 0xE0 == 0x80;

        let dev_eui = match self.by_dev_addr.get(&uplink.dev_addr) {
            Some(eui) => eui.clone(),
            None => {
                println!(
                    "[ns] uplink from unknown DevAddr {}",
                    hex_upper(&reversed(&uplink.dev_addr))
                );
                return None;
            }
        };

        let device = self.devices.get_mut(&dev_eui)?;

        // Reconstruct the full counter from the 16 bits on the wire.
        let fcnt = (device.fcnt_up & 0xFFFF_0000) | uplink.fcnt;

        if !lorawan::verify_mic(frame, &uplink.dev_addr, fcnt, &device.nwk_s_key) {
            println!("[ns] {} uplink failed MIC check (fcnt {})", dev_eui, fcnt);
            return None;
        }

        // A repeat of the last counter is a retransmission of a confirmed
        // frame whose acknowledgement went missing — accept it and acknowledge
        // again, but do not process the payload twice.
        let retransmission = fcnt == device.fcnt_up && fcnt != 0;
        if fcnt < device.fcnt_up {
            println!("[ns] {} replayed fcnt {} ignored", dev_eui, fcnt);
            return None;
        }
        device.fcnt_up = fcnt;

        if !retransmission {
            let payload = lorawan::decrypt_uplink(
                &uplink.encrypted_payload,
                &device.app_s_key,
                &uplink.dev_addr,
                fcnt,
            );
            let decoded = tlv::decode(&payload);
            println!(
                "[uplink] {} fcnt={} port={} {}{}",
                dev_eui,
                fcnt,
                uplink.fport.unwrap_or(0),
                decoded.summary(),
                if confirmed { " (confirmed)" } else { "" }
            );
            if let Some(offset) = decoded.undecoded_at {
                println!("[uplink] {} raw {} (stopped at {})", dev_eui, hex_upper(&payload), offset);
            }
        } else {
            println!("[ns] {} retransmission of fcnt {}", dev_eui, fcnt);
        }

        // A downlink is needed if we owe an acknowledgement or have something
        // queued; otherwise stay quiet and leave the airtime alone.
        let queued = device.queue.pop_front();
        if !confirmed && queued.is_none() {
            self.save();
            return None;
        }

        let fcnt_down = device.fcnt_down;
        let (port, payload) = match &queued {
            Some(dl) => (Some(dl.port), dl.payload.clone()),
            None => (None, Vec::new()),
        };
        let frame_pending = !device.queue.is_empty();

        let frame_out = lorawan::build_downlink(
            &device.nwk_s_key,
            &device.app_s_key,
            &device.dev_addr,
            fcnt_down,
            confirmed,
            frame_pending,
            port,
            &payload,
        );
        device.fcnt_down = fcnt_down.wrapping_add(1);

        let description = match &queued {
            Some(dl) => format!(
                "downlink port {} [{}]{}",
                dl.port,
                hex_upper(&dl.payload),
                if confirmed { " + ack" } else { "" }
            ),
            None => "ack".to_string(),
        };
        println!("[downlink] {} {}", dev_eui, description);

        self.save();

        Some(Transmit {
            frame: frame_out,
            delay_us: RX_DELAY_S as u32 * 1_000_000,
            description,
        })
    }
}

/// Allocate a stable DevAddr for a DevEUI.
///
/// Derived rather than assigned so a device keeps the same address across
/// rejoins, which makes traffic far easier to follow. The top 7 bits are the
/// NwkID, left at zero for a private network.
fn dev_addr_for(dev_eui: &[u8; 8]) -> [u8; 4] {
    // Little-endian, as the address appears in frames.
    [
        dev_eui[7],
        dev_eui[6],
        dev_eui[5],
        dev_eui[2] & 0x01,
    ]
}

fn reversed(bytes: &[u8; 4]) -> [u8; 4] {
    [bytes[3], bytes[2], bytes[1], bytes[0]]
}

pub fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02X}", b)).collect()
}
