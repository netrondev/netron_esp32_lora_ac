#!/usr/bin/env bash

detect_port() {
    local port="$1"
    if [ -n "$port" ]; then
        echo "$port"
        return
    fi
    port=$(ls /dev/ttyUSB* /dev/ttyACM* 2>/dev/null | head -1)
    if [ -z "$port" ]; then
        echo "No serial device found on /dev/ttyUSB* or /dev/ttyACM*" >&2
        exit 1
    fi
    echo "Auto-detected port: $port" >&2
    echo "$port"
}
