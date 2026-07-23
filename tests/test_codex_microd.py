from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from codex_microd import (  # noqa: E402
    ApiHandler,
    CommandMapper,
    DeviceWorker,
    effect_code,
    rgb,
    unit_interval,
)


class RecordingMapper:
    def __init__(self) -> None:
        self.events: list[dict[str, Any]] = []

    def handle(self, event: dict[str, Any]) -> None:
        self.events.append(event)


class RecordingDevice:
    def __init__(self) -> None:
        self.calls: list[tuple[str, Any]] = []

    def rpc(self, method: str, params: Any = None) -> None:
        self.calls.append((method, params))


class LogicalMicTests(unittest.TestCase):
    def test_two_switch_key_becomes_one_press_and_release(self) -> None:
        mapper = RecordingMapper()
        worker = DeviceWorker(mapper)

        for key, action in (
            ("ACT10", 1),
            ("ACT11", 1),
            ("ACT10", 0),
            ("ACT11", 0),
        ):
            worker._notification({"m": "v.oai.hid", "p": {"k": key, "act": action}})

        self.assertEqual(
            [
                (event["data"]["key"], event["data"]["action"])
                for event in mapper.events
            ],
            [("MIC", "press"), ("MIC", "release")],
        )

    def test_either_side_can_operate_the_wide_key(self) -> None:
        mapper = RecordingMapper()
        worker = DeviceWorker(mapper)

        worker._notification({"m": "v.oai.hid", "p": {"k": "ACT11", "act": 1}})
        worker._notification({"m": "v.oai.hid", "p": {"k": "ACT11", "act": 0}})

        self.assertEqual(
            [event["data"]["action"] for event in mapper.events],
            ["press", "release"],
        )

    def test_raw_switches_remain_available_to_subscribers(self) -> None:
        worker = DeviceWorker(RecordingMapper())
        events = worker.subscribe()

        worker._notification({"m": "v.oai.hid", "p": {"k": "ACT10", "act": 1}})

        raw = events.get_nowait()
        logical = events.get_nowait()
        self.assertEqual(raw["event"], "key.raw")
        self.assertEqual(raw["data"]["key"], "ACT10")
        self.assertEqual(logical["event"], "key")
        self.assertEqual(logical["data"]["key"], "MIC")


class NotificationTests(unittest.TestCase):
    def test_encoder_and_joystick_are_normalized(self) -> None:
        mapper = RecordingMapper()
        worker = DeviceWorker(mapper)

        worker._notification({"m": "v.oai.hid", "p": {"k": "ENC_CW", "act": 2}})
        worker._notification({"m": "v.oai.rad", "p": {"a": 0.75, "d": 1}})

        self.assertEqual(
            mapper.events,
            [
                {
                    "event": "encoder",
                    "data": {
                        "key": "ENC_CW",
                        "action": "step",
                        "action_code": 2,
                    },
                },
                {
                    "event": "joystick",
                    "data": {"angle": 0.75, "distance": 1},
                },
            ],
        )

    def test_unknown_notifications_are_preserved(self) -> None:
        mapper = RecordingMapper()
        worker = DeviceWorker(mapper)

        worker._notification({"m": "future.event", "p": {"value": 3}})

        self.assertEqual(
            mapper.events,
            [
                {
                    "event": "vendor",
                    "data": {
                        "method": "future.event",
                        "params": {"value": 3},
                    },
                }
            ],
        )


class LightingStateTests(unittest.TestCase):
    def test_latest_lighting_scene_is_replayed_in_stable_order(self) -> None:
        worker = DeviceWorker(RecordingMapper())
        zones = {
            "keys": {"e": 1, "c": 0x101010},
            "ambient": {"e": 2, "c": 0x202020},
        }
        worker.remember_lighting("v.oai.thstatus", [{"id": 3, "c": 3}])
        worker.remember_lighting("v.oai.thstatus", [{"id": 1, "c": 1}])
        worker.remember_lighting("v.oai.thstatus", [{"id": 3, "c": 33}])
        worker.remember_lighting("v.oai.rgbcfg", zones)
        device = RecordingDevice()

        worker.restore_lighting(device)

        self.assertEqual(
            device.calls,
            [
                ("v.oai.rgbcfg", zones),
                (
                    "v.oai.thstatus",
                    [{"id": 1, "c": 1}, {"id": 3, "c": 33}],
                ),
            ],
        )

    def test_unrelated_calls_are_not_remembered(self) -> None:
        worker = DeviceWorker(RecordingMapper())
        worker.remember_lighting("device.status", {"ignored": True})
        device = RecordingDevice()

        worker.restore_lighting(device)

        self.assertEqual(device.calls, [])


class ApiValidationTests(unittest.TestCase):
    def test_agent_payload(self) -> None:
        self.assertEqual(
            ApiHandler._agent(
                {
                    "id": 5,
                    "color": "#12abef",
                    "brightness": 0.25,
                    "effect": "shallow-breath",
                    "speed": 0.75,
                }
            ),
            {
                "id": 5,
                "c": 0x12ABEF,
                "b": 0.25,
                "e": 6,
                "s": 0.75,
                "sk": 0,
                "sa": 0,
            },
        )

    def test_zone_payload(self) -> None:
        self.assertEqual(
            ApiHandler._zone(
                {
                    "color": 0x010203,
                    "brightness": 0.5,
                    "effect": "breath",
                    "speed": 0.2,
                    "magic": 0.1,
                }
            ),
            {"e": 4, "b": 0.5, "s": 0.2, "m": 0.1, "c": 0x010203},
        )

    def test_invalid_values_are_rejected(self) -> None:
        invalid = (
            lambda: rgb("#nope00"),
            lambda: rgb(0x1000000),
            lambda: unit_interval(1.1, "brightness", 1),
            lambda: effect_code("sparkles"),
            lambda: ApiHandler._agent({"id": 6, "color": "#ffffff"}),
        )
        for operation in invalid:
            with self.subTest(operation=operation):
                with self.assertRaises(ValueError):
                    operation()


class CommandMapperTests(unittest.TestCase):
    def test_loads_argv_mappings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.toml"
            path.write_text(
                """
[mappings.MIC]
press = ["/usr/bin/example", "--start"]
release = ["/usr/bin/example", "--stop"]
""".lstrip()
            )

            mapper = CommandMapper(path)

        self.assertEqual(
            mapper.mappings["MIC"],
            {
                "press": ["/usr/bin/example", "--start"],
                "release": ["/usr/bin/example", "--stop"],
            },
        )

    def test_rejects_shell_strings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.toml"
            path.write_text('[mappings.MIC]\npress = "example --start"\n')

            with self.assertRaises(ValueError):
                CommandMapper(path)


if __name__ == "__main__":
    unittest.main()
