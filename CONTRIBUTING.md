# Contributing

Contributions are welcome, especially captures from different firmware
versions, Bluetooth and macOS hardware testing, reconnect fixes, and client
examples.

## Development setup

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Install the binary from a checkout when live-device testing is needed:

```bash
cargo install --path .
```

## Pull requests

Please include:

- What changed and why
- Hardware and firmware version, when hardware was involved
- Whether the result is code-tested, device-tested, or inferred
- Updated protocol/API/coverage documentation
- Tests that do not require physical hardware where practical

## Protocol research

Follow `docs/EXTENDING.md`. Start with read-only calls, use volatile lighting
for output experiments, and back up `keymap.json` before any persistent work.
Never include copied proprietary application source. Small wire-format facts,
independently verified behavior, and clean-room implementations are welcome.
