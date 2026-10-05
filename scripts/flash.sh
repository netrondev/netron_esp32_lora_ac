#!/usr/bin/env bash
set -e

source "$(dirname "$0")/detectdevice.sh"
PORT=$(detect_port "$1")

source ~/export-esp.sh

"$(dirname "$0")/build.sh"

espflash flash --chip esp32 -p "$PORT" "$(dirname "$0")/../firmware/target/xtensa-esp32-none-elf/release/app"
"$(dirname "$0")/getmac.sh" "$PORT"
espflash reset --chip esp32 -p "$PORT"
