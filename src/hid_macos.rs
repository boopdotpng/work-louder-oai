//! macOS IOKit backend.
//!
//! IOKit presents USB, Bluetooth, and Bluetooth LE HID devices through one
//! IOHIDDevice API. A dedicated thread runs a CFRunLoop that delivers input
//! reports and removal notices as they happen, so the daemon is idle between
//! events. A repeating timer scans for the keyboard only while disconnected.
//!
//! The device is opened without seizing it, so its normal keyboard, consumer,
//! and pointer reports keep reaching macOS. Opening a keyboard-class device
//! requires the Input Monitoring privacy permission.

use std::ffi::{c_char, c_void, CString};
use std::io;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::device::{DeviceInfo, Link, Selector};
use crate::hub::Hub;
use crate::protocol::REPORT_SIZE;

type CFTypeRef = *const c_void;
type CFIndex = isize;
type IOReturn = i32;
type Boolean = u8;

#[repr(C)]
struct CFRunLoopTimerContext {
    version: CFIndex,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

type ReportCallback = extern "C" fn(
    context: *mut c_void,
    result: IOReturn,
    sender: *mut c_void,
    report_type: u32,
    report_id: u32,
    report: *mut u8,
    length: CFIndex,
);
type RemovalCallback = extern "C" fn(context: *mut c_void, result: IOReturn, sender: *mut c_void);
type TimerCallback = extern "C" fn(timer: *mut c_void, info: *mut c_void);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetValue(number: CFTypeRef, kind: CFIndex, value: *mut c_void) -> Boolean;
    fn CFStringCreateWithCString(alloc: CFTypeRef, text: *const c_char, encoding: u32)
        -> CFTypeRef;
    fn CFStringGetCString(
        text: CFTypeRef,
        buffer: *mut c_char,
        size: CFIndex,
        encoding: u32,
    ) -> Boolean;
    fn CFSetGetCount(set: CFTypeRef) -> CFIndex;
    fn CFSetGetValues(set: CFTypeRef, values: *mut CFTypeRef);
    fn CFAbsoluteTimeGetCurrent() -> f64;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopRun();
    fn CFRunLoopAddTimer(run_loop: *mut c_void, timer: *mut c_void, mode: CFTypeRef);
    fn CFRunLoopTimerCreate(
        alloc: CFTypeRef,
        fire_date: f64,
        interval: f64,
        flags: usize,
        order: CFIndex,
        callout: TimerCallback,
        context: *mut CFRunLoopTimerContext,
    ) -> *mut c_void;
    fn CFRunLoopTimerSetNextFireDate(timer: *mut c_void, fire_date: f64);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDManagerCreate(alloc: CFTypeRef, options: u32) -> *mut c_void;
    fn IOHIDManagerSetDeviceMatching(manager: *mut c_void, matching: CFTypeRef);
    fn IOHIDManagerCopyDevices(manager: *mut c_void) -> CFTypeRef;
    fn IOHIDDeviceGetProperty(device: *mut c_void, key: CFTypeRef) -> CFTypeRef;
    fn IOHIDDeviceGetService(device: *mut c_void) -> u32;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    fn IOHIDDeviceOpen(device: *mut c_void, options: u32) -> IOReturn;
    fn IOHIDDeviceScheduleWithRunLoop(device: *mut c_void, run_loop: *mut c_void, mode: CFTypeRef);
    fn IOHIDDeviceUnscheduleFromRunLoop(
        device: *mut c_void,
        run_loop: *mut c_void,
        mode: CFTypeRef,
    );
    fn IOHIDDeviceRegisterInputReportCallback(
        device: *mut c_void,
        report: *mut u8,
        length: CFIndex,
        callback: Option<ReportCallback>,
        context: *mut c_void,
    );
    fn IOHIDDeviceRegisterRemovalCallback(
        device: *mut c_void,
        callback: Option<RemovalCallback>,
        context: *mut c_void,
    );
    fn IOHIDDeviceSetReport(
        device: *mut c_void,
        report_type: u32,
        report_id: CFIndex,
        report: *const u8,
        length: CFIndex,
    ) -> IOReturn;
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}

const UTF8: u32 = 0x0800_0100;
const NUMBER_SINT64: CFIndex = 4;
const REPORT_TYPE_OUTPUT: u32 = 1;
const REQUEST_LISTEN_EVENT: u32 = 1;
const ACCESS_UNKNOWN: u32 = 2;
const RETURN_NOT_PERMITTED: u32 = 0xe000_02e2;
const RETURN_EXCLUSIVE_ACCESS: u32 = 0xe000_02c5;
const SCAN_INTERVAL: f64 = 2.0;
const NEVER: f64 = 1.0e12;

pub const INPUT_MONITORING_HELP: &str = "macOS denied access to the Codex Micro. Allow \
    Input Monitoring for codex-micro (or the terminal running it) in System Settings > \
    Privacy & Security > Input Monitoring, then restart it.";

/// Map IOKit's `Transport` property onto the selector vocabulary.
pub fn normalize_transport(value: &str) -> String {
    let lowered = value.to_lowercase();
    if lowered.starts_with("bluetooth") {
        "bluetooth".into()
    } else if lowered.is_empty() {
        "unknown".into()
    } else {
        lowered
    }
}

/// Ensure byte 0 is the report ID, matching the hidraw layout.
pub fn normalize_report(report_id: u32, data: &[u8]) -> Vec<u8> {
    match u8::try_from(report_id) {
        Ok(id) if id != 0 && data.first() != Some(&id) => {
            let mut report = Vec::with_capacity(data.len() + 1);
            report.push(id);
            report.extend_from_slice(data);
            report
        }
        _ => data.to_vec(),
    }
}

struct CfString(CFTypeRef);

impl CfString {
    fn new(text: &str) -> Self {
        let text = CString::new(text).expect("no NUL in property name");
        Self(unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), UTF8) })
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

unsafe fn int_property(device: *mut c_void, key: &str) -> u32 {
    let key = CfString::new(key);
    let value = IOHIDDeviceGetProperty(device, key.0);
    if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
        return 0;
    }
    let mut number: i64 = 0;
    if CFNumberGetValue(value, NUMBER_SINT64, (&mut number as *mut i64).cast()) == 0 {
        return 0;
    }
    u32::try_from(number).unwrap_or(0)
}

unsafe fn string_property(device: *mut c_void, key: &str) -> String {
    let key = CfString::new(key);
    let value = IOHIDDeviceGetProperty(device, key.0);
    if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
        return String::new();
    }
    let mut buffer = [0 as c_char; 1024];
    if CFStringGetCString(value, buffer.as_mut_ptr(), buffer.len() as CFIndex, UTF8) == 0 {
        return String::new();
    }
    std::ffi::CStr::from_ptr(buffer.as_ptr())
        .to_string_lossy()
        .into_owned()
}

unsafe fn device_info(device: *mut c_void) -> DeviceInfo {
    let mut registry_id: u64 = 0;
    let service = IOHIDDeviceGetService(device);
    if service == 0 || IORegistryEntryGetRegistryEntryID(service, &mut registry_id) != 0 {
        registry_id = 0;
    }
    DeviceInfo {
        path: format!("iokit:{registry_id:x}"),
        vendor_id: int_property(device, "VendorID"),
        product_id: int_property(device, "ProductID"),
        product: string_property(device, "Product"),
        transport: normalize_transport(&string_property(device, "Transport")),
        serial: string_property(device, "SerialNumber"),
        max_input_report: int_property(device, "MaxInputReportSize"),
        max_output_report: int_property(device, "MaxOutputReportSize"),
    }
}

/// A fresh HID manager and the devices it currently sees.
struct Snapshot {
    manager: *mut c_void,
    set: CFTypeRef,
    devices: Vec<*mut c_void>,
}

impl Snapshot {
    unsafe fn take() -> Result<Self, String> {
        let manager = IOHIDManagerCreate(ptr::null(), 0);
        if manager.is_null() {
            return Err("IOHIDManagerCreate failed".into());
        }
        IOHIDManagerSetDeviceMatching(manager, ptr::null());
        let set = IOHIDManagerCopyDevices(manager);
        let mut devices = Vec::new();
        if !set.is_null() {
            let count = CFSetGetCount(set).max(0) as usize;
            let mut values: Vec<CFTypeRef> = vec![ptr::null(); count];
            CFSetGetValues(set, values.as_mut_ptr());
            devices = values
                .into_iter()
                .filter(|v| !v.is_null())
                .map(|v| v as *mut c_void)
                .collect();
        }
        Ok(Self {
            manager,
            set,
            devices,
        })
    }

    /// Keep the manager alive past the snapshot (for an open device).
    fn into_manager(mut self) -> *mut c_void {
        std::mem::replace(&mut self.manager, ptr::null_mut())
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        unsafe {
            if !self.set.is_null() {
                CFRelease(self.set);
            }
            if !self.manager.is_null() {
                CFRelease(self.manager);
            }
        }
    }
}

pub fn enumerate() -> Result<Vec<DeviceInfo>, String> {
    unsafe {
        let snapshot = Snapshot::take()?;
        Ok(snapshot.devices.iter().map(|d| device_info(*d)).collect())
    }
}

/// The open device. Shared with links so writes from client threads never
/// race a removal on the run-loop thread.
struct OpenDevice {
    device: *mut c_void,
    manager: *mut c_void,
    _buffer: Box<[u8]>,
    generation: u64,
}

// SAFETY: IOHIDDevice/IOHIDManager are CF objects usable from any thread;
// access is serialized by the surrounding Mutex.
unsafe impl Send for OpenDevice {}

#[derive(Default)]
struct Slots {
    current: Option<OpenDevice>,
    /// Removed devices, released outside their own callbacks.
    removed: Vec<OpenDevice>,
}

struct MacLink {
    slots: Arc<Mutex<Slots>>,
    generation: u64,
    info: DeviceInfo,
}

impl Link for MacLink {
    fn write(&self, report: &[u8]) -> io::Result<()> {
        let slots = self.slots.lock().unwrap();
        let Some(open) = slots
            .current
            .as_ref()
            .filter(|o| o.generation == self.generation)
        else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Codex Micro disconnected",
            ));
        };
        let status = unsafe {
            IOHIDDeviceSetReport(
                open.device,
                REPORT_TYPE_OUTPUT,
                report[0] as CFIndex,
                report.as_ptr(),
                report.len() as CFIndex,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "IOHIDDeviceSetReport failed: 0x{:08x}",
                status as u32
            )));
        }
        Ok(())
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }
}

/// Lives for the life of the process; its address is the callback context.
struct Backend {
    hub: Arc<Hub>,
    selector: Selector,
    slots: Arc<Mutex<Slots>>,
    run_loop: *mut c_void,
    timer: *mut c_void,
    generation: u64,
}

impl Backend {
    unsafe fn release_removed(&self) {
        let removed = std::mem::take(&mut self.slots.lock().unwrap().removed);
        for open in removed {
            IOHIDDeviceUnscheduleFromRunLoop(open.device, self.run_loop, kCFRunLoopDefaultMode);
            CFRelease(open.device);
            CFRelease(open.manager);
        }
    }

    unsafe fn connect(&mut self, context: *mut c_void) -> Result<MacLink, String> {
        let snapshot = Snapshot::take()?;
        let infos: Vec<DeviceInfo> = snapshot.devices.iter().map(|d| device_info(*d)).collect();
        let index = self.selector.select(&infos)?;
        let device = snapshot.devices[index];
        let info = infos[index].clone();

        let status = IOHIDDeviceOpen(device, 0);
        if status != 0 {
            return Err(match status as u32 {
                RETURN_NOT_PERMITTED => INPUT_MONITORING_HELP.into(),
                RETURN_EXCLUSIVE_ACCESS => {
                    "another program has exclusive access to the Codex Micro".into()
                }
                code => format!("IOHIDDeviceOpen failed: 0x{code:08x}"),
            });
        }
        CFRetain(device);
        let manager = snapshot.into_manager();

        let size = (info.max_input_report as usize).max(REPORT_SIZE);
        let mut buffer = vec![0u8; size].into_boxed_slice();
        IOHIDDeviceRegisterInputReportCallback(
            device,
            buffer.as_mut_ptr(),
            size as CFIndex,
            Some(on_report),
            context,
        );
        IOHIDDeviceRegisterRemovalCallback(device, Some(on_removal), context);
        IOHIDDeviceScheduleWithRunLoop(device, self.run_loop, kCFRunLoopDefaultMode);

        self.generation += 1;
        self.slots.lock().unwrap().current = Some(OpenDevice {
            device,
            manager,
            _buffer: buffer,
            generation: self.generation,
        });
        Ok(MacLink {
            slots: self.slots.clone(),
            generation: self.generation,
            info,
        })
    }
}

extern "C" fn on_scan_timer(_timer: *mut c_void, info: *mut c_void) {
    // SAFETY: `info` is the leaked Backend; only the run-loop thread uses it.
    let backend = unsafe { &mut *(info as *mut Backend) };
    unsafe {
        backend.release_removed();
        if backend.slots.lock().unwrap().current.is_some() {
            CFRunLoopTimerSetNextFireDate(backend.timer, CFAbsoluteTimeGetCurrent() + NEVER);
            return;
        }
        match backend.connect(info) {
            Ok(link) => {
                CFRunLoopTimerSetNextFireDate(backend.timer, CFAbsoluteTimeGetCurrent() + NEVER);
                backend.hub.connected(Arc::new(link));
            }
            Err(error) => backend.hub.disconnected(&error),
        }
    }
}

extern "C" fn on_report(
    context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
    _report_type: u32,
    report_id: u32,
    report: *mut u8,
    length: CFIndex,
) {
    let backend = unsafe { &*(context as *const Backend) };
    if report.is_null() || length <= 0 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(report, length as usize) };
    backend.hub.on_report(&normalize_report(report_id, data));
}

extern "C" fn on_removal(context: *mut c_void, _result: IOReturn, _sender: *mut c_void) {
    let backend = unsafe { &*(context as *const Backend) };
    {
        let mut slots = backend.slots.lock().unwrap();
        if let Some(open) = slots.current.take() {
            slots.removed.push(open);
        }
    }
    backend.hub.disconnected("Codex Micro disconnected");
    unsafe { CFRunLoopTimerSetNextFireDate(backend.timer, CFAbsoluteTimeGetCurrent() + 0.5) };
}

pub fn start(hub: Arc<Hub>, selector: Selector) {
    thread::Builder::new()
        .name("codex-micro-hid".into())
        .spawn(move || unsafe {
            if IOHIDCheckAccess(REQUEST_LISTEN_EVENT) == ACCESS_UNKNOWN {
                IOHIDRequestAccess(REQUEST_LISTEN_EVENT);
            }
            let backend = Box::into_raw(Box::new(Backend {
                hub,
                selector,
                slots: Arc::default(),
                run_loop: CFRunLoopGetCurrent(),
                timer: ptr::null_mut(),
                generation: 0,
            }));
            let mut context = CFRunLoopTimerContext {
                version: 0,
                info: backend.cast(),
                retain: ptr::null(),
                release: ptr::null(),
                copy_description: ptr::null(),
            };
            let timer = CFRunLoopTimerCreate(
                ptr::null(),
                CFAbsoluteTimeGetCurrent(),
                SCAN_INTERVAL,
                0,
                0,
                on_scan_timer,
                &mut context,
            );
            (*backend).timer = timer;
            CFRunLoopAddTimer((*backend).run_loop, timer, kCFRunLoopDefaultMode);
            CFRunLoopRun();
        })
        .expect("spawn HID thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_names() {
        assert_eq!(normalize_transport("Bluetooth Low Energy"), "bluetooth");
        assert_eq!(normalize_transport("Bluetooth"), "bluetooth");
        assert_eq!(normalize_transport("USB"), "usb");
        assert_eq!(normalize_transport(""), "unknown");
    }

    #[test]
    fn report_id_is_byte_zero() {
        assert_eq!(normalize_report(6, &[6, 2, 0]), [6, 2, 0]);
        assert_eq!(normalize_report(6, &[2, 0]), [6, 2, 0]);
        assert_eq!(normalize_report(0, &[2, 0]), [2, 0]);
    }

    #[test]
    fn enumeration_loads_iokit() {
        assert!(enumerate().is_ok());
    }
}
