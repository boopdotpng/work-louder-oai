#!/usr/bin/env python3
"""Client for the per-user codex-microd Unix-socket API."""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
from pathlib import Path
from typing import Any

EFFECTS = (
    "off",
    "solid",
    "snake",
    "rainbow",
    "breath",
    "gradient",
    "shallow-breath",
)


def default_socket_path() -> Path:
    runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    return Path(runtime) / "codex-microd.sock"


def color(value: str) -> str:
    cleaned = value.removeprefix("#")
    if len(cleaned) != 6:
        raise argparse.ArgumentTypeError("color must be #RRGGBB")
    try:
        int(cleaned, 16)
    except ValueError as error:
        raise argparse.ArgumentTypeError("color must be #RRGGBB") from error
    return f"#{cleaned.lower()}"


def request(path: Path, method: str, params: dict[str, Any] | None = None) -> Any:
    message: dict[str, Any] = {"id": 1, "method": method}
    if params is not None:
        message["params"] = params
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.connect(str(path))
        client.sendall(json.dumps(message, separators=(",", ":")).encode() + b"\n")
        encoded = client.makefile().readline()
        if not encoded:
            raise ConnectionError("daemon closed the socket without a response")
        response = json.loads(encoded)
    if "error" in response:
        error = response["error"]
        detail = error.get("message", error) if isinstance(error, dict) else error
        raise RuntimeError(str(detail))
    return response.get("result")


def events(path: Path) -> None:
    message = {
        "id": 1,
        "method": "events.subscribe",
        "params": {"events": ["key", "encoder", "joystick", "device"]},
    }
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.connect(str(path))
        client.sendall(json.dumps(message, separators=(",", ":")).encode() + b"\n")
        for line in client.makefile():
            value = json.loads(line)
            if "error" in value:
                raise RuntimeError(value["error"].get("message", value["error"]))
            if "event" in value:
                print(json.dumps(value, separators=(",", ":")), flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", type=Path, default=default_socket_path())
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("status")
    commands.add_parser("events")

    agent = commands.add_parser("agent")
    agent.add_argument("id", type=int, choices=range(6), metavar="0..5")
    agent.add_argument("color", type=color)
    agent.add_argument("--effect", choices=EFFECTS, default="solid")
    agent.add_argument("--brightness", type=float, default=0.5)
    agent.add_argument("--speed", type=float, default=0.4)

    agents = commands.add_parser("agents")
    agents.add_argument("colors", type=color, nargs=6, metavar="RGB")
    agents.add_argument("--effect", choices=EFFECTS, default="solid")
    agents.add_argument("--brightness", type=float, default=0.5)
    agents.add_argument("--speed", type=float, default=0.4)

    zones = commands.add_parser("zones")
    zones.add_argument("--keys", type=color, required=True)
    zones.add_argument("--ambient", type=color, required=True)
    zones.add_argument("--keys-effect", choices=EFFECTS, default="solid")
    zones.add_argument("--ambient-effect", choices=EFFECTS, default="solid")
    zones.add_argument("--brightness", type=float, default=0.5)
    zones.add_argument("--speed", type=float, default=0.4)

    args = parser.parse_args()
    try:
        if args.command == "events":
            events(args.socket)
            return 0
        if args.command == "status":
            result = request(args.socket, "device.status")
        elif args.command == "agent":
            result = request(
                args.socket,
                "lighting.agent.set",
                {
                    "id": args.id,
                    "color": args.color,
                    "effect": args.effect,
                    "brightness": args.brightness,
                    "speed": args.speed,
                },
            )
        elif args.command == "agents":
            result = request(
                args.socket,
                "lighting.agents.set",
                {
                    "agents": [
                        {
                            "id": key_id,
                            "color": key_color,
                            "effect": args.effect,
                            "brightness": args.brightness,
                            "speed": args.speed,
                        }
                        for key_id, key_color in enumerate(args.colors)
                    ]
                },
            )
        else:
            result = request(
                args.socket,
                "lighting.zones.set",
                {
                    "keys": {
                        "color": args.keys,
                        "effect": args.keys_effect,
                        "brightness": args.brightness,
                        "speed": args.speed,
                    },
                    "ambient": {
                        "color": args.ambient,
                        "effect": args.ambient_effect,
                        "brightness": args.brightness,
                        "speed": args.speed,
                    },
                },
            )
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (
        ConnectionError,
        FileNotFoundError,
        json.JSONDecodeError,
        OSError,
        RuntimeError,
    ) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
