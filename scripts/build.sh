#!/usr/bin/env bash
set -e

FIRMWARE_DIR="$(dirname "$0")/../firmware"

docker run --rm \
  -v "$FIRMWARE_DIR":/project \
  -v "$FIRMWARE_DIR/.cargo-cache":/root/.cargo/registry \
  esp32-rust bash -c "source /root/export-esp.sh && cargo build -p app --release"
