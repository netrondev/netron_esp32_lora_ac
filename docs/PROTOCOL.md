# ESP32 power monitor — LoRaWAN protocol

The contract between the firmware, the decoders in `software/`, and any
platform consuming the data. Anything here that changes has to change in all
three places.

## Activation

The devices are **OTAA, Class A, LoRaWAN 1.0.x**, on EU868. There is no ABP
support.

| Value | How it is derived |
|---|---|
| DevEUI | The ESP32 MAC expanded to an EUI-64: `MAC[0..3] + FF FE + MAC[3..6]`. A device with MAC `E0:8C:FE:34:C3:AC` has DevEUI `E08CFEFFFE34C3AC`. |
| JoinEUI (AppEUI) | `0000000000000000`. This is a private network and we hold no IEEE-assigned JoinEUI block; the network server identifies devices by DevEUI. |
| AppKey | The DevEUI repeated: `E08CFEFFFE34C3AC` → `E08CFEFFFE34C3ACE08CFEFFFE34C3AC`. |

The AppKey convention is Milesight's own, and it is what makes zero-touch
onboarding possible: a join request carries the DevEUI in clear, so the
gateway tooling can derive the key, verify the join MIC, and register the
device without anyone typing a key in. It also means **the AppKey is not a
secret** — anyone who can hear a join request can derive it. That is an
accepted trade for a private network on a private gateway; if these devices
ever join a network where that matters, move to random per-device keys
provisioned at flash time.

The join nonce is persisted in flash and incremented before every attempt,
including attempts that fail. Network servers reject a repeated nonce.

**FPort 85** is used for uplinks and downlinks alike.

## Frame format

Payloads are a flat sequence of records, following the Milesight convention:

```
channel(1) | type(1) | value(N)   channel(1) | type(1) | value(N)   ...
```

Values are **little-endian**. There is **no length byte on the wire** — the
length of each record is implied by its channel/type pair, so a decoder must
know every pair it might see. An unrecognised pair means the rest of the frame
cannot be located and decoding stops there.

## Uplinks

### Device information

Sent once after every successful join.

| Item | Bytes | Notes |
|---|---|---|
| Power on | `ff 0b ff` | |
| Protocol version | `ff 01 01` | This document's version |
| Hardware version | `ff 09 <major> <minor>` | |
| Firmware version | `ff 0a <major> <minor>` | From the crate version |
| Device class | `ff 0f 00` | Class A |
| Serial number | `ff 16 <8 bytes>` | The DevEUI |

Example, from a v0.7.0 device:

```
ff0bff ff0101 ff090100 ff0a0007 ff0f00 ff16e08cfefffe34c3ac
```

### Periodic report

One report per heartbeat, summarising every sample taken since the last one.

| Item | Bytes | Units |
|---|---|---|
| Current, average | `03 98 <u16>` | mA |
| Current, minimum | `04 98 <u16>` | mA |
| Current, maximum | `05 98 <u16>` | mA |
| Cumulative charge | `06 c8 <u32>` | mAh, since first power-on |
| Sample count | `07 04 <u16>` | samples in this window |

Example — 12 mA average, 8 mA min, 19 mA max, 56 mAh total, 4 samples:

```
03980c00 04980800 05981300 06c838000000 07040400
```

Reports are sent **confirmed**. If the network does not acknowledge, the frame
is retransmitted exactly once, reusing its frame counter, and then dropped.

`sample_count` is worth watching: a count of 0 means no measurement completed
in the window, and a count far from `report_interval / sample_interval` means
the device is not keeping up with the configured cadence.

## Downlinks

Sent on FPort 85. A Class A device only listens in the two windows following
one of its own uplinks, so a downlink is queued at the network server and
delivered after the device's next report — with a 60-second report interval,
expect up to a minute of latency.

| Command | Bytes | Range |
|---|---|---|
| Reboot | `ff 10 ff` | — |
| Sample interval | `ff 02 <u16>` | 1–3600 s |
| Report interval | `ff 03 <u16>` | 10–65535 s |
| Heartbeat jitter | `ff 04 <u16>` | 0–600 s |
| Report interval, in minutes | `ff 8e 00 <u16>` | 1–1092 min |

Several commands may be concatenated into one frame.

`ff 8e` is an alias accepted for compatibility with platforms built against
Milesight's newer sensors, which express this interval in minutes. It is
converted to seconds on arrival and echoed back in the canonical `ff 03`
seconds form, so what comes back always says what the device is actually
doing.

**Jitter** spreads transmissions out: the next report is scheduled at
`report_interval ± jitter`, redrawn each time. Devices powered up together
would otherwise transmit in lockstep and collide on air indefinitely. It is
not a delay — the average interval is unchanged.

### Echoes

Every accepted command is echoed back on channel `ff` with the same type and
value in the next uplink. **The echo is the acknowledgement.** A command that
is out of range is rejected and not echoed, so a value that comes back is one
that took effect.

Setting the report interval to 300 s:

```
downlink:  ff 03 2c 01
next uplink contains: ff 03 2c 01
```

A reboot is echoed *before* the device resets, so `ff 10 ff` coming back means
the reboot is about to happen, not that it has happened. The device rejoins
afterwards and sends a fresh device information packet.

## Working with payloads

```bash
cd software/milesight_d4

# Decode an uplink (hex or base64), e.g. from `milesight_d4 packets`
cargo run -- decode 03980c00049808000598130006c838000000 07040400

# Encode downlink commands
cargo run -- encode report=300 sample=5
cargo run -- encode reboot
```

`software/loradecode/decode.js` is the JavaScript equivalent, for use as a network
server codec, and mirrors `software/milesight_d4/src/tlv.rs`.
