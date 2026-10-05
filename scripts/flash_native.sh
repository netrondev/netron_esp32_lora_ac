#!/usr/bin/env bash
set -e

# Docker-free counterpart of flash.sh — builds with the host Xtensa toolchain
# from ~/export-esp.sh instead of the esp32-rust image.

source "$(dirname "$0")/detectdevice.sh"
PORT=$(detect_port "$1")

source ~/export-esp.sh

FIRMWARE_DIR="$(cd "$(dirname "$0")/../firmware" && pwd)"

# cd rather than --manifest-path: rustup picks the esp toolchain from
# firmware/rust-toolchain.toml based on the working directory.
(cd "$FIRMWARE_DIR" && cargo build -p app --release)

espflash flash --chip esp32 -p "$PORT" "$FIRMWARE_DIR/target/xtensa-esp32-none-elf/release/app"
"$(dirname "$0")/getmac.sh" "$PORT"
espflash reset --chip esp32 -p "$PORT"
