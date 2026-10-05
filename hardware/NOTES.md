ESP32 Board
===================

https://api.riot-os.org/group__boards__esp32__mh-et-live-minikit.html

LORA
===================

Switch from 433 to 868

Pick: `Ra-01SH`

Basic LoRa Modules (no MCU, SPI-controlled)
  ┌────────────────┬────────┬─────────────┬──────────────────┬─────────────────────────────┐
  │     Model      │  Chip  │  Frequency  │     Antenna      │            Notes            │
  ├────────────────┼────────┼─────────────┼──────────────────┼─────────────────────────────┤
  │ Ra-01          │ SX1278 │ 410–525 MHz │ Solder pad       │ Basic, spring antenna       │
  ├────────────────┼────────┼─────────────┼──────────────────┼─────────────────────────────┤
  │ Ra-01H         │ SX1278 │ 803–930 MHz │ Solder pad       │ High-freq variant of Ra-01  │
  ├────────────────┼────────┼─────────────┼──────────────────┼─────────────────────────────┤
  │ Ra-01S         │ SX1262 │ 410–525 MHz │ Half-hole / IPEX │ Upgraded chip vs Ra-01      │
  ├────────────────┼────────┼─────────────┼──────────────────┼─────────────────────────────┤
  │ Ra-01SH        │ SX1262 │ 803–930 MHz │ Half-hole / IPEX │ High-freq SX1262            │
  ├────────────────┼────────┼─────────────┼──────────────────┼─────────────────────────────┤
  │ Ra-01SC        │ SX1262 │ 410–525 MHz │ Half-hole / IPEX │ ~4.7 km range               │
  ├────────────────┼────────┼─────────────┼──────────────────┼─────────────────────────────┤
  │ Ra-01SCH       │ SX1262 │ 803–930 MHz │ Half-hole / IPEX │ High-freq SC variant        │
  ├────────────────┼────────┼─────────────┼──────────────────┼─────────────────────────────┤
  │ Ra-02          │ SX1278 │ 410–525 MHz │ IPEX             │ ~4 km range, popular module │
  ├────────────────┼────────┼─────────────┼──────────────────┼─────────────────────────────┤
  │ Ra-03SCH       │ SX1262 │ 803–930 MHz │ ?                │ Newer variant               │
  ├────────────────┼────────┼─────────────┼──────────────────┼─────────────────────────────┤
  │ Ra-05 / Ra-05U │ —      │ —           │ —                │ Less documented             │
  └────────────────┴────────┴─────────────┴──────────────────┴─────────────────────────────┘

 Key Differences

  - "H" suffix = High frequency (803–930 MHz, i.e. 868/915 MHz bands for EU/US/AU). Without H = low frequency (410–525 MHz, i.e. 433 MHz band for Asia).
  - "S" suffix = SX1262 chip upgrade (better sensitivity, lower power vs SX1278).
  - "C" suffix = improved range variant.
  - Ra-01/02 series = pure RF modules, no MCU — you drive them over SPI from your own MCU (ESP32, STM32, etc.).
  - Ra-08/08H = have a built-in ARM Cortex-M4 MCU + LoRaWAN stack. Can be used standalone or controlled via AT commands over UART. 128 KB flash, 16 KB SRAM,
  peripherals (UART, SPI, I2C, ADC, DAC, PWM, GPIO).
  - Ra-09 = uses STM32WLE5 (ST's integrated LoRa SoC) instead of ASR6601.


Breakout board for validation: https://make.net.za/product/3me0700/?gad_source=1&gad_campaignid=20932543566&gbraid=0AAAAADQewEzRSGi7vVBfLaXY5jl1-nzP3&gclid=CjwKCAiAkbbMBhB2EiwANbxtbTcVmdxRZWZPU0q8DljR8BTsExlmK40VW3ikzvSehiQ1thwra3_SrxoCh_QQAvD_BwE

https://www.semtech.com/products/wireless-rf/lora-connect/sx1262

## Lora Antenna

17.5mm 5.5mm 6.5mm

https://make.net.za/product/3me0850/

POWER SUPPLY
=================

For prototyping/low volume: Use the HLK-PM01 module (easier, safer, pre-certified)

smallest LS01-K3B05SS

For production (>1000 units): Consider the BP2522 IC approach like Shelly (lower BOM cost, but requires expertise)

  Alternative IC: The BP2836D is another Bright Power chip used in similar applications with higher power output capability..


AC supply
==========
References:
- https://www.youtube.com/watch?v=_MwQRQSlA0k