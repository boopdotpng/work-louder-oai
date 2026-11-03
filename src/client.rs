//! Socket client commands and direct-device probe commands.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::Mappings;
use crate::device::{self, Selector};
use crate::events::JoystickSectors;
use crate::hub::{Hub, Mapper, ReportTap};
use crate::protocol::{Decoder, LOG_CHANNEL, MAX_PAYLOAD, REPORT_ID, RPC_CHANNEL};

pub const DEFAULT_EVENTS: [&str; 5] = ["key", "encoder", "joystick", "joystick.sector", "device"];
const DESTRUCTIVE_METHODS: [&str; 4] = ["sys.bootloader", "fs.delete", "fs.write", "fs.writebin"];

fn connect(socket: &Path) -> Result<UnixStream, String> {
    UnixStream::connect(socket).map_err(|e| {
        format!(
            "{}: {e} (is the codex-micro daemon running?)",
            socket.display()
        )
    })
}

pub fn request(socket: &Path, method: &str, params: Option<Value>) -> Result<Value, String> {
    let mut message = json!({"id": 1, "method": method});
    if let Some(params) = params {
        message["params"] = params;
    }
    let mut stream = connect(socket)?;
    writeln!(stream, "{message}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.is_empty() {
        return Err("daemon closed the socket without a response".into());
    }
    let mut response: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if let Some(error) = response.get("error") {
        return Err(error["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string()));
    }
    Ok(response["result"].take())
}

pub fn events(socket: &Path, names: &[String]) -> Result<(), String> {
    let names: Vec<String> = if names.is_empty() {
        DEFAULT_EVENTS.iter().map(|s| s.to_string()).collect()
    } else {
        names.to_vec()
    };
    let mut stream = connect(socket)?;
    let message = json!({"id": 1, "method": "events.subscribe", "params": {"events": names}});
    writeln!(stream, "{message}").map_err(|e| e.to_string())?;
    let stdout = std::io::stdout();
    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if let Some(error) = value.get("error") {
            return Err(error["message"]
                .as_str()
                .unwrap_or("subscription failed")
                .to_string());
        }
        if value.get("event").is_some() {
            let mut out = stdout.lock();
            if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                return Ok(()); // stdout closed, e.g. piped into head
            }
        }
    }
    Ok(())
}

// ----- direct device access ----------------------------------------------

pub fn probe_list(selector: &Selector, all: bool) -> Result<(), String> {
    for info in device::enumerate()? {
        let matches = selector.matches(&info);
        if all || matches {
            let mut line = info.describe();
            line["codex_micro"] = matches.into();
            println!("{line}");
        }
    }
    Ok(())
}

fn open_direct(selector: Selector, tap: Option<ReportTap>) -> Result<Arc<Hub>, String> {
    let hub = Hub::new(
        Mapper::new(Mappings::new()),
        JoystickSectors {
            sectors: 0,
            ..Default::default()
        },
        tap,
    );
    device::start(hub.clone(), selector);
    hub.wait_for_connection(Duration::from_secs(10))?;
    Ok(hub)
}

pub fn probe_rpc(
    selector: Selector,
    method: &str,
    params: Option<Value>,
    allow_destructive: bool,
) -> Result<Value, String> {
    if DESTRUCTIVE_METHODS.contains(&method) && !allow_destructive {
        return Err(format!(
            "{method} is persistent or destructive; back up keymap.json and pass --unsafe to call it"
        ));
    }
    open_direct(selector, None)?.rpc(method, params)
}

/// Render one raw input report as a JSON line.
pub fn describe_report(decoder: &mut Decoder, report: &[u8]) -> Vec<Value> {
    if report.len() >= 3 && report[0] == REPORT_ID && report[1] == LOG_CHANNEL {
        let size = (report[2] as usize).min(MAX_PAYLOAD).min(report.len() - 3);
        return vec![json!({"log": String::from_utf8_lossy(&report[3..3 + size])})];
    }
    if report.len() >= 3 && report[0] == REPORT_ID && report[1] == RPC_CHANNEL {
        let payload = crate::protocol::rpc_payload(report).unwrap_or_default();
        return match decoder.push(payload) {
            Ok(messages) => messages
                .into_iter()
                .map(|m| json!({"rpc": Value::Object(m)}))
                .collect(),
            Err(error) => vec![json!({"error": error})],
        };
    }
    let hex: Vec<String> = report.iter().map(|b| format!("{b:02x}")).collect();
    vec![json!({"report": report.first(), "hex": hex.join(" ")})]
}

pub fn probe_monitor(selector: Selector) -> Result<(), String> {
    let decoder = Mutex::new(Decoder::default());
    let tap: ReportTap = Box::new(move |report| {
        let mut decoder = decoder.lock().unwrap();
        for line in describe_report(&mut decoder, report) {
            println!("{line}");
        }
    });
    let hub = open_direct(selector, Some(tap))?;
    if let Some(info) = hub.device_info() {
        println!("{}", json!({"connected": info.describe()}));
    }
    eprintln!("monitoring; press Ctrl-C to stop");
    loop {
        std::thread::park();
    }
}

pub fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_describes_each_report_kind() {
        let mut decoder = Decoder::default();
        assert_eq!(
            describe_report(&mut decoder, b"\x06\x01\x05hello"),
            [json!({"log": "hello"})]
        );
        assert_eq!(
            describe_report(&mut decoder, &[1, 0, 4]),
            [json!({"report": 1, "hex": "01 00 04"})]
        );
        assert_eq!(
            describe_report(&mut decoder, b"\x06\x02\x10{\"m\":\"x\",\"p\":{}}"),
            [json!({"rpc": {"m": "x", "p": {}}})]
        );
    }

    #[test]
    fn destructive_methods_need_unsafe() {
        let error = probe_rpc(Selector::default(), "fs.write", None, false).unwrap_err();
        assert!(error.contains("--unsafe"), "{error}");
    }
}
