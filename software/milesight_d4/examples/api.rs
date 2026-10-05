//! Raw gateway API probe.
//!
//! Usage:
//!   cargo run --example api -- GET /ns/application
//!   cargo run --example api -- POST /ns/application/add '{"name":"test"}'

use milesight_d4::MilesightClient;

fn config_path() -> String {
    format!("{}/config.json", env!("CARGO_MANIFEST_DIR"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: api <GET|POST> <path> [json_body]");
        std::process::exit(1);
    }

    let client = MilesightClient::from_config(&config_path())?;
    client.login().await?;

    let (status, body) = match args[1].to_uppercase().as_str() {
        "GET" => client.get_raw(&args[2]).await?,
        "POST" => {
            let body: serde_json::Value = match args.get(3) {
                Some(b) => serde_json::from_str(b)?,
                None => serde_json::json!({}),
            };
            client.post_raw(&args[2], &body).await?
        }
        other => {
            eprintln!("Unsupported method: {}", other);
            std::process::exit(1);
        }
    };

    println!("HTTP {}", status);
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
        Err(_) => println!("{}", body),
    }
    Ok(())
}
