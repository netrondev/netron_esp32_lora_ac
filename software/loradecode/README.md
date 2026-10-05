# Payload codec for the ESP32 power monitors

`decode.js` turns device uplinks into readable JSON and turns configuration
settings into downlink bytes. No dependencies, ES5 only, so it can be pasted
straight into ChirpStack, The Things Network or Node-RED.

The device uses **FPort 85** in both directions.

## Installing it on a network server

Paste the whole file into the codec field. It provides every entry point the
common platforms look for, so the same file works unchanged:

| Platform | Uses |
|---|---|
| ChirpStack v4 | `decodeUplink(input)` / `encodeDownlink(input)` |
| ChirpStack v3 | `Decode(fPort, bytes)` / `Encode(fPort, obj)` |
| The Things Network | `Decoder(bytes, port)` / `Encoder(obj, port)` |
| Node.js | `require("./decode.js")` |

## What an uplink decodes to

A periodic report:

```json
{
  "current_avg": 12,
  "current_min": 8,
  "current_max": 19,
  "cumulative_charge": 56,
  "sample_count": 4
}
```

Currents are **milliamps**, `cumulative_charge` is **milliamp-hours** since the
device was first powered on, and `sample_count` is how many measurements went
into this report.

`sample_count` is the field to watch if something looks wrong. It should be
roughly `report_interval / sample_interval` — a count of 0 means no measurement
completed, and a count well below that means the device is not keeping up.

After every join the device sends its identity instead:

```json
{
  "power_on": true,
  "protocol_version": 1,
  "hardware_version": "v1.0",
  "firmware_version": "v0.7",
  "lorawan_class": "A",
  "sn": "E08CFEFFFE34C3AC"
}
```

`sn` is the DevEUI.

An `undecoded_at` field means decoding stopped at that byte offset because it
met a record it did not recognise — usually a firmware newer than this decoder.
Everything before that offset is still valid.

## Sending configuration

All intervals are in **seconds**.

```js
encodeDownlink({ data: { report_interval: 300 } })   // report every 5 minutes
encodeDownlink({ data: { sample_interval: 5 } })     // measure every 5 seconds
encodeDownlink({ data: { jitter: 10 } })             // spread reports by ±10 s
encodeDownlink({ data: { reboot: true } })
```

Several settings can go in one downlink:

```js
encodeDownlink({ data: { report_interval: 60, sample_interval: 5, jitter: 10 } })
```

| Setting | Range | What it does |
|---|---|---|
| `report_interval` | 10–65535 s | How often the device transmits |
| `sample_interval` | 1–3600 s | How often it measures. Samples are collected and summarised into one report — they are not sent individually |
| `jitter` | 0–600 s | Spreads transmissions out. Devices powered on together would otherwise transmit in lockstep and collide on air. It does not change the average interval |
| `reboot` | — | Restarts the device. Configuration and the energy total survive |

Out-of-range values throw rather than being sent, because the device would
silently reject them.

### When it takes effect

These are Class A devices: they only listen briefly after transmitting. A
downlink therefore waits until the device's next report — **up to a full report
interval**. Queue it and be patient rather than sending it repeatedly.

The device **echoes back every command it accepted** in its next uplink, and
that echo is the acknowledgement:

```json
{ "report_interval": 300, "sample_interval": 5 }
```

If the echo does not arrive, the command either fell outside its range or never
reached the device — send it again.

A reboot is echoed *before* the device restarts, so `{"reboot": true}` coming
back means it is about to happen. The device rejoins afterwards and sends a
fresh identity packet.

## Worked example

`example.js` runs the whole cycle against payloads captured from a real device,
so it demonstrates the flow with no hardware attached:

```bash
node example.js
```

It builds a downlink, tracks it as unconfirmed, feeds in the uplinks that
follow, recognises the echo that confirms it, and shows what to do about a
command whose echo never arrives. `classifyUplink` and `PendingCommands` are
the two pieces worth lifting into your own integration.

## Command line

```bash
node decode.js 03980c00049808000598130006c83800000007040400
node decode.js A5gMAASYCAAFmBMABsg4AAAABwQEAA==
node decode.js encode report=60 sample=5 jitter=10
node decode.js encode reboot
```

`encode` prints the payload as hex and base64, ready to paste into whatever
queues the downlink.

## Wire format

`docs/PROTOCOL.md` has the byte-level detail — the channel map, the command
table, and the activation parameters.
