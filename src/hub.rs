//! Shared daemon state between the HID backend and socket clients.
//!
//! The backend thread calls `connected`, `on_report`, and `disconnected`.
//! Client threads call `rpc`, which serializes firmware calls with one lock
//! and waits for the backend to route the matching response back.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::config::Mappings;
use crate::device::{DeviceInfo, Link};
use crate::events::{JoystickSectors, Normalizer};
use crate::log;
use crate::protocol::{encode_rpc, rpc_payload, Decoder};

pub const RPC_TIMEOUT: Duration = Duration::from_secs(3);
const SUBSCRIBER_QUEUE: usize = 512;
const MAPPABLE_EVENTS: [&str; 3] = ["key", "encoder", "joystick.sector"];

pub type Spawner = Box<dyn Fn(&[String]) -> std::io::Result<()> + Send + Sync>;
pub type ReportTap = Box<dyn Fn(&[u8]) + Send + Sync>;

/// Run an argv directly, without a shell, in its own process group.
pub fn spawn_detached(argv: &[String]) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    // Reap the child so it does not linger as a zombie.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Executes configured argv mappings for normalized control events.
pub struct Mapper {
    mappings: Mappings,
    spawn: Spawner,
}

impl Mapper {
    pub fn new(mappings: Mappings) -> Self {
        Self::with_spawner(mappings, Box::new(spawn_detached))
    }

    pub fn with_spawner(mappings: Mappings, spawn: Spawner) -> Self {
        Self { mappings, spawn }
    }

    pub fn handle(&self, event: &Value) {
        if !event["event"]
            .as_str()
            .is_some_and(|e| MAPPABLE_EVENTS.contains(&e))
        {
            return;
        }
        let text = |v: &Value| {
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string())
        };
        let key = text(&event["data"]["key"]);
        let action = text(&event["data"]["action"]);
        if let Some(argv) = self.mappings.get(&key).and_then(|a| a.get(&action)) {
            if let Err(error) = (self.spawn)(argv) {
                log!("mapping {key}.{action} failed: {error}");
            }
        }
    }
}

/// One `events.subscribe` client. When it falls behind, the oldest queued
/// events are dropped so input handling never blocks.
pub struct Subscriber {
    filters: Option<HashSet<String>>,
    queue: Mutex<VecDeque<Arc<str>>>,
    ready: Condvar,
}

impl Subscriber {
    fn push(&self, name: &str, line: &Arc<str>) {
        if self.filters.as_ref().is_some_and(|f| !f.contains(name)) {
            return;
        }
        let mut queue = self.queue.lock().unwrap();
        if queue.len() >= SUBSCRIBER_QUEUE {
            queue.pop_front();
        }
        queue.push_back(line.clone());
        self.ready.notify_one();
    }

    pub fn next(&self) -> Arc<str> {
        let mut queue = self.queue.lock().unwrap();
        loop {
            if let Some(line) = queue.pop_front() {
                return line;
            }
            queue = self.ready.wait(queue).unwrap();
        }
    }

    #[cfg(test)]
    pub fn try_next(&self) -> Option<Arc<str>> {
        self.queue.lock().unwrap().pop_front()
    }
}

#[derive(Default)]
struct Connection {
    link: Option<Arc<dyn Link>>,
    last_error: Option<String>,
}

#[derive(Default)]
struct Lighting {
    agents: BTreeMap<i64, Value>,
    zones: Option<Value>,
}

type Pending = Option<(u64, Sender<Map<String, Value>>)>;

pub struct Hub {
    connection: Mutex<Connection>,
    connection_changed: Condvar,
    next_id: Mutex<u64>,
    pending: Mutex<Pending>,
    decoder: Mutex<Decoder>,
    normalizer: Mutex<Normalizer>,
    mapper: RwLock<Mapper>,
    subscribers: Mutex<Vec<Arc<Subscriber>>>,
    lighting: Mutex<Lighting>,
    tap: Option<ReportTap>,
    pub rpc_timeout: Duration,
}

impl Hub {
    pub fn new(mapper: Mapper, joystick: JoystickSectors, tap: Option<ReportTap>) -> Arc<Self> {
        Arc::new(Self {
            connection: Mutex::default(),
            connection_changed: Condvar::new(),
            next_id: Mutex::new(1),
            pending: Mutex::new(None),
            decoder: Mutex::default(),
            normalizer: Mutex::new(Normalizer::new(joystick)),
            mapper: RwLock::new(mapper),
            subscribers: Mutex::default(),
            lighting: Mutex::default(),
            tap,
            rpc_timeout: RPC_TIMEOUT,
        })
    }

    pub fn set_mapper(&self, mapper: Mapper) {
        *self.mapper.write().unwrap() = mapper;
    }

    pub fn set_joystick(&self, joystick: JoystickSectors) {
        self.normalizer.lock().unwrap().set_joystick(joystick);
    }

    pub fn device_info(&self) -> Option<DeviceInfo> {
        let connection = self.connection.lock().unwrap();
        connection.link.as_ref().map(|link| link.info().clone())
    }

    /// Wait until a device connects or the backend reports why it cannot.
    pub fn wait_for_connection(&self, timeout: Duration) -> Result<(), String> {
        let connection = self.connection.lock().unwrap();
        let (connection, _) = self
            .connection_changed
            .wait_timeout_while(connection, timeout, |c| {
                c.link.is_none() && c.last_error.is_none()
            })
            .unwrap();
        if connection.link.is_some() {
            return Ok(());
        }
        Err(connection
            .last_error
            .clone()
            .unwrap_or_else(|| "timed out waiting for the Codex Micro".into()))
    }

    // ----- subscribers -------------------------------------------------

    pub fn subscribe(&self, filters: Option<HashSet<String>>) -> Arc<Subscriber> {
        let subscriber = Arc::new(Subscriber {
            filters,
            queue: Mutex::default(),
            ready: Condvar::new(),
        });
        self.subscribers.lock().unwrap().push(subscriber.clone());
        subscriber
    }

    pub fn unsubscribe(&self, subscriber: &Arc<Subscriber>) {
        self.subscribers
            .lock()
            .unwrap()
            .retain(|s| !Arc::ptr_eq(s, subscriber));
    }

    pub fn broadcast(&self, event: &Value) {
        let name = event["event"].as_str().unwrap_or("");
        let line: Arc<str> = event.to_string().into();
        for subscriber in self.subscribers.lock().unwrap().iter() {
            subscriber.push(name, &line);
        }
    }

    fn emit(&self, event: &Value) {
        self.mapper.read().unwrap().handle(event);
        self.broadcast(event);
    }

    // ----- backend callbacks -------------------------------------------

    pub fn connected(self: &Arc<Self>, link: Arc<dyn Link>) {
        let info = link.info().clone();
        {
            let mut connection = self.connection.lock().unwrap();
            connection.link = Some(link);
            connection.last_error = None;
        }
        self.decoder.lock().unwrap().reset();
        self.normalizer.lock().unwrap().reset();
        self.connection_changed.notify_all();
        log!("connected over {}: {}", info.transport, info.product);
        self.broadcast(&json!({
            "event": "device",
            "data": {"connected": true, "connection": info.describe()},
        }));
        // Replay must not run on the backend thread: it waits for responses
        // that only the backend thread can deliver.
        let hub = self.clone();
        let link = self.connection.lock().unwrap().link.clone();
        thread::spawn(move || {
            if let Some(link) = link {
                hub.restore_lighting(&link);
            }
        });
    }

    pub fn disconnected(&self, error: &str) {
        let (was_connected, repeated) = {
            let mut connection = self.connection.lock().unwrap();
            let was_connected = connection.link.take().is_some();
            let repeated = connection.last_error.as_deref() == Some(error);
            connection.last_error = Some(error.to_string());
            (was_connected, repeated)
        };
        // Dropping the sender wakes any waiting RPC with an error.
        self.pending.lock().unwrap().take();
        self.connection_changed.notify_all();
        // A sleeping Bluetooth keyboard fails every reconnect attempt the same
        // way; report each state change only once.
        if was_connected || !repeated {
            log!("disconnected: {error}");
            self.broadcast(&json!({
                "event": "device",
                "data": {"connected": false, "error": error},
            }));
        }
    }

    pub fn on_report(&self, report: &[u8]) {
        if let Some(tap) = &self.tap {
            tap(report);
        }
        let Some(payload) = rpc_payload(report) else {
            return;
        };
        let messages = match self.decoder.lock().unwrap().push(payload) {
            Ok(messages) => messages,
            Err(error) => {
                log!("{error}");
                return;
            }
        };
        for message in messages {
            if let Some(id) = message.get("id") {
                let mut pending = self.pending.lock().unwrap();
                if pending
                    .as_ref()
                    .is_some_and(|(want, _)| id.as_u64() == Some(*want))
                {
                    let (_, sender) = pending.take().unwrap();
                    let _ = sender.send(message);
                }
                continue;
            }
            let events = self.normalizer.lock().unwrap().notification(&message);
            for event in &events {
                self.emit(event);
            }
        }
    }

    // ----- firmware calls -----------------------------------------------

    fn current_link(&self) -> Result<Arc<dyn Link>, String> {
        let connection = self.connection.lock().unwrap();
        connection
            .link
            .clone()
            .ok_or_else(|| "Codex Micro is not connected".into())
    }

    /// Send one request and wait for its reply. The caller holds `next_id`,
    /// which serializes every firmware call.
    fn call(
        &self,
        next_id: &mut u64,
        link: &Arc<dyn Link>,
        method: &str,
        params: Option<&Value>,
    ) -> Result<Value, String> {
        let id = *next_id;
        *next_id += 1;
        let (sender, receiver) = mpsc::channel();
        *self.pending.lock().unwrap() = Some((id, sender));

        let outcome = (|| {
            for report in encode_rpc(id, method, params) {
                link.write(&report).map_err(|e| e.to_string())?;
            }
            match receiver.recv_timeout(self.rpc_timeout) {
                Ok(message) => Ok(message),
                Err(RecvTimeoutError::Timeout) => Err(format!("timed out waiting for {method}")),
                Err(RecvTimeoutError::Disconnected) => Err("Codex Micro disconnected".into()),
            }
        })();
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.as_ref().is_some_and(|(want, _)| *want == id) {
                pending.take();
            }
        }
        let mut message = outcome?;
        if let Some(error) = message.get("error") {
            let detail = match error {
                Value::Object(e) => match e.get("message") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => error.to_string(),
                },
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            return Err(format!("{method}: {detail}"));
        }
        Ok(message.remove("result").unwrap_or(Value::Null))
    }

    pub fn rpc(&self, method: &str, params: Option<Value>) -> Result<Value, String> {
        let mut next_id = self.next_id.lock().unwrap();
        let link = self.current_link()?;
        let result = self.call(&mut next_id, &link, method, params.as_ref())?;
        if let Some(params) = &params {
            self.remember_lighting(method, params);
        }
        Ok(result)
    }

    /// Remember successful volatile lighting calls for reconnects.
    fn remember_lighting(&self, method: &str, params: &Value) {
        let mut lighting = self.lighting.lock().unwrap();
        match (method, params) {
            ("v.oai.thstatus", Value::Array(agents)) => {
                for agent in agents {
                    if let Some(id) = agent["id"].as_i64() {
                        lighting.agents.insert(id, agent.clone());
                    }
                }
            }
            ("v.oai.rgbcfg", Value::Object(_)) => lighting.zones = Some(params.clone()),
            _ => {}
        }
    }

    /// The calls that recreate the remembered scene, in replay order.
    pub fn lighting_scene(&self) -> Vec<(&'static str, Value)> {
        let lighting = self.lighting.lock().unwrap();
        let mut calls = Vec::new();
        if let Some(zones) = &lighting.zones {
            calls.push(("v.oai.rgbcfg", zones.clone()));
        }
        if !lighting.agents.is_empty() {
            calls.push((
                "v.oai.thstatus",
                Value::Array(lighting.agents.values().cloned().collect()),
            ));
        }
        calls
    }

    /// Replay the remembered scene onto `link`. Holding the call lock for
    /// the whole replay means no newer client call can be overwritten by a
    /// stale snapshot, and a replay for a superseded connection does nothing.
    fn restore_lighting(&self, link: &Arc<dyn Link>) {
        let mut next_id = self.next_id.lock().unwrap();
        match self.current_link() {
            Ok(current) if Arc::ptr_eq(&current, link) => {}
            _ => return,
        }
        for (method, params) in self.lighting_scene() {
            if let Err(error) = self.call(&mut next_id, link, method, Some(&params)) {
                log!("lighting replay failed: {error}");
                return;
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::protocol::{MAX_PAYLOAD, REPORT_SIZE};
    use std::io;
    use std::sync::Weak;

    /// Encode a device message the way the firmware sends it.
    pub fn device_reports(value: &Value) -> Vec<Vec<u8>> {
        value
            .to_string()
            .into_bytes()
            .chunks(MAX_PAYLOAD)
            .map(|chunk| {
                let mut report = vec![6, 2, chunk.len() as u8];
                report.extend_from_slice(chunk);
                report.resize(REPORT_SIZE, 0);
                report
            })
            .collect()
    }

    type Responder = Box<dyn Fn(&Value) -> Vec<Value> + Send + Sync>;

    /// A device that answers each complete request via `respond`.
    pub struct FakeLink {
        pub info: DeviceInfo,
        pub hub: Mutex<Weak<Hub>>,
        pub requests: Mutex<Vec<Value>>,
        partial: Mutex<Vec<u8>>,
        respond: Responder,
    }

    impl FakeLink {
        pub fn new(respond: impl Fn(&Value) -> Vec<Value> + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(Self {
                info: DeviceInfo {
                    path: "fake".into(),
                    vendor_id: 0x303a,
                    product_id: 0x8360,
                    product: "Codex Micro".into(),
                    transport: "bluetooth".into(),
                    ..Default::default()
                },
                hub: Mutex::new(Weak::new()),
                requests: Mutex::default(),
                partial: Mutex::default(),
                respond: Box::new(respond),
            })
        }

        /// Echo `{"id":N,"result":<params or true>}` for every request.
        pub fn echo() -> Arc<Self> {
            Self::new(|request| {
                let result = request.get("params").cloned().unwrap_or(Value::Bool(true));
                vec![json!({"id": request["id"], "result": result})]
            })
        }

        pub fn attach(self: &Arc<Self>, hub: &Arc<Hub>) {
            *self.hub.lock().unwrap() = Arc::downgrade(hub);
            hub.connected(self.clone());
        }
    }

    impl Link for FakeLink {
        fn write(&self, report: &[u8]) -> io::Result<()> {
            assert_eq!(report.len(), REPORT_SIZE);
            assert_eq!(&report[..2], &[6, 2]);
            let mut partial = self.partial.lock().unwrap();
            partial.extend_from_slice(&report[3..3 + report[2] as usize]);
            let Ok(request) = serde_json::from_slice::<Value>(&partial) else {
                return Ok(());
            };
            partial.clear();
            drop(partial);
            self.requests.lock().unwrap().push(request.clone());
            if let Some(hub) = self.hub.lock().unwrap().upgrade() {
                for reply in (self.respond)(&request) {
                    for report in device_reports(&reply) {
                        hub.on_report(&report);
                    }
                }
            }
            Ok(())
        }

        fn info(&self) -> &DeviceInfo {
            &self.info
        }
    }

    pub fn recording_mapper(mappings: Mappings) -> (Mapper, Arc<Mutex<Vec<Vec<String>>>>) {
        let launched = Arc::new(Mutex::new(Vec::new()));
        let sink = launched.clone();
        let mapper = Mapper::with_spawner(
            mappings,
            Box::new(move |argv| {
                sink.lock().unwrap().push(argv.to_vec());
                Ok(())
            }),
        );
        (mapper, launched)
    }

    fn hub() -> Arc<Hub> {
        Hub::new(
            Mapper::new(Mappings::new()),
            JoystickSectors::default(),
            None,
        )
    }

    fn drain(subscriber: &Subscriber) -> Vec<Value> {
        std::iter::from_fn(|| subscriber.try_next())
            .map(|line| serde_json::from_str(&line).unwrap())
            .collect()
    }

    #[test]
    fn rpc_round_trip_with_fragmented_reply_and_notification() {
        let hub = hub();
        let events = hub.subscribe(Some(HashSet::from(["key".to_string()])));
        let long = "x".repeat(150);
        let reply = long.clone();
        let link = FakeLink::new(move |request| {
            vec![
                json!({"m": "v.oai.hid", "p": {"k": "AG00", "act": 1}}),
                json!({"id": request["id"], "result": {"label": reply}}),
            ]
        });
        link.attach(&hub);

        let result = hub
            .rpc("example.method", Some(json!({"text": "y".repeat(100)})))
            .unwrap();

        assert_eq!(result, json!({"label": long}));
        assert_eq!(drain(&events)[0]["data"]["key"], "AG00");
    }

    #[test]
    fn firmware_errors_and_disconnects_are_reported() {
        let hub = hub();
        assert_eq!(
            hub.rpc("device.status", None).unwrap_err(),
            "Codex Micro is not connected"
        );

        let link = FakeLink::new(|r| vec![json!({"id": r["id"], "error": {"message": "nope"}})]);
        link.attach(&hub);
        assert_eq!(hub.rpc("fs.list", None).unwrap_err(), "fs.list: nope");

        hub.disconnected("Codex Micro disconnected");
        assert!(hub.rpc("fs.list", None).is_err());
    }

    #[test]
    fn rpc_times_out_without_reply() {
        let mut hub = Hub::new(
            Mapper::new(Mappings::new()),
            JoystickSectors::default(),
            None,
        );
        Arc::get_mut(&mut hub).unwrap().rpc_timeout = Duration::from_millis(50);
        FakeLink::new(|_| Vec::new()).attach(&hub);
        assert_eq!(
            hub.rpc("slow", None).unwrap_err(),
            "timed out waiting for slow"
        );
    }

    #[test]
    fn latest_lighting_scene_is_replayed_in_stable_order() {
        let hub = hub();
        FakeLink::echo().attach(&hub);
        let zones = json!({"keys": {"e": 1, "c": 1}, "ambient": {"e": 2, "c": 2}});
        hub.rpc("v.oai.thstatus", Some(json!([{"id": 3, "c": 3}])))
            .unwrap();
        hub.rpc("v.oai.thstatus", Some(json!([{"id": 1, "c": 1}])))
            .unwrap();
        hub.rpc("v.oai.thstatus", Some(json!([{"id": 3, "c": 33}])))
            .unwrap();
        hub.rpc("v.oai.rgbcfg", Some(zones.clone())).unwrap();
        hub.rpc("device.status", Some(json!({"ignored": true})))
            .unwrap();

        assert_eq!(
            hub.lighting_scene(),
            [
                ("v.oai.rgbcfg", zones),
                (
                    "v.oai.thstatus",
                    json!([{"id": 1, "c": 1}, {"id": 3, "c": 33}])
                ),
            ]
        );

        // A reconnect replays that scene to the new device.
        let replacement = FakeLink::echo();
        replacement.attach(&hub);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while replacement.requests.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        let methods: Vec<Value> = replacement
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r["method"].clone())
            .collect();
        assert_eq!(methods, [json!("v.oai.rgbcfg"), json!("v.oai.thstatus")]);
    }

    #[test]
    fn repeated_reconnect_failures_are_reported_once() {
        let hub = hub();
        let events = hub.subscribe(Some(HashSet::from(["device".to_string()])));
        hub.disconnected("not connected");
        hub.disconnected("not connected");
        hub.disconnected("permission denied");
        FakeLink::echo().attach(&hub);
        hub.disconnected("permission denied");

        let connected: Vec<Value> = drain(&events)
            .iter()
            .map(|e| e["data"]["connected"].clone())
            .collect();
        assert_eq!(
            connected,
            [json!(false), json!(false), json!(true), json!(false)]
        );
    }

    #[test]
    fn every_control_type_is_mappable() {
        let mut mappings = Mappings::new();
        for (key, action, argv) in [
            ("AG00", "press", "ag00"),
            ("MIC", "release", "mic-up"),
            ("ENC_CW", "step", "louder"),
            ("ENC_CLK", "press", "mute"),
            ("JOY2", "press", "down"),
            ("ACT10", "press", "raw-switch-is-not-mappable"),
        ] {
            mappings
                .entry(key.into())
                .or_default()
                .insert(action.into(), vec![argv.into()]);
        }
        let (mapper, launched) = recording_mapper(mappings);
        let hub = Hub::new(mapper, JoystickSectors::default(), None);
        for message in [
            json!({"m": "v.oai.hid", "p": {"k": "AG00", "act": 1}}),
            json!({"m": "v.oai.hid", "p": {"k": "AG00", "act": 0}}),
            json!({"m": "v.oai.hid", "p": {"k": "ACT10", "act": 1}}),
            json!({"m": "v.oai.hid", "p": {"k": "ACT10", "act": 0}}),
            json!({"m": "v.oai.hid", "p": {"k": "ENC_CW", "act": 2}}),
            json!({"m": "v.oai.hid", "p": {"k": "ENC_CLK", "act": 1}}),
            json!({"m": "v.oai.rad", "p": {"a": 0.5, "d": 1}}),
        ] {
            for report in device_reports(&message) {
                hub.on_report(&report);
            }
        }
        let launched: Vec<String> = launched
            .lock()
            .unwrap()
            .iter()
            .map(|a| a[0].clone())
            .collect();
        assert_eq!(launched, ["ag00", "mic-up", "louder", "mute", "down"]);
    }

    #[test]
    fn slow_subscribers_drop_oldest_events() {
        let hub = hub();
        let events = hub.subscribe(None);
        for i in 0..(SUBSCRIBER_QUEUE + 10) {
            hub.broadcast(&json!({"event": "vendor", "data": {"i": i}}));
        }
        let received = drain(&events);
        assert_eq!(received.len(), SUBSCRIBER_QUEUE);
        assert_eq!(received[0]["data"]["i"], 10);
    }

    #[test]
    fn spawn_detached_runs_real_commands() {
        assert!(spawn_detached(&["true".into()]).is_ok());
        assert!(spawn_detached(&["/nonexistent/program".into()]).is_err());
    }
}
