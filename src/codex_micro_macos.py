"""macOS IOKit HID transport for the Codex Micro, implemented with ctypes.

IOKit presents USB, classic Bluetooth, and Bluetooth LE HID devices through the
same IOHIDDevice API, so one transport serves every connection type. Reports
use the same layout as Linux hidraw: byte 0 is the report ID.

The device is opened without seizing it, so its ordinary keyboard, consumer,
and pointer functions keep working. macOS requires the Input Monitoring
privacy permission for the process that opens a keyboard-class HID device.
"""

from __future__ import annotations

import collections
import ctypes
import time
from ctypes import (
    CFUNCTYPE,
    POINTER,
    byref,
    c_bool,
    c_char_p,
    c_double,
    c_int,
    c_int32,
    c_int64,
    c_long,
    c_uint8,
    c_uint32,
    c_uint64,
    c_ulong,
    c_void_p,
)
from typing import Any

from codex_micro import REPORT_SIZE, DeviceInfo, DeviceSelector

_IOKIT = "/System/Library/Frameworks/IOKit.framework/IOKit"
_CORE_FOUNDATION = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"

kCFStringEncodingUTF8 = 0x08000100
kCFNumberSInt64Type = 4
kCFRunLoopRunFinished = 1
kIOHIDOptionsTypeNone = 0
kIOHIDReportTypeOutput = 1
kIOHIDRequestTypeListenEvent = 1
kIOHIDAccessTypeGranted = 0
kIOHIDAccessTypeUnknown = 2
kIOReturnNotPermitted = 0xE00002E2
kIOReturnExclusiveAccess = 0xE00002C5
RUN_LOOP_MODE = "com.github.boopdotpng.codex-micro"
MAX_QUEUED_REPORTS = 4096

INPUT_MONITORING_HELP = (
    "macOS denied access to the Codex Micro. Allow Input Monitoring for the "
    "program running codex-microd (Terminal when run by hand, or the Python "
    "interpreter when run by launchd) in System Settings > Privacy & Security "
    "> Input Monitoring, then restart it."
)

# void (*IOHIDReportCallback)(void *context, IOReturn result, void *sender,
#     IOHIDReportType type, uint32_t reportID, uint8_t *report, CFIndex length)
ReportCallback = CFUNCTYPE(
    None, c_void_p, c_int32, c_void_p, c_int, c_uint32, POINTER(c_uint8), c_long
)
# void (*IOHIDCallback)(void *context, IOReturn result, void *sender)
RemovalCallback = CFUNCTYPE(None, c_void_p, c_int32, c_void_p)


def _bind(library: ctypes.CDLL, name: str, restype: Any, *argtypes: Any) -> Any:
    function = getattr(library, name)
    function.restype = restype
    function.argtypes = list(argtypes)
    return function


class _Frameworks:
    """Typed bindings for the small IOKit/CoreFoundation surface we use."""

    def __init__(self) -> None:
        cf = ctypes.CDLL(_CORE_FOUNDATION)
        iokit = ctypes.CDLL(_IOKIT)

        self.CFRelease = _bind(cf, "CFRelease", None, c_void_p)
        self.CFRetain = _bind(cf, "CFRetain", c_void_p, c_void_p)
        self.CFGetTypeID = _bind(cf, "CFGetTypeID", c_ulong, c_void_p)
        self.CFNumberGetTypeID = _bind(cf, "CFNumberGetTypeID", c_ulong)
        self.CFStringGetTypeID = _bind(cf, "CFStringGetTypeID", c_ulong)
        self.CFNumberGetValue = _bind(
            cf, "CFNumberGetValue", c_bool, c_void_p, c_long, c_void_p
        )
        self.CFStringCreateWithCString = _bind(
            cf, "CFStringCreateWithCString", c_void_p, c_void_p, c_char_p, c_uint32
        )
        self.CFStringGetCString = _bind(
            cf, "CFStringGetCString", c_bool, c_void_p, c_char_p, c_long, c_uint32
        )
        self.CFSetGetCount = _bind(cf, "CFSetGetCount", c_long, c_void_p)
        self.CFSetGetValues = _bind(
            cf, "CFSetGetValues", None, c_void_p, POINTER(c_void_p)
        )
        self.CFRunLoopGetCurrent = _bind(cf, "CFRunLoopGetCurrent", c_void_p)
        self.CFRunLoopRunInMode = _bind(
            cf, "CFRunLoopRunInMode", c_int32, c_void_p, c_double, c_bool
        )

        self.IOHIDManagerCreate = _bind(
            iokit, "IOHIDManagerCreate", c_void_p, c_void_p, c_uint32
        )
        self.IOHIDManagerSetDeviceMatching = _bind(
            iokit, "IOHIDManagerSetDeviceMatching", None, c_void_p, c_void_p
        )
        self.IOHIDManagerCopyDevices = _bind(
            iokit, "IOHIDManagerCopyDevices", c_void_p, c_void_p
        )
        self.IOHIDDeviceGetProperty = _bind(
            iokit, "IOHIDDeviceGetProperty", c_void_p, c_void_p, c_void_p
        )
        self.IOHIDDeviceGetService = _bind(
            iokit, "IOHIDDeviceGetService", c_uint32, c_void_p
        )
        self.IORegistryEntryGetRegistryEntryID = _bind(
            iokit,
            "IORegistryEntryGetRegistryEntryID",
            c_int32,
            c_uint32,
            POINTER(c_uint64),
        )
        self.IOHIDDeviceOpen = _bind(
            iokit, "IOHIDDeviceOpen", c_int32, c_void_p, c_uint32
        )
        self.IOHIDDeviceClose = _bind(
            iokit, "IOHIDDeviceClose", c_int32, c_void_p, c_uint32
        )
        self.IOHIDDeviceScheduleWithRunLoop = _bind(
            iokit, "IOHIDDeviceScheduleWithRunLoop", None, c_void_p, c_void_p, c_void_p
        )
        self.IOHIDDeviceUnscheduleFromRunLoop = _bind(
            iokit,
            "IOHIDDeviceUnscheduleFromRunLoop",
            None,
            c_void_p,
            c_void_p,
            c_void_p,
        )
        self.IOHIDDeviceRegisterInputReportCallback = _bind(
            iokit,
            "IOHIDDeviceRegisterInputReportCallback",
            None,
            c_void_p,
            POINTER(c_uint8),
            c_long,
            ReportCallback,
            c_void_p,
        )
        self.IOHIDDeviceRegisterRemovalCallback = _bind(
            iokit,
            "IOHIDDeviceRegisterRemovalCallback",
            None,
            c_void_p,
            RemovalCallback,
            c_void_p,
        )
        self.IOHIDDeviceSetReport = _bind(
            iokit,
            "IOHIDDeviceSetReport",
            c_int32,
            c_void_p,
            c_int,
            c_long,
            c_char_p,
            c_long,
        )
        # Input Monitoring helpers exist on macOS 10.15 and later.
        try:
            self.IOHIDCheckAccess = _bind(iokit, "IOHIDCheckAccess", c_int, c_int)
            self.IOHIDRequestAccess = _bind(iokit, "IOHIDRequestAccess", c_bool, c_int)
        except AttributeError:
            self.IOHIDCheckAccess = None
            self.IOHIDRequestAccess = None

        self._strings: dict[str, int] = {}
        self.run_loop_mode = self.cfstring(RUN_LOOP_MODE)

    def cfstring(self, value: str) -> int:
        """Return a cached, intentionally immortal CFString."""
        cached = self._strings.get(value)
        if cached is None:
            cached = self.CFStringCreateWithCString(
                None, value.encode(), kCFStringEncodingUTF8
            )
            if not cached:
                raise RuntimeError(f"CFStringCreateWithCString failed for {value!r}")
            self._strings[value] = cached
        return cached

    def int_property(self, device: int, key: str) -> int:
        ref = self.IOHIDDeviceGetProperty(device, self.cfstring(key))
        if not ref or self.CFGetTypeID(ref) != self.CFNumberGetTypeID():
            return 0
        value = c_int64()
        if not self.CFNumberGetValue(ref, kCFNumberSInt64Type, byref(value)):
            return 0
        return value.value

    def str_property(self, device: int, key: str) -> str:
        ref = self.IOHIDDeviceGetProperty(device, self.cfstring(key))
        if not ref or self.CFGetTypeID(ref) != self.CFStringGetTypeID():
            return ""
        buffer = ctypes.create_string_buffer(1024)
        if not self.CFStringGetCString(ref, buffer, len(buffer), kCFStringEncodingUTF8):
            return ""
        return buffer.value.decode("utf-8", errors="replace")

    def registry_id(self, device: int) -> int:
        service = self.IOHIDDeviceGetService(device)
        if not service:
            return 0
        entry_id = c_uint64()
        if self.IORegistryEntryGetRegistryEntryID(service, byref(entry_id)) != 0:
            return 0
        return entry_id.value


_frameworks: _Frameworks | None = None


def frameworks() -> _Frameworks:
    global _frameworks
    if _frameworks is None:
        _frameworks = _Frameworks()
    return _frameworks


def normalize_transport(value: str) -> str:
    """Map IOKit's Transport property onto the selector vocabulary."""
    lowered = value.casefold()
    if lowered.startswith("bluetooth"):
        return "bluetooth"
    if lowered == "usb":
        return "usb"
    return lowered or "unknown"


def normalize_report(report_id: int, data: bytes) -> bytes:
    """Ensure byte 0 is the report ID, as it is on Linux hidraw."""
    if report_id and (not data or data[0] != report_id):
        return bytes((report_id,)) + data
    return data


def _error_code(status: int) -> int:
    return status & 0xFFFFFFFF


def _device_info(fw: _Frameworks, device: int) -> DeviceInfo:
    return DeviceInfo(
        path=f"iokit:{fw.registry_id(device):x}",
        vendor_id=fw.int_property(device, "VendorID"),
        product_id=fw.int_property(device, "ProductID"),
        product=fw.str_property(device, "Product"),
        transport=normalize_transport(fw.str_property(device, "Transport")),
        serial=fw.str_property(device, "SerialNumber"),
        max_input_report=fw.int_property(device, "MaxInputReportSize"),
        max_output_report=fw.int_property(device, "MaxOutputReportSize"),
    )


def _copy_devices(fw: _Frameworks, manager: int) -> tuple[int, list[int]]:
    """Return the owned CFSet and the device refs it contains."""
    fw.IOHIDManagerSetDeviceMatching(manager, None)
    device_set = fw.IOHIDManagerCopyDevices(manager)
    if not device_set:
        return 0, []
    count = fw.CFSetGetCount(device_set)
    values = (c_void_p * count)()
    fw.CFSetGetValues(device_set, values)
    return device_set, [value for value in values if value]


def iokit_devices() -> list[DeviceInfo]:
    """Enumerate every HID device. Enumeration needs no privacy permission."""
    fw = frameworks()
    manager = fw.IOHIDManagerCreate(None, kIOHIDOptionsTypeNone)
    if not manager:
        raise RuntimeError("IOHIDManagerCreate failed")
    try:
        device_set, devices = _copy_devices(fw, manager)
        try:
            return [_device_info(fw, device) for device in devices]
        finally:
            if device_set:
                fw.CFRelease(device_set)
    finally:
        fw.CFRelease(manager)


class IOKitTransport:
    """IOHIDDevice transport serviced by the calling thread's run loop.

    Open, read, write, and close must all happen on the same thread. The
    daemon satisfies this because one worker thread owns the device.
    """

    def __init__(self, selector: DeviceSelector) -> None:
        self.selector = selector
        self.info: DeviceInfo | None = None
        self._fw: _Frameworks | None = None
        self._manager = 0
        self._device = 0
        self._run_loop = 0
        self._buffer: Any = None
        self._report_callback: Any = None
        self._removal_callback: Any = None
        self._reports: collections.deque[bytes] = collections.deque(
            maxlen=MAX_QUEUED_REPORTS
        )
        self._removed = False
        self._opened = False

    def _request_input_monitoring(self, fw: _Frameworks) -> None:
        """Trigger the one-time Input Monitoring prompt when undecided."""
        if fw.IOHIDCheckAccess is None or fw.IOHIDRequestAccess is None:
            return
        if fw.IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeUnknown:
            fw.IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)

    def open(self) -> None:
        fw = frameworks()
        self._fw = fw
        self._reports.clear()
        self._removed = False
        self._request_input_monitoring(fw)

        manager = fw.IOHIDManagerCreate(None, kIOHIDOptionsTypeNone)
        if not manager:
            raise RuntimeError("IOHIDManagerCreate failed")
        self._manager = manager
        try:
            device_set, devices = _copy_devices(fw, manager)
            try:
                entries = [(_device_info(fw, device), device) for device in devices]
                chosen = self.selector.select([info for info, _ in entries])
                device = next(ref for info, ref in entries if info == chosen)
                self._device = fw.CFRetain(device)
                self.info = chosen
            finally:
                if device_set:
                    fw.CFRelease(device_set)

            status = fw.IOHIDDeviceOpen(self._device, kIOHIDOptionsTypeNone)
            if status != 0:
                code = _error_code(status)
                if code == kIOReturnNotPermitted:
                    raise PermissionError(INPUT_MONITORING_HELP)
                if code == kIOReturnExclusiveAccess:
                    raise OSError(
                        "another program has exclusive access to the Codex Micro"
                    )
                raise OSError(f"IOHIDDeviceOpen failed: 0x{code:08x}")
            self._opened = True

            size = max(chosen.max_input_report, REPORT_SIZE)
            self._buffer = (c_uint8 * size)()
            self._report_callback = ReportCallback(self._on_report)
            self._removal_callback = RemovalCallback(self._on_removal)
            self._run_loop = fw.CFRunLoopGetCurrent()
            fw.IOHIDDeviceRegisterInputReportCallback(
                self._device, self._buffer, size, self._report_callback, None
            )
            fw.IOHIDDeviceRegisterRemovalCallback(
                self._device, self._removal_callback, None
            )
            fw.IOHIDDeviceScheduleWithRunLoop(
                self._device, self._run_loop, fw.run_loop_mode
            )
        except BaseException:
            self.close()
            raise

    def _on_report(
        self,
        _context: Any,
        _result: int,
        _sender: Any,
        _report_type: int,
        report_id: int,
        report: Any,
        length: int,
    ) -> None:
        try:
            data = ctypes.string_at(report, length) if length > 0 else b""
            self._reports.append(normalize_report(report_id, data))
        except Exception:  # never propagate into IOKit
            pass

    def _on_removal(self, _context: Any, _result: int, _sender: Any) -> None:
        self._removed = True

    def read(self, timeout: float) -> bytes | None:
        fw = self._fw
        assert fw is not None and self._device
        deadline = time.monotonic() + max(timeout, 0.0)
        while True:
            if self._reports:
                return self._reports.popleft()
            if self._removed:
                raise OSError("Codex Micro disconnected")
            remaining = max(deadline - time.monotonic(), 0.0)
            status = fw.CFRunLoopRunInMode(fw.run_loop_mode, remaining, True)
            if self._reports or self._removed:
                continue
            if status == kCFRunLoopRunFinished:
                raise OSError("HID run loop has no sources; device was removed")
            if time.monotonic() >= deadline:
                return None

    def write(self, report: bytes) -> None:
        fw = self._fw
        assert fw is not None and self._device
        if self._removed:
            raise OSError("Codex Micro disconnected")
        status = fw.IOHIDDeviceSetReport(
            self._device, kIOHIDReportTypeOutput, report[0], report, len(report)
        )
        if status != 0:
            if self._removed:
                raise OSError("Codex Micro disconnected")
            raise OSError(f"IOHIDDeviceSetReport failed: 0x{_error_code(status):08x}")

    def close(self) -> None:
        fw = self._fw
        if fw is None:
            return
        if self._device:
            if self._opened and not self._removed:
                if self._report_callback is not None and self._buffer is not None:
                    fw.IOHIDDeviceRegisterInputReportCallback(
                        self._device,
                        self._buffer,
                        len(self._buffer),
                        ReportCallback(),
                        None,
                    )
                    fw.IOHIDDeviceRegisterRemovalCallback(
                        self._device, RemovalCallback(), None
                    )
                if self._run_loop:
                    fw.IOHIDDeviceUnscheduleFromRunLoop(
                        self._device, self._run_loop, fw.run_loop_mode
                    )
                fw.IOHIDDeviceClose(self._device, kIOHIDOptionsTypeNone)
            fw.CFRelease(self._device)
            self._device = 0
        if self._manager:
            fw.CFRelease(self._manager)
            self._manager = 0
        self._opened = False
        self._run_loop = 0
        self._reports.clear()
