# Hardware and feature coverage

This matrix separates live verification from protocol inference. The reference
device used during development ran firmware `v0.4.1` over USB.

## Inputs

| Capability | Status | Evidence |
| --- | --- | --- |
| Six Agent keys | Verified | `AG00` through `AG05`, press and release |
| Seven Command switches | Verified | `ACT06` through `ACT12`, press and release |
| Wide Mic key | Verified | Physical 2U key uses `ACT10` + `ACT11`; daemon emits one logical `MIC` event |
| Encoder counterclockwise | Verified | `ENC_CC`, `act: 2` |
| Encoder clockwise | Verified | `ENC_CW`, `act: 2` |
| Encoder press/release | Verified | `ENC_CLK`, `act: 1` / `act: 0` |
| Analog joystick | Verified | Continuous normalized angle and distance |
| Joystick click | Unverified | No distinct click event was observed |
| Pairing touch control | Out of scope | Used by firmware for Bluetooth channel/pairing control |
| Rear power control | Out of scope | Hardware power control, not an application key |

All 13 mechanical switch identifiers were observed. The wide Mic keycap is
mounted over two of those switches, so the number of logical keycaps is lower
than the number of switches.

## Lighting

| Capability | Status | Evidence |
| --- | --- | --- |
| Six upper Agent LEDs | Verified | Individually addressed as IDs `0..5` |
| Agent solid RGB | Verified | Six distinct simultaneous colors |
| Agent animation | Verified | Shallow-breath on an individual key |
| Seven lower Command LEDs | Verified as one zone | Whole group changed color together |
| Command-key animation | Verified as one zone | Breath effect |
| Perimeter/ambient RGB | Verified | Independent color from Command-key zone |
| Perimeter animation | Verified | Snake/pulse-style animation |
| Lower Command LEDs individually | Not exposed | Thread ID `6` was accepted but changed no lower LED |

Known numeric effects are `off`, `solid`, `snake`, `rainbow`, `breath`,
`gradient`, and `shallow-breath`. Solid, snake, breath, and shallow-breath were
visually verified. The remaining known codes are implemented but have not all
been visually checked on every zone.

## Status and transport

| Capability | Status |
| --- | --- |
| Firmware/profile/layer status | Verified |
| Battery percentage | Verified |
| Charging state | Verified |
| USB discovery and reconnect | Verified in normal use |
| Lighting replay after reconnect | Implemented; hardware reconnect regression test still useful |
| Bluetooth HID transport | Unverified |
| Multiple simultaneous API clients | Implemented |
| Event callback stream | Verified with keys, encoder, and joystick |
| Direct command mappings | Verified |

## What “complete” means here

The core Linux integration is complete for the USB workflow:

- Read every application-facing mechanical switch
- Read encoder rotation and press
- Read continuous joystick position
- Control every firmware-exposed lighting domain
- Subscribe from other applications
- Map normalized input events to local commands

Remaining items are optional transport research, robustness testing, or
hardware controls that are not known to be application inputs.
