# Scope of work — secure activation and remote configuration

Moving the power monitors from a single shared key set and compiled-in timings
onto per-device secure activation, and making them configurable over the air so
a unit in the field can be adjusted without being collected and reflashed.

- Secure per-device activation (OTAA), replacing the shared keys used across
  the whole fleet.
- Remote configuration by downlink: reporting interval, measurement interval,
  transmission spread and remote reboot, all in seconds.
- Measurement separated from transmission — readings are taken at the
  measurement interval and combined into one report per reporting interval,
  sent as average, minimum, maximum and reading count.
- Acknowledged transmissions, with automatic re-join if the network stops
  responding, and confirmation echoed back for every setting applied.
- Settings, cumulative energy total and network session retained across power
  cycles and reboots.
- New payload format following the conventions of the Milesight sensors
  already in use.
- Corrected a fault in the radio driver that stalled the device after each
  transmission and made downlink reception impossible.
- Established that the gateway cannot deliver downlinks — no HTTP route, and
  its MQTT forwarding reboots the gateway (UG63 firmware 64.0.0.4-r1) — and
  built a replacement network server alongside it to handle joins, decoding
  and the queue of pending commands.
- JavaScript decoder, command encoder and worked example for integration, plus
  written specification of the message format and command set.
- Verified end to end on live hardware.

Not included: the two units in the field still run the previous firmware and
need reflashing over USB before they can join under the new scheme.
