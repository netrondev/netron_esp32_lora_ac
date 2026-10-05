//! Dump the gateway's raw radio traffic, decoding what we can.
//!
//! Uplink payloads are encrypted with a session key the network server holds
//! and we do not, so the frames here are shown at the LoRaWAN layer only.
//! To see decrypted payloads, use `milesight_d4 packets`, which reads the
//! network server's own decrypted view, and `milesight_d4 decode <hex>`.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use milesight_d4::lorawan;
use milesight_d4::MilesightClient;

fn mtype_name(m: u8) -> &'static str {
    match m & 0xE0 {
        0x00 => "JoinRequest",
        0x20 => "JoinAccept",
        0x40 => "UnconfDataUp",
        0x60 => "UnconfDataDown",
        0x80 => "ConfDataUp",
        0xA0 => "ConfDataDown",
        0xC0 => "RejoinRequest",
        _ => "Proprietary",
    }
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02X}", x)).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = MilesightClient::from_config("config.json")?;
    client.login().await?;
    let t = client.get_traffic().await?;
    println!("{} traffic entries\n", t.traffic_data.len());

    for e in &t.traffic_data {
        let b64 = e.data.clone().unwrap_or_default();
        let frame = B64.decode(b64.trim()).unwrap_or_default();
        let dir = if e.direction == Some(0) { "UP" } else { "DN" };
        println!(
            "── id {} [{}] {}  {} MHz {} ch{} RSSI {} SNR {}",
            e.id.unwrap_or(-1),
            dir,
            e.time.clone().unwrap_or_default(),
            e.freq.clone().unwrap_or_default(),
            e.rate.clone().unwrap_or_default(),
            e.channel.clone().unwrap_or_default(),
            e.rssi.clone().unwrap_or_default(),
            e.snr.clone().unwrap_or_default()
        );
        if frame.is_empty() {
            println!("   (no data)\n");
            continue;
        }
        let mhdr = frame[0];
        println!(
            "   MHDR 0x{:02X} = {}  ({} bytes)",
            mhdr,
            mtype_name(mhdr),
            frame.len()
        );

        match mhdr & 0xE0 {
            0x00 => match lorawan::parse_join_request(&frame) {
                Some(join) => {
                    let mic_ok = lorawan::verify_join_mic(&frame, &join.app_key());
                    println!("   JoinEUI  {}", join.join_eui_hex());
                    println!("   DevEUI   {}", join.dev_eui_hex());
                    println!("   DevNonce 0x{:04X}", join.dev_nonce);
                    println!(
                        "   MIC vs DevEUI-derived AppKey: {}",
                        if mic_ok { "OK — one of ours" } else { "mismatch" }
                    );
                }
                None => println!("   (unexpected join length)"),
            },
            // Both data-up types; a confirmed uplink is 0x80, not 0x60.
            0x40 | 0x80 => {
                if let Some(up) = lorawan::parse_uplink(&frame) {
                    println!(
                        "   DevAddr {}  FCnt {}  FPort {:?}",
                        lorawan::dev_addr_to_api_hex(&up.dev_addr),
                        up.fcnt,
                        up.fport
                    );
                    if !up.encrypted_payload.is_empty() {
                        println!("   FRMPayload (encrypted) {}", hexs(&up.encrypted_payload));
                    }
                }
            }
            _ => {
                println!("   payload {}", hexs(&frame));
            }
        }
        println!();
    }
    Ok(())
}
