# Service and application API

## Goal

`codex-microd` is a long-running per-user service and the only process
that opens the Codex Micro HID endpoint. Other applications should never need
udev permissions or knowledge of HID report framing.

The current daemon provides:

- Request/response operations for status and lighting
- A subscribed event stream for keys, encoder, joystick, and connect/disconnect
- Declarative mappings from device events to commands
- Automatic reconnect handling
- In-memory replay of the most recent Agent and zone lighting after reconnect

Virtual-keyboard output and lighting-scene arbitration remain planned.

## Local transport

Use a Unix domain socket under:

```text
$XDG_RUNTIME_DIR/codex-microd.sock
```

Messages are newline-delimited JSON. Each request has an application-generated
ID so clients can match responses:

```json
{"id":1,"method":"device.status"}
```

```json
{"id":1,"result":{"battery":38,"is_charging":true}}
```

Errors preserve the request ID and carry a human-readable message:

```json
{"id":1,"error":{"message":"unknown method: example"}}
```

The current API is intentionally small and additive. Clients should ignore
unknown result members and unknown event data members. A future incompatible
wire change will use a different socket name or an explicit protocol-version
handshake.

This transport is simple to call from Python, JavaScript, Rust, shell tools,
and desktop applications. Socket permissions will restrict access to the
current user.

## Implemented methods

| Method | Purpose |
| --- | --- |
| `device.status` | Firmware, profile, layer, battery, and charging state |
| `lighting.agent.set` | Update one upper Agent LED |
| `lighting.agents.set` | Update one or more upper Agent LEDs atomically |
| `lighting.zones.set` | Update grouped Command LEDs and perimeter |
| `events.subscribe` | Keep the connection open and stream selected events |

### Lighting

```json
{
  "id":2,
  "method":"lighting.agent.set",
  "params":{
    "id":0,
    "color":"#ff8800",
    "brightness":0.7,
    "effect":"breath",
    "speed":0.4
  }
}
```

```json
{
  "id":3,
  "method":"lighting.agents.set",
  "params":{
    "agents":[
      {"id":0,"color":"#ff0000","effect":"solid"},
      {"id":1,"color":"#00ff00","effect":"shallow-breath"}
    ]
  }
}
```

```json
{
  "id":4,
  "method":"lighting.zones.set",
  "params":{
    "keys":{"color":"#ffffff","brightness":0.5,"effect":"solid"},
    "ambient":{"color":"#00ffaa","brightness":0.5,"effect":"snake","speed":0.4}
  }
}
```

The six upper Agent LEDs are individually addressable. The seven lower Command
key LEDs are currently verified only as one `keys` zone. The perimeter is the
independent `ambient` zone.

Colors accept `"#RRGGBB"` or packed integers. Brightness, speed, and magic are
normalized numbers from `0` through `1`. Effects are:

```text
off solid snake rainbow breath gradient shallow-breath
```

### Event subscriptions

A client subscribes once and keeps the socket open:

```json
{"id":5,"method":"events.subscribe","params":{"events":["key","encoder","joystick"]}}
```

The daemon acknowledges the request, then streams events:

```json
{"event":"key","data":{"key":"ACT09","action":"press"}}
{"event":"key","data":{"key":"ACT09","action":"release"}}
{"event":"key","data":{"key":"MIC","action":"press","raw_keys":["ACT10","ACT11"]}}
{"event":"encoder","data":{"key":"ENC_CW","action":"step","action_code":2}}
{"event":"joystick","data":{"angle":0.763528,"distance":1}}
```

An application implements a callback by listening to this stream and invoking
its own function for matching events. Disconnecting removes the subscription.
Available filters are `key`, `key.raw`, `encoder`, `joystick`, `device`, and
`vendor`. Unknown firmware notifications remain observable as `vendor`.

After the subscription acknowledgement, events are ordered as the daemon
observes them. Each client has a bounded queue; if a slow client falls more
than 512 events behind, the daemon drops its oldest queued events so it cannot
block input handling for other clients.

The included CLI exposes this stream directly:

```bash
codex-microctl events
```

## Shortcut strategies

### Direct command mappings

The portable default is to execute an argv list directly for a matching device
event:

```toml
[mappings.MIC]
press = ["/path/to/start-recording"]
release = ["/path/to/stop-recording"]
```

This works independently of GNOME, KDE, or another desktop because the daemon
handles the button itself. Commands must use an argv array rather than a shell
string. Restart `codex-microd.service` after editing its configuration.

Configuration is loaded from:

```text
${XDG_CONFIG_HOME:-$HOME/.config}/codex-micro/config.toml
```

### Virtual keyboard chords

Some users will want the desktop environment to retain ownership of shortcut
configuration. An optional `uinput` adapter can emit a normal chord such as
`Super+Space`; GNOME, KDE, Sway, or another session then handles it normally.

This introduces one distro/session setup step for `/dev/uinput` permission.
The daemon's HID and application APIs remain the same.

Direct command mapping is preferred for the voice button because it avoids
desktop-specific shortcut configuration and preserves distinct press and
release behavior for hold-to-speak. The wide Mic key's two raw switches,
`ACT10` and `ACT11`, are coalesced into one logical `MIC` event. Virtual chords
are useful for applications that expose only a keyboard shortcut.

## Ownership and arbitration

The daemon serializes all device RPC calls, so responses cannot be consumed by
the wrong client. Multiple clients can subscribe concurrently.

Current lighting calls are last-writer-wins. A future scene arbitration layer
could provide:

- A base lighting state persists until changed
- A client can acquire a named, prioritized temporary scene
- Disconnecting a client releases its temporary scenes
- The next-highest scene becomes visible automatically
- Short-lived animations may have a time-to-live

This allows, for example, normal Agent status colors, a temporary recording
animation around the perimeter, and a brief error flash without each
application needing to restore the state it replaced.
