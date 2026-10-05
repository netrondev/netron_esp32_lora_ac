# lorans — network server for the ESP32 power monitors

A minimal LoRaWAN 1.0.x network server. The gateway runs as a plain Semtech UDP
packet forwarder and this handles joins, session keys, uplink decoding and the
downlink queue.

## Why this exists

The gateway has a perfectly good built-in network server, and we are not using
it, for one reason: **it has no way to send a downlink that works.**

- There is no HTTP downlink endpoint. The web UI exposes `/ns/device*`,
  `/ns/application*`, `/ns/packets`, `/ns/traffic` and `/packet/*`, and nothing
  that queues a downlink.
- Its only downlink path is the application MQTT forwarder, and enabling that
  **reboots the gateway**. Observed on UG63-868M firmware 64.0.0.4-r1: the
  gateway connects to the broker, subscribes to `<app>/downlink` and
  `<app>/request`, resets the connection, and reboots — `run_time` returns to
  zero — within about twenty seconds, having exchanged no data. Then it repeats.

Without downlinks there is no remote configuration, so the network server moved
to this side, where we control the whole exchange.

## Running it

```bash
cargo run -p lorans           # or: cargo run --manifest-path software/lorans/Cargo.toml
```

It listens on UDP 1700 for the packet forwarder and TCP 127.0.0.1:7788 for
control. Sessions are persisted to `sessions.json` (override with
`LORANS_STATE`).

**Keep that state file.** If the server forgets a session, the device carries
on using keys we no longer hold, every uplink fails its MIC check, and — having
no session — we cannot even tell it to rejoin. The firmware recovers on its own
after four unacknowledged reports, but that is a fallback, not a plan.

## Pointing the gateway at it

```bash
cd software/milesight_d4
cargo run -- set-forwarder semtech 192.168.1.100 1700   # this host
cargo run -- set-forwarder embedded                      # revert to the gateway's own NS
```

While in Semtech mode the gateway's own device list, packet log and
applications go unused — devices join *here*, and `milesight_d4 packets` will
not show them.

## Sending commands

Intervals are in seconds. See `docs/PROTOCOL.md` for the wire format.

```bash
echo 'list' | nc -q1 127.0.0.1 7788
echo 'downlink E08CFEFFFE34C3AC report=300' | nc -q1 127.0.0.1 7788
echo 'downlink E08CFEFFFE34C3AC report=60 sample=5 jitter=10' | nc -q1 127.0.0.1 7788
echo 'downlink E08CFEFFFE34C3AC reboot' | nc -q1 127.0.0.1 7788
echo 'downlink E08CFEFFFE34C3AC FF032C01' | nc -q1 127.0.0.1 7788   # raw hex
```

A Class A device only listens in the two windows after one of its own uplinks,
so a queued command goes out after the device's next report — up to a full
report interval away. One downlink is delivered per uplink; the rest stay
queued, with the frame-pending bit set.

The device echoes every command it accepted in its next uplink, and that echo
is the acknowledgement. A command that comes back took effect; one that does
not was either out of range and rejected, or never arrived — a queued downlink
is transmitted once and not retried, so if no echo appears, queue it again.

## What it does not do

One channel (868.1 SF7), RX1 only, no ADR, no MAC commands, no class B or C,
and it accepts exactly one AppKey convention — `DevEUI‖DevEUI`, which is what
our firmware uses. Anything else on air is logged and ignored.
