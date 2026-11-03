# Agent guide

This repository is a small, clean Rust implementation of the Work Louder Codex
Micro HID protocol for macOS and Linux: one binary that is both the background
daemon and its CLI.

## Start here

- Read `README.md` for user-facing behavior and installation.
- Read `docs/COVERAGE.md` before claiming hardware support.
- Read `docs/PROTOCOL.md` before changing HID or RPC framing.
- Read `docs/SERVICE.md` before changing the Unix-socket API.
- Read `docs/EXTENDING.md` before probing a new firmware method.

## Repository map

```text
src/main.rs              argument parsing, daemon startup, signal handling
src/protocol.rs          report-6 framing, JSON stream decoder, lighting payloads
src/events.rs            notification normalization, Mic coalescing, joystick sectors
src/hub.rs               shared state: serialized RPC, subscribers, mappings, replay
src/api.rs               JSONL Unix-socket server
src/client.rs            CLI socket client and direct-device probe commands
src/config.rs            config.toml parsing and validation
src/device.rs            device identity, selection, backend interface
src/hid_macos.rs         IOKit backend on a CFRunLoop (macOS)
src/hid_linux.rs         hidraw backend (Linux)
docs/                    protocol, API, coverage, and extension notes
examples/                client/configuration examples and factory keymap
launchd/                 macOS LaunchAgent template and installer
systemd/                 Linux user service
udev/                    Linux device access rule
```

## Verification

Run these before committing:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo clippy --target aarch64-apple-darwin --all-targets -- -D warnings
```

The last command type-checks the macOS backend from Linux after
`rustup target add aarch64-apple-darwin`. It cannot link or run there.

Live-device checks are optional and must be called out separately. Stop the
daemon before opening the direct probe:

```bash
# macOS
launchctl bootout gui/$(id -u)/com.github.boopdotpng.codex-microd
codex-micro probe status
./launchd/install.sh

# Linux
systemctl --user stop codex-microd.service
codex-micro probe status
systemctl --user start codex-microd.service
```

IOKit code cannot run on Linux; keep logic out of `hid_macos.rs` where
possible so it is covered by the hardware-free tests (`hub::tests::FakeLink`
simulates a device), and rely on the macOS CI job for the FFI.

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

- Keep the binary small: dependencies are `libc`, `serde_json`, and `toml`.
  Do not add an async runtime or CLI framework.
- One process owns the HID descriptor; other programs use the Unix socket.
- One backend thread owns the device; one lock serializes firmware RPC calls.
- Never block the backend thread on a firmware reply; it delivers replies.
- Never seize the HID device; its keyboard functions must keep working.
- Preserve raw vendor events while exposing normalized application events.
- Unknown notifications should remain observable as `vendor` events.
- New API methods require protocol documentation and hardware-free tests.
