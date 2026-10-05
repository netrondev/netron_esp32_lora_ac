//! Class A MAC layer: joining, transmitting, and the receive windows.
//!
//! A Class A device is only reachable in the two short windows that follow one
//! of its own uplinks, so every downlink — including all remote configuration —
//! arrives as a consequence of a transmission. That makes this module the only
//! place where downlink timing lives.

use esp_hal::time::Instant;
use esp_println::println;

use crate::lora::{Bandwidth, LoRaDriver};
use crate::lorawan::{
    self, Downlink, LoRaWanSession, UplinkOptions, JOIN_REQUEST_LEN,
};

/// EU868 RX2 frequency.
const RX2_FREQ_HZ: u32 = 869_525_000;
/// EU868 RX2 default data rate is DR0 = SF12BW125.
const RX2_SF: u8 = 12;

/// Uplink data rate, which RX1 mirrors (with a zero DR offset).
const UPLINK_SF: u8 = 7;
const UPLINK_FREQ_HZ: u32 = 868_100_000;

/// Delay from end of join request to the first join-accept window.
const JOIN_ACCEPT_DELAY1_S: u64 = 5;

/// How early a window opens, in milliseconds.
///
/// Absorbs the jitter in the TxDone timestamp and the time the radio needs to
/// settle after being retuned. Opening early costs a little receive time;
/// opening late loses the packet outright.
const WINDOW_GUARD_MS: u64 = 20;

/// How long to listen once a window is open.
///
/// This bounds how long we wait for a *preamble*, not for a whole packet — the
/// radio's timer stops once it detects one and reception then runs to
/// completion. RX2 gets longer because an SF12 preamble alone lasts ~260 ms.
const RX1_WINDOW_MS: u32 = 200;
const RX2_WINDOW_MS: u32 = 600;

/// Largest frame we will receive: a join accept with a CFList is 33 bytes,
/// and a data downlink can carry a full payload.
const MAX_FRAME: usize = 128;

/// Outcome of one uplink and its receive windows.
pub struct UplinkResult {
    /// The radio reported the frame went out.
    pub transmitted: bool,
    /// A downlink was received, verified and decrypted.
    pub downlink: Option<Downlink>,
}

/// Attempt one OTAA join.
///
/// The caller must have incremented and persisted `session.dev_nonce` before
/// calling: a nonce that goes out on air is spent whether or not the join
/// succeeds, and reusing one gets the join silently dropped by the network.
pub fn join<R: LoRaDriver>(radio: &mut R, session: &mut LoRaWanSession) -> bool {
    let mut frame = [0u8; JOIN_REQUEST_LEN];
    let len = lorawan::build_join_request(session, &mut frame);

    radio.set_iq_inverted(false);
    let tx_done = match radio.transmit(&frame[..len]) {
        Some(instant) => instant,
        None => {
            println!("{{\"event\":\"join\",\"ok\":false,\"reason\":\"tx_failed\"}}");
            return false;
        }
    };

    println!(
        "{{\"event\":\"join_request\",\"dev_nonce\":{}}}",
        session.dev_nonce
    );

    let mut buf = [0u8; MAX_FRAME];
    // Join accept windows sit at +5 s and +6 s, not at the data RxDelay.
    let received = receive_windows(
        radio,
        tx_done,
        &mut buf,
        JOIN_ACCEPT_DELAY1_S,
        session.rx2_dr_sf(),
    );

    match received {
        Some(len) if lorawan::parse_join_accept(session, &buf[..len]) => {
            println!(
                "{{\"event\":\"join\",\"ok\":true,\"dev_addr\":\"{:02X}{:02X}{:02X}{:02X}\",\"rx_delay_s\":{}}}",
                session.dev_addr[3],
                session.dev_addr[2],
                session.dev_addr[1],
                session.dev_addr[0],
                session.rx_delay_s
            );
            true
        }
        Some(len) => {
            println!(
                "{{\"event\":\"join\",\"ok\":false,\"reason\":\"bad_accept\",\"len\":{}}}",
                len
            );
            false
        }
        None => {
            println!("{{\"event\":\"join\",\"ok\":false,\"reason\":\"no_accept\"}}");
            false
        }
    }
}

/// Send one uplink and listen in both receive windows.
///
/// Returns whether the frame went out, and any downlink that arrived. The
/// decrypted downlink payload is written to `downlink_buf`.
///
/// The frame counter is deliberately not advanced here: a confirmed frame that
/// goes unacknowledged is retransmitted with the same counter, so when to
/// advance is the caller's decision.
pub fn send_uplink<R: LoRaDriver>(
    radio: &mut R,
    session: &mut LoRaWanSession,
    opts: UplinkOptions,
    port: u8,
    payload: &[u8],
    downlink_buf: &mut [u8],
) -> UplinkResult {
    let mut frame = [0u8; MAX_FRAME];
    let len = lorawan::build_uplink(session, opts, port, payload, &mut frame);

    radio.set_iq_inverted(false);
    let tx_done = match radio.transmit(&frame[..len]) {
        Some(instant) => instant,
        None => {
            return UplinkResult {
                transmitted: false,
                downlink: None,
            }
        }
    };

    let mut raw = [0u8; MAX_FRAME];
    let rx_delay = session.rx_delay_s as u64;
    let rx2_sf = session.rx2_dr_sf();

    let downlink = match receive_windows(radio, tx_done, &mut raw, rx_delay, rx2_sf) {
        Some(len) => lorawan::parse_downlink(session, &raw[..len], downlink_buf),
        None => None,
    };

    UplinkResult {
        transmitted: true,
        downlink,
    }
}

/// Listen in RX1, then RX2 if RX1 was empty.
///
/// Both windows are timed from the end of the uplink, so RX2 is opened
/// relative to the same reference rather than to whenever RX1 gave up.
fn receive_windows<R: LoRaDriver>(
    radio: &mut R,
    tx_done: Instant,
    buf: &mut [u8],
    rx1_delay_s: u64,
    rx2_sf: u8,
) -> Option<usize> {
    // RX1: same channel and data rate as the uplink, inverted IQ.
    radio.set_iq_inverted(true);
    let rx1_open_ms = rx1_delay_s * 1000 - WINDOW_GUARD_MS;
    if let Some(len) = radio.receive_window(buf, tx_done, rx1_open_ms, RX1_WINDOW_MS) {
        println!(
            "{{\"event\":\"rx1\",\"len\":{},\"at_ms\":{}}}",
            len,
            tx_done.elapsed().as_millis()
        );
        restore_uplink_config(radio);
        return Some(len);
    }

    // RX2: fixed frequency and data rate, one second after RX1.
    radio.set_frequency(RX2_FREQ_HZ);
    radio.set_datarate(rx2_sf, Bandwidth::Bw125);
    let rx2_open_ms = (rx1_delay_s + 1) * 1000 - WINDOW_GUARD_MS;
    let result = radio.receive_window(buf, tx_done, rx2_open_ms, RX2_WINDOW_MS);
    if let Some(len) = result {
        println!(
            "{{\"event\":\"rx2\",\"len\":{},\"at_ms\":{}}}",
            len,
            tx_done.elapsed().as_millis()
        );
    }

    restore_uplink_config(radio);
    result
}

/// Put the radio back on the uplink channel and data rate.
fn restore_uplink_config<R: LoRaDriver>(radio: &mut R) {
    radio.set_frequency(UPLINK_FREQ_HZ);
    radio.set_datarate(UPLINK_SF, Bandwidth::Bw125);
    radio.set_iq_inverted(false);
}

impl LoRaWanSession {
    /// Spreading factor for the RX2 data rate index.
    ///
    /// EU868 maps DR0..DR5 to SF12..SF7 at BW125.
    fn rx2_dr_sf(&self) -> u8 {
        match self.rx2_dr {
            0 => 12,
            1 => 11,
            2 => 10,
            3 => 9,
            4 => 8,
            5 => 7,
            _ => RX2_SF,
        }
    }
}
