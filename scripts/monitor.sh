#!/usr/bin/env bash
set -e

source "$(dirname "$0")/detectdevice.sh"
PORT=$(detect_port "$1")

docker run --rm -it \
  -v "$(dirname "$0")/../firmware":/project \
  --device="$PORT" \
  esp32-rust bash -c "source /root/export-esp.sh && espflash monitor -p $PORT"
