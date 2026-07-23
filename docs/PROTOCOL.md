# Codex Micro HID and firmware protocol

Clean-room protocol notes for native Linux support of the Work Louder Codex
Micro.

## Reference hardware

- USB vendor/product: `303a:8360`
- Manufacturer/product: `Work Louder Codex Micro`
- Development firmware: `v0.4.1`
- Codex integration layer: profile `0`, layer `1` on the reference device

The `/dev/hidrawN` number changes after reconnecting. Match USB VID/PID and,
when needed, serial rather than hard-coding a numbered path.

## Linux permissions

`udev/70-codex-micro.rules` grants the active local session access to the
Codex Micro hidraw interface with `TAG+="uaccess"`. It does not make the device
world-writable.

After changing the rule, reload udev and reconnect the device.

## Research provenance

An older community Linux build of Work Louder Input (`0.13.2-Community`) did
not recognize this product:

- Recognized Creator Micro V2 product IDs: `0x8297`, `0x8298`
- Codex Micro product ID: `0x8360`

A newer Work Louder device registry recognizes `0x8360` as `codex_micro`.
This repository contains independently verified wire facts and original code;
it does not contain extracted packages or proprietary application source.

## HID reports

The device exposes one HID interface with several report IDs:

| Report | Purpose | Direction and size |
| --- | --- | --- |
| `1` | Boot keyboard and five keyboard LEDs | Input: modifiers plus six keycodes; output: five LED bits |
| `2` | Consumer control | Input: one 16-bit usage |
| `3` | Mouse/pointer | Input: five buttons, X, Y, wheel, horizontal wheel |
| `4` | Joystick/gamepad | Input: six signed axes, hat, and 32 buttons |
| `6` | Vendor protocol | Input: 63 bytes; output: 63 bytes |

Report `6` carries configuration, status, firmware logs, and lighting commands.

### Vendor report framing

Each raw HID packet is 64 bytes including its report ID:

```text
byte 0: report ID, always 6
byte 1: channel
byte 2: payload length, 0 through 61
bytes 3 through 63: UTF-8 payload followed by zero padding
```

Channels observed:

- `1`: firmware debug/log text
- `2`: JSON-RPC

Messages longer than 61 bytes are split across consecutive reports. JSON-RPC
responses may likewise arrive in fragments and must be assembled until they
form complete JSON.

## JSON-RPC

Requests omit the `"jsonrpc": "2.0"` member:

```json
{"method":"sys.version","params":null,"id":1}
```

Responses use the matching request ID:

```json
{"id":1,"result":{"version":"v0.4.1"}}
```

Firmware errors use an `error` member. Notifications omit `id` and use the
abbreviated `m`/`p` keys described below.

Verified read-only calls:

- `sys.version`
- `device.status`
- `fs.list`
- `fs.read`

Additional known methods:

- `sys.bootloader`
- `sys.selftest`
- `fs.delete`
- `fs.readbin`
- `fs.write`
- `fs.writebin`
- `lights.preview`
- `host.focused_app`
- `ui.active_screen`
- `ui.home_accent_color`

`sys.bootloader`, `fs.delete`, `fs.write`, and `fs.writebin` are persistent or
destructive. Do not experiment with them without saving `keymap.json` and
documenting a recovery path.

### Device status result

```json
{
  "version": "v0.4.1",
  "profile_index": 0,
  "layer_index": 1,
  "battery": 30,
  "is_charging": true
}
```

## Onboard configuration

The reference device filesystem contains:

```text
keymap.json
```

The file describes profiles, layers, the encoder, 13 keys, joystick sectors,
macros, multi-actions, and optional per-layer lighting.

The factory Codex layer uses vendor actions:

```text
Encoder:
  KV_OAI_ENC_CC
  KV_OAI_ENC_CW
  KV_OAI_ENC_CLK

Agent keys:
  KV_OAI_AG00 through KV_OAI_AG05

Command keys:
  KV_OAI_ACT06 through KV_OAI_ACT12
```

These actions are intended for the Codex desktop integration. A Linux service
can either listen for their vendor notifications or rewrite selected controls
to standard HID keycodes/macros.

## Vendor input notifications

The factory Codex actions arrive as JSON notifications on report `6`, channel
`2`. They do not need to be converted to ordinary keyboard events first.

Key and encoder notifications use this abbreviated form:

```json
{"m":"v.oai.hid","p":{"k":"AG00","act":1}}
```

Observed control identifiers:

- Agent keys: `AG00` through `AG05`
- Command keys: `ACT06` through `ACT12`
- Encoder: `ENC_CC`, `ENC_CW`, and `ENC_CLK`

Observed actions:

- `act: 0`: release
- `act: 1`: press
- `act: 2`: encoder rotation step

All 13 key identifiers, both encoder directions, and encoder press/release were
captured successfully.

Factory switch layout:

```text
AG00  AG01
AG02  AG03  AG04  AG05
ACT06 ACT07 ACT08 ACT09
ACT10 ACT11 ACT12
```

The factory 2U Mic keycap spans `ACT10` and `ACT11`. The daemon retains their
raw events as `key.raw` and emits one logical `MIC` press when the first switch
closes, then one release after both switches open.

The analog joystick uses:

```json
{"m":"v.oai.rad","p":{"a":0.763528,"d":1}}
```

- `a`: normalized angular position, wrapping from `0.0` through `1.0`
- `d`: normalized distance from center, from `0.0` through `1.0`
- Center/release is reported as `{"a":0,"d":0}`

The physical cardinal direction corresponding to angle zero still needs a
labeled capture. No separate joystick-click event has yet been confirmed.

## Lighting

The documented global preview method is:

```json
{
  "method": "lights.preview",
  "params": {
    "backlight": {
      "effect": "solid",
      "brightness": 0.5,
      "speed": 0.5,
      "magic": 0.5,
      "color": 16711680
    },
    "underglow": {
      "effect": "breath",
      "brightness": 0.5,
      "speed": 0.5,
      "magic": 0.5,
      "color": 255
    }
  },
  "id": 1
}
```

Known effects:

- `off`
- `solid`
- `snake`
- `rainbow`
- `breath`
- `gradient`

Brightness, speed, and magic are normalized from `0.0` through `1.0`. Colors
are 24-bit RGB integers.

Live testing establishes three lighting domains:

1. **Six upper Agent keys:** individually addressable.
2. **Seven lower Command keys:** verified as one grouped `keys`/`backlight`
   zone. The generic preview test colored only these seven keys.
3. **Perimeter:** an independent `ambient`/`underglow` zone.

### Individual Agent keys

The six Agent LEDs use:

```json
{
  "method":"v.oai.thstatus",
  "params":[
    {"id":0,"c":16711680,"b":0.35,"e":1,"s":0,"sk":0,"sa":0}
  ],
  "id":1
}
```

Agent IDs `0` through `5` map in physical row-major order: top-to-bottom and
left-to-right.

```text
0 1
2 3 4 5
```

Fields:

- `id`: Agent key index
- `c`: packed 24-bit RGB
- `b`: brightness, `0.0` through `1.0`
- `e`: numeric effect
- `s`: effect speed, `0.0` through `1.0`
- `sk`: synchronize the grouped key zone, `0` or `1`
- `sa`: synchronize the ambient zone, `0` or `1`

The last two fields may be omitted to leave their state unchanged.

### OpenAI lighting zones

The grouped lower keys and perimeter use:

```json
{
  "method":"v.oai.rgbcfg",
  "params":{
    "keys":{"e":1,"b":0.35,"s":0,"m":0,"c":16711680},
    "ambient":{"e":2,"b":0.35,"s":0.4,"m":0,"c":65535}
  },
  "id":1
}
```

`keys` is the lower Command-key zone. `ambient` is the perimeter.

Numeric effects for `v.oai.thstatus` and `v.oai.rgbcfg`:

| Code | Effect |
| ---: | --- |
| `0` | off |
| `1` | solid |
| `2` | snake |
| `3` | rainbow |
| `4` | breath |
| `5` | gradient |
| `6` | shallow breath, from half brightness to full |

Solid colors, breath, shallow breath, and snake effects were verified on the
live device. The firmware accepted Agent ID `6` but no lower key changed.
Together with the official integration's fixed six-slot model, this indicates
that only the six upper Agent LEDs are individually exposed; the lower seven
are exposed as a group.

`lights.preview` changes lighting immediately and is not persisted to flash.
Persistent layer lighting can be represented in `keymap.json`, but persistent
writes will not be attempted until a backup and restore path is implemented.

The daemon remembers successful `v.oai.rgbcfg` and `v.oai.thstatus` calls in
memory and replays them after a USB reconnect. It does not write this live
state to device flash.

## Service input surface

The native service exposes:

- Press and release for all 13 mechanical keys
- Encoder counterclockwise, clockwise, and press
- Normalized joystick direction and magnitude
- Current profile and layer
- Battery and charging state
- Unknown vendor notifications
- Independent RGB state and control for all six Agent keys
- Grouped RGB state and control for the seven Command keys
- Independent perimeter/underglow state and control

## Implemented service

A per-user service owns the raw HID connection and provides a local API.
Only one process should own the configuration channel at a time.

Current components:

```text
codex-microd
  device discovery and reconnect
  HID report decoder/encoder
  serialized JSON-RPC request queue
  vendor notification reader
  action dispatcher
  lighting calls
    six Agent-key LEDs
    grouped Command-key RGB
    perimeter/underglow
  JSONL Unix-socket API

codex-microctl
  status
  events
  agent
  agents
  zones
```

Current commands:

```text
codex-microctl status
codex-microctl events
codex-microctl agent 0 '#00ff00' --effect breath
codex-microctl zones --keys '#ffffff' --ambient '#ff6600'
```

An example factory configuration is included at
`examples/factory-keymap.json`.
Persistent writes remain intentionally disabled.

## Open questions

1. Are joystick axes available simultaneously through report `4` and vendor
   notifications?
2. Does the device emit hotplug-safe state snapshots after reconnect?
3. Does pressing the joystick produce a distinct event?
