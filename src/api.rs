//! JSONL Unix-socket API. One thread per client.

use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use serde_json::{json, Map, Value};

use crate::hub::Hub;
use crate::protocol::{agent_payload, zone_payload};

pub fn default_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CODEX_MICROD_SOCKET").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    if cfg!(target_os = "macos") {
        return crate::home_dir().join("Library/Application Support/codex-micro/codex-microd.sock");
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    runtime.join("codex-microd.sock")
}

/// Remove a stale socket, refusing to touch anything else or a live daemon.
fn ensure_socket_available(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !metadata.file_type().is_socket() {
        return Err(format!(
            "refusing to replace non-socket path: {}",
            path.display()
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(format!(
            "another daemon is already listening at {}",
            path.display()
        )),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Bind the user-only (`0600`) daemon socket.
pub fn bind(path: &Path) -> Result<UnixListener, String> {
    ensure_socket_available(path)?;
    let listener = UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(listener)
}

pub fn serve(listener: UnixListener, hub: Arc<Hub>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let hub = hub.clone();
                thread::spawn(move || handle_client(stream, &hub));
            }
            Err(error) => crate::log!("accept failed: {error}"),
        }
    }
}

fn send(stream: &mut UnixStream, value: &Value) -> io::Result<()> {
    let mut line = value.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes())
}

fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

enum Reply {
    Result(Value),
    Subscribe(Option<HashSet<String>>),
}

fn parse_request(line: &str, request_id: &mut Value, hub: &Hub) -> Result<Reply, String> {
    let request: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let Value::Object(request) = request else {
        return Err("request must be a JSON object".into());
    };
    *request_id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(Value::String(method)) = request.get("method") else {
        return Err("method must be a string".into());
    };
    let params = match request.get("params") {
        Some(Value::Object(params)) => params.clone(),
        Some(value) if !is_falsy(value) => return Err("params must be an object".into()),
        _ => Map::new(),
    };
    if method == "events.subscribe" {
        return subscription_filters(&params).map(Reply::Subscribe);
    }
    dispatch(hub, method, &params).map(Reply::Result)
}

pub fn handle_client(stream: UnixStream, hub: &Arc<Hub>) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            return;
        };
        let mut request_id = Value::Null;
        let reply = match parse_request(&line, &mut request_id, hub) {
            Ok(Reply::Subscribe(filters)) => {
                return stream_events(writer, hub, request_id, filters)
            }
            Ok(Reply::Result(result)) => json!({"id": request_id, "result": result}),
            Err(error) => json!({"id": request_id, "error": {"message": error}}),
        };
        if send(&mut writer, &reply).is_err() {
            return;
        }
    }
}

fn subscription_filters(params: &Map<String, Value>) -> Result<Option<HashSet<String>>, String> {
    match params.get("events") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(names)) => names
            .iter()
            .map(|n| n.as_str().map(str::to_string))
            .collect::<Option<HashSet<_>>>()
            .map(Some)
            .ok_or_else(|| "events must be an array of event names".into()),
        Some(_) => Err("events must be an array of event names".into()),
    }
}

fn stream_events(
    mut writer: UnixStream,
    hub: &Arc<Hub>,
    request_id: Value,
    filters: Option<HashSet<String>>,
) {
    let subscriber = hub.subscribe(filters);
    if send(
        &mut writer,
        &json!({"id": request_id, "result": {"subscribed": true}}),
    )
    .is_ok()
    {
        loop {
            let line = subscriber.next();
            let written = writer
                .write_all(line.as_bytes())
                .and_then(|_| writer.write_all(b"\n"));
            if written.is_err() {
                break;
            }
        }
    }
    hub.unsubscribe(&subscriber);
}

pub fn dispatch(hub: &Hub, method: &str, params: &Map<String, Value>) -> Result<Value, String> {
    match method {
        "device.status" => {
            let mut result = hub.rpc("device.status", None)?;
            if let (Value::Object(object), Some(info)) = (&mut result, hub.device_info()) {
                object.insert("connection".into(), info.describe());
            }
            Ok(result)
        }
        "lighting.agent.set" => {
            let agent = agent_payload(&Value::Object(params.clone()))?;
            hub.rpc("v.oai.thstatus", Some(json!([agent])))
        }
        "lighting.agents.set" => {
            let Some(Value::Array(agents)) = params.get("agents").filter(|a| !is_falsy(a)) else {
                return Err("agents must be a non-empty array".into());
            };
            let agents = agents
                .iter()
                .map(agent_payload)
                .collect::<Result<Vec<_>, _>>()?;
            hub.rpc("v.oai.thstatus", Some(Value::Array(agents)))
        }
        "lighting.zones.set" => {
            let (Some(Value::Object(keys)), Some(Value::Object(ambient))) =
                (params.get("keys"), params.get("ambient"))
            else {
                return Err("keys and ambient must both be objects".into());
            };
            hub.rpc(
                "v.oai.rgbcfg",
                Some(json!({"keys": zone_payload(keys)?, "ambient": zone_payload(ambient)?})),
            )
        }
        _ => Err(format!("unknown method: {method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mappings;
    use crate::events::JoystickSectors;
    use crate::hub::tests::{device_reports, FakeLink};
    use crate::hub::Mapper;
    use std::io::BufRead;
    use std::time::Duration;

    struct Client {
        reader: BufReader<UnixStream>,
        writer: UnixStream,
    }

    impl Client {
        fn connect(path: &Path) -> Self {
            let stream = UnixStream::connect(path).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            Self {
                writer: stream.try_clone().unwrap(),
                reader: BufReader::new(stream),
            }
        }

        fn send(&mut self, line: &str) -> Value {
            writeln!(self.writer, "{line}").unwrap();
            self.read()
        }

        fn read(&mut self) -> Value {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        }
    }

    fn server() -> (Arc<Hub>, Arc<FakeLink>, PathBuf, tempdir::TempDir) {
        let dir = tempdir::TempDir::new();
        let path = dir.0.join("nested/codex-microd.sock");
        let listener = bind(&path).unwrap();
        let hub = Hub::new(
            Mapper::new(Mappings::new()),
            JoystickSectors::default(),
            None,
        );
        let link = FakeLink::new(|request| {
            let result = match request["method"].as_str() {
                Some("device.status") => json!({"version": "v0.4.1", "battery": 80}),
                _ => request["params"].clone(),
            };
            vec![json!({"id": request["id"], "result": result})]
        });
        link.attach(&hub);
        let serving = hub.clone();
        thread::spawn(move || serve(listener, serving));
        (hub, link, path, dir)
    }

    mod tempdir {
        pub struct TempDir(pub std::path::PathBuf);

        impl TempDir {
            pub fn new() -> Self {
                use std::sync::atomic::{AtomicU32, Ordering};
                static COUNTER: AtomicU32 = AtomicU32::new(0);
                let path = std::env::temp_dir().join(format!(
                    "codex-micro-test-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::SeqCst)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn socket_is_user_only_and_refuses_live_daemon() {
        let (_hub, _link, path, _dir) = server();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let parent = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(parent & 0o777, 0o700);
        assert!(bind(&path).unwrap_err().contains("already listening"));
    }

    #[test]
    fn stale_socket_is_replaced_but_files_are_not() {
        let dir = tempdir::TempDir::new();
        let path = dir.0.join("d.sock");
        drop(UnixListener::bind(&path).unwrap()); // leaves a dead socket file
        assert!(bind(&path).is_ok());

        let file = dir.0.join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(bind(&file).unwrap_err().contains("non-socket"));
    }

    #[test]
    fn status_includes_connection() {
        let (_hub, _link, path, _dir) = server();
        let reply = Client::connect(&path).send(r#"{"id":1,"method":"device.status"}"#);
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["battery"], 80);
        assert_eq!(reply["result"]["connection"]["transport"], "bluetooth");
        assert_eq!(reply["result"]["connection"]["vendor_id"], "303a");
    }

    #[test]
    fn lighting_methods_send_validated_firmware_payloads() {
        let (_hub, link, path, _dir) = server();
        let mut client = Client::connect(&path);
        let agent = client.send(
            r##"{"id":2,"method":"lighting.agent.set","params":{"id":0,"color":"#ff8800","effect":"breath"}}"##,
        );
        assert_eq!(
            agent["result"],
            json!([{"id":0,"c":0xff8800,"b":1,"e":4,"s":0.4,"sk":0,"sa":0}])
        );

        let zones = client.send(
            r##"{"id":3,"method":"lighting.zones.set","params":{"keys":{"color":"#ffffff"},"ambient":{"color":255,"effect":"snake"}}}"##,
        );
        assert_eq!(
            zones["result"]["ambient"],
            json!({"e":2,"b":1,"s":0.4,"m":0,"c":255})
        );

        let requests = link.requests.lock().unwrap();
        let methods: Vec<&str> = requests
            .iter()
            .filter_map(|r| r["method"].as_str())
            .collect();
        // The connect-time replay may also appear; it re-sends current state.
        assert!(
            methods.contains(&"v.oai.thstatus") && methods.contains(&"v.oai.rgbcfg"),
            "{methods:?}"
        );
    }

    #[test]
    fn errors_preserve_request_id() {
        let (_hub, _link, path, _dir) = server();
        let mut client = Client::connect(&path);
        for (line, message) in [
            (r#"{"id":4,"method":"nope"}"#, "unknown method: nope"),
            (
                r##"{"id":5,"method":"lighting.agent.set","params":{"id":9,"color":"#fff"}}"##,
                "agent id must be from 0 through 5",
            ),
            (
                r#"{"id":6,"method":"lighting.agents.set","params":{"agents":[]}}"#,
                "agents must be a non-empty array",
            ),
            (
                r#"{"id":7,"method":"device.status","params":[1]}"#,
                "params must be an object",
            ),
            (r#"{"id":8,"method":3}"#, "method must be a string"),
            ("[1]", "request must be a JSON object"),
        ] {
            let reply = client.send(line);
            assert_eq!(reply["error"]["message"], message, "{line}");
        }
        assert_eq!(client.send(r#"{"id":9,"method":"nope"}"#)["id"], 9);
    }

    #[test]
    fn subscribers_receive_filtered_events() {
        let (hub, _link, path, _dir) = server();
        let mut client = Client::connect(&path);
        let ack =
            client.send(r#"{"id":10,"method":"events.subscribe","params":{"events":["key"]}}"#);
        assert_eq!(ack, json!({"id": 10, "result": {"subscribed": true}}));

        for message in [
            json!({"m": "v.oai.rad", "p": {"a": 0.1, "d": 0.1}}),
            json!({"m": "v.oai.hid", "p": {"k": "ACT09", "act": 1}}),
        ] {
            for report in device_reports(&message) {
                hub.on_report(&report);
            }
        }
        let event = client.read();
        assert_eq!(event["event"], "key");
        assert_eq!(event["data"]["key"], "ACT09");
    }
}
