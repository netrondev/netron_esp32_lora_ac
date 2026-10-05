//! Persistent OTAA session, join nonce and device configuration.
//!
//! This is a second wear-levelled store alongside [`crate::flash_storage`],
//! kept separate because the two records have very different write cadences
//! and very different consequences if lost. It lives in its own sector at
//! `0xD000`, which — like the energy store at `0xE000` — sits inside the NVS
//! partition and therefore survives `espflash flash`. Anything at 0x10000 or
//! above is erased on every reflash.
//!
//! Wear: the record is written on every uplink so the frame counter is never
//! behind what the network has seen. At 32 slots per sector and a one-minute
//! report interval that is one erase every ~32 minutes, or roughly 16k erases
//! a year against a 100k-cycle endurance.

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_println::println;
use esp_storage::FlashStorage;

use crate::config::{DeviceConfig, CONFIG_BYTES};
use crate::lorawan::LoRaWanSession;

/// Distinct from the energy store's magic so a mis-set base address is caught.
const SLOT_MAGIC: u32 = 0x07AA_5E55;

/// Base address: the second-to-last NVS sector.
const STORAGE_BASE: u32 = 0x0000_D000;
const SECTOR_SIZE: usize = 4096;
const SLOT_SIZE: usize = 128;
const SLOTS_PER_SECTOR: usize = SECTOR_SIZE / SLOT_SIZE;

/// Everything that has to outlive a reset.
#[derive(Clone, Copy)]
pub struct SessionRecord {
    /// Next join nonce to use. Incremented before every join attempt.
    pub dev_nonce: u16,
    pub joined: bool,
    pub dev_addr: [u8; 4],
    pub nwk_s_key: [u8; 16],
    pub app_s_key: [u8; 16],
    pub fcnt_up: u32,
    pub fcnt_down: u32,
    pub rx_delay_s: u8,
    pub rx1_dr_offset: u8,
    pub rx2_dr: u8,
    pub config: DeviceConfig,
}

impl Default for SessionRecord {
    fn default() -> Self {
        Self {
            dev_nonce: 0,
            joined: false,
            dev_addr: [0; 4],
            nwk_s_key: [0; 16],
            app_s_key: [0; 16],
            fcnt_up: 0,
            fcnt_down: 0,
            rx_delay_s: 1,
            rx1_dr_offset: 0,
            rx2_dr: 0,
            config: DeviceConfig::default(),
        }
    }
}

impl SessionRecord {
    /// Snapshot the parts of a live session that need to persist.
    pub fn from_session(session: &LoRaWanSession, config: &DeviceConfig) -> Self {
        Self {
            dev_nonce: session.dev_nonce,
            joined: session.joined,
            dev_addr: session.dev_addr,
            nwk_s_key: session.nwk_s_key,
            app_s_key: session.app_s_key,
            fcnt_up: session.fcnt_up,
            fcnt_down: session.fcnt_down,
            rx_delay_s: session.rx_delay_s,
            rx1_dr_offset: session.rx1_dr_offset,
            rx2_dr: session.rx2_dr,
            config: *config,
        }
    }

    /// Restore a saved session into a freshly constructed one.
    pub fn apply_to(&self, session: &mut LoRaWanSession) {
        session.dev_nonce = self.dev_nonce;
        session.joined = self.joined;
        session.dev_addr = self.dev_addr;
        session.nwk_s_key = self.nwk_s_key;
        session.app_s_key = self.app_s_key;
        session.fcnt_up = self.fcnt_up;
        session.fcnt_down = self.fcnt_down;
        session.rx_delay_s = self.rx_delay_s;
        session.rx1_dr_offset = self.rx1_dr_offset;
        session.rx2_dr = self.rx2_dr;
    }

    fn to_bytes(&self, sequence: u32) -> [u8; SLOT_SIZE] {
        let mut buf = [0xFFu8; SLOT_SIZE];
        buf[0..4].copy_from_slice(&SLOT_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&sequence.to_le_bytes());
        buf[8..10].copy_from_slice(&self.dev_nonce.to_le_bytes());
        buf[10] = self.joined as u8;
        buf[11] = self.rx_delay_s;
        buf[12] = self.rx1_dr_offset;
        buf[13] = self.rx2_dr;
        buf[14] = 0;
        buf[15] = 0;
        buf[16..20].copy_from_slice(&self.dev_addr);
        buf[20..36].copy_from_slice(&self.nwk_s_key);
        buf[36..52].copy_from_slice(&self.app_s_key);
        buf[52..56].copy_from_slice(&self.fcnt_up.to_le_bytes());
        buf[56..60].copy_from_slice(&self.fcnt_down.to_le_bytes());
        buf[60..60 + CONFIG_BYTES].copy_from_slice(&self.config.to_bytes());
        buf
    }

    fn from_bytes(buf: &[u8; SLOT_SIZE]) -> Option<(Self, u32)> {
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic != SLOT_MAGIC {
            return None;
        }
        let sequence = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);

        let mut nwk_s_key = [0u8; 16];
        nwk_s_key.copy_from_slice(&buf[20..36]);
        let mut app_s_key = [0u8; 16];
        app_s_key.copy_from_slice(&buf[36..52]);

        Some((
            Self {
                dev_nonce: u16::from_le_bytes([buf[8], buf[9]]),
                joined: buf[10] == 1,
                dev_addr: [buf[16], buf[17], buf[18], buf[19]],
                nwk_s_key,
                app_s_key,
                fcnt_up: u32::from_le_bytes([buf[52], buf[53], buf[54], buf[55]]),
                fcnt_down: u32::from_le_bytes([buf[56], buf[57], buf[58], buf[59]]),
                rx_delay_s: buf[11],
                rx1_dr_offset: buf[12],
                rx2_dr: buf[13],
                config: DeviceConfig::from_bytes(&buf[60..60 + CONFIG_BYTES]),
            },
            sequence,
        ))
    }
}

pub struct SessionStore {
    flash: FlashStorage,
    current_slot: usize,
    current_sequence: u32,
    loaded: Option<SessionRecord>,
}

impl SessionStore {
    /// Scan the sector for the most recent record.
    pub fn new() -> Self {
        let mut flash = FlashStorage::new();
        let mut best: Option<(usize, u32, SessionRecord)> = None;

        for i in 0..SLOTS_PER_SECTOR {
            let offset = STORAGE_BASE + (i * SLOT_SIZE) as u32;
            let mut buf = [0u8; SLOT_SIZE];
            if flash.read(offset, &mut buf).is_err() {
                continue;
            }
            if let Some((record, sequence)) = SessionRecord::from_bytes(&buf) {
                if best.map_or(true, |(_, best_seq, _)| sequence > best_seq) {
                    best = Some((i, sequence, record));
                }
            }
        }

        match best {
            Some((slot, sequence, record)) => {
                println!(
                    "{{\"event\":\"session_store\",\"found\":true,\"slot\":{},\"seq\":{},\"joined\":{},\"dev_nonce\":{},\"fcnt_up\":{}}}",
                    slot, sequence, record.joined, record.dev_nonce, record.fcnt_up
                );
                Self {
                    flash,
                    current_slot: slot,
                    current_sequence: sequence,
                    loaded: Some(record),
                }
            }
            None => {
                println!("{{\"event\":\"session_store\",\"found\":false}}");
                Self {
                    flash,
                    current_slot: SLOTS_PER_SECTOR - 1,
                    current_sequence: 0,
                    loaded: None,
                }
            }
        }
    }

    /// The most recent stored record, or defaults if the store is empty.
    pub fn load(&self) -> SessionRecord {
        self.loaded.unwrap_or_default()
    }

    /// Write a record to the next slot, erasing the sector when wrapping.
    pub fn save(&mut self, record: &SessionRecord) -> bool {
        let next_slot = (self.current_slot + 1) % SLOTS_PER_SECTOR;
        let next_sequence = self.current_sequence.wrapping_add(1);

        if next_slot == 0 {
            if self
                .flash
                .erase(STORAGE_BASE, STORAGE_BASE + SECTOR_SIZE as u32)
                .is_err()
            {
                println!("{{\"event\":\"session_erase\",\"ok\":false}}");
                return false;
            }
        }

        let offset = STORAGE_BASE + (next_slot * SLOT_SIZE) as u32;
        let buf = record.to_bytes(next_sequence);

        if self.flash.write(offset, &buf).is_err() {
            println!("{{\"event\":\"session_write\",\"ok\":false,\"addr\":\"0x{:X}\"}}", offset);
            return false;
        }

        self.current_slot = next_slot;
        self.current_sequence = next_sequence;
        self.loaded = Some(*record);
        true
    }
}
