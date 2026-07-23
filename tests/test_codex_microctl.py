from __future__ import annotations

import argparse
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from codex_microctl import color  # noqa: E402


class ColorTests(unittest.TestCase):
    def test_normalizes_hex_color(self) -> None:
        self.assertEqual(color("A0b1C2"), "#a0b1c2")
        self.assertEqual(color("#FFFFFF"), "#ffffff")

    def test_rejects_invalid_color(self) -> None:
        for value in ("#fff", "red", "#12345z"):
            with self.subTest(value=value):
                with self.assertRaises(argparse.ArgumentTypeError):
                    color(value)


if __name__ == "__main__":
    unittest.main()
