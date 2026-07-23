#!/usr/bin/env python3
"""Dependency-free Codex Micro HID/RPC transport and diagnostic CLI.

The explicit ``rpc`` subcommand is intended for protocol research. It can call
destructive firmware methods, so prefer the typed status and lighting commands.
"""

from __future__ import annotations

import argparse
import codecs
import json
import os
import select
import sys
import time
from collections.abc import Callable
from glob import glob
from pathlib import Path
from typing import Any

USB_VENDOR = "303A"
USB_PRODUCT = "8360"
REPORT_ID = 6
RPC_CHANNEL = 2
REPORT_SIZE = 64
MAX_PAYLOAD = 61
MAX_RPC_BUFFER = 1024 * 1024
EFFECT_CODES = {
    "off": 0,
    "solid": 1,
    "snake": 2,
    "rainbow": 3,
    "breath": 4,
    "gradient": 5,
    "shallow-breath": 6,
}


def discover_hidraw() -> Path:
    """Find the Codex Micro hidraw node by VID/PID."""
    for candidate in sorted(glob("/dev/hidraw*")):
        node = Path(candidate)
        uevent = Path("/sys/class/hidraw") / node.name / "device/uevent"
        try:
            properties = uevent.read_text()
        except OSError:
            continue
        hid_id = next(
            (
                line.partition("=")[2]
                for line in properties.splitlines()
                if line.startswith("HID_ID=")
            ),
            "",
        )
        fields = hid_id.split(":")
        if (
            len(fields) == 3
            and fields[1].lstrip("0").upper() == USB_VENDOR
            and fields[2].lstrip("0").upper() == USB_PRODUCT
        ):
            return node
    raise RuntimeError("Work Louder Codex Micro (303a:8360) is not connected")


class CodexMicro:
    def __init__(
        self,
        path: Path | None = None,
        on_notification: Callable[[dict[str, Any]], None] | None = None,
    ) -> None:
        self.path = path or discover_hidraw()
        self.fd: int | None = None
        self.on_notification = on_notification
        self._next_id = 1
        self._rpc_buffer = ""
        self._decoder = json.JSONDecoder()
        self._utf8_decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")

    def __enter__(self) -> CodexMicro:
        self.fd = os.open(self.path, os.O_RDWR | os.O_NONBLOCK)
        self.drain()
        return self

    def __exit__(self, *_: object) -> None:
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None

    def drain(self) -> None:
        """Discard already-queued reports before making a request."""
        assert self.fd is not None
        while True:
            readable, _, _ = select.select([self.fd], [], [], 0)
            if not readable:
                return
            try:
                os.read(self.fd, REPORT_SIZE)
            except BlockingIOError:
                return

    def _write_rpc(self, value: dict[str, Any]) -> None:
        assert self.fd is not None
        encoded = json.dumps(value, separators=(",", ":")).encode()
        for offset in range(0, len(encoded), MAX_PAYLOAD):
            chunk = encoded[offset : offset + MAX_PAYLOAD]
            report = bytes((REPORT_ID, RPC_CHANNEL, len(chunk))) + chunk
            report += bytes(REPORT_SIZE - len(report))
            written = os.write(self.fd, report)
            if written != REPORT_SIZE:
                raise RuntimeError(f"short HID write: {written}/{REPORT_SIZE}")

    def _decoded_messages(self, payload: bytes) -> list[dict[str, Any]]:
        self._rpc_buffer += self._utf8_decoder.decode(payload)
        if len(self._rpc_buffer) > MAX_RPC_BUFFER:
            self._rpc_buffer = ""
            self._utf8_decoder.reset()
            raise RuntimeError("RPC receive buffer exceeded 1 MiB")
        messages: list[dict[str, Any]] = []
        while True:
            self._rpc_buffer = self._rpc_buffer.lstrip("\r\n\t ")
            if not self._rpc_buffer:
                break
            try:
                value, end = self._decoder.raw_decode(self._rpc_buffer)
            except json.JSONDecodeError:
                break
            self._rpc_buffer = self._rpc_buffer[end:]
            if isinstance(value, dict):
                messages.append(value)
        return messages

    def read_messages(self, timeout: float) -> list[dict[str, Any]]:
        """Read and decode the next available report-6 RPC messages."""
        assert self.fd is not None
        readable, _, _ = select.select([self.fd], [], [], timeout)
        if not readable:
            return []
        report = os.read(self.fd, REPORT_SIZE)
        if len(report) < 3 or report[0] != REPORT_ID:
            return []
        channel, size = report[1], report[2]
        if channel != RPC_CHANNEL or size > MAX_PAYLOAD:
            return []
        return self._decoded_messages(report[3 : 3 + size])

    def poll(self, timeout: float = 0.1) -> None:
        """Dispatch unsolicited device notifications for up to `timeout`."""
        for message in self.read_messages(timeout):
            if self.on_notification is not None and "id" not in message:
                self.on_notification(message)

    def rpc(self, method: str, params: Any | None = None, timeout: float = 3.0) -> Any:
        assert self.fd is not None
        request_id = self._next_id
        self._next_id += 1
        request: dict[str, Any] = {"method": method, "id": request_id}
        if params is not None:
            request["params"] = params
        self._write_rpc(request)

        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"timed out waiting for {method!r}")
            for message in self.read_messages(remaining):
                if message.get("id") != request_id:
                    if self.on_notification is not None and "id" not in message:
                        self.on_notification(message)
                    continue
                if "error" in message:
                    error = message["error"]
                    if isinstance(error, dict):
                        detail = error.get("message", json.dumps(error))
                    else:
                        detail = str(error)
                    raise RuntimeError(f"{method}: {detail}")
                return message.get("result")


def parse_color(value: str) -> int:
    cleaned = value.removeprefix("#")
    if len(cleaned) != 6:
        raise argparse.ArgumentTypeError("colors must be six-digit RGB hex")
    try:
        return int(cleaned, 16)
    except ValueError as error:
        raise argparse.ArgumentTypeError("colors must be six-digit RGB hex") from error


def side_config(effect: str, brightness: float, color: int) -> dict[str, Any]:
    return {
        "effect": effect,
        "brightness": brightness,
        "speed": 0.5,
        "magic": 0.5,
        "color": color,
    }


def oai_side_config(effect: str, brightness: float, color: int) -> dict[str, Any]:
    return {
        "e": EFFECT_CODES[effect],
        "b": brightness,
        "s": 0.4 if effect not in ("off", "solid") else 0,
        "m": 0,
        "c": color,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", type=Path, help="override /dev/hidraw discovery")
    commands = parser.add_subparsers(dest="command", required=True)

    commands.add_parser("status", help="read device status")

    rpc_parser = commands.add_parser(
        "rpc", help="make an unfiltered RPC call (advanced and potentially unsafe)"
    )
    rpc_parser.add_argument("method")
    rpc_parser.add_argument(
        "params", nargs="?", default=None, help="JSON params (omit for none)"
    )

    preview = commands.add_parser(
        "preview", help="send a volatile backlight/underglow preview"
    )
    preview.add_argument("--backlight", type=parse_color, required=True)
    preview.add_argument("--underglow", type=parse_color, required=True)
    preview.add_argument(
        "--backlight-effect",
        choices=("off", "solid", "snake", "rainbow", "breath", "gradient"),
        default="solid",
    )
    preview.add_argument(
        "--underglow-effect",
        choices=("off", "solid", "snake", "rainbow", "breath", "gradient"),
        default="solid",
    )
    preview.add_argument(
        "--brightness", type=float, default=0.35, choices=None, metavar="0..1"
    )

    zones = commands.add_parser(
        "zones", help="control the grouped Command-key and perimeter zones"
    )
    zones.add_argument("--keys", type=parse_color, required=True)
    zones.add_argument("--ambient", type=parse_color, required=True)
    zones.add_argument("--keys-effect", choices=tuple(EFFECT_CODES), default="solid")
    zones.add_argument("--ambient-effect", choices=tuple(EFFECT_CODES), default="solid")
    zones.add_argument("--brightness", type=float, default=0.35, metavar="0..1")

    agent = commands.add_parser("agent", help="control one upper Agent-key LED")
    agent.add_argument("id", type=int, choices=range(6), metavar="0..5")
    agent.add_argument("color", type=parse_color)
    agent.add_argument("--effect", choices=tuple(EFFECT_CODES), default="solid")
    agent.add_argument("--brightness", type=float, default=0.35, metavar="0..1")
    agent.add_argument("--speed", type=float, default=0.4, metavar="0..1")

    agents = commands.add_parser(
        "agents", help="set all six Agent-key LEDs in physical row-major order"
    )
    agents.add_argument("colors", type=parse_color, nargs=6, metavar="RGB")
    agents.add_argument("--effect", choices=tuple(EFFECT_CODES), default="solid")
    agents.add_argument("--brightness", type=float, default=0.35, metavar="0..1")
    agents.add_argument("--speed", type=float, default=0.4, metavar="0..1")

    args = parser.parse_args()
    if args.command == "rpc":
        params = json.loads(args.params) if args.params is not None else None
        method = args.method
    elif args.command == "status":
        params = None
        method = "device.status"
    elif args.command == "preview":
        if not 0.0 <= args.brightness <= 1.0:
            parser.error("--brightness must be between 0 and 1")
        method = "lights.preview"
        params = {
            "backlight": side_config(
                args.backlight_effect, args.brightness, args.backlight
            ),
            "underglow": side_config(
                args.underglow_effect, args.brightness, args.underglow
            ),
        }
    elif args.command == "zones":
        if not 0.0 <= args.brightness <= 1.0:
            parser.error("--brightness must be between 0 and 1")
        method = "v.oai.rgbcfg"
        params = {
            "keys": oai_side_config(args.keys_effect, args.brightness, args.keys),
            "ambient": oai_side_config(
                args.ambient_effect, args.brightness, args.ambient
            ),
        }
    else:
        if not 0.0 <= args.brightness <= 1.0:
            parser.error("--brightness must be between 0 and 1")
        if not 0.0 <= args.speed <= 1.0:
            parser.error("--speed must be between 0 and 1")
        colors = args.colors if args.command == "agents" else [args.color]
        ids = range(6) if args.command == "agents" else [args.id]
        method = "v.oai.thstatus"
        params = [
            {
                "id": key_id,
                "c": color,
                "b": args.brightness,
                "e": EFFECT_CODES[args.effect],
                "s": args.speed,
                "sk": 0,
                "sa": 0,
            }
            for key_id, color in zip(ids, colors, strict=True)
        ]

    try:
        with CodexMicro(args.device) as device:
            result = device.rpc(method, params)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (OSError, RuntimeError, TimeoutError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
