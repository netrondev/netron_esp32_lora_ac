use milesight_d4::{AddDeviceRequest, MilesightClient};

fn config_path() -> String {
    format!("{}/config.json", env!("CARGO_MANIFEST_DIR"))
}

fn make_client() -> MilesightClient {
    MilesightClient::from_config(&config_path()).expect("Failed to create client from config")
}

#[tokio::test]
#[ignore]
async fn test_01_login() {
    let client = make_client();
    client.login().await.expect("Login failed");
    println!("Login successful");
}

#[tokio::test]
#[ignore]
async fn test_02_get_status() {
    let client = make_client();
    client.login().await.expect("Login failed");

    let status = client.get_status().await.expect("Failed to get status");
    println!("Model: {:?}", status.model);
    println!("EUI: {:?}", status.eui);
    println!("Firmware: {:?}", status.firmware_version);
    println!("Uptime: {:?}", status.run_time);
    assert!(status.model.is_some());
}

#[tokio::test]
#[ignore]
async fn test_03_get_radio_config() {
    let client = make_client();
    client.login().await.expect("Login failed");

    let radio = client
        .get_radio_config()
        .await
        .expect("Failed to get radio config");
    println!("Radio 0 freq: {:?}", radio.radio_0_freq);
    println!("Radio 1 freq: {:?}", radio.radio_1_freq);
    assert!(radio.radio_0_freq.is_some());
}

#[tokio::test]
#[ignore]
async fn test_04_get_ns_general() {
    let client = make_client();
    client.login().await.expect("Login failed");

    let general = client
        .get_ns_general()
        .await
        .expect("Failed to get NS general");
    println!("Channel plan: {:?}", general.channel_plan);
    println!("Channel mask: {:?}", general.channel_mask);
}

#[tokio::test]
#[ignore]
async fn test_05_get_devices() {
    let client = make_client();
    client.login().await.expect("Login failed");

    let device_list = client
        .get_devices("", 10, 0)
        .await
        .expect("Failed to get devices");
    println!("Total devices: {}", device_list.total);
    let devices = device_list.devices();
    for dev in &devices {
        println!("  Device: {:?} (EUI: {:?})", dev.name, dev.dev_eui);
    }
}

#[tokio::test]
#[ignore]
async fn test_06_get_applications() {
    let client = make_client();
    client.login().await.expect("Login failed");

    let apps = client
        .get_applications()
        .await
        .expect("Failed to get applications");
    println!("Total applications: {}", apps.total);
    for app in &apps.result {
        println!(
            "  App: {:?} (id: {:?}, devices: {:?})",
            app.name, app.id, app.total_devices
        );
    }
}

#[tokio::test]
#[ignore]
async fn test_07_get_packets() {
    let client = make_client();
    client.login().await.expect("Login failed");

    let packets = client.get_packets(5, 0).await.expect("Failed to get packets");
    println!("Total packets: {}", packets.total);
    for pkt in &packets.result {
        println!(
            "  {} {:?} devEUI={:?} fCnt={:?} rssi={:?}",
            pkt.time.as_deref().unwrap_or("?"),
            pkt.r#type,
            pkt.dev_eui,
            pkt.f_cnt,
            pkt.rssi,
        );
    }
}

#[tokio::test]
#[ignore]
async fn test_08_get_traffic() {
    let client = make_client();
    client.login().await.expect("Login failed");

    let traffic = client.get_traffic().await.expect("Failed to get traffic");
    println!("Traffic entries: {}", traffic.traffic_data.len());
    for entry in traffic.traffic_data.iter().take(3) {
        println!(
            "  dir={:?} freq={:?} rate={:?} rssi={:?}",
            entry.direction, entry.freq, entry.rate, entry.rssi,
        );
    }
}

#[tokio::test]
#[ignore]
async fn test_09_add_and_delete_device() {
    let client = make_client();
    client.login().await.expect("Login failed");

    let test_dev_eui = "AABBCCDDEE001122";

    // Add a test device, the same way `watch` onboards a real one.
    let req = AddDeviceRequest::otaa(
        "test_device_api",
        test_dev_eui,
        "0000000000000000",
        format!("{}{}", test_dev_eui, test_dev_eui),
        85,
        Some("API test device".to_string()),
    );

    let add_result = client.add_device(&req).await.expect("Failed to add device");
    println!("Add device result: {:?}", add_result);

    // Verify the device exists
    let device_list = client
        .get_devices(test_dev_eui, 10, 0)
        .await
        .expect("Failed to search device");
    let devices = device_list.devices();
    let found = devices.iter().any(|d| {
        d.dev_eui
            .as_ref()
            .map(|e| e.to_uppercase() == test_dev_eui.to_uppercase())
            .unwrap_or(false)
    });
    assert!(found, "Test device should exist after adding");
    println!("Device verified: exists");

    // Delete the test device
    let del_result = client
        .delete_devices(&[test_dev_eui])
        .await
        .expect("Failed to delete device");
    println!("Delete device result: {:?}", del_result);

    // Verify deletion
    let device_list = client
        .get_devices(test_dev_eui, 10, 0)
        .await
        .expect("Failed to search device");
    let devices = device_list.devices();
    let still_exists = devices.iter().any(|d| {
        d.dev_eui
            .as_ref()
            .map(|e| e.to_uppercase() == test_dev_eui.to_uppercase())
            .unwrap_or(false)
    });
    assert!(
        !still_exists,
        "Test device should not exist after deletion"
    );
    println!("Device verified: deleted");
}

#[tokio::test]
#[ignore]
async fn test_10_get_packet_general() {
    let client = make_client();
    client.login().await.expect("Login failed");

    let general = client
        .get_packet_general()
        .await
        .expect("Failed to get packet general");
    println!("Gateway EUI: {:?}", general.eui);
    println!("PF enabled: {:?}", general.enable);
    println!("PF status: {:?}", general.status);
    println!("Semtech server: {:?}", general.semtech);
}
