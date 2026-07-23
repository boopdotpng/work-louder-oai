# Agent guide

This repository is a clean, dependency-free Linux implementation of the
Work Louder Codex Micro HID protocol.

## Start here

- Read `README.md` for user-facing behavior and installation.
- Read `docs/COVERAGE.md` before claiming hardware support.
- Read `docs/PROTOCOL.md` before changing HID or RPC framing.
- Read `docs/SERVICE.md` before changing the Unix-socket API.
- Read `docs/EXTENDING.md` before probing a new firmware method.

## Repository map

```text
src/codex_micro.py       HID discovery, report framing, RPC transport, probe
src/codex_microd.py      device owner, reconnect loop, event normalization, API
src/codex_microctl.py    CLI client for the daemon socket
tests/                   hardware-free unit tests
docs/                    protocol, API, coverage, and extension notes
examples/                client/configuration examples and factory keymap
systemd/                 generic user service
udev/                    device access rule
```

## Verification

Run these before committing:

```bash
python3 -m compileall -q src tests examples
python3 -m unittest discover -s tests -v
```

If `ruff` is installed:

```bash
ruff check .
ruff format --check .
```

Live-device checks are optional and must be called out separately. Stop the
daemon before opening the direct probe:

```bash
systemctl --user stop codex-microd.service
codex-micro-probe status
systemctl --user start codex-microd.service
```

## Safety invariants

- Do not call `sys.bootloader`, `fs.delete`, `fs.write`, or `fs.writebin` in
  tests or exploratory scripts.
- Do not persist device configuration unless the task explicitly requires it
  and a fresh `keymap.json` backup has been saved.
- Prefer typed daemon methods over exposing arbitrary firmware RPC calls.
- Lighting calls in this project are volatile and safe to replay on reconnect.
- Keep the daemon socket mode at `0600`; command mappings execute local
  programs with the user's privileges.
- Do not copy proprietary application source into this repository. Record
  independently observed wire facts and implement them cleanly.

## Design conventions

- Python 3.11+ and the standard library only at runtime.
- One process owns the HID descriptor; other programs use the Unix socket.
- One worker thread serializes firmware RPC calls.
- Preserve raw vendor events while exposing normalized application events.
- Unknown notifications should remain observable as `vendor` events.
- New API methods require protocol documentation and hardware-free tests.
