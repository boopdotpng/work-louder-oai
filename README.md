# work-louder-oai

Native, dependency-free Linux support for the Work Louder Codex Micro
(`303a:8360`).

> This is an independent community project. It is not affiliated with or
> supported by Work Louder or OpenAI.

## Features

- All 13 mechanical switch events
- Encoder clockwise, counterclockwise, press, and release
- Continuous analog joystick angle and distance
- Individual RGB and animation control for the six upper Agent LEDs
- Grouped RGB and animations for the seven lower Command LEDs
- Independent perimeter/ambient RGB and animations
- Battery, charging, firmware, profile, and layer status
- Per-user daemon with reconnect and volatile lighting replay
- User-only JSONL Unix-socket API for application callbacks
- Direct press/release/step command mappings
- Logical coalescing of the wide two-switch Mic key

The runtime uses only Python 3.11+ standard-library modules.

## Documentation

- [Hardware coverage](docs/COVERAGE.md)
- [HID and firmware protocol](docs/PROTOCOL.md)
- [Daemon and application API](docs/SERVICE.md)
- [Extending the protocol safely](docs/EXTENDING.md)
- [Contributing](CONTRIBUTING.md)
- [Coding-agent guide](AGENTS.md)

## Install

Install the three commands from a checkout:

```bash
python3 -m pip install --user .
```

Install the udev rule, reload it, and reconnect the keyboard:

```bash
sudo install -Dm644 udev/70-codex-micro.rules \
  /etc/udev/rules.d/70-codex-micro.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Install and start the user service:

```bash
install -Dm644 systemd/codex-microd.service \
  ~/.config/systemd/user/codex-microd.service
systemctl --user daemon-reload
systemctl --user enable --now codex-microd.service
```

The daemon creates `$XDG_RUNTIME_DIR/codex-microd.sock` with mode `0600`.

## CLI

```bash
codex-microctl status
codex-microctl events

# Seven lower Command LEDs as one zone, plus the perimeter
codex-microctl zones \
  --keys '#ff0000' \
  --keys-effect breath \
  --ambient '#0000ff' \
  --ambient-effect snake

# One upper Agent LED; IDs are row-major
codex-microctl agent 0 '#ffffff' --effect shallow-breath

# All six upper Agent LEDs
codex-microctl agents \
  '#ff0000' '#00ff00' '#0000ff' \
  '#ffff00' '#ff00ff' '#00ffff'
```

Lighting calls are volatile and do not modify `keymap.json`.

## Map controls to commands

Copy `config/example.toml` to
`~/.config/codex-micro/config.toml`, then edit argv arrays:

```toml
[mappings.MIC]
press = ["/path/to/start-recording"]
release = ["/path/to/stop-recording"]

[mappings.ACT09]
press = ["/usr/bin/notify-send", "Codex Micro", "ACT09 pressed"]
```

Mappings execute directly without a shell. Restart the daemon after changing
the file:

```bash
systemctl --user restart codex-microd.service
```

## Application callbacks

Run `codex-microctl events` or connect an application to the Unix socket.
`examples/client.py` is a minimal standard-library subscriber.

```json
{"id":1,"method":"events.subscribe","params":{"events":["key","encoder","joystick"]}}
```

## Protocol research

`codex-micro-probe` opens hidraw directly. Stop the daemon before using it:

```bash
systemctl --user stop codex-microd.service
codex-micro-probe status
systemctl --user start codex-microd.service
```

The raw `rpc` probe can invoke persistent or destructive firmware methods.
Read [the extension guide](docs/EXTENDING.md) before experimenting.

## License

MIT
