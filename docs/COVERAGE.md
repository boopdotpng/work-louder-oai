# Hardware and feature coverage

This matrix separates live verification from protocol inference. The reference
device used during development ran firmware `v0.4.1` over USB on Linux.
"Verified" below means verified there unless a row says otherwise.

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
| Joystick sector keys | Code-tested | `JOY0..JOYn` derived from angle/distance; angle-zero direction still unlabeled |
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
| Linux USB discovery and reconnect | Verified in normal use |
| Lighting replay after reconnect | Implemented; hardware reconnect regression test still useful |
| Multiple simultaneous API clients | Implemented |
| Event callback stream | Verified with keys, encoder, and joystick |
| Direct key command mappings | Verified |
| Encoder and joystick-sector mappings | Code-tested |

## Platforms

| Platform and transport | Status |
| --- | --- |
| Linux hidraw over USB | Verified |
| Linux hidraw over Bluetooth | Unverified; selected by HID bus `0005` |
| macOS IOKit over USB | Unverified on hardware; transport logic code-tested with a fake IOKit |
| macOS IOKit over Bluetooth | Unverified on hardware |
| macOS launchd agent | Unverified on hardware |

The macOS transport uses the same 64-byte report-6 framing as Linux. IOKit
handles USB and Bluetooth HID through one API, so Bluetooth works without
special code *if* the firmware exposes vendor report `6` over Bluetooth. That
is the main open question.

### Bluetooth checklist

With the daemon stopped and the keyboard connected only over Bluetooth:

1. `codex-micro-probe list --all`: find the keyboard and note `transport`,
   `vendor_id`, `product_id`, and `product`. If it is not marked
   `"codex_micro": true`, pass `--name` with part of its product name.
2. `codex-micro-probe --transport bluetooth status`: a JSON result proves RPC
   works over Bluetooth.
3. `codex-micro-probe --transport bluetooth monitor`: press keys, turn the
   encoder, and move the joystick. `rpc` lines with `v.oai.hid` and
   `v.oai.rad` prove that input notifications arrive over Bluetooth. If only
   `report` lines appear (ordinary keyboard reports), the firmware does not
   route vendor actions over Bluetooth.
4. `codex-micro-probe --transport bluetooth agent 0 '#00ff00'`: confirms that
   output reports work.

Please record the results using the template in
[EXTENDING.md](EXTENDING.md#live-test-record).

## What “complete” means here

The core integration is complete for the Linux USB workflow, and the same
code paths serve macOS:

- Read every application-facing mechanical switch
- Read encoder rotation and press
- Read continuous joystick position
- Control every firmware-exposed lighting domain
- Subscribe from other applications
- Map every normalized input control to local commands

Remaining items are optional transport research, robustness testing, or
hardware controls that are not known to be application inputs.
