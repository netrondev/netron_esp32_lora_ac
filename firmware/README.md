# Firmware

Bare-metal (`no_std`) Rust firmware for the ESP32. The main binary measures AC
current and reports it over LoRaWAN. The [root README](../README.md) covers
the hardware, pinout, toolchain setup and LED patterns.

## Building

The toolchain is pinned to the `esp` channel by `rust-toolchain.toml`, so run
Cargo from this directory, not with `--manifest-path`:

```bash
source ~/export-esp.sh
cargo build -p app --release
espflash flash --chip esp32 -p /dev/ttyUSB0 --monitor target/xtensa-esp32-none-elf/release/app
```

`../scripts/flash_native.sh` does the same and finds the port automatically.
To build in Docker instead, run `docker build -t esp32-rust .` here, then use
`../scripts/flash.sh`.

To build one of the bench tests below, name it with `--bin`. It lands at
`target/xtensa-esp32-none-elf/release/<name>`:

```bash
cargo build -p app --release --bin otaa_test
```

## Binaries

Binaries are declared explicitly in `app/Cargo.toml` (`autobins = false`).

| Binary | Purpose |
|---|---|
| `app` | Production firmware (`src/bin/main.rs`) |
| `otaa_test` | Joins, then sends a confirmed uplink every 30 s and prints what comes back. Use it to check joins and downlinks on a new gateway |
| `ra01sh_test` | Checks SPI communication with the SX1262 |
| `rx_test` | Listens for raw LoRa packets and prints them |
| `adc_test` | ADC sampling with heavy oversampling |
| `acs712_current_test` | Current measurement on its own, without the radio |

## Source layout

| Module | What it does |
|---|---|
| `lora/sx1262.rs` | SX1262 driver. `sx1278.rs` is the driver for the earlier 433 MHz SX1278 boards |
| `lorawan.rs` | LoRaWAN 1.0.x: join, MIC, encryption, uplink/downlink framing |
| `mac.rs` | Class A MAC layer: join, transmit and the two receive windows |
| `packet.rs` | Uplink payload encoding and the min/avg/max sample accumulator |
| `config.rs` | Device configuration and downlink command parsing |
| `session_store.rs` | Persists the LoRaWAN session and configuration in flash |
| `flash_storage.rs` | Wear-levelled flash storage for the cumulative energy total |

The payload format and command set are specified in
[`docs/PROTOCOL.md`](../docs/PROTOCOL.md).
