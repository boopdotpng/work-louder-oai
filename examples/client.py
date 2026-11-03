#!/usr/bin/env python3
"""Minimal codex-micro daemon client using only the Python standard library."""

from __future__ import annotations

import json
import os
import socket
import sys
from pathlib import Path


def socket_path() -> Path:
    if "CODEX_MICROD_SOCKET" in os.environ:
        return Path(os.environ["CODEX_MICROD_SOCKET"])
    if sys.platform == "darwin":
        return Path.home() / "Library/Application Support/codex-micro/codex-microd.sock"
    runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    return Path(runtime) / "codex-microd.sock"


def subscribe() -> None:
    request = {
        "id": 1,
        "method": "events.subscribe",
        "params": {"events": ["key", "encoder", "joystick.sector", "device"]},
    }
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.connect(str(socket_path()))
        client.sendall(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        for encoded in client.makefile():
            message = json.loads(encoded)
            if message.get("event") == "key":
                on_key(message["data"])
            elif "event" in message:
                print(message)


def on_key(event: dict[str, object]) -> None:
    """Replace this body with an application callback."""
    print(f"key={event.get('key')} action={event.get('action')}")


if __name__ == "__main__":
    subscribe()
