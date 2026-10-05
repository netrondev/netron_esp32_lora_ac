#!/usr/bin/env bash
set -e

source "$(dirname "$0")/detectdevice.sh"
PORT=$(detect_port "$1")

source ~/export-esp.sh
MAC=$(espflash board-info -p "$PORT" 2>/dev/null | grep -i "mac address" | awk '{print toupper($NF)}')
echo -e "\033[92m$MAC\033[0m"
