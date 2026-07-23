# Security

## Reporting

Please report security issues privately through GitHub's security advisory
interface rather than opening a public issue.

## Local security model

- `codex-microd` listens only on a Unix socket under `$XDG_RUNTIME_DIR`.
- The socket is created with mode `0600`.
- The service does not listen on TCP or accept remote connections.
- Command mappings run configured argv arrays without a shell, but they still
  execute with the user's privileges. Protect
  `~/.config/codex-micro/config.toml` accordingly.
- The included udev rule uses `TAG+="uaccess"` instead of making hidraw
  world-writable.

The direct `codex-micro-probe rpc` command is intentionally low-level and can
invoke destructive firmware methods. It should not be exposed to untrusted
input.
