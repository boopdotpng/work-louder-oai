from __future__ import annotations

import ctypes
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

import codex_micro_macos  # noqa: E402
from codex_micro import (  # noqa: E402
    MAX_PAYLOAD,
    REPORT_SIZE,
    CodexMicro,
    DeviceInfo,
    DeviceSelector,
    describe_report,
)


def device(transport: str, **overrides: object) -> DeviceInfo:
    values: dict[str, object] = {
        "path": f"{transport}-path",
        "vendor_id": 0x303A,
        "product_id": 0x8360,
        "product": "Codex Micro",
        "transport": transport,
    }
    values.update(overrides)
    return DeviceInfo(**values)  # type: ignore[arg-type]


class SelectorTests(unittest.TestCase):
    def test_auto_prefers_bluetooth(self) -> None:
        usb, bluetooth = device("usb"), device("bluetooth")

        self.assertEqual(DeviceSelector().select([usb, bluetooth]), bluetooth)

    def test_auto_falls_back_to_usb(self) -> None:
        usb = device("usb")

        self.assertEqual(DeviceSelector().select([usb]), usb)

    def test_explicit_transport_is_required(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "over bluetooth"):
            DeviceSelector("bluetooth").select([device("usb")])

    def test_matches_by_name_when_ids_differ(self) -> None:
        renamed = device(
            "bluetooth", vendor_id=0x02E5, product_id=0x1, product="Codex Micro BLE"
        )
        other = device("bluetooth", vendor_id=0x05AC, product_id=1, product="Mouse")

        self.assertEqual(DeviceSelector().select([other, renamed]), renamed)
        self.assertFalse(DeviceSelector().matches(other))

    def test_larger_output_report_breaks_ties(self) -> None:
        keyboard = device("bluetooth", path="a", max_output_report=2)
        vendor = device("bluetooth", path="b", max_output_report=64)

        self.assertEqual(DeviceSelector().select([keyboard, vendor]), vendor)

    def test_rejects_unknown_transport(self) -> None:
        with self.assertRaises(ValueError):
            DeviceSelector("serial")


class FakeTransport:
    def __init__(self, reports: list[bytes]) -> None:
        self.info = device("bluetooth")
        self.reports = list(reports)
        self.written: list[bytes] = []
        self.is_open = False

    def open(self) -> None:
        self.is_open = True

    def close(self) -> None:
        self.is_open = False

    def read(self, timeout: float) -> bytes | None:
        return self.reports.pop(0) if self.reports else None

    def write(self, report: bytes) -> None:
        self.written.append(report)


def rpc_reports(value: object) -> list[bytes]:
    encoded = json.dumps(value, separators=(",", ":")).encode()
    reports = []
    for offset in range(0, len(encoded), MAX_PAYLOAD):
        chunk = encoded[offset : offset + MAX_PAYLOAD]
        report = bytes((6, 2, len(chunk))) + chunk
        reports.append(report + bytes(REPORT_SIZE - len(report)))
    return reports


class RpcTransportTests(unittest.TestCase):
    def test_rpc_frames_requests_and_collects_fragmented_response(self) -> None:
        notifications: list[dict[str, object]] = []
        long_result = {"label": "x" * 150}
        transport = FakeTransport(
            [bytes((1, 0, 0))]  # keyboard report is ignored
            + rpc_reports({"m": "v.oai.hid", "p": {"k": "AG00", "act": 1}})
            + rpc_reports({"id": 1, "result": long_result})
        )
        micro = CodexMicro(on_notification=notifications.append, transport=transport)
        params = {"text": "y" * 100}
        result = micro.rpc("example.method", params)

        self.assertEqual(result, long_result)
        self.assertEqual(
            notifications, [{"m": "v.oai.hid", "p": {"k": "AG00", "act": 1}}]
        )
        request = b"".join(r[3 : 3 + r[2]] for r in transport.written)
        self.assertEqual(
            json.loads(request), {"method": "example.method", "id": 1, "params": params}
        )
        for report in transport.written:
            self.assertEqual(len(report), REPORT_SIZE)
            self.assertEqual(report[:2], bytes((6, 2)))

    def test_injected_transport_is_reused_across_reconnects(self) -> None:
        transport = FakeTransport([])
        micro = CodexMicro(transport=transport)

        with micro:
            self.assertTrue(transport.is_open)
        self.assertFalse(transport.is_open)
        self.assertIs(micro.transport, transport)

    def test_monitor_describes_each_report_kind(self) -> None:
        micro = CodexMicro(transport=FakeTransport([]))
        log = bytes((6, 1, 5)) + b"hello"
        keyboard = bytes((1, 0, 4))

        self.assertEqual(describe_report(micro, log), [{"log": "hello"}])
        self.assertEqual(
            describe_report(micro, keyboard), [{"report": 1, "hex": "01 00 04"}]
        )
        (rpc,) = rpc_reports({"m": "x", "p": {}})
        self.assertEqual(describe_report(micro, rpc), [{"rpc": {"m": "x", "p": {}}}])


class MacOSHelperTests(unittest.TestCase):
    def test_transport_names(self) -> None:
        normalize = codex_micro_macos.normalize_transport
        self.assertEqual(normalize("Bluetooth Low Energy"), "bluetooth")
        self.assertEqual(normalize("Bluetooth"), "bluetooth")
        self.assertEqual(normalize("USB"), "usb")
        self.assertEqual(normalize(""), "unknown")

    def test_report_id_is_always_byte_zero(self) -> None:
        normalize = codex_micro_macos.normalize_report
        self.assertEqual(normalize(6, b"\x06\x02\x00"), b"\x06\x02\x00")
        self.assertEqual(normalize(6, b"\x02\x00"), b"\x06\x02\x00")
        self.assertEqual(normalize(0, b"\x02\x00"), b"\x02\x00")


class FakeFrameworks:
    """Stand-in for the IOKit/CoreFoundation bindings, driven by tests."""

    IOHIDCheckAccess = None
    IOHIDRequestAccess = None
    run_loop_mode = 99

    def __init__(self, devices: dict[int, DeviceInfo], open_status: int = 0) -> None:
        self.devices = devices
        self.open_status = open_status
        self.pending: list[tuple[int, bytes]] = []
        self.remove_on_run = False
        self.set_reports: list[tuple[int, int, bytes]] = []
        self.retained: list[int] = []
        self.released: list[int] = []
        self.closed: list[int] = []
        self.report_callback: object = None
        self.removal_callback: object = None

    def IOHIDManagerCreate(self, *_: object) -> int:
        return 1

    def IOHIDManagerSetDeviceMatching(self, *_: object) -> None:
        pass

    def IOHIDManagerCopyDevices(self, _manager: int) -> int:
        return 2

    def CFSetGetCount(self, _set: int) -> int:
        return len(self.devices)

    def CFSetGetValues(self, _set: int, values: object) -> None:
        for index, ref in enumerate(self.devices):
            values[index] = ref  # type: ignore[index]

    def CFRetain(self, ref: int) -> int:
        self.retained.append(ref)
        return ref

    def CFRelease(self, ref: int) -> None:
        self.released.append(ref)

    def IOHIDDeviceOpen(self, ref: int, _options: int) -> int:
        return self.open_status

    def IOHIDDeviceClose(self, ref: int, _options: int) -> int:
        self.closed.append(ref)
        return 0

    def CFRunLoopGetCurrent(self) -> int:
        return 3

    def IOHIDDeviceRegisterInputReportCallback(self, *args: object) -> None:
        self.report_callback = args[3]

    def IOHIDDeviceRegisterRemovalCallback(self, *args: object) -> None:
        self.removal_callback = args[1]

    def IOHIDDeviceScheduleWithRunLoop(self, *_: object) -> None:
        pass

    def IOHIDDeviceUnscheduleFromRunLoop(self, *_: object) -> None:
        pass

    def IOHIDDeviceSetReport(
        self, ref: int, kind: int, report_id: int, data: bytes, length: int
    ) -> int:
        self.set_reports.append((kind, report_id, data[:length]))
        return 0

    def CFRunLoopRunInMode(self, _mode: int, _seconds: float, _once: bool) -> int:
        if self.remove_on_run:
            self.removal_callback(None, 0, None)  # type: ignore[operator]
            return 4
        if not self.pending:
            return 3  # timed out
        report_id, data = self.pending.pop(0)
        buffer = (ctypes.c_uint8 * len(data)).from_buffer_copy(data)
        self.report_callback(  # type: ignore[operator]
            None, 0, None, 0, report_id, buffer, len(data)
        )
        return 4  # handled source

    def int_property(self, ref: int, key: str) -> int:
        info = self.devices[ref]
        return {
            "VendorID": info.vendor_id,
            "ProductID": info.product_id,
            "MaxInputReportSize": info.max_input_report,
            "MaxOutputReportSize": info.max_output_report,
        }[key]

    def str_property(self, ref: int, key: str) -> str:
        info = self.devices[ref]
        transport = {"usb": "USB", "bluetooth": "Bluetooth Low Energy"}
        return {
            "Product": info.product,
            "Transport": transport[info.transport],
            "SerialNumber": "",
        }[key]

    def registry_id(self, ref: int) -> int:
        return ref


class IOKitTransportTests(unittest.TestCase):
    def setUp(self) -> None:
        self.original = codex_micro_macos.frameworks

    def tearDown(self) -> None:
        codex_micro_macos.frameworks = self.original

    def install(self, fake: FakeFrameworks) -> None:
        codex_micro_macos.frameworks = lambda: fake  # type: ignore[assignment]

    def test_opens_preferred_device_and_exchanges_reports(self) -> None:
        fake = FakeFrameworks(
            {
                10: device("usb", max_input_report=64, max_output_report=64),
                11: device("bluetooth", max_input_report=64, max_output_report=64),
                12: device("bluetooth", vendor_id=0x05AC, product="Trackpad"),
            }
        )
        self.install(fake)
        transport = codex_micro_macos.IOKitTransport(DeviceSelector())

        transport.open()
        self.assertEqual(fake.retained, [11])
        self.assertEqual(transport.info.transport, "bluetooth")  # type: ignore[union-attr]
        self.assertEqual(transport.info.path, "iokit:b")  # type: ignore[union-attr]

        fake.pending = [(6, b"\x06\x02\x02{}"), (1, b"\x00\x00")]
        self.assertEqual(transport.read(0.1), b"\x06\x02\x02{}")
        self.assertEqual(transport.read(0.1), b"\x01\x00\x00")
        self.assertIsNone(transport.read(0))

        transport.write(bytes((6, 2, 0)) + bytes(61))
        self.assertEqual(fake.set_reports, [(1, 6, bytes((6, 2, 0)) + bytes(61))])

        transport.close()
        self.assertEqual(fake.closed, [11])
        self.assertIn(11, fake.released)
        self.assertIn(1, fake.released)  # manager

    def test_permission_denied_explains_input_monitoring(self) -> None:
        fake = FakeFrameworks(
            {10: device("usb")}, open_status=-(0x100000000 - 0xE00002E2)
        )
        self.install(fake)
        transport = codex_micro_macos.IOKitTransport(DeviceSelector())

        with self.assertRaisesRegex(PermissionError, "Input Monitoring"):
            transport.open()
        self.assertEqual(fake.closed, [])  # never opened, so never closed
        self.assertIn(10, fake.released)

    def test_removal_raises_disconnect(self) -> None:
        fake = FakeFrameworks({10: device("usb")})
        self.install(fake)
        transport = codex_micro_macos.IOKitTransport(DeviceSelector())
        transport.open()

        fake.remove_on_run = True
        with self.assertRaisesRegex(OSError, "disconnected"):
            transport.read(0.1)
        with self.assertRaisesRegex(OSError, "disconnected"):
            transport.write(bytes(64))
        transport.close()
        self.assertEqual(fake.closed, [])  # removed devices are not closed

    def test_missing_device_reports_not_connected(self) -> None:
        self.install(FakeFrameworks({}))
        transport = codex_micro_macos.IOKitTransport(DeviceSelector())

        with self.assertRaisesRegex(RuntimeError, "not connected"):
            transport.open()


@unittest.skipUnless(sys.platform == "darwin", "IOKit is only available on macOS")
class IOKitSmokeTests(unittest.TestCase):
    def test_bindings_load_and_enumerate(self) -> None:
        devices = codex_micro_macos.iokit_devices()

        self.assertIsInstance(devices, list)
        for info in devices:
            self.assertIsInstance(info, DeviceInfo)


if __name__ == "__main__":
    unittest.main()
