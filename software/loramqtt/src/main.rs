//! Minimal MQTT broker for a Milesight LoRaWAN gateway.
//!
//! Prints and decodes uplinks the gateway forwards, and routes published
//! messages to whoever has subscribed. That routing is what makes downlinks
//! possible: the gateway subscribes to its configured downlink topic, and
//! anything published there — with `mosquitto_pub`, say — is delivered to it
//! and queued for the device's next receive window.
//!
//! Speaks enough of MQTT 3.1.1 and 5.0 for that job and no more: QoS 0
//! delivery, no retained messages, no persistent sessions.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use milesight_d4::tlv;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

// MQTT packet types (upper 4 bits of first byte)
const CONNECT: u8 = 1;
const CONNACK: u8 = 2;
const PUBLISH: u8 = 3;
const PUBACK: u8 = 4;
const SUBSCRIBE: u8 = 8;
const SUBACK: u8 = 9;
const PINGREQ: u8 = 12;
const PINGRESP: u8 = 13;
const DISCONNECT: u8 = 14;

fn decode_remaining_length(stream: &mut TcpStream) -> std::io::Result<usize> {
    let mut multiplier: usize = 1;
    let mut value: usize = 0;
    let mut buf = [0u8; 1];
    loop {
        stream.read_exact(&mut buf)?;
        value += (buf[0] & 0x7F) as usize * multiplier;
        if buf[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
        if multiplier > 128 * 128 * 128 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "malformed remaining length",
            ));
        }
    }
    Ok(value)
}

fn encode_remaining_length(mut len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out
}

fn read_utf8_string(data: &[u8], offset: &mut usize) -> String {
    if *offset + 2 > data.len() {
        return String::new();
    }
    let len = ((data[*offset] as usize) << 8) | data[*offset + 1] as usize;
    *offset += 2;
    if *offset + len > data.len() {
        return String::new();
    }
    let s = String::from_utf8_lossy(&data[*offset..*offset + len]).to_string();
    *offset += len;
    s
}

fn send_packet(stream: &mut TcpStream, ptype: u8, flags: u8, payload: &[u8]) -> std::io::Result<()> {
    let header_byte = (ptype << 4) | (flags & 0x0F);
    let mut packet = vec![header_byte];
    packet.extend(encode_remaining_length(payload.len()));
    packet.extend_from_slice(payload);
    stream.write_all(&packet)
}

/// Decode an uplink payload with the shared TLV decoder.
fn decode_uplink_payload(bytes: &[u8], json: &serde_json::Value) {
    let decoded = tlv::decode(bytes);
    if decoded.fields.is_empty() {
        return;
    }
    let device_name = json
        .get("deviceName")
        .and_then(|v| v.as_str())
        .or_else(|| json.get("devEUI").and_then(|v| v.as_str()))
        .unwrap_or("?");
    println!("[d4] {} | {}", device_name, decoded.summary());
    if let Some(offset) = decoded.undecoded_at {
        println!("[d4] {} | stopped decoding at byte {}", device_name, offset);
    }
}

/// Connected clients, so a publish can be routed to its subscribers.
#[derive(Default)]
struct Broker {
    clients: Mutex<HashMap<usize, Client>>,
}

struct Client {
    peer: String,
    stream: TcpStream,
    subscriptions: Vec<String>,
}

impl Broker {
    fn add(&self, id: usize, peer: String, stream: TcpStream) {
        self.clients.lock().unwrap().insert(
            id,
            Client {
                peer,
                stream,
                subscriptions: Vec::new(),
            },
        );
    }

    fn remove(&self, id: usize) {
        self.clients.lock().unwrap().remove(&id);
    }

    fn subscribe(&self, id: usize, filter: String) {
        if let Some(client) = self.clients.lock().unwrap().get_mut(&id) {
            client.subscriptions.push(filter);
        }
    }

    /// Deliver a message to every subscriber whose filter matches.
    ///
    /// Delivered at QoS 0 regardless of what the subscriber asked for: the
    /// gateway re-queues downlinks itself, so there is nothing useful for this
    /// broker to add by tracking acknowledgements.
    fn route(&self, topic: &str, payload: &[u8], from: usize) {
        let mut clients = self.clients.lock().unwrap();
        for (id, client) in clients.iter_mut() {
            if *id == from {
                continue;
            }
            if !client.subscriptions.iter().any(|f| topic_matches(f, topic)) {
                continue;
            }
            let mut packet = Vec::new();
            packet.extend_from_slice(&(topic.len() as u16).to_be_bytes());
            packet.extend_from_slice(topic.as_bytes());
            packet.extend_from_slice(payload);
            match send_packet(&mut client.stream, PUBLISH, 0, &packet) {
                Ok(()) => println!(
                    "[mqtt] delivered {} bytes on {} to {}",
                    payload.len(),
                    topic,
                    client.peer
                ),
                Err(e) => println!("[mqtt] delivery to {} failed: {}", client.peer, e),
            }
        }
    }
}

/// MQTT topic filter matching, including `+` and `#` wildcards.
fn topic_matches(filter: &str, topic: &str) -> bool {
    let mut f = filter.split('/');
    let mut t = topic.split('/');

    loop {
        match (f.next(), t.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(_)) => continue,
            (Some(a), Some(b)) if a == b => continue,
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn handle_client(mut stream: TcpStream, broker: Arc<Broker>, id: usize) {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    println!("[mqtt] new connection from {}", peer);

    match stream.try_clone() {
        Ok(writer) => broker.add(id, peer.clone(), writer),
        Err(e) => {
            println!("[mqtt] {} cannot be registered for delivery: {}", peer, e);
            return;
        }
    }

    loop {
        // Read fixed header byte
        let mut header = [0u8; 1];
        if let Err(e) = stream.read_exact(&mut header) {
            println!("[mqtt] {} disconnected: {} ({:?})", peer, e, e.kind());
            break;
        }

        let ptype = header[0] >> 4;
        let flags = header[0] & 0x0F;

        let remaining = match decode_remaining_length(&mut stream) {
            Ok(v) => v,
            Err(e) => {
                println!("[mqtt] {} decode error: {}", peer, e);
                break;
            }
        };

        let mut payload = vec![0u8; remaining];
        if remaining > 0 {
            if stream.read_exact(&mut payload).is_err() {
                println!("[mqtt] {} read error", peer);
                break;
            }
        }

        if std::env::var("MQTT_TRACE").is_ok() {
            let preview: String = payload
                .iter()
                .take(48)
                .map(|b| format!("{:02x}", b))
                .collect();
            println!(
                "[trace] {} type={} flags={:x} len={} {}",
                peer, ptype, flags, remaining, preview
            );
        }

        match ptype {
            CONNECT => {
                let mut off = 0;
                let protocol = read_utf8_string(&payload, &mut off);
                let level = if off < payload.len() {
                    let l = payload[off];
                    off += 1;
                    l
                } else {
                    0
                };
                let connect_flags = if off < payload.len() { payload[off] } else { 0 };
                off += 1;
                let _keep_alive = if off + 1 < payload.len() {
                    ((payload[off] as u16) << 8) | payload[off + 1] as u16
                } else {
                    60
                };
                off += 2;

                let client_id = read_utf8_string(&payload, &mut off);

                // Skip will topic/message if present
                if connect_flags & 0x04 != 0 {
                    let _will_topic = read_utf8_string(&payload, &mut off);
                    let _will_msg = read_utf8_string(&payload, &mut off);
                }

                let username = if connect_flags & 0x80 != 0 {
                    read_utf8_string(&payload, &mut off)
                } else {
                    String::new()
                };

                println!(
                    "[mqtt] CONNECT protocol={} level={} client_id={} username={} keep_alive={}",
                    protocol, level, client_id, username, _keep_alive
                );

                // CONNACK. MQTT 5.0 (level 5) adds a property-length byte after
                // the reason code; 3.1.1 (level 4) ends at the return code.
                let connack: &[u8] = if level >= 5 {
                    &[0x00, 0x00, 0x00]
                } else {
                    &[0x00, 0x00]
                };
                let _ = send_packet(&mut stream, CONNACK, 0, connack);
                println!("[mqtt] CONNACK sent (accepted, v{})", level);
            }

            SUBSCRIBE => {
                if payload.len() < 2 {
                    continue;
                }
                let message_id = ((payload[0] as u16) << 8) | payload[1] as u16;
                let mut off = 2;
                let mut granted = Vec::new();

                while off < payload.len() {
                    let topic = read_utf8_string(&payload, &mut off);
                    let qos = if off < payload.len() {
                        let q = payload[off];
                        off += 1;
                        q
                    } else {
                        0
                    };
                    println!("[mqtt] SUBSCRIBE topic={} qos={}", topic, qos);
                    broker.subscribe(id, topic);
                    // Grant QoS 0 whatever was asked: that is all we deliver.
                    granted.push(0);
                    let _ = qos;
                }

                // SUBACK
                let mut suback = vec![(message_id >> 8) as u8, message_id as u8];
                suback.extend(&granted);
                let _ = send_packet(&mut stream, SUBACK, 0, &suback);
            }

            PUBLISH => {
                let mut off = 0;
                let topic = read_utf8_string(&payload, &mut off);

                let qos = (flags >> 1) & 0x03;
                let message_id = if qos > 0 && off + 1 < payload.len() {
                    let mid = ((payload[off] as u16) << 8) | payload[off + 1] as u16;
                    off += 2;
                    Some(mid)
                } else {
                    None
                };

                let data = &payload[off..];

                // Try to parse as JSON for pretty printing
                match std::str::from_utf8(data) {
                    Ok(text) => {
                        match serde_json::from_str::<serde_json::Value>(text) {
                            Ok(json) => {
                                println!(
                                    "\n[mqtt] PUBLISH topic={} ({} bytes)",
                                    topic,
                                    data.len()
                                );
                                println!("{}", serde_json::to_string_pretty(&json).unwrap());
                                // Decode the device payload if present
                                if let Some(data_b64) = json.get("data").and_then(|v| v.as_str()) {
                                    if let Ok(bytes) = BASE64.decode(data_b64) {
                                        decode_uplink_payload(&bytes, &json);
                                    }
                                }
                            }
                            Err(_) => {
                                println!(
                                    "[mqtt] PUBLISH topic={} payload={}",
                                    topic, text
                                );
                            }
                        }
                    }
                    Err(_) => {
                        println!(
                            "[mqtt] PUBLISH topic={} ({} bytes, binary)",
                            topic,
                            data.len()
                        );
                    }
                }

                // PUBACK for QoS 1
                if let Some(mid) = message_id {
                    let ack = [(mid >> 8) as u8, mid as u8];
                    let _ = send_packet(&mut stream, PUBACK, 0, &ack);
                }

                broker.route(&topic, data, id);
            }

            PINGREQ => {
                let _ = send_packet(&mut stream, PINGRESP, 0, &[]);
            }

            DISCONNECT => {
                println!("[mqtt] {} DISCONNECT", peer);
                break;
            }

            _ => {
                println!("[mqtt] {} unknown packet type={}", peer, ptype);
            }
        }
    }

    broker.remove(id);
}

fn main() {
    let addr = "0.0.0.0:1883";
    let listener = TcpListener::bind(addr).expect("failed to bind to port 1883");
    println!("=== LoRa MQTT broker listening on {} ===", addr);
    println!("Send a downlink with, for example:");
    println!(
        "  mosquitto_pub -h localhost -t d4/downlink -m '{{\"devEUI\":\"...\",\"fPort\":85,\"confirmed\":false,\"data\":\"/wMsAQ==\"}}'"
    );

    let broker = Arc::new(Broker::default());
    let next_id = AtomicUsize::new(1);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let broker = Arc::clone(&broker);
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                thread::spawn(move || handle_client(stream, broker, id));
            }
            Err(e) => {
                eprintln!("[mqtt] accept error: {}", e);
            }
        }
    }
}
