from __future__ import annotations

import argparse
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from codex_micro import CodexMicro, oai_side_config, parse_color  # noqa: E402


class RpcDecoderTests(unittest.TestCase):
    def test_decodes_fragmented_utf8_and_json(self) -> None:
        device = CodexMicro(Path("/dev/null"))
        encoded = json.dumps(
            {"id": 1, "result": {"label": "café"}},
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode()
        split = encoded.index("é".encode()) + 1

        self.assertEqual(device._decoded_messages(encoded[:split]), [])
        self.assertEqual(
            device._decoded_messages(encoded[split:]),
            [{"id": 1, "result": {"label": "café"}}],
        )

    def test_decodes_concatenated_messages(self) -> None:
        device = CodexMicro(Path("/dev/null"))

        self.assertEqual(
            device._decoded_messages(b'{"id":1,"result":true}\r\n{"m":"event","p":{}}'),
            [
                {"id": 1, "result": True},
                {"m": "event", "p": {}},
            ],
        )

    def test_retains_incomplete_trailing_message(self) -> None:
        device = CodexMicro(Path("/dev/null"))

        self.assertEqual(
            device._decoded_messages(b'{"id":1,"result":true}{"id":2'),
            [{"id": 1, "result": True}],
        )
        self.assertEqual(
            device._decoded_messages(b',"result":false}'),
            [{"id": 2, "result": False}],
        )


class ValueParsingTests(unittest.TestCase):
    def test_parse_color(self) -> None:
        self.assertEqual(parse_color("#12AbEf"), 0x12ABEF)
        self.assertEqual(parse_color("000000"), 0)

    def test_parse_color_rejects_invalid_values(self) -> None:
        for value in ("fff", "#12345g", "#1234567"):
            with self.subTest(value=value):
                with self.assertRaises(argparse.ArgumentTypeError):
                    parse_color(value)

    def test_oai_side_config_uses_effect_defaults(self) -> None:
        self.assertEqual(
            oai_side_config("breath", 0.5, 0x123456),
            {"e": 4, "b": 0.5, "s": 0.4, "m": 0, "c": 0x123456},
        )
        self.assertEqual(oai_side_config("solid", 1, 0)["s"], 0)


if __name__ == "__main__":
    unittest.main()
