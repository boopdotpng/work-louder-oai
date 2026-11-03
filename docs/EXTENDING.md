# Extending the protocol

This guide is for contributors and coding agents adding firmware methods,
events, hardware revisions, or client features.

## Safety levels

### Safe to inspect

- HID report descriptors
- `sys.version`
- `device.status`
- `fs.list`
- `fs.read`
- Vendor notifications

### Reversible output

- `lights.preview`
- `v.oai.rgbcfg`
- `v.oai.thstatus`

These calls alter live lighting but do not write flash.

### Persistent or destructive

- `fs.write`
- `fs.writebin`
- `fs.delete`
- `sys.bootloader`

Do not use these during routine exploration. Save a fresh `keymap.json` backup
and document a recovery path before any persistent experiment.

## Direct probing

Only one process should consume the report-6 response stream. Stop the daemon
(and quit the Codex desktop app, if installed) before using the direct probe:

```bash
# macOS
launchctl bootout gui/$(id -u)/com.github.boopdotpng.codex-microd
codex-micro probe list --all
codex-micro probe status
codex-micro probe monitor
codex-micro probe rpc fs.list '{"checksum":true}'
codex-micro probe rpc lights.preview '{"backlight":{"effect":"solid","brightness":0.5,"speed":0.5,"magic":0.5,"color":16711680},"underglow":{"effect":"breath","brightness":0.5,"speed":0.5,"magic":0.5,"color":255}}'
./launchd/install.sh

# Linux
systemctl --user stop codex-microd.service
codex-micro probe status
systemctl --user start codex-microd.service
```

`list` and `monitor` are read-only. `--transport bluetooth|usb` and `--name`
choose the device the same way the daemon does.

The raw `rpc` subcommand refuses `sys.bootloader`, `fs.delete`, `fs.write`,
and `fs.writebin` unless `--unsafe` is passed, but otherwise does not filter
method names. Treat it like a firmware debugging tool, not an application
API.

## Adding a notification

1. Capture the raw JSON object from report `6`, channel `2`.
2. Add the exact wire shape to `docs/PROTOCOL.md`.
3. Normalize it in `Normalizer::notification` (`src/events.rs`).
4. Preserve unknown messages as `vendor` events.
5. Add a hardware-free unit test with a representative notification.
6. Update `docs/COVERAGE.md` with firmware and test status.

Do not discard raw identifiers merely because a logical alias exists. For
example, the daemon publishes raw `ACT10`/`ACT11` events as `key.raw` while
also coalescing them into the logical wide `MIC` key.

## Adding a firmware output method

1. Establish whether the call is read-only, volatile, persistent, or
   destructive.
2. Verify the smallest possible payload on hardware.
3. Document the wire method and abbreviated fields.
4. Add a typed method to `dispatch` in `src/api.rs` rather than exposing
   arbitrary RPC.
5. Validate all numbers, colors, IDs, and enums at the API boundary.
6. Decide whether volatile state should be replayed after reconnect.
7. Add CLI support only after the daemon method is stable.

## Framing pitfalls

- A raw report is 64 bytes including report ID `6`.
- Vendor payload chunks contain at most 61 bytes.
- JSON can span several reports.
- UTF-8 code points can also span reports; use an incremental decoder.
- Notifications can arrive while waiting for an RPC response.
- `/dev/hidrawN` numbers change after reconnect.
- Match HID VID/PID (`303a:8360`) or product name rather than a numbered
  device path.
- On macOS, keep every IOKit call for one device on the thread that opened
  it.

## Clean-room rule

Do not commit proprietary application source, extracted packages, or copied
implementation text. It is appropriate to document independently observed
method names, field shapes, report bytes, and device behavior, then implement
those facts in original code.

## Live test record

When reporting a new result, include:

- Device product and VID/PID as reported by `codex-micro probe list`
- Firmware version
- Operating system and USB or Bluetooth transport
- Request payload or input action
- Exact response/notification
- Visible behavior
- Whether a reconnect was required

This makes it possible to distinguish firmware changes from client regressions.
