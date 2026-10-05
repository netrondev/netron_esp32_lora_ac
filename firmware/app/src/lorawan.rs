//! LoRaWAN 1.0.x OTAA framing.
//!
//! Provides join-request construction, join-accept processing and session-key
//! derivation, plus uplink building and downlink parsing with the encryption
//! and MIC calculation required by the LoRaWAN 1.0 spec.

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};

/// Join-request message type.
const MHDR_JOIN_REQUEST: u8 = 0x00;
/// Join-accept message type.
const MHDR_JOIN_ACCEPT: u8 = 0x20;
/// Unconfirmed data up message type.
pub const MHDR_UNCONFIRMED_UP: u8 = 0x40;
/// Confirmed data up message type.
pub const MHDR_CONFIRMED_UP: u8 = 0x80;

/// Length of a join-request frame: MHDR + JoinEUI + DevEUI + DevNonce + MIC.
pub const JOIN_REQUEST_LEN: usize = 1 + 8 + 8 + 2 + 4;

/// FCtrl ACK bit, set on an uplink acknowledging a confirmed downlink.
const FCTRL_ACK: u8 = 0x20;
/// FCtrl FPending bit in a downlink — the network has more queued for us.
const FCTRL_FPENDING: u8 = 0x10;

/// OTAA session state.
///
/// The identity fields are device-lifetime values; everything from `joined`
/// down is established by a join accept and is discarded on a rejoin.
pub struct LoRaWanSession {
    // ----- Identity (persistent) -----
    /// Device EUI, big-endian as printed. Sent little-endian in the join request.
    pub dev_eui: [u8; 8],
    /// Join EUI (AppEUI), big-endian. Sent little-endian in the join request.
    pub join_eui: [u8; 8],
    /// Root key used to sign the join request and derive session keys.
    pub app_key: [u8; 16],
    /// Monotonic join counter. Must never repeat, so it is persisted and
    /// incremented before every join attempt.
    pub dev_nonce: u16,

    // ----- Session (from join accept) -----
    /// Whether a join accept has been processed and the session below is valid.
    pub joined: bool,
    /// Device address (appears little-endian in frames).
    pub dev_addr: [u8; 4],
    /// Network session key — used for MIC calculation.
    pub nwk_s_key: [u8; 16],
    /// Application session key — used for payload encryption (FPort > 0).
    pub app_s_key: [u8; 16],
    /// Uplink frame counter.
    pub fcnt_up: u32,
    /// Next expected downlink frame counter.
    pub fcnt_down: u32,
    /// Delay from end of uplink to the RX1 window, in seconds.
    pub rx_delay_s: u8,
    /// RX1 data rate offset from the uplink data rate.
    pub rx1_dr_offset: u8,
    /// RX2 data rate index.
    pub rx2_dr: u8,
}

impl LoRaWanSession {
    /// Create an unjoined session for a device identity.
    pub fn new(dev_eui: [u8; 8], join_eui: [u8; 8], app_key: [u8; 16], dev_nonce: u16) -> Self {
        Self {
            dev_eui,
            join_eui,
            app_key,
            dev_nonce,
            joined: false,
            dev_addr: [0; 4],
            nwk_s_key: [0; 16],
            app_s_key: [0; 16],
            fcnt_up: 0,
            fcnt_down: 0,
            rx_delay_s: 1,
            rx1_dr_offset: 0,
            rx2_dr: 0,
        }
    }
}

/// A parsed, MIC-verified, decrypted downlink.
#[derive(Debug, Clone, Copy)]
pub struct Downlink {
    /// FPort. 0 means the payload carries MAC commands.
    pub port: u8,
    /// Number of decrypted payload bytes written to the caller's buffer.
    pub len: usize,
    /// The network acknowledged our confirmed uplink.
    pub ack: bool,
    /// The network has more downlinks queued for us.
    pub frame_pending: bool,
}

/// How an uplink should be framed.
#[derive(Debug, Clone, Copy, Default)]
pub struct UplinkOptions {
    /// Ask the network to acknowledge this frame.
    pub confirmed: bool,
    /// Acknowledge a confirmed downlink we just received.
    pub ack: bool,
}

// ----- AES-CMAC (RFC 4493) helpers ------------------------------------------

/// Rb constant for AES-128 CMAC subkey generation.
const RB: u8 = 0x87;

/// Left-shift a 16-byte block by one bit.
fn left_shift_block(input: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut overflow = 0u8;
    for i in (0..16).rev() {
        out[i] = (input[i] << 1) | overflow;
        overflow = (input[i] >> 7) & 1;
    }
    out
}

/// Generate CMAC subkeys K1 and K2 from the cipher key.
fn cmac_subkeys(cipher: &Aes128) -> ([u8; 16], [u8; 16]) {
    // L = AES-ECB(K, 0^16)
    let mut l = GenericArray::default();
    cipher.encrypt_block(&mut l);

    let l_arr: [u8; 16] = l.into();

    // K1
    let mut k1 = left_shift_block(&l_arr);
    if l_arr[0] & 0x80 != 0 {
        k1[15] ^= RB;
    }

    // K2
    let mut k2 = left_shift_block(&k1);
    if k1[0] & 0x80 != 0 {
        k2[15] ^= RB;
    }

    (k1, k2)
}

/// Compute full 16-byte AES-CMAC over `data`.
fn aes_cmac(key: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let (k1, k2) = cmac_subkeys(&cipher);

    let n = if data.is_empty() {
        1
    } else {
        (data.len() + 15) / 16
    };
    let last_block_complete = !data.is_empty() && (data.len() % 16 == 0);

    // Build the last (possibly padded) block
    let mut last = [0u8; 16];
    if last_block_complete {
        let offset = (n - 1) * 16;
        last.copy_from_slice(&data[offset..offset + 16]);
        for i in 0..16 {
            last[i] ^= k1[i];
        }
    } else {
        // Pad: existing bytes, then 0x80, then zeros
        let offset = (n - 1) * 16;
        let remaining = data.len() - offset;
        last[..remaining].copy_from_slice(&data[offset..]);
        last[remaining] = 0x80;
        for i in 0..16 {
            last[i] ^= k2[i];
        }
    }

    // CBC-MAC
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

    // XOR with prepared last block
    for j in 0..16 {
        x[j] ^= last[j];
    }
    let mut block = GenericArray::from(x);
    cipher.encrypt_block(&mut block);

    block.into()
}

// ----- LoRaWAN payload encryption (spec 4.3.3) ------------------------------

/// Encrypt (or decrypt) payload using LoRaWAN AES-CTR-like scheme.
/// `dir`: 0 = uplink, 1 = downlink.
fn encrypt_payload(
    key: &[u8; 16],
    dev_addr: &[u8; 4],
    dir: u8,
    fcnt: u32,
    payload: &[u8],
    out: &mut [u8],
) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let num_blocks = (payload.len() + 15) / 16;

    for i in 0..num_blocks {
        // Build Ai block (spec section 4.3.3)
        let mut ai = [0u8; 16];
        ai[0] = 0x01;
        // ai[1..5] = 0x00 (four zeros)
        ai[5] = dir;
        ai[6] = dev_addr[0];
        ai[7] = dev_addr[1];
        ai[8] = dev_addr[2];
        ai[9] = dev_addr[3];
        ai[10] = (fcnt & 0xFF) as u8;
        ai[11] = ((fcnt >> 8) & 0xFF) as u8;
        ai[12] = ((fcnt >> 16) & 0xFF) as u8;
        ai[13] = ((fcnt >> 24) & 0xFF) as u8;
        // ai[14] = 0x00
        ai[15] = (i + 1) as u8;

        let mut si = GenericArray::from(ai);
        cipher.encrypt_block(&mut si);

        let start = i * 16;
        let end = core::cmp::min(start + 16, payload.len());
        for j in start..end {
            out[j] = payload[j] ^ si[j - start];
        }
    }
}

// ----- MIC calculation (spec 4.4) -------------------------------------------

/// Compute the 4-byte MIC for a LoRaWAN frame.
/// `msg` is MHDR..FRMPayload (everything before MIC).
/// `dir`: 0 = uplink, 1 = downlink.
fn compute_mic(
    nwk_s_key: &[u8; 16],
    dev_addr: &[u8; 4],
    dir: u8,
    fcnt: u32,
    msg: &[u8],
) -> [u8; 4] {
    // Build B0 block
    let mut b0 = [0u8; 16];
    b0[0] = 0x49;
    // b0[1..5] = 0x00
    b0[5] = dir;
    b0[6] = dev_addr[0];
    b0[7] = dev_addr[1];
    b0[8] = dev_addr[2];
    b0[9] = dev_addr[3];
    b0[10] = (fcnt & 0xFF) as u8;
    b0[11] = ((fcnt >> 8) & 0xFF) as u8;
    b0[12] = ((fcnt >> 16) & 0xFF) as u8;
    b0[13] = ((fcnt >> 24) & 0xFF) as u8;
    // b0[14] = 0x00
    b0[15] = msg.len() as u8;

    // Concatenate B0 || msg into a temporary buffer.
    // Max LoRaWAN payload is ~255 bytes; 16 + 255 + overhead is well under 300.
    let total = 16 + msg.len();
    let mut cmac_input = [0u8; 280];
    cmac_input[..16].copy_from_slice(&b0);
    cmac_input[16..total].copy_from_slice(msg);

    let full = aes_cmac(nwk_s_key, &cmac_input[..total]);
    let mut mic = [0u8; 4];
    mic.copy_from_slice(&full[..4]);
    mic
}

// ----- OTAA join ------------------------------------------------------------

/// AES-128 ECB encrypt one block in place.
fn aes_encrypt_block(key: &[u8; 16], block: &mut [u8; 16]) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut b = GenericArray::from(*block);
    cipher.encrypt_block(&mut b);
    *block = b.into();
}

/// Build a join request (MHDR = 0x00).
///
/// Layout: MHDR(1) | JoinEUI(8,LE) | DevEUI(8,LE) | DevNonce(2,LE) | MIC(4)
///
/// The MIC is a plain CMAC over the whole message with the AppKey — the B0
/// block used for data frames does not apply here.
///
/// The caller is responsible for having incremented and persisted
/// `session.dev_nonce` beforehand: a repeated DevNonce is rejected by the
/// network server, and a join that is attempted but never completed still
/// burns the nonce.
pub fn build_join_request(session: &LoRaWanSession, buf: &mut [u8]) -> usize {
    buf[0] = MHDR_JOIN_REQUEST;

    // Both EUIs go on the wire least-significant byte first.
    for i in 0..8 {
        buf[1 + i] = session.join_eui[7 - i];
        buf[9 + i] = session.dev_eui[7 - i];
    }

    buf[17] = (session.dev_nonce & 0xFF) as u8;
    buf[18] = (session.dev_nonce >> 8) as u8;

    let mic = aes_cmac(&session.app_key, &buf[..19]);
    buf[19..23].copy_from_slice(&mic[..4]);

    JOIN_REQUEST_LEN
}

/// Derive the session keys from a join accept.
///
/// `NwkSKey = AES-ECB(AppKey, 0x01 | AppNonce | NetID | DevNonce | pad)`
/// `AppSKey = AES-ECB(AppKey, 0x02 | AppNonce | NetID | DevNonce | pad)`
fn derive_session_keys(
    app_key: &[u8; 16],
    app_nonce: &[u8; 3],
    net_id: &[u8; 3],
    dev_nonce: u16,
) -> ([u8; 16], [u8; 16]) {
    let derive = |prefix: u8| {
        let mut block = [0u8; 16];
        block[0] = prefix;
        block[1..4].copy_from_slice(app_nonce);
        block[4..7].copy_from_slice(net_id);
        block[7] = (dev_nonce & 0xFF) as u8;
        block[8] = (dev_nonce >> 8) as u8;
        aes_encrypt_block(app_key, &mut block);
        block
    };

    (derive(0x01), derive(0x02))
}

/// Decrypt and verify a join accept, and install the resulting session.
///
/// Returns false if the frame is not a join accept, has an unexpected length,
/// or fails MIC verification — in all of which cases the session is untouched.
///
/// Note the join accept is *encrypted* with the AppKey, so decrypting it means
/// running AES in the encrypt direction (LoRaWAN 1.0 spec, section 6.2.5).
pub fn parse_join_accept(session: &mut LoRaWanSession, data: &[u8]) -> bool {
    // MHDR(1) + encrypted body. The body is 16 bytes, or 32 with a CFList.
    if data.len() != 17 && data.len() != 33 {
        return false;
    }
    if data[0] & 0xE0 != MHDR_JOIN_ACCEPT {
        return false;
    }

    let mut plain = [0u8; 32];
    let body_len = data.len() - 1;
    for offset in (0..body_len).step_by(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(&data[1 + offset..1 + offset + 16]);
        aes_encrypt_block(&session.app_key, &mut block);
        plain[offset..offset + 16].copy_from_slice(&block);
    }

    // MIC covers MHDR followed by the decrypted body, excluding the MIC itself.
    let mic_offset = body_len - 4;
    let mut mic_input = [0u8; 29];
    mic_input[0] = data[0];
    mic_input[1..1 + mic_offset].copy_from_slice(&plain[..mic_offset]);
    let computed = aes_cmac(&session.app_key, &mic_input[..1 + mic_offset]);
    if computed[..4] != plain[mic_offset..mic_offset + 4] {
        return false;
    }

    let app_nonce: [u8; 3] = [plain[0], plain[1], plain[2]];
    let net_id: [u8; 3] = [plain[3], plain[4], plain[5]];
    let (nwk_s_key, app_s_key) =
        derive_session_keys(&session.app_key, &app_nonce, &net_id, session.dev_nonce);

    // DevAddr arrives little-endian and is kept in that order, which is how
    // it appears in every subsequent frame.
    session.dev_addr = [plain[6], plain[7], plain[8], plain[9]];
    session.rx1_dr_offset = (plain[10] >> 4) & 0x07;
    session.rx2_dr = plain[10] & 0x0F;
    // A RxDelay of 0 means 1 second.
    session.rx_delay_s = if plain[11] & 0x0F == 0 {
        1
    } else {
        plain[11] & 0x0F
    };
    session.nwk_s_key = nwk_s_key;
    session.app_s_key = app_s_key;
    session.fcnt_up = 0;
    session.fcnt_down = 0;
    session.joined = true;

    // A CFList may follow. We transmit on the join channel only, so the extra
    // channels it would add are deliberately ignored.
    true
}

// ----- Public API -----------------------------------------------------------

/// Build a data-up frame.
///
/// Writes the complete frame into `buf` and returns the total length.
///
/// The frame counter is deliberately *not* incremented here. A confirmed
/// uplink that goes unacknowledged is retransmitted with the same FCnt, so
/// advancing the counter is the caller's decision once it is done with the
/// frame.
///
/// Frame layout:
///   MHDR(1) | DevAddr(4,LE) | FCtrl(1) | FCnt(2,LE) | FPort(1) | FRMPayload(N) | MIC(4)
pub fn build_uplink(
    session: &LoRaWanSession,
    opts: UplinkOptions,
    port: u8,
    payload: &[u8],
    buf: &mut [u8],
) -> usize {
    let fcnt = session.fcnt_up;
    let frame_len = 1 + 4 + 1 + 2 + 1 + payload.len() + 4; // MHDR + DevAddr + FCtrl + FCnt + FPort + payload + MIC

    buf[0] = if opts.confirmed {
        MHDR_CONFIRMED_UP
    } else {
        MHDR_UNCONFIRMED_UP
    };

    // DevAddr (little-endian in frame)
    buf[1] = session.dev_addr[0];
    buf[2] = session.dev_addr[1];
    buf[3] = session.dev_addr[2];
    buf[4] = session.dev_addr[3];

    // FCtrl: no ADR, no FOpts; ACK only when answering a confirmed downlink.
    buf[5] = if opts.ack { FCTRL_ACK } else { 0x00 };

    // FCnt: lower 16 bits, little-endian
    buf[6] = (fcnt & 0xFF) as u8;
    buf[7] = ((fcnt >> 8) & 0xFF) as u8;

    // FPort
    buf[8] = port;

    // Encrypt payload (AppSKey for FPort > 0)
    let key = if port > 0 {
        &session.app_s_key
    } else {
        &session.nwk_s_key
    };
    encrypt_payload(
        key,
        &session.dev_addr,
        0, // uplink
        fcnt,
        payload,
        &mut buf[9..],
    );

    // MIC over MHDR..FRMPayload
    let mic_end = 9 + payload.len();
    let mic = compute_mic(
        &session.nwk_s_key,
        &session.dev_addr,
        0, // uplink
        fcnt,
        &buf[..mic_end],
    );
    buf[mic_end..mic_end + 4].copy_from_slice(&mic);

    frame_len
}

/// Parse a downlink frame, verify MIC, decrypt payload.
///
/// On success returns `(port, payload_length)` with decrypted payload in `payload_buf`.
///
/// `data` is the raw received bytes (MHDR through MIC inclusive).
/// `payload_buf` receives the decrypted FRMPayload.
pub fn parse_downlink(
    session: &mut LoRaWanSession,
    data: &[u8],
    payload_buf: &mut [u8],
) -> Option<Downlink> {
    // Minimum frame: MHDR(1) + DevAddr(4) + FCtrl(1) + FCnt(2) + MIC(4) = 12
    if data.len() < 12 {
        return None;
    }

    let mhdr = data[0];
    // Unconfirmed Data Down = 0x60, Confirmed Data Down = 0xA0
    let mtype = mhdr & 0xE0;
    if mtype != 0x60 && mtype != 0xA0 {
        return None;
    }

    // Check DevAddr matches
    if data[1] != session.dev_addr[0]
        || data[2] != session.dev_addr[1]
        || data[3] != session.dev_addr[2]
        || data[4] != session.dev_addr[3]
    {
        return None;
    }

    let fctrl = data[5];
    let fopts_len = (fctrl & 0x0F) as usize;
    let fcnt_low = (data[6] as u32) | ((data[7] as u32) << 8);

    // Reconstruct full 32-bit FCnt using stored upper bits
    let fcnt = (session.fcnt_down & 0xFFFF0000) | fcnt_low;

    // Reject replays: fcnt_down holds the next counter value we will accept.
    if fcnt < session.fcnt_down {
        return None;
    }

    // Header length = MHDR(1) + DevAddr(4) + FCtrl(1) + FCnt(2) + FOpts(N)
    let fhdr_len = 7 + fopts_len;

    // Determine if FPort and FRMPayload exist
    let has_payload = data.len() > fhdr_len + 1 + 4; // +1 for FPort, +4 for MIC
    let port;
    let payload_offset;
    let payload_len;

    if has_payload {
        // data[0] = MHDR
        // data[1..5] = DevAddr
        // data[5] = FCtrl
        // data[6..8] = FCnt
        // data[8..8+fopts_len] = FOpts
        // data[8+fopts_len] = FPort (if present)
        // data[8+fopts_len+1 .. len-4] = FRMPayload
        // data[len-4 .. len] = MIC
        let fport_idx = 8 + fopts_len;
        if data.len() < fport_idx + 1 + 4 {
            // Not enough data for FPort + MIC
            return None;
        }
        port = data[fport_idx];
        payload_offset = fport_idx + 1;
        payload_len = data.len() - 4 - payload_offset;
    } else {
        // No FPort/payload, just header + MIC
        port = 0;
        payload_offset = 0;
        payload_len = 0;
    }

    // Verify MIC
    let msg = &data[..data.len() - 4];
    let received_mic = &data[data.len() - 4..];
    let computed_mic = compute_mic(
        &session.nwk_s_key,
        &session.dev_addr,
        1, // downlink
        fcnt,
        msg,
    );
    if received_mic != computed_mic {
        return None;
    }

    // Decrypt payload
    if payload_len > 0 && payload_len <= payload_buf.len() {
        let key = if port > 0 {
            &session.app_s_key
        } else {
            &session.nwk_s_key
        };
        encrypt_payload(
            key,
            &session.dev_addr,
            1, // downlink
            fcnt,
            &data[payload_offset..payload_offset + payload_len],
            payload_buf,
        );
    }

    // Update downlink frame counter
    session.fcnt_down = fcnt + 1;

    Some(Downlink {
        port,
        len: payload_len,
        ack: fctrl & FCTRL_ACK != 0,
        frame_pending: fctrl & FCTRL_FPENDING != 0,
    })
}
