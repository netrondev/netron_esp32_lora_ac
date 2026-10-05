//! A minimal LoRaWAN network server for the D4 power monitors.
//!
//! The gateway runs as a plain Semtech UDP packet forwarder (Network Server →
//! type "Semtech", pointed at this host on port 1700) rather than using its
//! own built-in network server. That puts joins, session keys and the downlink
//! queue on this side, which is what makes remote configuration possible: the
//! gateway's own downlink path is its MQTT application forwarder, and enabling
//! that reboots the gateway.
//!
//! Run it, then queue commands over the control port:
//!
//!   cargo run -p lorans
//!   echo 'downlink E08CFEFFFE34C3AC report=300' | nc -q1 127.0.0.1 7788
//!   echo 'list' | nc -q1 127.0.0.1 7788

mod ns;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use milesight_d4::tlv::{self, Command};
use ns::{hex_upper, NetworkServer};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread;

/// Semtech GWMP packet types.
const PUSH_DATA: u8 = 0x00;
const PUSH_ACK: u8 = 0x01;
const PULL_DATA: u8 = 0x02;
const PULL_RESP: u8 = 0x03;
const PULL_ACK: u8 = 0x04;
const TX_ACK: u8 = 0x05;

const GWMP_VERSION: u8 = 0x02;

/// Port the packet forwarder sends to, for both uplinks and its downlink poll.
const UDP_PORT: u16 = 1700;
/// Line-oriented control port for queuing downlinks.
const CONTROL_ADDR: &str = "127.0.0.1:7788";
/// LoRaWAN port used by our devices.
const FPORT: u8 = 85;

/// Transmit power in dBm. EU868 allows 16 dBm ERP; the gateway's antenna gain
/// is already accounted for in its own configuration.
const TX_POWER_DBM: u8 = 14;

fn main() -> std::io::Result<()> {
    let state_path = std::env::var("LORANS_STATE").unwrap_or_else(|_| "sessions.json".to_string());
    let server = Arc::new(Mutex::new(NetworkServer::load(&state_path)));

    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            if let Err(e) = run_control(server) {
                eprintln!("[control] stopped: {}", e);
            }
        });
    }

    let socket = UdpSocket::bind(("0.0.0.0", UDP_PORT))?;
    println!("[ns] listening for packet forwarder traffic on 0.0.0.0:{}", UDP_PORT);
    println!("[ns] control port on {}", CONTROL_ADDR);

    // Where to send downlinks. The forwarder opens a second source port for
    // its PULL_DATA keepalives, and PULL_RESP has to go back to that one.
    let mut downlink_addr: Option<SocketAddr> = None;
    let mut buf = [0u8; 65_536];

    loop {
        let (len, from) = match socket.recv_from(&mut buf) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[udp] receive error: {}", e);
                continue;
            }
        };
        if len < 4 {
            continue;
        }

        let token = [buf[1], buf[2]];
        match buf[3] {
            PUSH_DATA => {
                // version(1) token(2) id(1) gateway EUI(8) then JSON
                let _ = socket.send_to(&[GWMP_VERSION, token[0], token[1], PUSH_ACK], from);
                if len > 12 {
                    handle_push_data(&buf[12..len], &server, &socket, downlink_addr);
                }
            }
            PULL_DATA => {
                let _ = socket.send_to(&[GWMP_VERSION, token[0], token[1], PULL_ACK], from);
                if downlink_addr != Some(from) {
                    println!("[udp] downlink path is {}", from);
                    downlink_addr = Some(from);
                }
            }
            TX_ACK => {
                if len > 12 {
                    let text = String::from_utf8_lossy(&buf[12..len]);
                    // An empty object, or an error field set to NONE, means the
                    // transmission was accepted.
                    if text.contains("error") && !text.contains("NONE") {
                        println!("[udp] transmit rejected by gateway: {}", text.trim());
                    }
                }
            }
            other => println!("[udp] unhandled packet type 0x{:02x} from {}", other, from),
        }
    }
}

/// Process the uplinks in a PUSH_DATA payload.
fn handle_push_data(
    json: &[u8],
    server: &Arc<Mutex<NetworkServer>>,
    socket: &UdpSocket,
    downlink_addr: Option<SocketAddr>,
) {
    let value: serde_json::Value = match serde_json::from_slice(json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[udp] malformed PUSH_DATA: {}", e);
            return;
        }
    };

    let Some(packets) = value.get("rxpk").and_then(|v| v.as_array()) else {
        return; // Status report rather than an uplink.
    };

    for packet in packets {
        let Some(data) = packet.get("data").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(frame) = BASE64.decode(data) else {
            continue;
        };

        let tmst = packet.get("tmst").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let freq = packet.get("freq").and_then(|v| v.as_f64()).unwrap_or(868.1);
        let datr = packet
            .get("datr")
            .and_then(|v| v.as_str())
            .unwrap_or("SF7BW125")
            .to_string();
        let rssi = packet.get("rssi").and_then(|v| v.as_i64()).unwrap_or(0);
        let snr = packet.get("lsnr").and_then(|v| v.as_f64()).unwrap_or(0.0);

        println!(
            "[rx] {:.1} MHz {} rssi {} snr {:.1} — {} bytes",
            freq,
            datr,
            rssi,
            snr,
            frame.len()
        );

        let transmit = server.lock().unwrap().handle_uplink(&frame);

        let Some(transmit) = transmit else { continue };
        let Some(addr) = downlink_addr else {
            println!("[tx] cannot send {}: gateway has not opened a downlink path yet", transmit.description);
            continue;
        };

        // RX1: same channel and data rate as the uplink, inverted polarity,
        // scheduled off the gateway's own clock so it lands in the window.
        let txpk = serde_json::json!({
            "txpk": {
                "imme": false,
                "tmst": tmst.wrapping_add(transmit.delay_us),
                "freq": freq,
                "rfch": 0,
                "powe": TX_POWER_DBM,
                "modu": "LORA",
                "datr": datr,
                "codr": "4/5",
                "ipol": true,
                "size": transmit.frame.len(),
                "data": BASE64.encode(&transmit.frame),
                "ncrc": true,
            }
        });

        let body = txpk.to_string();
        let mut packet_out = vec![GWMP_VERSION, 0x00, 0x00, PULL_RESP];
        packet_out.extend_from_slice(body.as_bytes());

        match socket.send_to(&packet_out, addr) {
            Ok(_) => println!(
                "[tx] scheduled {} at +{} ms",
                transmit.description,
                transmit.delay_us / 1000
            ),
            Err(e) => eprintln!("[tx] send failed: {}", e),
        }
    }
}

/// Line-oriented control interface.
fn run_control(server: Arc<Mutex<NetworkServer>>) -> std::io::Result<()> {
    let listener = TcpListener::bind(CONTROL_ADDR)?;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let server = Arc::clone(&server);
        thread::spawn(move || {
            if let Err(e) = handle_control_client(stream, server) {
                eprintln!("[control] client error: {}", e);
            }
        });
    }
    Ok(())
}

fn handle_control_client(
    stream: TcpStream,
    server: Arc<Mutex<NetworkServer>>,
) -> std::io::Result<()> {
    let mut out = stream.try_clone()?;
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let command = parts.next().unwrap_or("");

        match command {
            "list" => {
                let server = server.lock().unwrap();
                for device in server.devices() {
                    writeln!(
                        out,
                        "{} DevAddr {} fcnt_up {} fcnt_down {} queued {}",
                        device.dev_eui,
                        hex_upper(&[
                            device.dev_addr[3],
                            device.dev_addr[2],
                            device.dev_addr[1],
                            device.dev_addr[0]
                        ]),
                        device.fcnt_up,
                        device.fcnt_down,
                        device.queue.len()
                    )?;
                }
            }
            "downlink" => {
                let Some(dev_eui) = parts.next() else {
                    writeln!(out, "usage: downlink <devEUI> <report=300|sample=5|jitter=10|reboot|hex>")?;
                    continue;
                };
                let args: Vec<&str> = parts.collect();
                if args.is_empty() {
                    writeln!(out, "nothing to send")?;
                    continue;
                }

                // Accept either named commands or a raw hex payload.
                let payload = match encode_arguments(&args) {
                    Some(bytes) => bytes,
                    None => {
                        writeln!(out, "could not parse: {}", args.join(" "))?;
                        continue;
                    }
                };

                let result = server
                    .lock()
                    .unwrap()
                    .queue_downlink(dev_eui, FPORT, payload.clone());
                match result {
                    Ok(()) => writeln!(
                        out,
                        "queued {} for {} — it will go out after the device's next uplink",
                        hex_upper(&payload),
                        dev_eui.to_uppercase()
                    )?,
                    Err(e) => writeln!(out, "{}", e)?,
                }
            }
            "help" => {
                writeln!(out, "list")?;
                writeln!(out, "downlink <devEUI> report=<s> sample=<s> jitter=<s> reboot")?;
                writeln!(out, "downlink <devEUI> <hex>")?;
            }
            other => writeln!(out, "unknown command: {}", other)?,
        }
    }
    Ok(())
}

/// Turn control-port arguments into a downlink payload.
fn encode_arguments(args: &[&str]) -> Option<Vec<u8>> {
    if args.len() == 1 {
        if let Some(bytes) = milesight_d4::lorawan::hex_decode(args[0]) {
            return Some(bytes);
        }
    }
    let mut commands = Vec::new();
    for arg in args {
        commands.push(Command::parse(arg)?);
    }
    Some(tlv::encode_commands(&commands))
}
