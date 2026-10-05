pub mod lorawan;
pub mod tlv;

use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use reqwest::Client;
use serde::{Deserialize, Serialize};

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub gateway_url: String,
    pub username: String,
    pub password: String,
}

impl Config {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }
}

// ── Password encryption ─────────────────────────────────────────────────────
// The Milesight web UI encrypts passwords with AES-128-CBC before sending.
// Key: "1111111111111111" (UTF-8), IV: "2222222222222222" (UTF-8)

fn encrypt_password(password: &str) -> String {
    let key = b"1111111111111111";
    let iv = b"2222222222222222";
    let mut buf = [0u8; 256];
    let plaintext = password.as_bytes();
    buf[..plaintext.len()].copy_from_slice(plaintext);
    let ct = Aes128CbcEnc::new(key.into(), iv.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .expect("buffer too small for encryption");
    BASE64.encode(ct)
}

// ── API response types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeviceInfo {
    pub need_enter: Option<i32>,
    pub need_set_passwd: Option<i32>,
    pub background_run: Option<i32>,
    pub login: Option<i32>,
    pub oem_id: Option<String>,
    pub product_id: Option<i32>,
    pub sonboardsn: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StatusOverview {
    pub model: Option<String>,
    pub eui: Option<String>,
    pub frequency_band: Option<String>,
    pub sn: Option<String>,
    pub firmware_version: Option<String>,
    pub hardware_version: Option<String>,
    pub local_time: Option<String>,
    pub run_time: Option<String>,
    pub cpu_temp: Option<String>,
    pub wlan_status: Option<i32>,
    pub ssid: Option<String>,
    pub lora_status: Option<i32>,
    pub pkt_fwd_status: Option<i32>,
    pub pkt_fwd_type: Option<i32>,
    pub cellular_status: Option<i32>,
    pub ip: Option<String>,
    pub link_in_use: Option<i32>,
    pub ethernet: Option<EthernetStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EthernetStatus {
    pub status: Option<i32>,
    pub r#type: Option<i32>,
    pub ip: Option<String>,
    pub mac: Option<String>,
    pub gateway: Option<String>,
    pub dns: Option<String>,
    pub connection_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeviceList {
    pub total: i32,
    /// Raw device entries. The gateway may send duplicate JSON keys
    /// (e.g. `applicationName` twice), so we keep them as raw values.
    pub result: Vec<serde_json::Value>,
    #[serde(rename = "deviceMax")]
    pub device_max: Option<i32>,
}

impl DeviceList {
    /// Parse raw device values into typed Device structs.
    /// Tolerates the duplicate-key bug in the Milesight firmware by
    /// first deduplicating keys in each JSON object.
    pub fn devices(&self) -> Vec<Device> {
        self.result
            .iter()
            .filter_map(|v| {
                if let serde_json::Value::Object(map) = v {
                    // serde_json::Map already deduplicates (last value wins),
                    // so re-serialising and parsing through it works.
                    let clean = serde_json::Value::Object(map.clone());
                    serde_json::from_value(clean).ok()
                } else {
                    None
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "devEUI")]
    pub dev_eui: Option<String>,
    #[serde(rename = "appEUI")]
    pub app_eui: Option<String>,
    pub class_mode: Option<String>,
    pub net_access: Option<String>,
    #[serde(rename = "fPort")]
    pub f_port: Option<i32>,
    pub skip_f_cnt_check: Option<bool>,
    pub dev_addr: Option<String>,
    pub app_key: Option<String>,
    #[serde(rename = "nwkSKey")]
    pub nwk_s_key: Option<String>,
    #[serde(rename = "appSKey")]
    pub app_s_key: Option<String>,
    #[serde(rename = "fCntUp")]
    pub f_cnt_up: Option<i64>,
    #[serde(rename = "fCntDown")]
    pub f_cnt_down: Option<i64>,
    pub active: Option<i32>,
    pub application_id: Option<String>,
    pub application_name: Option<String>,
    pub create_time: Option<String>,
    pub last_time: Option<String>,
    /// Catch any extra or duplicate fields the gateway might send.
    #[serde(flatten)]
    pub extra: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddDeviceRequest {
    pub name: String,
    #[serde(rename = "devEUI")]
    pub dev_eui: String,
    pub class_mode: String,
    pub net_access: String,
    #[serde(rename = "fPort")]
    pub f_port: i32,
    // ABP-only fields. An OTAA device gets its address and session keys from
    // the join exchange, so sending empty strings here would have the gateway
    // store a session that does not exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_addr: Option<String>,
    #[serde(rename = "nwkSKey", skip_serializing_if = "Option::is_none")]
    pub nwk_s_key: Option<String>,
    #[serde(rename = "appSKey", skip_serializing_if = "Option::is_none")]
    pub app_s_key: Option<String>,
    pub skip_f_cnt_check: bool,
    #[serde(rename = "fCntUp")]
    pub f_cnt_up: i64,
    #[serde(rename = "fCntDown")]
    pub f_cnt_down: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_key: Option<String>,
    #[serde(rename = "appEUI", skip_serializing_if = "Option::is_none")]
    pub app_eui: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl AddDeviceRequest {
    /// Build an OTAA registration.
    ///
    /// The gateway needs the AppKey up front — it is what the join request is
    /// signed with, so without it the join cannot even be authenticated.
    pub fn otaa(
        name: impl Into<String>,
        dev_eui: impl Into<String>,
        app_eui: impl Into<String>,
        app_key: impl Into<String>,
        f_port: i32,
        description: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            dev_eui: dev_eui.into(),
            class_mode: "Class A".to_string(),
            net_access: "OTAA".to_string(),
            f_port,
            dev_addr: None,
            nwk_s_key: None,
            app_s_key: None,
            skip_f_cnt_check: false,
            f_cnt_up: 0,
            f_cnt_down: 0,
            app_key: Some(app_key.into()),
            app_eui: Some(app_eui.into()),
            description,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApplicationList {
    pub total: i32,
    pub result: Vec<Application>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Application {
    pub id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub total_devices: Option<i32>,
    pub act_devices: Option<i32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NsGeneral {
    pub channel_plan: Option<i32>,
    pub channel_mask: Option<String>,
    #[serde(default)]
    pub additional_plan: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RadioConfig {
    pub region_freq: Option<i32>,
    pub radio_0_freq: Option<String>,
    pub radio_1_freq: Option<String>,
    pub multi_enable_0: Option<i32>,
    pub multi_radio_0: Option<i32>,
    pub multi_freq_0: Option<String>,
    pub multi_enable_1: Option<i32>,
    pub multi_radio_1: Option<i32>,
    pub multi_freq_1: Option<String>,
    pub multi_enable_2: Option<i32>,
    pub multi_radio_2: Option<i32>,
    pub multi_freq_2: Option<String>,
    pub multi_enable_3: Option<i32>,
    pub multi_radio_3: Option<i32>,
    pub multi_freq_3: Option<String>,
    pub multi_enable_4: Option<i32>,
    pub multi_radio_4: Option<i32>,
    pub multi_freq_4: Option<String>,
    pub multi_enable_5: Option<i32>,
    pub multi_radio_5: Option<i32>,
    pub multi_freq_5: Option<String>,
    pub multi_enable_6: Option<i32>,
    pub multi_radio_6: Option<i32>,
    pub multi_freq_6: Option<String>,
    pub multi_enable_7: Option<i32>,
    pub multi_radio_7: Option<i32>,
    pub multi_freq_7: Option<String>,
    pub lora_enable: Option<i32>,
    pub lora_radio: Option<i32>,
    pub lora_freq: Option<String>,
    pub lora_bandwidth: Option<i32>,
    pub spread_factor: Option<i32>,
    pub fsk_enable: Option<i32>,
    pub fsk_radio: Option<i32>,
    pub fsk_freq: Option<String>,
    pub fsk_bandwidth: Option<i32>,
    pub data_rate: Option<i64>,
    pub expert_enable: Option<i32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TrafficData {
    pub traffic_data: Vec<TrafficEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TrafficEntry {
    pub id: Option<i32>,
    pub direction: Option<i32>,
    pub time: Option<String>,
    pub freq: Option<String>,
    pub rate: Option<String>,
    pub channel: Option<String>,
    pub rssi: Option<String>,
    pub snr: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NsTrafficList {
    pub total: i32,
    /// Kept as raw values because the gateway firmware emits duplicate JSON
    /// keys (`codeRate` twice on join frames), which strict deserialisation
    /// rejects. Use [`NsTrafficList::entries`] for the typed view.
    pub result: Vec<serde_json::Value>,
}

impl NsTrafficList {
    /// Parse raw entries into typed ones, tolerating duplicate keys the same
    /// way [`DeviceList::devices`] does — `serde_json::Map` keeps the last
    /// value for a repeated key, so a round trip through it cleans them up.
    pub fn entries(&self) -> Vec<NsTrafficEntry> {
        self.result
            .iter()
            .filter_map(|v| {
                if let serde_json::Value::Object(map) = v {
                    serde_json::from_value(serde_json::Value::Object(map.clone())).ok()
                } else {
                    None
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NsTrafficEntry {
    #[serde(rename = "devEUI")]
    pub dev_eui: Option<String>,
    #[serde(rename = "gwEUI")]
    pub gw_eui: Option<String>,
    pub frequency: Option<i64>,
    pub data_rate: Option<String>,
    pub time: Option<String>,
    pub dev_addr: Option<String>,
    #[serde(rename = "appEUI")]
    pub app_eui: Option<String>,
    pub class_type: Option<String>,
    pub timestamp: Option<i64>,
    pub r#type: Option<String>,
    pub modulation: Option<String>,
    pub bandwidth: Option<i32>,
    pub spread_factor: Option<i32>,
    #[serde(rename = "fPort")]
    pub f_port: Option<String>,
    pub bit_rate: Option<i32>,
    pub code_rate: Option<String>,
    pub adr: Option<String>,
    #[serde(rename = "fCnt")]
    pub f_cnt: Option<i64>,
    pub adr_ack_req: Option<String>,
    pub ack: Option<String>,
    pub rssi: Option<String>,
    #[serde(rename = "loraSNR")]
    pub lora_snr: Option<String>,
    pub size: Option<i32>,
    pub payload_base64: Option<String>,
    pub payload_hex: Option<String>,
    pub mic: Option<String>,
    pub power: Option<String>,
    pub immediately: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacketGeneral {
    pub eui: Option<String>,
    pub gateway_id: Option<String>,
    pub enable: Option<i32>,
    pub expert_enable: Option<i32>,
    pub types: Option<i32>,
    pub status: Option<i32>,
    pub semtech: Option<SemtechConfig>,
    pub devicehub: Option<i32>,
    pub mip: Option<i32>,
    pub data_retransmission: Option<i32>,
    pub pending_data: Option<i32>,
    #[serde(default)]
    pub region_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SemtechConfig {
    pub server_address: Option<String>,
    pub uplink_port: Option<i32>,
    pub downlink_port: Option<i32>,
}

// ── Client ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MilesightClient {
    client: Client,
    config: Config,
    base_url: String,
}

#[derive(Debug, Deserialize)]
struct LoginError {
    #[allow(dead_code)]
    code: Option<i32>,
    msg: Option<String>,
    #[allow(dead_code)]
    try_times: Option<i32>,
    #[allow(dead_code)]
    expire_seconds: Option<i32>,
}

impl MilesightClient {
    /// Create a new client from a config file path.
    pub fn from_config(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let config = Config::from_file(path)?;
        Self::new(config)
    }

    /// Create a new client from a Config struct.
    pub fn new(config: Config) -> Result<Self, Box<dyn std::error::Error>> {
        let base_url = config.gateway_url.trim_end_matches('/').to_string();
        let client = Client::builder()
            .danger_accept_invalid_certs(true)
            .cookie_store(true)
            .build()?;
        Ok(Self {
            client,
            config,
            base_url,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Authenticate with the gateway. Must be called before other methods.
    pub async fn login(&self) -> Result<(), Box<dyn std::error::Error>> {
        let encrypted = encrypt_password(&self.config.password);
        let body = serde_json::json!({
            "username": self.config.username,
            "password": encrypted,
        });
        let resp = self
            .client
            .post(self.url("/login"))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<LoginError>(&text) {
                return Err(format!(
                    "Login failed: {}",
                    err.msg.unwrap_or_else(|| "unknown error".into())
                )
                .into());
            }
            return Err(format!("Login failed with status {}: {}", status, text).into());
        }
        Ok(())
    }

    /// Log out from the gateway.
    pub async fn logout(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.client.post(self.url("/logout")).send().await?;
        Ok(())
    }

    /// Get device info (GET /login).
    pub async fn get_device_info(&self) -> Result<DeviceInfo, Box<dyn std::error::Error>> {
        let resp = self.client.get(self.url("/login")).send().await?;
        Ok(resp.json().await?)
    }

    /// Get gateway status overview.
    pub async fn get_status(&self) -> Result<StatusOverview, Box<dyn std::error::Error>> {
        let resp = self.client.get(self.url("/status/overview")).send().await?;
        Ok(resp.json().await?)
    }

    /// List NS devices.
    pub async fn get_devices(
        &self,
        search: &str,
        limit: i32,
        offset: i32,
    ) -> Result<DeviceList, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/ns/device?search={}&limit={}&offset={}",
            self.base_url, search, limit, offset
        );
        let resp = self.client.get(&url).send().await?;
        Ok(resp.json().await?)
    }

    /// Get a single device by devEUI.
    pub async fn get_device(&self, dev_eui: &str) -> Result<Device, Box<dyn std::error::Error>> {
        let resp = self
            .client
            .get(self.url(&format!("/ns/device/{}", dev_eui)))
            .send()
            .await?;
        Ok(resp.json().await?)
    }

    /// Add a device to the network server.
    ///
    /// See [`AddDeviceRequest::otaa`] for the OTAA form.
    pub async fn add_device(
        &self,
        device: &AddDeviceRequest,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let resp = self
            .client
            .post(self.url("/ns/device/add"))
            .json(device)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format!("Add device failed ({}): {}", status, text).into());
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// Bind devices to an application (for MQTT forwarding etc).
    pub async fn bind_devices_to_application(
        &self,
        dev_euis: &[&str],
        application_id: &str,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let body = serde_json::json!({
            "ids": dev_euis,
            "applicationId": application_id,
        });
        let resp = self
            .client
            .post(self.url("/ns/device/bind"))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format!("Bind device failed ({}): {}", status, text).into());
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// Delete devices by devEUI list.
    pub async fn delete_devices(
        &self,
        dev_euis: &[&str],
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let body = serde_json::json!({ "ids": dev_euis });
        let resp = self
            .client
            .delete(self.url("/ns/device"))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format!("Delete device failed ({}): {}", status, text).into());
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// List NS applications.
    pub async fn get_applications(&self) -> Result<ApplicationList, Box<dyn std::error::Error>> {
        let resp = self.client.get(self.url("/ns/application")).send().await?;
        Ok(resp.json().await?)
    }

    /// Get NS traffic (decoded packet log).
    pub async fn get_packets(
        &self,
        limit: i32,
        offset: i32,
    ) -> Result<NsTrafficList, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/ns/traffic?limit={}&offset={}",
            self.base_url, limit, offset
        );
        let resp = self.client.get(&url).send().await?;
        Ok(resp.json().await?)
    }

    /// Get packet forwarder traffic (raw LoRa frames).
    pub async fn get_traffic(&self) -> Result<TrafficData, Box<dyn std::error::Error>> {
        let resp = self.client.get(self.url("/packet/traffic")).send().await?;
        Ok(resp.json().await?)
    }

    /// Get radio/channel configuration.
    pub async fn get_radio_config(&self) -> Result<RadioConfig, Box<dyn std::error::Error>> {
        let resp = self.client.get(self.url("/packet/radio")).send().await?;
        Ok(resp.json().await?)
    }

    /// Get NS general configuration (channel plan).
    pub async fn get_ns_general(&self) -> Result<NsGeneral, Box<dyn std::error::Error>> {
        let resp = self.client.get(self.url("/ns/general")).send().await?;
        Ok(resp.json().await?)
    }

    /// Create an NS application. Returns the new application id.
    ///
    /// The gateway forwards uplinks to MQTT per-application, so a device must
    /// be bound to an application before its packets reach the broker.
    pub async fn create_application(
        &self,
        name: &str,
        description: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let body = serde_json::json!({
            "name": name,
            "description": description,
            "totalDevices": 0,
            "devices": [],
        });
        let resp = self
            .client
            .post(self.url("/ns/application"))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format!("Create application failed ({}): {}", status, text).into());
        }
        let v: serde_json::Value = serde_json::from_str(&text)?;
        v.get("id")
            .and_then(|i| i.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("Create application: no id in response: {}", text).into())
    }

    /// Point an application's MQTT forwarder at a broker.
    ///
    /// Mirrors the payload the web UI sends on the application's MQTT tab:
    /// a flat connection config plus one object per topic channel. Only the
    /// uplink topic is populated; the other channels are left disabled.
    pub async fn set_application_mqtt(
        &self,
        app_id: &str,
        name: &str,
        address: &str,
        port: u16,
        uplink_topic: &str,
        enable: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Every channel gets a distinct non-empty topic, including when
        // disabling. Leaving them blank makes the gateway's MQTT client
        // subscribe to an empty topic, which is illegal in MQTT and takes the
        // whole gateway down — the very state we are usually trying to escape.
        let uplink_topic = if uplink_topic.is_empty() {
            format!("{}/uplink", name)
        } else {
            uplink_topic.to_string()
        };
        let address = if address.is_empty() { "127.0.0.1" } else { address };
        let downlink = format!("{}/downlink", name);
        let join = format!("{}/join", name);
        let ack = format!("{}/ack", name);
        let req = format!("{}/request", name);
        let resp = format!("{}/response", name);
        let gwinfo = format!("{}/gateway", name);
        let body = serde_json::json!({
            "enable": if enable { 1 } else { 0 },
            "name": name,
            "address": address,
            "port": port,
            "clientId": name,
            "connTimeout": 30,
            "keepAlive": 60,
            "retrans": 0,
            "reconnect": 1,
            "reconnectTime": 4,
            "cleanSession": 0,
            "user": { "enable": 0, "name": "", "password": "" },
            "tls": { "enable": 0, "mode": 0, "ca": "", "client": "", "key": "" },
            "will": { "enable": 0, "topic": "", "qos": 0, "retained": 0, "payload": "" },
            "uplinkData": { "topic": uplink_topic, "qos": 0, "retained": 0, "interval": 0 },
            "downlinkData": { "topic": downlink, "qos": 0, "interval": 0 },
            "joinNotify": { "topic": join, "qos": 0, "retained": 0, "interval": 0 },
            "ackNotify": { "topic": ack, "qos": 0, "retained": 0, "interval": 0 },
            "appRequest": { "topic": req, "qos": 0, "interval": 0 },
            "appResponse": { "topic": resp, "qos": 0, "retained": 0, "interval": 0 },
            "gatewayInfo": { "topic": gwinfo, "qos": 0, "retained": 0, "interval": 0 }
        });
        let resp = self
            .client
            .put(self.url(&format!("/ns/application/{}", app_id)))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format!("Set application MQTT failed ({}): {}", status, text).into());
        }
        Ok(())
    }

    /// Get full application detail (includes the `applys` MQTT config array).
    pub async fn get_application_detail(
        &self,
        app_id: &str,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let resp = self
            .client
            .get(self.url(&format!("/ns/application/detail?id={}", app_id)))
            .send()
            .await?;
        Ok(resp.json().await?)
    }

    /// Raw GET returning the response body as text (for API exploration).
    pub async fn get_raw(&self, path: &str) -> Result<(u16, String), Box<dyn std::error::Error>> {
        let resp = self.client.get(self.url(path)).send().await?;
        let status = resp.status().as_u16();
        Ok((status, resp.text().await?))
    }

    /// Raw POST of a JSON body, returning the status code and body text.
    pub async fn post_raw(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, String), Box<dyn std::error::Error>> {
        let resp = self.client.post(self.url(path)).json(body).send().await?;
        let status = resp.status().as_u16();
        Ok((status, resp.text().await?))
    }

    /// Point the packet forwarder at a network server.
    ///
    /// `types` selects the mode: 0 = Semtech UDP forwarder, 7 = the gateway's
    /// own embedded network server. Switching to 0 hands joins, session keys
    /// and downlink scheduling to whatever is listening on `address`.
    ///
    /// The current configuration is read first and sent back with only these
    /// fields changed, so the other forwarder settings survive.
    pub async fn set_packet_forwarder(
        &self,
        types: i32,
        address: &str,
        uplink_port: u16,
        downlink_port: u16,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let (_, current) = self.get_raw("/packet/general").await?;
        let mut body: serde_json::Value = serde_json::from_str(&current)?;

        body["types"] = serde_json::json!(types);
        body["enable"] = serde_json::json!(1);
        body["semtech"] = serde_json::json!({
            "server_address": address,
            "uplink_port": uplink_port,
            "downlink_port": downlink_port,
        });
        // Read-only fields the gateway reports but does not accept back.
        if let Some(map) = body.as_object_mut() {
            map.remove("status");
            map.remove("pending_data");
        }

        let (status, text) = self.post_raw("/packet/general", &body).await?;
        if !(200..300).contains(&status) {
            return Err(format!("Set packet forwarder failed ({}): {}", status, text).into());
        }
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::json!({})))
    }

    /// Get packet forwarder general configuration.
    pub async fn get_packet_general(&self) -> Result<PacketGeneral, Box<dyn std::error::Error>> {
        let resp = self.client.get(self.url("/packet/general")).send().await?;
        Ok(resp.json().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_password() {
        // Synthetic value: the real gateway password belongs in the gitignored
        // config.json, not in a committed assertion.
        let encrypted = encrypt_password("example_password");
        assert_eq!(encrypted, "2A9hW0VS221cDiOe8vCJJdi8Yk1nOeWJ4j9WnaBTPD4=");
    }

    #[test]
    fn test_config_parse() {
        let json = r#"{"gateway_url":"https://192.168.1.1","username":"admin","password":"test"}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.gateway_url, "https://192.168.1.1");
        assert_eq!(config.username, "admin");
        assert_eq!(config.password, "test");
    }
}
