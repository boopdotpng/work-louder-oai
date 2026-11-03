# Security

## Reporting

Please report security issues privately through GitHub's security advisory
interface rather than opening a public issue.

## Local security model

- `codex-micro daemon` listens only on a Unix socket in a `0700` directory:
  `~/Library/Application Support/codex-micro/` on macOS and
  `$XDG_RUNTIME_DIR` on Linux.
- The socket is created with mode `0600`.
- The service does not listen on TCP or accept remote connections.
- Command mappings run configured argv arrays without a shell, but they still
  execute with the user's privileges. Protect
  `~/.config/codex-micro/config.toml` accordingly.
- The included udev rule uses `TAG+="uaccess"` instead of making hidraw
  world-writable.

The direct `codex-micro probe rpc` command is intentionally low-level. It
refuses known destructive firmware methods without `--unsafe`, but it should
not be exposed to untrusted input.
