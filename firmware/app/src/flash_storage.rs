//! Simple wear-leveling flash storage for cumulative energy counter
//!
//! Uses a rotating slot system within a flash sector to distribute writes.
//! Each slot contains: magic (4 bytes) + sequence (4 bytes) + data (8 bytes) = 16 bytes
//!
//! With 4KB sector and 16-byte slots = 256 slots per sector
//! At 100k erase cycles and writing every 5 minutes:
//! - 256 slots * 100k erases = 25.6 million writes
//! - 25.6M writes / (12 writes/hour * 24 * 365) = ~243 years

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_println::println;
use esp_storage::FlashStorage;

/// Magic number to identify valid slots
const SLOT_MAGIC: u32 = 0xCAFE_1234;

/// Size of each slot in bytes (must be multiple of 4 for word alignment)
const SLOT_SIZE: usize = 32;

/// Flash sector size
const SECTOR_SIZE: usize = 4096;

/// Number of slots per sector
const SLOTS_PER_SECTOR: usize = SECTOR_SIZE / SLOT_SIZE;

/// Base address for our storage
///
/// IMPORTANT: The factory app partition spans 0x10000 to 0x400000 (almost entire flash).
/// Any address in this range gets ERASED when flashing the app!
///
/// Safe locations (not erased during app flash):
/// - NVS partition: 0x9000 to 0xF000 (24KB = 6 sectors)
/// - phy_init: 0xF000 to 0x10000 (4KB, but has calibration data)
///
/// We use 0xE000 - the last 4KB sector of NVS. This is safe because:
/// 1. NVS partition is never erased during app flashing
/// 2. Typical NVS usage is small and won't reach the last sector
/// 3. We're using raw flash, not the NVS library, so no conflict
const STORAGE_BASE: u32 = 0x0000_E000;

/// Slot layout:
/// - bytes 0-3: magic (u32)
/// - bytes 4-7: sequence number (u32) - monotonically increasing
/// - bytes 8-15: cumulative_uah (u64)
/// - bytes 16-19: fcnt_up (u32) - LoRaWAN uplink frame counter
/// - bytes 20-31: reserved (0xFF)
#[derive(Clone, Copy)]
struct Slot {
    magic: u32,
    sequence: u32,
    cumulative_uah: u64,
    fcnt_up: u32,
}

impl Slot {
    fn to_bytes(&self) -> [u8; SLOT_SIZE] {
        let mut buf = [0xFFu8; SLOT_SIZE];
        buf[0..4].copy_from_slice(&self.magic.to_le_bytes());
        buf[4..8].copy_from_slice(&self.sequence.to_le_bytes());
        buf[8..16].copy_from_slice(&self.cumulative_uah.to_le_bytes());
        buf[16..20].copy_from_slice(&self.fcnt_up.to_le_bytes());
        buf
    }

    fn from_bytes(buf: &[u8; SLOT_SIZE]) -> Self {
        Self {
            magic: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            sequence: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            cumulative_uah: u64::from_le_bytes([
                buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
            ]),
            fcnt_up: u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
        }
    }

    fn is_valid(&self) -> bool {
        self.magic == SLOT_MAGIC
    }
}

pub struct WearLevelingStorage {
    flash: FlashStorage,
    current_slot: usize,
    current_sequence: u32,
}

impl WearLevelingStorage {
    /// Initialize storage, scanning for the most recent valid slot
    pub fn new() -> Self {
        println!("{{\"event\":\"flash_init\",\"base_addr\":\"0x{:X}\",\"sector_size\":{},\"slots\":{}}}",
            STORAGE_BASE, SECTOR_SIZE, SLOTS_PER_SECTOR);

        let mut flash = FlashStorage::new();
        let mut best_slot: Option<(usize, u32, u64)> = None; // (index, sequence, value)
        let mut valid_count = 0u32;
        let mut erased_count = 0u32;
        let mut corrupt_count = 0u32;

        // Scan all slots to find the one with highest sequence number
        for i in 0..SLOTS_PER_SECTOR {
            let offset = STORAGE_BASE + (i * SLOT_SIZE) as u32;
            let mut buf = [0u8; SLOT_SIZE];

            if flash.read(offset, &mut buf).is_ok() {
                let slot = Slot::from_bytes(&buf);

                // Check if slot is erased (all 0xFF)
                let is_erased = buf.iter().all(|&b| b == 0xFF);

                if slot.is_valid() {
                    valid_count += 1;
                    // Print first few and last few valid slots for debugging
                    if valid_count <= 3 || i >= SLOTS_PER_SECTOR - 3 {
                        println!("  slot[{}]: seq={} val={} uAh", i, slot.sequence, slot.cumulative_uah);
                    }
                    match best_slot {
                        None => best_slot = Some((i, slot.sequence, slot.cumulative_uah)),
                        Some((_, best_seq, _)) if slot.sequence > best_seq => {
                            best_slot = Some((i, slot.sequence, slot.cumulative_uah))
                        }
                        _ => {}
                    }
                } else if is_erased {
                    erased_count += 1;
                } else {
                    corrupt_count += 1;
                    if corrupt_count <= 3 {
                        println!("  slot[{}]: corrupt magic=0x{:08X}", i, slot.magic);
                    }
                }
            }
        }

        println!("{{\"event\":\"flash_scan\",\"valid\":{},\"erased\":{},\"corrupt\":{}}}",
            valid_count, erased_count, corrupt_count);

        let (current_slot, current_sequence) = match best_slot {
            Some((idx, seq, val)) => {
                println!("{{\"event\":\"flash_best\",\"slot\":{},\"seq\":{},\"val_uah\":{}}}", idx, seq, val);
                (idx, seq)
            },
            None => {
                println!("{{\"event\":\"flash_empty\",\"msg\":\"no valid slots found, starting fresh\"}}");
                (0, 0)
            }
        };

        // Verify flash is actually working by doing a test read
        let test_offset = STORAGE_BASE;
        let mut test_buf = [0u8; 16];
        match flash.read(test_offset, &mut test_buf) {
            Ok(_) => {
                println!("{{\"event\":\"flash_test_read\",\"ok\":true,\"first_bytes\":\"0x{:02X}{:02X}{:02X}{:02X}\"}}",
                    test_buf[0], test_buf[1], test_buf[2], test_buf[3]);
            }
            Err(_) => {
                println!("{{\"event\":\"flash_test_read\",\"ok\":false}}");
            }
        }

        // If no valid slots found, do a startup test to verify flash write works
        if valid_count == 0 {
            println!("{{\"event\":\"flash_test_write\",\"msg\":\"testing flash write capability\"}}");

            // Erase the sector first
            match flash.erase(STORAGE_BASE, STORAGE_BASE + SECTOR_SIZE as u32) {
                Ok(_) => println!("{{\"event\":\"flash_test_erase\",\"ok\":true}}"),
                Err(_) => println!("{{\"event\":\"flash_test_erase\",\"ok\":false}}"),
            }

            // Write a test pattern
            let test_pattern: [u8; 16] = [0xDE, 0xAD, 0xBE, 0xEF, 0x12, 0x34, 0x56, 0x78,
                                          0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x22, 0x33, 0x44];
            match flash.write(STORAGE_BASE, &test_pattern) {
                Ok(_) => {
                    // Read back and verify
                    let mut read_back = [0u8; 16];
                    if flash.read(STORAGE_BASE, &mut read_back).is_ok() {
                        let matches = read_back == test_pattern;
                        println!("{{\"event\":\"flash_test_verify\",\"ok\":{},\"wrote\":\"{:02X}{:02X}{:02X}{:02X}\",\"read\":\"{:02X}{:02X}{:02X}{:02X}\"}}",
                            matches,
                            test_pattern[0], test_pattern[1], test_pattern[2], test_pattern[3],
                            read_back[0], read_back[1], read_back[2], read_back[3]);
                    }
                }
                Err(_) => println!("{{\"event\":\"flash_test_write\",\"ok\":false}}"),
            }

            // Erase again to leave clean for actual use
            let _ = flash.erase(STORAGE_BASE, STORAGE_BASE + SECTOR_SIZE as u32);
        }

        Self {
            flash,
            current_slot,
            current_sequence,
        }
    }

    /// Load the cumulative value and fcnt_up from flash
    pub fn load(&mut self) -> (u64, u32) {
        if self.current_sequence == 0 {
            return (0, 0);
        }

        let offset = STORAGE_BASE + (self.current_slot * SLOT_SIZE) as u32;
        let mut buf = [0u8; SLOT_SIZE];

        if self.flash.read(offset, &mut buf).is_ok() {
            let slot = Slot::from_bytes(&buf);
            if slot.is_valid() {
                // Old 16-byte slots won't have fcnt_up — treat 0xFFFFFFFF as 0
                let fcnt = if slot.fcnt_up == 0xFFFFFFFF { 0 } else { slot.fcnt_up };
                return (slot.cumulative_uah, fcnt);
            }
        }

        (0, 0)
    }

    /// Save the cumulative value and fcnt_up to flash with wear leveling
    pub fn save(&mut self, cumulative_uah: u64, fcnt_up: u32) -> bool {
        // Move to next slot
        let next_slot = (self.current_slot + 1) % SLOTS_PER_SECTOR;
        let next_sequence = self.current_sequence.wrapping_add(1);

        // Check if we need to erase (when wrapping back to slot 0)
        if next_slot == 0 {
            println!("{{\"event\":\"flash_erase\",\"addr\":\"0x{:X}\"}}", STORAGE_BASE);
            let erase_result = self.flash.erase(STORAGE_BASE, STORAGE_BASE + SECTOR_SIZE as u32);
            if erase_result.is_err() {
                println!("{{\"event\":\"flash_erase\",\"ok\":false}}");
                return false;
            }
            println!("{{\"event\":\"flash_erase\",\"ok\":true}}");
        }

        // Write new slot
        let slot = Slot {
            magic: SLOT_MAGIC,
            sequence: next_sequence,
            cumulative_uah,
            fcnt_up,
        };

        let offset = STORAGE_BASE + (next_slot * SLOT_SIZE) as u32;
        let buf = slot.to_bytes();

        let write_result = self.flash.write(offset, &buf);
        if write_result.is_ok() {
            // Verify write by reading back
            let mut verify_buf = [0u8; SLOT_SIZE];
            if self.flash.read(offset, &mut verify_buf).is_ok() {
                let verified = verify_buf == buf;
                if !verified {
                    println!("{{\"event\":\"flash_verify\",\"ok\":false,\"wrote\":\"{:02X}{:02X}{:02X}{:02X}\",\"read\":\"{:02X}{:02X}{:02X}{:02X}\"}}",
                        buf[0], buf[1], buf[2], buf[3],
                        verify_buf[0], verify_buf[1], verify_buf[2], verify_buf[3]);
                    return false;
                }
            }

            self.current_slot = next_slot;
            self.current_sequence = next_sequence;
            true
        } else {
            println!("{{\"event\":\"flash_write\",\"ok\":false,\"addr\":\"0x{:X}\"}}", offset);
            false
        }
    }

    /// Get current slot index (for debugging)
    pub fn current_slot(&self) -> usize {
        self.current_slot
    }

    /// Get current sequence number (for debugging)
    pub fn current_sequence(&self) -> u32 {
        self.current_sequence
    }

    /// Calculate estimated remaining writes before wear-out
    /// Assumes 100k erase cycles per sector
    pub fn estimated_remaining_writes(&self) -> u64 {
        const ERASE_CYCLES: u64 = 100_000;
        let erases_done = self.current_sequence as u64 / SLOTS_PER_SECTOR as u64;
        let remaining_erases = ERASE_CYCLES.saturating_sub(erases_done);
        remaining_erases * SLOTS_PER_SECTOR as u64
    }
}
