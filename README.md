# work-louder-oai

A small native background service for the Work Louder Codex Micro
(`303a:8360`) on macOS and Linux, with no Codex desktop app needed. It owns
the keyboard, runs your own commands for every control, and exposes lighting
and events to other programs.

> This is an independent community project. It is not affiliated with or
> supported by Work Louder or OpenAI.

One Rust binary, `codex-micro`, is both the daemon and its CLI. The release
build is about 0.7 MB and uses about 2.6 MB of memory. On macOS it is
event-driven: it sleeps until the keyboard sends something, and it scans for
the keyboard every 2 seconds only while disconnected.

## Features

- Map every control to a command: all 12 logical keys, encoder steps and
  press, and the joystick as four (or up to 16) directional keys
- Bluetooth-first connection selection with USB fallback
- Individual RGB and animation control for the six upper Agent LEDs
- Grouped RGB and animations for the seven lower Command LEDs
- Independent perimeter/ambient RGB and animations
- Battery, charging, firmware, profile, layer, and connection status
- Automatic reconnect with replay of the current lighting
- User-only JSONL Unix-socket API for application callbacks
- Logical coalescing of the wide two-switch Mic key

## Documentation

- [Hardware coverage](docs/COVERAGE.md)
- [HID and firmware protocol](docs/PROTOCOL.md)
- [Daemon and application API](docs/SERVICE.md)
- [Extending the protocol safely](docs/EXTENDING.md)
- [Contributing](CONTRIBUTING.md)
- [Coding-agent guide](AGENTS.md)

## Install on macOS

Requires macOS 10.15 or later. Quit the Codex desktop app if it is installed;
only one program should drive the keyboard's configuration channel.

Build and install the binary with Rust (`brew install rust`, or
[rustup](https://rustup.rs)):

```bash
cargo install --path .        # installs ~/.cargo/bin/codex-micro
```

Check that macOS can see the keyboard, over Bluetooth or USB:

```bash
codex-micro probe list
codex-micro probe status
```

The first `probe status` asks for Input Monitoring permission. macOS requires
it for any program that opens a keyboard-class HID device. If it fails with
"macOS denied access", enable your terminal app in System Settings >
Privacy & Security > Input Monitoring and run it again.

Install the launchd agent so the daemon starts at login:

```bash
./launchd/install.sh
tail -f ~/Library/Logs/codex-microd.log
```

Under launchd, the `codex-micro` binary itself asks for access. Enable it
under Input Monitoring too (the installer prints its path), then restart the
agent:

```bash
launchctl kickstart -k gui/$(id -u)/com.github.boopdotpng.codex-microd
```

macOS ties this permission to the exact build. After reinstalling a rebuilt
binary, toggle its Input Monitoring entry off and on again.

The daemon listens on
`~/Library/Application Support/codex-micro/codex-microd.sock` with mode
`0600`. Remove the agent with `./launchd/install.sh --uninstall`.

### Bluetooth

Pair the keyboard in System Settings > Bluetooth as usual. By default the
daemon uses Bluetooth whenever the keyboard is connected that way and falls
back to USB otherwise. To require one connection type, set `transport` in the
config file or run `codex-micro daemon --transport bluetooth`.

Bluetooth has not been verified on hardware yet; see
[Hardware coverage](docs/COVERAGE.md#bluetooth-checklist) for a short
checklist that confirms it.

## Install on Linux

```bash
cargo install --path .
sudo install -Dm644 udev/70-codex-micro.rules \
  /etc/udev/rules.d/70-codex-micro.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
install -Dm644 systemd/codex-microd.service \
  ~/.config/systemd/user/codex-microd.service
systemctl --user daemon-reload
systemctl --user enable --now codex-microd.service
```

The Linux socket is `$XDG_RUNTIME_DIR/codex-microd.sock`.

## Map controls to commands

Copy `config/example.toml` to `~/.config/codex-micro/config.toml` and edit
it. Every control can be mapped:

```text
AG00  AG01                        ENC_CC / ENC_CW   step
AG02  AG03  AG04  AG05            ENC_CLK           press / release
ACT06 ACT07 ACT08 ACT09           JOY0 .. JOY3      press / release
MIC (wide key)    ACT12
```

```toml
[mappings.AG00]
press = ["/usr/bin/open", "-a", "Terminal"]

[mappings.MIC]
press = ["/path/to/start-recording"]
release = ["/path/to/stop-recording"]

[mappings.ENC_CW]
step = ["/usr/bin/osascript", "-e",
  "set volume output volume ((output volume of (get volume settings)) + 4)"]
```

Commands are argv arrays and run directly without a shell. Use absolute paths
because launchd provides a minimal `PATH`. `shortcuts run "Name"` and
`open -a App` work well on macOS. Sending keystrokes through `osascript` and
System Events also needs Accessibility permission for `codex-micro`.

The joystick is split into equal sectors, `JOY0` through `JOY3` by default.
Run `codex-micro events joystick`, push the stick in the direction you want
for `JOY0`, and set `[joystick] offset` to the angle that appears.

Reload mappings without restarting:

```bash
launchctl kill HUP gui/$(id -u)/com.github.boopdotpng.codex-microd   # macOS
systemctl --user kill -s HUP codex-microd.service                   # Linux
```

## CLI

```bash
codex-micro status
codex-micro events

# Seven lower Command LEDs as one zone, plus the perimeter
codex-micro zones \
  --keys '#ff0000' \
  --keys-effect breath \
  --ambient '#0000ff' \
  --ambient-effect snake

# One upper Agent LED; IDs are row-major
codex-micro agent 0 '#ffffff' --effect shallow-breath

# All six upper Agent LEDs
codex-micro agents \
  '#ff0000' '#00ff00' '#0000ff' \
  '#ffff00' '#ff00ff' '#00ffff'
```

Lighting calls are volatile and do not modify `keymap.json`. Run
`codex-micro --help` for every option.

## Application callbacks

Run `codex-micro events` or connect an application to the Unix socket.
`examples/client.py` is a minimal subscriber; any language that can open a
Unix socket works.

```json
{"id":1,"method":"events.subscribe","params":{"events":["key","encoder","joystick.sector"]}}
```

## Protocol research

`codex-micro probe` opens the device directly. Stop the daemon first:

```bash
launchctl bootout gui/$(id -u)/com.github.boopdotpng.codex-microd   # macOS
systemctl --user stop codex-microd.service                         # Linux

codex-micro probe list --all     # every HID device and its transport
codex-micro probe status
codex-micro probe monitor        # every input report, read-only

./launchd/install.sh                                               # macOS
systemctl --user start codex-microd.service                        # Linux
```

`codex-micro probe rpc METHOD [JSON]` sends a raw firmware call. It refuses
the known persistent or destructive methods unless you pass `--unsafe`. Read
[the extension guide](docs/EXTENDING.md) before experimenting.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo check --target aarch64-apple-darwin   # type-check macOS code from Linux
```

## License

MIT
