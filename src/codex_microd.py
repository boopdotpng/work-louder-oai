#!/usr/bin/env python3
"""Per-user Codex Micro daemon with a JSONL Unix-socket API."""

from __future__ import annotations

import argparse
import json
import os
import queue
import signal
import socket
import socketserver
import stat
import subprocess
import threading
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol

from codex_micro import (
    EFFECT_CODES,
    PRODUCT_NAME,
    TRANSPORTS,
    CodexMicro,
    DeviceInfo,
    DeviceSelector,
    default_socket_path,
    oai_side_config,
)

ACTION_NAMES = {0: "release", 1: "press", 2: "step"}
MAPPABLE_EVENTS = ("key", "encoder", "joystick.sector")
MAPPING_ACTIONS = ("press", "release", "step")


def log(message: str) -> None:
    print(f"codex-microd: {message}", flush=True)


def default_config_path() -> Path:
    config_home = os.environ.get("XDG_CONFIG_HOME")
    root = Path(config_home) if config_home else Path.home() / ".config"
    return root / "codex-micro" / "config.toml"


def rgb(value: Any) -> int:
    if isinstance(value, int) and 0 <= value <= 0xFFFFFF:
        return value
    if isinstance(value, str):
        cleaned = value.removeprefix("#")
        if len(cleaned) == 6:
            try:
                return int(cleaned, 16)
            except ValueError:
                pass
    raise ValueError("color must be #RRGGBB or an integer from 0 through 16777215")


def unit_interval(value: Any, name: str, default: float) -> float:
    if value is None:
        return default
    number = float(value)
    if not 0 <= number <= 1:
        raise ValueError(f"{name} must be between 0 and 1")
    return number


def effect_code(value: Any, default: str = "solid") -> int:
    name = default if value is None else str(value)
    try:
        return EFFECT_CODES[name]
    except KeyError as error:
        raise ValueError(f"unknown effect: {name}") from error


@dataclass
class DeviceCall:
    method: str
    params: Any
    complete: threading.Event = field(default_factory=threading.Event)
    result: Any = None
    error: Exception | None = None


class EventMapper(Protocol):
    def handle(self, event: dict[str, Any]) -> None: ...


@dataclass(frozen=True)
class JoystickSectors:
    """Turn the analog joystick into virtual JOY0..JOYn keys.

    The circle is divided into ``sectors`` equal slices. ``JOY0`` is centered
    on ``offset`` and later sectors follow in increasing-angle order. A sector
    is pressed once the stick reaches ``press`` distance and released when it
    falls below ``release`` or the stick moves to another sector.
    """

    sectors: int = 4
    offset: float = 0.0
    press: float = 0.6
    release: float = 0.4

    def __post_init__(self) -> None:
        if not isinstance(self.sectors, int) or not 0 <= self.sectors <= 16:
            raise ValueError("joystick sectors must be an integer from 0 through 16")
        for name in ("offset", "press", "release"):
            unit_interval(getattr(self, name), f"joystick {name}", 0)
        if self.release > self.press:
            raise ValueError("joystick release must not exceed press")

    def sector(self, angle: float) -> int:
        width = 1 / self.sectors
        return int(((angle - self.offset + width / 2) % 1.0) / width) % self.sectors


@dataclass(frozen=True)
class DaemonConfig:
    mappings: dict[str, dict[str, list[str]]] = field(default_factory=dict)
    joystick: JoystickSectors = field(default_factory=JoystickSectors)
    selector: DeviceSelector = field(default_factory=DeviceSelector)


def parse_mappings(raw_mappings: Any) -> dict[str, dict[str, list[str]]]:
    if not isinstance(raw_mappings, dict):
        raise ValueError("config [mappings] must be a table")
    mappings: dict[str, dict[str, list[str]]] = {}
    for key, actions in raw_mappings.items():
        if not isinstance(actions, dict):
            raise ValueError(f"mapping {key} must be a table")
        parsed: dict[str, list[str]] = {}
        for action, argv in actions.items():
            if action not in MAPPING_ACTIONS:
                raise ValueError(f"unsupported mapping action: {key}.{action}")
            if (
                not isinstance(argv, list)
                or not argv
                or not all(isinstance(part, str) and part for part in argv)
            ):
                raise ValueError(f"{key}.{action} must be a non-empty argv array")
            parsed[action] = argv
        mappings[str(key)] = parsed
    return mappings


def load_config(path: Path) -> DaemonConfig:
    if not path.exists():
        return DaemonConfig()
    with path.open("rb") as config_file:
        document = tomllib.load(config_file)

    joystick = document.get("joystick", {})
    if not isinstance(joystick, dict):
        raise ValueError("config [joystick] must be a table")
    unknown = set(joystick) - {"sectors", "offset", "press", "release"}
    if unknown:
        raise ValueError(f"unknown [joystick] settings: {', '.join(sorted(unknown))}")

    device = document.get("device", {})
    if not isinstance(device, dict):
        raise ValueError("config [device] must be a table")
    unknown = set(device) - {"transport", "name"}
    if unknown:
        raise ValueError(f"unknown [device] settings: {', '.join(sorted(unknown))}")

    try:
        sectors = JoystickSectors(
            **{k: float(v) if k != "sectors" else v for k, v in joystick.items()}
        )
    except TypeError as error:
        raise ValueError(f"invalid [joystick] setting: {error}") from error

    return DaemonConfig(
        mappings=parse_mappings(document.get("mappings", {})),
        joystick=sectors,
        selector=DeviceSelector(
            transport=str(device.get("transport", "auto")),
            name=str(device.get("name", PRODUCT_NAME)),
        ),
    )


class CommandMapper:
    """Execute argv mappings for normalized control actions without a shell."""

    def __init__(self, mappings: dict[str, dict[str, list[str]]]) -> None:
        self.mappings = mappings

    def handle(self, event: dict[str, Any]) -> None:
        if event.get("event") not in MAPPABLE_EVENTS:
            return
        data = event.get("data")
        if not isinstance(data, dict):
            return
        key = data.get("key")
        action = data.get("action")
        argv = self.mappings.get(str(key), {}).get(str(action))
        if argv is None:
            return
        try:
            subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
        except OSError as error:
            log(f"mapping {key}.{action} failed: {error}")


class DeviceWorker:
    """Own the HID descriptor and serialize calls from all socket clients."""

    def __init__(
        self,
        mapper: EventMapper,
        selector: DeviceSelector | None = None,
        joystick: JoystickSectors | None = None,
    ) -> None:
        self.mapper = mapper
        self.selector = selector or DeviceSelector()
        self.joystick = joystick or JoystickSectors()
        self.joystick_sector: int | None = None
        self.device_info: DeviceInfo | None = None
        self.calls: queue.Queue[DeviceCall] = queue.Queue()
        self.subscribers: set[queue.Queue[dict[str, Any]]] = set()
        self.subscribers_lock = threading.Lock()
        self.stop_requested = threading.Event()
        self.connected = threading.Event()
        self.mic_switches_down: set[str] = set()
        self.agent_lighting: dict[int, dict[str, Any]] = {}
        self.zone_lighting: dict[str, Any] | None = None
        self.thread = threading.Thread(
            target=self._run, name="codex-micro-hid", daemon=True
        )

    def start(self) -> None:
        self.thread.start()

    def stop(self) -> None:
        self.stop_requested.set()
        self.thread.join(timeout=4)

    def call(self, method: str, params: Any = None, timeout: float = 4) -> Any:
        if not self.connected.is_set():
            raise RuntimeError("Codex Micro is not connected")
        request = DeviceCall(method, params)
        self.calls.put(request)
        if not request.complete.wait(timeout):
            raise TimeoutError(f"timed out waiting for {method}")
        if request.error is not None:
            raise request.error
        return request.result

    def remember_lighting(self, method: str, params: Any) -> None:
        """Remember successful volatile lighting calls for reconnects."""
        if method == "v.oai.thstatus" and isinstance(params, list):
            for agent in params:
                if isinstance(agent, dict) and isinstance(agent.get("id"), int):
                    self.agent_lighting[agent["id"]] = dict(agent)
        elif method == "v.oai.rgbcfg" and isinstance(params, dict):
            self.zone_lighting = dict(params)

    def restore_lighting(self, device: CodexMicro) -> None:
        """Replay the in-memory lighting scene after a device reconnect."""
        if self.zone_lighting is not None:
            device.rpc("v.oai.rgbcfg", self.zone_lighting)
        if self.agent_lighting:
            agents = [
                self.agent_lighting[key_id] for key_id in sorted(self.agent_lighting)
            ]
            device.rpc("v.oai.thstatus", agents)

    def subscribe(self) -> queue.Queue[dict[str, Any]]:
        events: queue.Queue[dict[str, Any]] = queue.Queue(maxsize=512)
        with self.subscribers_lock:
            self.subscribers.add(events)
        return events

    def unsubscribe(self, events: queue.Queue[dict[str, Any]]) -> None:
        with self.subscribers_lock:
            self.subscribers.discard(events)

    def broadcast(self, event: dict[str, Any]) -> None:
        with self.subscribers_lock:
            subscribers = list(self.subscribers)
        for subscriber in subscribers:
            try:
                subscriber.put_nowait(event)
            except queue.Full:
                try:
                    subscriber.get_nowait()
                    subscriber.put_nowait(event)
                except queue.Empty:
                    pass

    def emit(self, event: dict[str, Any]) -> None:
        self.mapper.handle(event)
        self.broadcast(event)

    def _mic_switch_event(self, key: str, action: Any) -> None:
        was_down = bool(self.mic_switches_down)
        if action == 1:
            self.mic_switches_down.add(key)
        elif action == 0:
            self.mic_switches_down.discard(key)
        is_down = bool(self.mic_switches_down)
        if not was_down and is_down:
            self.emit(
                {
                    "event": "key",
                    "data": {
                        "key": "MIC",
                        "action": "press",
                        "action_code": 1,
                        "raw_keys": ["ACT10", "ACT11"],
                    },
                }
            )
        elif was_down and not is_down:
            self.emit(
                {
                    "event": "key",
                    "data": {
                        "key": "MIC",
                        "action": "release",
                        "action_code": 0,
                        "raw_keys": ["ACT10", "ACT11"],
                    },
                }
            )

    def _joystick_sector_event(self, sector: int, action: int) -> None:
        self.emit(
            {
                "event": "joystick.sector",
                "data": {
                    "key": f"JOY{sector}",
                    "action": ACTION_NAMES[action],
                    "action_code": action,
                    "sector": sector,
                },
            }
        )

    def _joystick(self, angle: Any, distance: Any) -> None:
        settings = self.joystick
        if settings.sectors == 0:
            return
        try:
            angle = float(angle)
            distance = float(distance)
        except (TypeError, ValueError):
            return
        active = self.joystick_sector
        if distance >= settings.press:
            sector = settings.sector(angle)
            if sector != active:
                if active is not None:
                    self._joystick_sector_event(active, 0)
                self.joystick_sector = sector
                self._joystick_sector_event(sector, 1)
        elif distance < settings.release and active is not None:
            self.joystick_sector = None
            self._joystick_sector_event(active, 0)

    def _notification(self, message: dict[str, Any]) -> None:
        method = message.get("m")
        params = message.get("p")
        if not isinstance(params, dict):
            params = {}
        if method == "v.oai.hid":
            key = str(params.get("k", ""))
            action = params.get("act")
            data = {
                "key": key,
                "action": ACTION_NAMES.get(action, action),
                "action_code": action,
            }
            if key.startswith("ENC_"):
                event = {"event": "encoder", "data": data}
                self.emit(event)
                return
            if key in ("ACT10", "ACT11"):
                self.broadcast({"event": "key.raw", "data": data})
                self._mic_switch_event(key, action)
                return
            else:
                event = {"event": "key", "data": data}
        elif method == "v.oai.rad":
            self.emit(
                {
                    "event": "joystick",
                    "data": {
                        "angle": params.get("a"),
                        "distance": params.get("d"),
                    },
                }
            )
            self._joystick(params.get("a"), params.get("d"))
            return
        else:
            event = {
                "event": "vendor",
                "data": {"method": method, "params": params},
            }
        self.emit(event)

    def _fail_queued_calls(self, error: Exception) -> None:
        while True:
            try:
                request = self.calls.get_nowait()
            except queue.Empty:
                return
            request.error = error
            request.complete.set()

    def _run(self) -> None:
        last_error: str | None = None
        while not self.stop_requested.is_set():
            try:
                with CodexMicro(
                    selector=self.selector, on_notification=self._notification
                ) as device:
                    self.mic_switches_down.clear()
                    self.joystick_sector = None
                    self.device_info = device.info
                    self.restore_lighting(device)
                    self.connected.set()
                    last_error = None
                    connection = device.info.describe() if device.info else {}
                    log(
                        "connected"
                        + (f" over {device.info.transport}" if device.info else "")
                    )
                    self.broadcast(
                        {
                            "event": "device",
                            "data": {"connected": True, "connection": connection},
                        }
                    )
                    while not self.stop_requested.is_set():
                        try:
                            request = self.calls.get_nowait()
                        except queue.Empty:
                            device.poll(0.1)
                            continue
                        try:
                            request.result = device.rpc(request.method, request.params)
                            self.remember_lighting(request.method, request.params)
                        except Exception as error:
                            request.error = error
                        finally:
                            request.complete.set()
            except (OSError, RuntimeError) as error:
                was_connected = self.connected.is_set()
                self.connected.clear()
                self.device_info = None
                self._fail_queued_calls(error)
                message = str(error)
                # Bluetooth devices sleep often; report each state change once
                # instead of once per reconnect attempt.
                if was_connected or message != last_error:
                    log(f"disconnected: {message}")
                    self.broadcast(
                        {
                            "event": "device",
                            "data": {"connected": False, "error": message},
                        }
                    )
                last_error = message
                self.stop_requested.wait(1)
        self.connected.clear()


class ApiHandler(socketserver.StreamRequestHandler):
    server: ApiServer

    def send(self, value: dict[str, Any]) -> None:
        self.wfile.write(json.dumps(value, separators=(",", ":")).encode() + b"\n")
        self.wfile.flush()

    def handle(self) -> None:
        for encoded in self.rfile:
            request_id: Any = None
            try:
                request = json.loads(encoded)
                if not isinstance(request, dict):
                    raise ValueError("request must be a JSON object")
                request_id = request.get("id")
                method = request.get("method")
                params = request.get("params") or {}
                if not isinstance(method, str):
                    raise ValueError("method must be a string")
                if not isinstance(params, dict):
                    raise ValueError("params must be an object")
                if method == "events.subscribe":
                    self._subscribe(request_id, params)
                    return
                result = self.dispatch(method, params)
                self.send({"id": request_id, "result": result})
            except (OSError, ValueError, RuntimeError, TimeoutError) as error:
                try:
                    self.send({"id": request_id, "error": {"message": str(error)}})
                except OSError:
                    return

    def dispatch(self, method: str, params: dict[str, Any]) -> Any:
        worker = self.server.worker
        if method == "device.status":
            result = worker.call("device.status")
            info = worker.device_info
            if isinstance(result, dict) and info is not None:
                result = {**result, "connection": info.describe()}
            return result
        if method == "lighting.agent.set":
            agent = self._agent(params)
            return worker.call("v.oai.thstatus", [agent])
        if method == "lighting.agents.set":
            values = params.get("agents")
            if not isinstance(values, list) or not values:
                raise ValueError("agents must be a non-empty array")
            return worker.call("v.oai.thstatus", [self._agent(v) for v in values])
        if method == "lighting.zones.set":
            keys = params.get("keys")
            ambient = params.get("ambient")
            if not isinstance(keys, dict) or not isinstance(ambient, dict):
                raise ValueError("keys and ambient must both be objects")
            return worker.call(
                "v.oai.rgbcfg",
                {
                    "keys": self._zone(keys),
                    "ambient": self._zone(ambient),
                },
            )
        raise ValueError(f"unknown method: {method}")

    @staticmethod
    def _agent(params: Any) -> dict[str, Any]:
        if not isinstance(params, dict):
            raise ValueError("each agent must be an object")
        key_id = int(params.get("id", -1))
        if key_id not in range(6):
            raise ValueError("agent id must be from 0 through 5")
        brightness = unit_interval(params.get("brightness"), "brightness", 1)
        speed = unit_interval(params.get("speed"), "speed", 0.4)
        return {
            "id": key_id,
            "c": rgb(params.get("color")),
            "b": brightness,
            "e": effect_code(params.get("effect")),
            "s": speed,
            "sk": 0,
            "sa": 0,
        }

    @staticmethod
    def _zone(params: dict[str, Any]) -> dict[str, Any]:
        brightness = unit_interval(params.get("brightness"), "brightness", 1)
        effect = str(params.get("effect", "solid"))
        if effect not in EFFECT_CODES:
            raise ValueError(f"unknown effect: {effect}")
        value = oai_side_config(effect, brightness, rgb(params.get("color")))
        value["s"] = unit_interval(params.get("speed"), "speed", value["s"])
        value["m"] = unit_interval(params.get("magic"), "magic", 0)
        return value

    def _subscribe(self, request_id: Any, params: dict[str, Any]) -> None:
        requested = params.get("events")
        if requested is None:
            filters = None
        elif isinstance(requested, list) and all(
            isinstance(name, str) for name in requested
        ):
            filters = set(requested)
        else:
            raise ValueError("events must be an array of event names")
        events = self.server.worker.subscribe()
        try:
            self.send({"id": request_id, "result": {"subscribed": True}})
            while True:
                event = events.get()
                if filters is None or event.get("event") in filters:
                    self.send(event)
        except OSError:
            pass
        finally:
            self.server.worker.unsubscribe(events)


class ApiServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

    def __init__(self, path: Path, worker: DeviceWorker) -> None:
        self.worker = worker
        super().__init__(str(path), ApiHandler)


def ensure_socket_available(path: Path) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if not path.exists():
        return
    mode = path.stat().st_mode
    if not stat.S_ISSOCK(mode):
        raise RuntimeError(f"refusing to replace non-socket path: {path}")
    probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        probe.connect(str(path))
    except ConnectionRefusedError:
        path.unlink()
    else:
        raise RuntimeError(f"another daemon is already listening at {path}")
    finally:
        probe.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", type=Path, default=default_socket_path())
    parser.add_argument("--config", type=Path, default=default_config_path())
    parser.add_argument(
        "--transport",
        choices=TRANSPORTS,
        help="override [device] transport; auto prefers Bluetooth, then USB",
    )
    args = parser.parse_args()

    config = load_config(args.config)
    selector = config.selector
    if args.transport is not None:
        selector = DeviceSelector(args.transport, selector.name)
    ensure_socket_available(args.socket)
    worker = DeviceWorker(CommandMapper(config.mappings), selector, config.joystick)

    def reload_config(*_: object) -> None:
        try:
            updated = load_config(args.config)
        except (OSError, ValueError) as error:
            log(f"config reload failed, keeping previous mappings: {error}")
            return
        worker.mapper = CommandMapper(updated.mappings)
        worker.joystick = updated.joystick
        log(f"reloaded {args.config} ([device] changes need a restart)")

    worker.start()
    try:
        with ApiServer(args.socket, worker) as server:
            os.chmod(args.socket, 0o600)

            def terminate(*_: object) -> None:
                # shutdown() blocks until serve_forever returns, so it must
                # run on a thread other than the one serving requests.
                threading.Thread(target=server.shutdown, daemon=True).start()

            signal.signal(signal.SIGTERM, terminate)
            signal.signal(signal.SIGHUP, reload_config)
            log(f"listening on {args.socket}")
            server.serve_forever(poll_interval=0.25)
    except KeyboardInterrupt:
        pass
    finally:
        worker.stop()
        if args.socket.exists() and stat.S_ISSOCK(args.socket.stat().st_mode):
            args.socket.unlink()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
