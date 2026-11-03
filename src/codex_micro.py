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
from dataclasses import asdict, dataclass
from glob import glob
from pathlib import Path
from typing import Any, Protocol

USB_VENDOR_ID = 0x303A
USB_PRODUCT_ID = 0x8360
PRODUCT_NAME = "Codex Micro"
REPORT_ID = 6
LOG_CHANNEL = 1
RPC_CHANNEL = 2
REPORT_SIZE = 64
MAX_PAYLOAD = 61
MAX_RPC_BUFFER = 1024 * 1024
TRANSPORTS = ("auto", "bluetooth", "usb")
EFFECT_CODES = {
    "off": 0,
    "solid": 1,
    "snake": 2,
    "rainbow": 3,
    "breath": 4,
    "gradient": 5,
    "shallow-breath": 6,
}
# Linux HID bus numbers from <linux/input.h>.
LINUX_BUSES = {0x03: "usb", 0x05: "bluetooth"}


def default_socket_path() -> Path:
    """Per-user daemon socket path, shared by the daemon and its clients."""
    override = os.environ.get("CODEX_MICROD_SOCKET")
    if override:
        return Path(override)
    if sys.platform == "darwin":
        return (
            Path.home()
            / "Library"
            / "Application Support"
            / "codex-micro"
            / "codex-microd.sock"
        )
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    return Path(runtime) / "codex-microd.sock"


@dataclass(frozen=True)
class DeviceInfo:
    """One HID device as seen by the operating system."""

    path: str
    vendor_id: int
    product_id: int
    product: str
    transport: str
    serial: str = ""
    max_input_report: int = 0
    max_output_report: int = 0

    def describe(self) -> dict[str, Any]:
        value = asdict(self)
        value["vendor_id"] = f"{self.vendor_id:04x}"
        value["product_id"] = f"{self.product_id:04x}"
        return value


@dataclass(frozen=True)
class DeviceSelector:
    """Which Codex Micro connection to use.

    ``transport="auto"`` prefers Bluetooth and falls back to USB. A device
    matches when it has the Codex Micro USB IDs or when its product name
    contains ``name``; Bluetooth connections may advertise different IDs.
    """

    transport: str = "auto"
    name: str = PRODUCT_NAME

    def __post_init__(self) -> None:
        if self.transport not in TRANSPORTS:
            raise ValueError(f"transport must be one of: {', '.join(TRANSPORTS)}")

    def matches(self, info: DeviceInfo) -> bool:
        if (info.vendor_id, info.product_id) == (USB_VENDOR_ID, USB_PRODUCT_ID):
            return True
        return bool(self.name) and self.name.casefold() in info.product.casefold()

    def select(self, devices: list[DeviceInfo]) -> DeviceInfo:
        candidates = [info for info in devices if self.matches(info)]
        if self.transport != "auto":
            candidates = [c for c in candidates if c.transport == self.transport]
        if not candidates:
            where = "" if self.transport == "auto" else f" over {self.transport}"
            raise RuntimeError(f"Work Louder Codex Micro is not connected{where}")
        preference = {"bluetooth": 0, "usb": 1}
        # The vendor report is the largest output report, so a larger output
        # size breaks ties between several interfaces of the same device.
        return min(
            candidates,
            key=lambda c: (preference.get(c.transport, 2), -c.max_output_report),
        )


class Transport(Protocol):
    """Raw HID report I/O. Reports include the report ID as byte 0."""

    info: DeviceInfo | None

    def open(self) -> None: ...

    def close(self) -> None: ...

    def read(self, timeout: float) -> bytes | None: ...

    def write(self, report: bytes) -> None: ...


def linux_devices() -> list[DeviceInfo]:
    devices: list[DeviceInfo] = []
    for candidate in sorted(glob("/dev/hidraw*")):
        node = Path(candidate)
        uevent = Path("/sys/class/hidraw") / node.name / "device/uevent"
        try:
            lines = uevent.read_text().splitlines()
        except OSError:
            continue
        properties = dict(line.partition("=")[::2] for line in lines)
        fields = properties.get("HID_ID", "").split(":")
        if len(fields) != 3:
            continue
        try:
            bus, vendor_id, product_id = (int(field, 16) for field in fields)
        except ValueError:
            continue
        devices.append(
            DeviceInfo(
                path=str(node),
                vendor_id=vendor_id,
                product_id=product_id,
                product=properties.get("HID_NAME", ""),
                transport=LINUX_BUSES.get(bus, f"bus-{bus:04x}"),
                serial=properties.get("HID_UNIQ", ""),
            )
        )
    return devices


def list_devices() -> list[DeviceInfo]:
    """Enumerate every HID device visible to this process."""
    if sys.platform == "darwin":
        from codex_micro_macos import iokit_devices

        return iokit_devices()
    if sys.platform.startswith("linux"):
        return linux_devices()
    raise RuntimeError(f"unsupported platform: {sys.platform}")


class HidrawTransport:
    """Linux /dev/hidraw transport."""

    def __init__(self, path: Path, info: DeviceInfo | None = None) -> None:
        self.path = path
        self.info = info
        self.fd: int | None = None

    def open(self) -> None:
        self.fd = os.open(self.path, os.O_RDWR | os.O_NONBLOCK)

    def close(self) -> None:
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None

    def read(self, timeout: float) -> bytes | None:
        assert self.fd is not None
        readable, _, _ = select.select([self.fd], [], [], timeout)
        if not readable:
            return None
        try:
            return os.read(self.fd, REPORT_SIZE)
        except BlockingIOError:
            return None

    def write(self, report: bytes) -> None:
        assert self.fd is not None
        written = os.write(self.fd, report)
        if written != len(report):
            raise RuntimeError(f"short HID write: {written}/{len(report)}")


def open_transport(selector: DeviceSelector, path: Path | None = None) -> Transport:
    """Create an unopened transport for the current platform."""
    if path is not None:
        if sys.platform == "darwin":
            raise RuntimeError("--device is Linux-only; use --transport on macOS")
        return HidrawTransport(path)
    if sys.platform == "darwin":
        from codex_micro_macos import IOKitTransport

        return IOKitTransport(selector)
    if sys.platform.startswith("linux"):
        info = selector.select(linux_devices())
        return HidrawTransport(Path(info.path), info)
    raise RuntimeError(f"unsupported platform: {sys.platform}")


class CodexMicro:
    def __init__(
        self,
        path: Path | None = None,
        on_notification: Callable[[dict[str, Any]], None] | None = None,
        *,
        selector: DeviceSelector | None = None,
        transport: Transport | None = None,
    ) -> None:
        self.path = path
        self.selector = selector or DeviceSelector()
        self.transport = transport
        self._owns_transport = transport is None
        self.on_notification = on_notification
        self._next_id = 1
        self._rpc_buffer = ""
        self._decoder = json.JSONDecoder()
        self._utf8_decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")

    @property
    def info(self) -> DeviceInfo | None:
        return self.transport.info if self.transport is not None else None

    def __enter__(self) -> CodexMicro:
        if self.transport is None:
            self.transport = open_transport(self.selector, self.path)
        self.transport.open()
        self._rpc_buffer = ""
        self._utf8_decoder.reset()
        try:
            self.drain()
        except BaseException:
            self.__exit__()
            raise
        return self

    def __exit__(self, *_: object) -> None:
        if self.transport is not None:
            self.transport.close()
            if self._owns_transport:
                self.transport = None

    def drain(self, limit: int = 256) -> None:
        """Discard already-queued reports before making a request."""
        assert self.transport is not None
        for _ in range(limit):
            if self.transport.read(0) is None:
                return

    def _write_rpc(self, value: dict[str, Any]) -> None:
        assert self.transport is not None
        encoded = json.dumps(value, separators=(",", ":")).encode()
        for offset in range(0, len(encoded), MAX_PAYLOAD):
            chunk = encoded[offset : offset + MAX_PAYLOAD]
            report = bytes((REPORT_ID, RPC_CHANNEL, len(chunk))) + chunk
            report += bytes(REPORT_SIZE - len(report))
            self.transport.write(report)

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

    def read_report(self, timeout: float) -> bytes | None:
        """Read one raw input report of any report ID."""
        assert self.transport is not None
        return self.transport.read(timeout)

    def decode_report(self, report: bytes) -> list[dict[str, Any]]:
        """Decode a report-6 RPC fragment; other reports yield nothing."""
        if len(report) < 3 or report[0] != REPORT_ID:
            return []
        channel, size = report[1], report[2]
        if channel != RPC_CHANNEL or size > MAX_PAYLOAD:
            return []
        return self._decoded_messages(report[3 : 3 + size])

    def read_messages(self, timeout: float) -> list[dict[str, Any]]:
        """Read and decode the next available report-6 RPC messages."""
        report = self.read_report(timeout)
        if report is None:
            return []
        return self.decode_report(report)

    def poll(self, timeout: float = 0.1) -> None:
        """Dispatch unsolicited device notifications for up to `timeout`."""
        for message in self.read_messages(timeout):
            if self.on_notification is not None and "id" not in message:
                self.on_notification(message)

    def rpc(self, method: str, params: Any | None = None, timeout: float = 3.0) -> Any:
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


def describe_report(device: CodexMicro, report: bytes) -> list[dict[str, Any]]:
    """Render one raw input report for the ``monitor`` command."""
    if len(report) >= 3 and report[0] == REPORT_ID and report[1] == LOG_CHANNEL:
        size = min(report[2], MAX_PAYLOAD)
        text = report[3 : 3 + size].decode("utf-8", errors="replace")
        return [{"log": text}]
    if len(report) >= 3 and report[0] == REPORT_ID and report[1] == RPC_CHANNEL:
        return [{"rpc": message} for message in device.decode_report(report)]
    report_id = report[0] if report else None
    return [{"report": report_id, "hex": report.hex(" ")}]


def list_command(show_all: bool, selector: DeviceSelector) -> int:
    for info in list_devices():
        matches = selector.matches(info)
        if show_all or matches:
            print(json.dumps({**info.describe(), "codex_micro": matches}))
    return 0


def monitor_command(device: CodexMicro) -> int:
    info = device.info
    if info is not None:
        print(json.dumps({"connected": info.describe()}), flush=True)
    try:
        while True:
            report = device.read_report(1.0)
            if report is None:
                continue
            for line in describe_report(device, report):
                print(json.dumps(line, ensure_ascii=False), flush=True)
    except KeyboardInterrupt:
        return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--device", type=Path, help="Linux only: override /dev/hidraw discovery"
    )
    parser.add_argument(
        "--transport",
        choices=TRANSPORTS,
        default="auto",
        help="connection to use; auto prefers Bluetooth, then USB",
    )
    parser.add_argument(
        "--name",
        default=PRODUCT_NAME,
        help="product-name substring that also identifies the device",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    list_parser = commands.add_parser(
        "list", help="list matching HID devices and their transports"
    )
    list_parser.add_argument(
        "--all", action="store_true", help="include every HID device"
    )
    commands.add_parser(
        "monitor", help="print every input report until interrupted (read-only)"
    )
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
    selector = DeviceSelector(args.transport, args.name)
    if args.command == "list":
        try:
            return list_command(args.all, selector)
        except (OSError, RuntimeError) as error:
            print(f"error: {error}", file=sys.stderr)
            return 1
    if args.command == "monitor":
        try:
            with CodexMicro(args.device, selector=selector) as device:
                return monitor_command(device)
        except (OSError, RuntimeError) as error:
            print(f"error: {error}", file=sys.stderr)
            return 1
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
        with CodexMicro(args.device, selector=selector) as device:
            result = device.rpc(method, params)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (OSError, RuntimeError, TimeoutError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
