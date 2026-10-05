use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use milesight_d4::lorawan;
use milesight_d4::tlv::{self, Command};
use milesight_d4::{AddDeviceRequest, MilesightClient};
use std::collections::HashSet;
use std::env;
use std::process::exit;

fn config_path() -> String {
    for path in [
        format!("{}/config.json", env!("CARGO_MANIFEST_DIR")),
        "config.json".to_string(),
    ] {
        if std::path::Path::new(&path).exists() {
            return path;
        }
    }
    "config.json".to_string()
}

/// LoRaWAN port used for both uplinks and downlink commands.
const FPORT: i32 = 85;

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02X}", b)).collect()
}

/// Accept a payload as either hex or base64, whichever parses.
fn parse_payload(text: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = lorawan::hex_decode(text) {
        return Some(bytes);
    }
    BASE64.decode(text.trim()).ok()
}

/// Bind a device to an application, by name if given, else the first one.
async fn bind_device(
    client: &MilesightClient,
    dev_eui: &str,
    app_name: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let apps = client.get_applications().await?;
    let app = match app_name {
        Some(wanted) => apps
            .result
            .iter()
            .find(|a| a.name.as_deref() == Some(wanted))
            .ok_or_else(|| format!("No application named '{}'", wanted))?,
        None => apps
            .result
            .first()
            .ok_or("The gateway has no applications to bind to")?,
    };
    let app_id = app.id.clone().ok_or("Application has no id")?;
    client.bind_devices_to_application(&[dev_eui], &app_id).await?;
    Ok(app.name.clone().unwrap_or(app_id))
}

fn usage() -> ! {
    eprintln!("Usage: milesight_d4 <command> [args...]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  status                    Gateway status (model, EUI, uptime)");
    eprintln!("  devices                   List all NS devices");
    eprintln!("  device <devEUI>           Get single device details");
    eprintln!("  add-device <name> <devEUI> [appKey]");
    eprintln!("                            Add OTAA device (AppKey defaults to");
    eprintln!("                            DevEUI+DevEUI, which is what our firmware uses)");
    eprintln!("  delete-device <devEUI>    Delete device");
    eprintln!("  applications              List applications");
    eprintln!("  add-application <name> [description]");
    eprintln!("                            Create an NS application");
    eprintln!("  set-mqtt <name> <broker_addr> [port] [uplink_topic]");
    eprintln!("                            Create/reuse application <name> and point");
    eprintln!("                            its MQTT forwarder at the broker");
    eprintln!("  mqtt-off <name>           Disable an application's MQTT forwarder");
    eprintln!("  bind <devEUI> <appId>     Bind a device to an application");
    eprintln!("  packets [limit]           NS decoded packets (default: 20)");
    eprintln!("  traffic                   Raw packet forwarder traffic");
    eprintln!("  radio                     Radio/channel configuration");
    eprintln!("  ns-general                NS channel plan config");
    eprintln!("  packet-general            Packet forwarder config");
    eprintln!("  set-forwarder <mode> [addr] [port]");
    eprintln!("                            semtech <addr> = forward to an external");
    eprintln!("                            network server; embedded = gateway's own");
    eprintln!("  watch [poll_secs] [app]   Auto-onboard devices from their join");
    eprintln!("                            requests (default: 2s, first application)");
    eprintln!("  decode <hex|base64>       Decode an uplink payload");
    eprintln!("  encode <cmd> [cmd...]     Encode downlink commands to hex.");
    eprintln!("                            Commands: reboot, report=<s>, sample=<s>, jitter=<s>");
    exit(1);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
    }

    let client = MilesightClient::from_config(&config_path())?;
    client.login().await?;

    match args[1].as_str() {
        "status" => {
            let s = client.get_status().await?;
            println!("{}", serde_json::to_string_pretty(&s)?);
        }
        "devices" => {
            let d = client.get_devices("", 100, 0).await?;
            println!("{}", serde_json::to_string_pretty(&d)?);
        }
        "device" => {
            if args.len() < 3 { usage(); }
            let d = client.get_device(&args[2]).await?;
            println!("{}", serde_json::to_string_pretty(&d)?);
        }
        "add-device" => {
            if args.len() < 4 {
                eprintln!("Usage: milesight_d4 add-device <name> <devEUI> [appKey]");
                exit(1);
            }
            let dev_eui = args[3].to_uppercase();
            // Default to the AppKey our firmware derives, so the common case
            // needs no key handling at all.
            let app_key = match args.get(4) {
                Some(k) => k.to_uppercase(),
                None => format!("{}{}", dev_eui, dev_eui),
            };
            // The gateway rejects a device with no description ("description
            // error"), so always send one.
            let req = AddDeviceRequest::otaa(
                args[2].clone(),
                dev_eui,
                hex_upper(&lorawan::JOIN_EUI),
                app_key,
                FPORT,
                Some("D4 power monitor".to_string()),
            );
            let resp = client.add_device(&req).await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        "delete-device" => {
            if args.len() < 3 { usage(); }
            let resp = client.delete_devices(&[&args[2]]).await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        "applications" => {
            let a = client.get_applications().await?;
            println!("{}", serde_json::to_string_pretty(&a)?);
        }
        "add-application" => {
            if args.len() < 3 { usage(); }
            let desc = args.get(3).cloned().unwrap_or_default();
            let id = client.create_application(&args[2], &desc).await?;
            println!("Created application '{}' with id {}", args[2], id);
        }
        "set-mqtt" => {
            if args.len() < 4 {
                eprintln!("Usage: milesight_d4 set-mqtt <name> <broker_addr> [port] [uplink_topic]");
                exit(1);
            }
            let name = &args[2];
            let address = &args[3];
            let port: u16 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1883);
            let default_topic = format!("{}/uplink", name);
            let topic = args.get(5).unwrap_or(&default_topic);

            // Reuse an application of the same name if one already exists.
            let apps = client.get_applications().await?;
            let existing = apps.result.iter().find(|a| a.name.as_deref() == Some(name.as_str()));
            let app_id = match existing.and_then(|a| a.id.clone()) {
                Some(id) => {
                    println!("Reusing existing application '{}' (id {})", name, id);
                    id
                }
                None => {
                    let id = client.create_application(name, "D4 power monitors").await?;
                    println!("Created application '{}' (id {})", name, id);
                    id
                }
            };

            client
                .set_application_mqtt(&app_id, name, address, port, topic, true)
                .await?;
            println!("MQTT forwarding -> {}:{} topic '{}'", address, port, topic);
        }
        "mqtt-off" => {
            if args.len() < 3 {
                eprintln!("Usage: milesight_d4 mqtt-off <application_name>");
                exit(1);
            }
            let name = &args[2];
            let apps = client.get_applications().await?;
            let app_id = apps
                .result
                .iter()
                .find(|a| a.name.as_deref() == Some(name.as_str()))
                .and_then(|a| a.id.clone())
                .ok_or_else(|| format!("No application named '{}'", name))?;
            // Keep the topics and address populated while disabling: blank
            // ones are what crash the gateway.
            client
                .set_application_mqtt(&app_id, name, "127.0.0.1", 1883, "", false)
                .await?;
            println!("MQTT forwarding disabled for application '{}'", name);
        }
        "bind" => {
            if args.len() < 4 {
                eprintln!("Usage: milesight_d4 bind <devEUI> <appId>");
                exit(1);
            }
            let resp = client.bind_devices_to_application(&[&args[2]], &args[3]).await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        "packets" => {
            let limit = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20);
            let p = client.get_packets(limit, 0).await?;
            println!("{}", serde_json::to_string_pretty(&p)?);
        }
        "traffic" => {
            let t = client.get_traffic().await?;
            println!("{}", serde_json::to_string_pretty(&t)?);
        }
        "radio" => {
            let r = client.get_radio_config().await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        "ns-general" => {
            let g = client.get_ns_general().await?;
            println!("{}", serde_json::to_string_pretty(&g)?);
        }
        "set-forwarder" => {
            if args.len() < 3 {
                eprintln!("Usage: milesight_d4 set-forwarder <semtech|embedded> [address] [port]");
                eprintln!("  semtech <address> [port]  Forward to an external network server");
                eprintln!("  embedded                  Use the gateway's own network server");
                exit(1);
            }
            let (types, address) = match args[2].as_str() {
                "semtech" => {
                    let address = args.get(3).cloned().unwrap_or_else(|| {
                        eprintln!("An address is required for semtech mode");
                        exit(1);
                    });
                    (0, address)
                }
                "embedded" => (7, args.get(3).cloned().unwrap_or_default()),
                other => {
                    eprintln!("Unknown mode: {}", other);
                    exit(1);
                }
            };
            let port: u16 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1700);
            let resp = client
                .set_packet_forwarder(types, &address, port, port)
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            println!(
                "Forwarder set to {} ({})",
                args[2],
                if address.is_empty() { "-" } else { &address }
            );
        }
        "packet-general" => {
            let g = client.get_packet_general().await?;
            println!("{}", serde_json::to_string_pretty(&g)?);
        }
        "decode" => {
            if args.len() < 3 {
                eprintln!("Usage: milesight_d4 decode <hex|base64>");
                exit(1);
            }
            let payload = parse_payload(&args[2]).ok_or("Payload is neither hex nor base64")?;
            let decoded = tlv::decode(&payload);
            println!("{}", decoded.summary());
            if let Some(offset) = decoded.undecoded_at {
                eprintln!("Stopped decoding at byte {}", offset);
            }
        }
        "encode" => {
            if args.len() < 3 {
                eprintln!("Usage: milesight_d4 encode <cmd> [cmd...]");
                eprintln!("  e.g. milesight_d4 encode report=300 sample=5");
                exit(1);
            }
            let mut commands = Vec::new();
            for arg in &args[2..] {
                match Command::parse(arg) {
                    Some(cmd) => commands.push(cmd),
                    None => {
                        eprintln!("Unrecognised command: {}", arg);
                        exit(1);
                    }
                }
            }
            let payload = tlv::encode_commands(&commands);
            println!("{}", hex_upper(&payload));
            println!("base64: {}", BASE64.encode(&payload));
            println!("fPort:  {}", FPORT);
        }
        "watch" => {
            let poll_secs = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2u64);
            let app_name = args.get(3).cloned();
            println!(
                "Watching for join requests (polling every {}s)...",
                poll_secs
            );

            // Devices are onboarded from their join request: it carries the
            // DevEUI, and our AppKey convention (DevEUI repeated) means the
            // request can be authenticated without knowing anything about the
            // device in advance.
            //
            // NOTE: the traffic `id` is only the row index in the gateway's ring
            // buffer (newest = 0) and is reused for every new packet, so we must
            // dedup on the raw frame contents, not on `id`.
            let mut known_euis: HashSet<String> = HashSet::new();
            let mut seen_frames: HashSet<String> = HashSet::new();

            let devices = client.get_devices("", 100, 0).await?;
            for dev in devices.devices() {
                if let Some(eui) = dev.dev_eui {
                    known_euis.insert(eui.to_uppercase());
                }
            }
            println!("Known devices: {}", known_euis.len());
            for eui in &known_euis {
                println!("  {}", eui);
            }

            loop {
                let traffic = client.get_traffic().await?;

                for entry in &traffic.traffic_data {
                    if entry.direction != Some(0) {
                        continue;
                    }

                    let data_b64 = match &entry.data {
                        Some(d) => d,
                        None => continue,
                    };

                    // Skip frames we've already examined this run. Keyed on the
                    // raw frame (not the positional `id`) so newly arriving
                    // packets are always picked up.
                    if !seen_frames.insert(data_b64.clone()) {
                        continue;
                    }

                    let frame = match BASE64.decode(data_b64) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };

                    let join = match lorawan::parse_join_request(&frame) {
                        Some(j) => j,
                        None => continue,
                    };

                    let dev_eui = join.dev_eui_hex();
                    if known_euis.contains(&dev_eui) {
                        continue;
                    }

                    // Only onboard devices whose join request verifies against
                    // the AppKey our firmware would have used.
                    let app_key = join.app_key();
                    if !lorawan::verify_join_mic(&frame, &app_key) {
                        println!(
                            "Ignoring join from {} — MIC does not match our AppKey convention",
                            dev_eui
                        );
                        known_euis.insert(dev_eui);
                        continue;
                    }

                    println!("\n=== New device joining ===");
                    println!("  DevEUI:  {}", dev_eui);
                    println!("  JoinEUI: {}", join.join_eui_hex());
                    println!("  Nonce:   {}", join.dev_nonce);

                    let req = AddDeviceRequest::otaa(
                        join.device_name(),
                        dev_eui.clone(),
                        join.join_eui_hex(),
                        join.app_key_hex(),
                        FPORT,
                        Some("Auto-onboarded D4 power monitor".to_string()),
                    );

                    match client.add_device(&req).await {
                        Ok(_) => {
                            println!("  Registered as OTAA. It will join on its next attempt.");
                            known_euis.insert(dev_eui.clone());

                            match bind_device(&client, &dev_eui, app_name.as_deref()).await {
                                Ok(name) => println!("  Bound to application: {}", name),
                                Err(e) => eprintln!("  Bind failed: {}", e),
                            }
                        }
                        Err(e) => eprintln!("  Registration failed: {}", e),
                    }
                }

                // Keep seen_frames from growing unbounded
                if seen_frames.len() > 10000 {
                    seen_frames.clear();
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(poll_secs)).await;
            }
        }
        other => {
            eprintln!("Unknown command: {}", other);
            usage();
        }
    }

    Ok(())
}
