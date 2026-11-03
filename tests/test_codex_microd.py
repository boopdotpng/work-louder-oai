from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from codex_micro import DeviceSelector  # noqa: E402
from codex_microd import (  # noqa: E402
    ApiHandler,
    CommandMapper,
    DeviceWorker,
    JoystickSectors,
    effect_code,
    load_config,
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
        worker = DeviceWorker(mapper, joystick=JoystickSectors(sectors=0))

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


class JoystickSectorTests(unittest.TestCase):
    def sector_events(self, mapper: RecordingMapper) -> list[tuple[str, str]]:
        return [
            (event["data"]["key"], event["data"]["action"])
            for event in mapper.events
            if event["event"] == "joystick.sector"
        ]

    def test_sector_boundaries_are_centered_on_offset(self) -> None:
        sectors = JoystickSectors(sectors=4, offset=0.1)
        self.assertEqual(sectors.sector(0.1), 0)
        self.assertEqual(sectors.sector(0.99), 0)
        self.assertEqual(sectors.sector(0.2), 0)
        self.assertEqual(sectors.sector(0.24), 1)
        self.assertEqual(sectors.sector(0.6), 2)
        self.assertEqual(sectors.sector(0.85), 3)

    def test_press_move_and_release_with_hysteresis(self) -> None:
        mapper = RecordingMapper()
        worker = DeviceWorker(mapper)

        for angle, distance in (
            (0.0, 0.3),  # below press threshold
            (0.0, 0.9),  # JOY0 press
            (0.02, 0.5),  # inside hysteresis band: still held
            (0.25, 1.0),  # moved to JOY1
            (0.25, 0.1),  # released
            (0, 0),
        ):
            worker._notification({"m": "v.oai.rad", "p": {"a": angle, "d": distance}})

        self.assertEqual(
            self.sector_events(mapper),
            [
                ("JOY0", "press"),
                ("JOY0", "release"),
                ("JOY1", "press"),
                ("JOY1", "release"),
            ],
        )

    def test_invalid_settings_are_rejected(self) -> None:
        for settings in (
            {"sectors": 17},
            {"sectors": 4.0},
            {"press": 1.5},
            {"press": 0.3, "release": 0.5},
        ):
            with self.subTest(settings=settings):
                with self.assertRaises(ValueError):
                    JoystickSectors(**settings)


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


def write_config(directory: str, text: str) -> Path:
    path = Path(directory) / "config.toml"
    path.write_text(text)
    return path


class ConfigTests(unittest.TestCase):
    def test_loads_argv_mappings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = load_config(
                write_config(
                    directory,
                    """
[mappings.MIC]
press = ["/usr/bin/example", "--start"]
release = ["/usr/bin/example", "--stop"]
""",
                )
            )

        self.assertEqual(
            config.mappings["MIC"],
            {
                "press": ["/usr/bin/example", "--start"],
                "release": ["/usr/bin/example", "--stop"],
            },
        )

    def test_rejects_shell_strings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = write_config(directory, '[mappings.MIC]\npress = "example"\n')

            with self.assertRaises(ValueError):
                load_config(path)

    def test_missing_file_uses_defaults(self) -> None:
        config = load_config(Path("/nonexistent/codex-micro/config.toml"))

        self.assertEqual(config.mappings, {})
        self.assertEqual(config.selector, DeviceSelector())
        self.assertEqual(config.joystick, JoystickSectors())

    def test_device_and_joystick_sections(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = load_config(
                write_config(
                    directory,
                    """
[device]
transport = "bluetooth"
name = "Micro"

[joystick]
sectors = 8
offset = 0.25
press = 0.7
release = 0.2
""",
                )
            )

        self.assertEqual(config.selector, DeviceSelector("bluetooth", "Micro"))
        self.assertEqual(config.joystick, JoystickSectors(8, 0.25, 0.7, 0.2))

    def test_rejects_unknown_settings(self) -> None:
        for text in (
            '[device]\ntransport = "serial"\n',
            "[device]\nport = 1\n",
            "[joystick]\ndeadzone = 0.1\n",
            '[joystick]\npress = "high"\n',
            '[mappings.AG00]\nhold = ["/bin/true"]\n',
        ):
            with self.subTest(text=text):
                with tempfile.TemporaryDirectory() as directory:
                    with self.assertRaises(ValueError):
                        load_config(write_config(directory, text))

    def test_example_config_is_valid(self) -> None:
        example = Path(__file__).resolve().parents[1] / "config" / "example.toml"
        config = load_config(example)

        for control in ("MIC", "ENC_CW", "ENC_CC", "ENC_CLK", "JOY0"):
            self.assertIn(control, config.mappings)


class RecordingCommandMapper(CommandMapper):
    def __init__(self, mappings: dict[str, dict[str, list[str]]]) -> None:
        super().__init__(mappings)
        self.launched: list[list[str]] = []

    def handle(self, event: dict[str, Any]) -> None:
        import codex_microd

        original = codex_microd.subprocess.Popen
        codex_microd.subprocess.Popen = lambda argv, **_: self.launched.append(argv)
        try:
            super().handle(event)
        finally:
            codex_microd.subprocess.Popen = original


class CommandMapperTests(unittest.TestCase):
    def test_every_control_type_is_mappable(self) -> None:
        mapper = RecordingCommandMapper(
            {
                "AG00": {"press": ["ag00"]},
                "MIC": {"release": ["mic-up"]},
                "ENC_CW": {"step": ["louder"]},
                "ENC_CLK": {"press": ["mute"]},
                "JOY2": {"press": ["down"]},
            }
        )
        worker = DeviceWorker(mapper)

        for message in (
            {"m": "v.oai.hid", "p": {"k": "AG00", "act": 1}},
            {"m": "v.oai.hid", "p": {"k": "AG00", "act": 0}},
            {"m": "v.oai.hid", "p": {"k": "ACT10", "act": 1}},
            {"m": "v.oai.hid", "p": {"k": "ACT10", "act": 0}},
            {"m": "v.oai.hid", "p": {"k": "ENC_CW", "act": 2}},
            {"m": "v.oai.hid", "p": {"k": "ENC_CLK", "act": 1}},
            {"m": "v.oai.rad", "p": {"a": 0.5, "d": 1}},
        ):
            worker._notification(message)

        self.assertEqual(
            mapper.launched,
            [["ag00"], ["mic-up"], ["louder"], ["mute"], ["down"]],
        )

    def test_raw_mic_switches_and_vendor_events_do_not_launch(self) -> None:
        mapper = RecordingCommandMapper(
            {"ACT10": {"press": ["raw"]}, "future.event": {"press": ["x"]}}
        )
        worker = DeviceWorker(mapper)

        worker._notification({"m": "v.oai.hid", "p": {"k": "ACT10", "act": 1}})
        worker._notification({"m": "future.event", "p": {}})

        self.assertEqual(mapper.launched, [])


if __name__ == "__main__":
    unittest.main()
