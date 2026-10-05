#!/usr/bin/env python3
"""Simple serial monitor for ESP32 debugging"""
import serial
import sys
import time

PORT = "/dev/ttyUSB0"
BAUD = 115200

def main():
    print(f"Opening {PORT} at {BAUD} baud...")
    try:
        ser = serial.Serial(PORT, BAUD, timeout=1)
        print("Connected. Press Ctrl+C to exit.\n")
        while True:
            if ser.in_waiting:
                line = ser.readline()
                try:
                    text = line.decode('utf-8', errors='replace').rstrip()
                    print(text)
                except:
                    print(f"[raw] {line}")
    except KeyboardInterrupt:
        print("\nExiting...")
    except Exception as e:
        print(f"Error: {e}")
        sys.exit(1)
    finally:
        if 'ser' in locals():
            ser.close()

if __name__ == "__main__":
    main()
