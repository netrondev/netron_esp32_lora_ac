# System overview

```mermaid
flowchart TB
    subgraph AC["AC Mains Input (230V AC)"]
        MAINS["230V AC\nMains"]
    end

    subgraph PSU["Power Supply (PCB)"]
        BRIDGE["MB10S\nBridge Rectifier\n~325V DC"]
        BUCK["BP2525D\nNon-isolated Buck\n5V / 300mA"]
        BRIDGE --> BUCK
    end

    subgraph SENSE["Current Sensing"]
        ACS["ACS712-20A\nHall-Effect Sensor\n±20A AC/DC\nAnalog out @ 185mV/A"]
    end

    subgraph MCU["Microcontroller — ESP32 Mini (Wemos D1 Mini32)"]
        ADC["ADC GPIO34\n(ACS712 output)"]
        FW["Rust Firmware\n• RMS current calc\n• min/avg/max per report\n• Cumulative energy (flash)\n• OTAA Class A, remote config"]
        ADC --> FW
    end

    subgraph RADIO["LoRa Radio"]
        LORA["Ra-01SH\nSX1262\n868 MHz\nSPI interface"]
        ANT["868 MHz\nAntenna"]
        LORA --> ANT
    end

    subgraph GW["LoRa Gateway"]
        GWDEV["Milesight UG63\nSemtech UDP\npacket forwarder"]
    end

    subgraph SERVER["Server"]
        NS["lorans\nnetwork server\njoins, keys, decode,\ndownlink queue"]
    end

    %% Power flow
    MAINS -->|"AC In"| BRIDGE
    MAINS -->|"AC through sensor"| ACS
    BUCK -->|"5V regulated"| MCU
    BUCK -->|"5V regulated"| RADIO

    %% Sensor to MCU
    ACS -->|"Analog voltage\n(GPIO34)"| ADC

    %% MCU to LoRa (SPI)
    FW -->|"SPI\nGPIO18/19/23/5\nRST GPIO27\nDIO1 GPIO26"| LORA

    %% RF link
    ANT <-.->|"LoRaWAN 868MHz\nuplinks + downlinks"| GWDEV

    %% Gateway to server
    GWDEV <-->|"UDP 1700"| NS

    %% Styles
    classDef power fill:#ff9f43,stroke:#e17055,color:#000
    classDef mcu fill:#0984e3,stroke:#74b9ff,color:#fff
    classDef radio fill:#6c5ce7,stroke:#a29bfe,color:#fff
    classDef server fill:#00b894,stroke:#55efc4,color:#000
    classDef sense fill:#fdcb6e,stroke:#e17055,color:#000

    class BRIDGE,BUCK power
    class ADC,FW mcu
    class LORA,ANT radio
    class NS server
    class ACS sense
```

## Data flow

| Stage | Component | Protocol |
|-------|-----------|----------|
| AC sense | ACS712-20A → GPIO34 | Analog voltage |
| Compute | ESP32 (Rust firmware) | Internal |
| Transmit | SX1262 Ra-01SH | LoRaWAN 868 MHz, FPort 85 |
| Forward | Milesight UG63 gateway | Semtech UDP |
| Decode and configure | `software/lorans` | See [`docs/PROTOCOL.md`](../docs/PROTOCOL.md) |

## Power budget (5V rail)

| Component | Current |
|-----------|---------|
| ESP32 (no WiFi/BLE) | ~80 mA |
| SX1262 TX peak | ~120 mA |
| ACS712 | ~10 mA |
| **Total peak** | **~200 mA** |
| BP2525D capacity | 300 mA cont / 500 mA pulse |
