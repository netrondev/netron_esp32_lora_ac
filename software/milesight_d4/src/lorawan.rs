//! LoRaWAN 1.0.x frame parsing, MIC verification, and payload decryption.
//!
//! Ported from the firmware's `lorawan.rs` for use in auto-onboarding.
//! Uses the same AES-CMAC and AES-CTR implementations.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;

/// JoinEUI used by the fleet. All zeros: a private network with no
/// IEEE-assigned JoinEUI block. Must match the firmware's `JOIN_EUI`.
pub const JOIN_EUI: [u8; 8] = [0; 8];

/// Message type of a join request.
const MHDR_JOIN_REQUEST: u8 = 0x00;

/// A join request frame: MHDR(1) JoinEUI(8) DevEUI(8) DevNonce(2) MIC(4).
pub const JOIN_REQUEST_LEN: usize = 23;

/// Parsed LoRaWAN uplink frame.
pub struct UplinkFrame {
    pub dev_addr: [u8; 4],
    pub fcnt: u32,
    pub fport: Option<u8>,
    pub encrypted_payload: Vec<u8>,
    pub mic: [u8; 4],
}

/// A parsed join request.
pub struct JoinRequest {
    pub join_eui: [u8; 8],
    pub dev_eui: [u8; 8],
    pub dev_nonce: u16,
    pub mic: [u8; 4],
}

impl JoinRequest {
    /// DevEUI in the big-endian hex form the gateway API expects.
    pub fn dev_eui_hex(&self) -> String {
        hex_upper(&self.dev_eui)
    }

    pub fn join_eui_hex(&self) -> String {
        hex_upper(&self.join_eui)
    }

    /// The device's AppKey.
    ///
    /// Our firmware derives it as DevEUI repeated, following the convention
    /// Milesight uses for its own sensors. That is what lets a device be
    /// onboarded from nothing but the join request it puts on air.
    pub fn app_key(&self) -> [u8; 16] {
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&self.dev_eui);
        key[8..].copy_from_slice(&self.dev_eui);
        key
    }

    pub fn app_key_hex(&self) -> String {
        hex_upper(&self.app_key())
    }

    /// Stable device name derived from the DevEUI's MAC portion.
    pub fn device_name(&self) -> String {
        let e = &self.dev_eui;
        format!(
            "d4_{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            e[0], e[1], e[2], e[5], e[6], e[7]
        )
    }
}

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02X}", b)).collect()
}

// ── AES-CMAC (RFC 4493) ────────────────────────────────────────────────────

const RB: u8 = 0x87;

fn left_shift_block(input: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut overflow = 0u8;
    for i in (0..16).rev() {
        out[i] = (input[i] << 1) | overflow;
        overflow = (input[i] >> 7) & 1;
    }
    out
}

fn cmac_subkeys(cipher: &Aes128) -> ([u8; 16], [u8; 16]) {
    let mut l = GenericArray::default();
    cipher.encrypt_block(&mut l);
    let l_arr: [u8; 16] = l.into();

    let mut k1 = left_shift_block(&l_arr);
    if l_arr[0] & 0x80 != 0 {
        k1[15] ^= RB;
    }

    let mut k2 = left_shift_block(&k1);
    if k1[0] & 0x80 != 0 {
        k2[15] ^= RB;
    }

    (k1, k2)
}

fn aes_cmac(key: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let (k1, k2) = cmac_subkeys(&cipher);

    let n = if data.is_empty() {
        1
    } else {
        (data.len() + 15) / 16
    };
    let last_block_complete = !data.is_empty() && (data.len() % 16 == 0);

    let mut last = [0u8; 16];
    if last_block_complete {
        let offset = (n - 1) * 16;
        last.copy_from_slice(&data[offset..offset + 16]);
        for i in 0..16 {
            last[i] ^= k1[i];
        }
    } else {
        let offset = (n - 1) * 16;
        let remaining = data.len() - offset;
        last[..remaining].copy_from_slice(&data[offset..]);
        last[remaining] = 0x80;
        for i in 0..16 {
            last[i] ^= k2[i];
        }
    }

    let mut x = [0u8; 16];
    for i in 0..n - 1 {
        let offset = i * 16;
        for j in 0..16 {
            x[j] ^= data[offset + j];
        }
        let mut block = GenericArray::from(x);
        cipher.encrypt_block(&mut block);
        x = block.into();
    }

    for j in 0..16 {
        x[j] ^= last[j];
    }
    let mut block = GenericArray::from(x);
    cipher.encrypt_block(&mut block);
    block.into()
}

// ── LoRaWAN payload encryption/decryption (spec 4.3.3) ─────────────────────

fn encrypt_payload(
    key: &[u8; 16],
    dev_addr: &[u8; 4],
    dir: u8,
    fcnt: u32,
    payload: &[u8],
) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let num_blocks = (payload.len() + 15) / 16;
    let mut out = vec![0u8; payload.len()];

    for i in 0..num_blocks {
        let mut ai = [0u8; 16];
        ai[0] = 0x01;
        ai[5] = dir;
        ai[6] = dev_addr[0];
        ai[7] = dev_addr[1];
        ai[8] = dev_addr[2];
        ai[9] = dev_addr[3];
        ai[10] = (fcnt & 0xFF) as u8;
        ai[11] = ((fcnt >> 8) & 0xFF) as u8;
        ai[12] = ((fcnt >> 16) & 0xFF) as u8;
        ai[13] = ((fcnt >> 24) & 0xFF) as u8;
        ai[15] = (i + 1) as u8;

        let mut si = GenericArray::from(ai);
        cipher.encrypt_block(&mut si);

        let start = i * 16;
        let end = std::cmp::min(start + 16, payload.len());
        for j in start..end {
            out[j] = payload[j] ^ si[j - start];
        }
    }

    out
}

// ── MIC calculation (spec 4.4) ─────────────────────────────────────────────

fn compute_mic(
    nwk_s_key: &[u8; 16],
    dev_addr: &[u8; 4],
    dir: u8,
    fcnt: u32,
    msg: &[u8],
) -> [u8; 4] {
    let mut b0 = [0u8; 16];
    b0[0] = 0x49;
    b0[5] = dir;
    b0[6] = dev_addr[0];
    b0[7] = dev_addr[1];
    b0[8] = dev_addr[2];
    b0[9] = dev_addr[3];
    b0[10] = (fcnt & 0xFF) as u8;
    b0[11] = ((fcnt >> 8) & 0xFF) as u8;
    b0[12] = ((fcnt >> 16) & 0xFF) as u8;
    b0[13] = ((fcnt >> 24) & 0xFF) as u8;
    b0[15] = msg.len() as u8;

    let mut cmac_input = Vec::with_capacity(16 + msg.len());
    cmac_input.extend_from_slice(&b0);
    cmac_input.extend_from_slice(msg);

    let full = aes_cmac(nwk_s_key, &cmac_input);
    let mut mic = [0u8; 4];
    mic.copy_from_slice(&full[..4]);
    mic
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Parse a raw LoRaWAN uplink frame (hex-encoded from packet forwarder traffic).
pub fn parse_uplink(frame: &[u8]) -> Option<UplinkFrame> {
    // Min: MHDR(1) + DevAddr(4) + FCtrl(1) + FCnt(2) + MIC(4) = 12
    if frame.len() < 12 {
        return None;
    }

    let mhdr = frame[0];
    let mtype = mhdr & 0xE0;
    // 0x40 = Unconfirmed Data Up, 0x80 = Confirmed Data Up
    if mtype != 0x40 && mtype != 0x80 {
        return None;
    }

    let dev_addr = [frame[1], frame[2], frame[3], frame[4]];
    let fctrl = frame[5];
    let fopts_len = (fctrl & 0x0F) as usize;
    let fcnt = (frame[6] as u32) | ((frame[7] as u32) << 8);

    let fhdr_end = 8 + fopts_len;

    let (fport, payload_start) = if frame.len() > fhdr_end + 4 {
        (Some(frame[fhdr_end]), fhdr_end + 1)
    } else {
        (None, fhdr_end)
    };

    let payload_end = frame.len() - 4;
    let encrypted_payload = if payload_start < payload_end {
        frame[payload_start..payload_end].to_vec()
    } else {
        Vec::new()
    };

    let mut mic = [0u8; 4];
    mic.copy_from_slice(&frame[frame.len() - 4..]);

    Some(UplinkFrame {
        dev_addr,
        fcnt,
        fport,
        encrypted_payload,
        mic,
    })
}

/// Verify the MIC of a raw uplink frame using the given NwkSKey.
pub fn verify_mic(frame: &[u8], dev_addr: &[u8; 4], fcnt: u32, nwk_s_key: &[u8; 16]) -> bool {
    if frame.len() < 12 {
        return false;
    }
    let msg = &frame[..frame.len() - 4];
    let received_mic = &frame[frame.len() - 4..];
    let computed = compute_mic(nwk_s_key, dev_addr, 0, fcnt, msg);
    received_mic == computed
}

/// Decrypt an uplink payload using the given AppSKey.
pub fn decrypt_uplink(
    encrypted: &[u8],
    app_s_key: &[u8; 16],
    dev_addr: &[u8; 4],
    fcnt: u32,
) -> Vec<u8> {
    encrypt_payload(app_s_key, dev_addr, 0, fcnt, encrypted)
}

// ── Network-server side ────────────────────────────────────────────────────

/// Derive the session keys established by a join.
///
/// `NwkSKey = AES-ECB(AppKey, 0x01 | AppNonce | NetID | DevNonce | pad)`
/// `AppSKey = AES-ECB(AppKey, 0x02 | AppNonce | NetID | DevNonce | pad)`
pub fn derive_session_keys(
    app_key: &[u8; 16],
    app_nonce: &[u8; 3],
    net_id: &[u8; 3],
    dev_nonce: u16,
) -> ([u8; 16], [u8; 16]) {
    let cipher = Aes128::new(GenericArray::from_slice(app_key));
    let derive = |prefix: u8| {
        let mut block = [0u8; 16];
        block[0] = prefix;
        block[1..4].copy_from_slice(app_nonce);
        block[4..7].copy_from_slice(net_id);
        block[7..9].copy_from_slice(&dev_nonce.to_le_bytes());
        let mut b = GenericArray::from(block);
        cipher.encrypt_block(&mut b);
        b.into()
    };
    (derive(0x01), derive(0x02))
}

/// Build a join accept, ready to transmit.
///
/// The join accept is signed and then *encrypted with AES in the decrypt
/// direction*, so that the device recovers it by running AES encrypt — which
/// is all a constrained device needs to implement (LoRaWAN 1.0, section 6.2.5).
pub fn build_join_accept(
    app_key: &[u8; 16],
    app_nonce: &[u8; 3],
    net_id: &[u8; 3],
    dev_addr: &[u8; 4],
    dl_settings: u8,
    rx_delay: u8,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(app_nonce);
    body.extend_from_slice(net_id);
    body.extend_from_slice(dev_addr);
    body.push(dl_settings);
    body.push(rx_delay);

    // MIC covers MHDR and the cleartext body.
    let mut mic_input = vec![MHDR_JOIN_ACCEPT];
    mic_input.extend_from_slice(&body);
    let mic = aes_cmac(app_key, &mic_input);
    body.extend_from_slice(&mic[..4]);

    let cipher = Aes128::new(GenericArray::from_slice(app_key));
    let mut frame = vec![MHDR_JOIN_ACCEPT];
    for chunk in body.chunks(16) {
        let mut block = [0u8; 16];
        block[..chunk.len()].copy_from_slice(chunk);
        let mut b = GenericArray::from(block);
        cipher.decrypt_block(&mut b);
        frame.extend_from_slice(&b);
    }
    frame
}

/// Build a downlink data frame.
///
/// Pass an empty payload with `port` `None` for a frame that only carries an
/// acknowledgement.
pub fn build_downlink(
    nwk_s_key: &[u8; 16],
    app_s_key: &[u8; 16],
    dev_addr: &[u8; 4],
    fcnt: u32,
    ack: bool,
    frame_pending: bool,
    port: Option<u8>,
    payload: &[u8],
) -> Vec<u8> {
    let mut fctrl = 0u8;
    if ack {
        fctrl |= 0x20;
    }
    if frame_pending {
        fctrl |= 0x10;
    }

    let mut frame = vec![MHDR_UNCONFIRMED_DOWN];
    frame.extend_from_slice(dev_addr);
    frame.push(fctrl);
    frame.extend_from_slice(&(fcnt as u16).to_le_bytes());

    if let Some(port) = port {
        frame.push(port);
        if !payload.is_empty() {
            let key = if port == 0 { nwk_s_key } else { app_s_key };
            frame.extend_from_slice(&encrypt_payload(key, dev_addr, 1, fcnt, payload));
        }
    }

    let mic = compute_mic(nwk_s_key, dev_addr, 1, fcnt, &frame);
    frame.extend_from_slice(&mic);
    frame
}

/// Message type of an unconfirmed downlink.
const MHDR_UNCONFIRMED_DOWN: u8 = 0x60;
/// Message type of a join accept.
const MHDR_JOIN_ACCEPT: u8 = 0x20;

/// Parse a join request, without verifying it.
///
/// The EUIs are transmitted least-significant byte first and are returned in
/// big-endian order, matching how they are printed and how the gateway API
/// wants them.
pub fn parse_join_request(frame: &[u8]) -> Option<JoinRequest> {
    if frame.len() != JOIN_REQUEST_LEN || frame[0] & 0xE0 != MHDR_JOIN_REQUEST {
        return None;
    }

    let mut join_eui = [0u8; 8];
    let mut dev_eui = [0u8; 8];
    for i in 0..8 {
        join_eui[i] = frame[8 - i];
        dev_eui[i] = frame[16 - i];
    }

    let mut mic = [0u8; 4];
    mic.copy_from_slice(&frame[19..23]);

    Some(JoinRequest {
        join_eui,
        dev_eui,
        dev_nonce: u16::from_le_bytes([frame[17], frame[18]]),
        mic,
    })
}

/// Verify a join request's MIC against an AppKey.
///
/// Unlike data frames, the join MIC is a plain CMAC over the whole message —
/// there is no B0 block, because there is no session yet.
pub fn verify_join_mic(frame: &[u8], app_key: &[u8; 16]) -> bool {
    if frame.len() != JOIN_REQUEST_LEN {
        return false;
    }
    let computed = aes_cmac(app_key, &frame[..19]);
    computed[..4] == frame[19..23]
}


/// Decode a hex string to bytes.
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16).ok()?;
        out.push(byte);
    }
    Some(out)
}

/// Format DevAddr bytes (as stored in frame, LE) to big-endian hex string for gateway API.
pub fn dev_addr_to_api_hex(dev_addr: &[u8; 4]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}",
        dev_addr[3], dev_addr[2], dev_addr[1], dev_addr[0]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_decode() {
        assert_eq!(hex_decode("D401"), Some(vec![0xD4, 0x01]));
        assert_eq!(hex_decode("ff"), Some(vec![0xFF]));
        assert_eq!(hex_decode("zz"), None);
    }

    #[test]
    fn test_dev_addr_to_api_hex() {
        // DevAddr in frame (LE) for MAC 00:70:07:2D:26:30
        // dev_addr = [mac[2], mac[3], mac[4], mac[5]] = [0x07, 0x2D, 0x26, 0x30]
        assert_eq!(dev_addr_to_api_hex(&[0x07, 0x2D, 0x26, 0x30]), "30262D07");
    }

    /// A join request built the way the firmware builds one, so this covers
    /// both the little-endian EUI ordering and the MIC.
    fn sample_join_request() -> ([u8; 8], Vec<u8>) {
        let dev_eui: [u8; 8] = [0x00, 0x70, 0x07, 0xFF, 0xFE, 0x2D, 0x26, 0x30];
        let mut app_key = [0u8; 16];
        app_key[..8].copy_from_slice(&dev_eui);
        app_key[8..].copy_from_slice(&dev_eui);

        let mut frame = vec![MHDR_JOIN_REQUEST];
        for i in 0..8 {
            frame.push(JOIN_EUI[7 - i]);
        }
        for i in 0..8 {
            frame.push(dev_eui[7 - i]);
        }
        frame.extend_from_slice(&7u16.to_le_bytes());
        let mic = aes_cmac(&app_key, &frame);
        frame.extend_from_slice(&mic[..4]);

        (dev_eui, frame)
    }

    #[test]
    fn test_parse_join_request() {
        let (dev_eui, frame) = sample_join_request();
        let join = parse_join_request(&frame).expect("should parse");

        assert_eq!(join.dev_eui, dev_eui);
        assert_eq!(join.dev_eui_hex(), "007007FFFE2D2630");
        assert_eq!(join.join_eui, JOIN_EUI);
        assert_eq!(join.dev_nonce, 7);
        assert_eq!(join.device_name(), "d4_0070072d2630");
    }

    #[test]
    fn test_join_mic_verification() {
        let (_, frame) = sample_join_request();
        let join = parse_join_request(&frame).unwrap();

        assert!(verify_join_mic(&frame, &join.app_key()));
        assert!(!verify_join_mic(&frame, &[0xAA; 16]));
    }

    #[test]
    fn test_app_key_is_dev_eui_repeated() {
        let (dev_eui, frame) = sample_join_request();
        let join = parse_join_request(&frame).unwrap();
        assert_eq!(&join.app_key()[..8], &dev_eui);
        assert_eq!(&join.app_key()[8..], &dev_eui);
    }
}
