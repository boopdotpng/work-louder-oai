//! codex-micro: a small background service and CLI for the Work Louder
//! Codex Micro on macOS and Linux.

mod api;
mod client;
mod config;
mod device;
mod events;
#[cfg(target_os = "linux")]
mod hid_linux;
#[cfg(target_os = "macos")]
mod hid_macos;
mod hub;
mod protocol;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{json, Value};

use crate::config::Config;
use crate::device::{Selector, TransportPreference};
use crate::hub::{Hub, Mapper};
use crate::protocol::{effect_code, parse_hex_color, EFFECTS};

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { eprintln!("codex-micro: {}", format_args!($($arg)*)) };
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

const USAGE: &str = "\
usage: codex-micro <command> [options]

Background service:
  daemon [--config PATH] [--socket PATH] [--transport auto|bluetooth|usb]

Commands that talk to the daemon (all accept --socket PATH):
  status                          firmware, battery, and connection
  events [EVENT...]               stream events as JSON lines
  agent ID COLOR [--effect E] [--brightness N] [--speed N]
  agents C0 C1 C2 C3 C4 C5 [--effect E] [--brightness N] [--speed N]
  zones --keys COLOR --ambient COLOR [--keys-effect E] [--ambient-effect E]
        [--brightness N] [--speed N]

Direct device access (stop the daemon first):
  probe [--transport T] [--name TEXT] list [--all]
  probe [--transport T] [--name TEXT] status
  probe [--transport T] [--name TEXT] monitor
  probe [--transport T] [--name TEXT] rpc METHOD [JSON] [--unsafe]

Colors are #RRGGBB. Effects: off solid snake rainbow breath gradient shallow-breath
";

struct Args {
    positional: Vec<String>,
    options: HashMap<String, String>,
    flags: HashSet<String>,
}

impl Args {
    fn parse(raw: &[String], valued: &[&str], boolean: &[&str]) -> Result<Self, String> {
        let mut args = Args {
            positional: Vec::new(),
            options: HashMap::new(),
            flags: HashSet::new(),
        };
        let mut iter = raw.iter();
        while let Some(arg) = iter.next() {
            if let Some(name) = arg.strip_prefix("--") {
                let (name, inline) = match name.split_once('=') {
                    Some((n, v)) => (n, Some(v.to_string())),
                    None => (name, None),
                };
                if valued.contains(&name) {
                    let value = match inline {
                        Some(v) => v,
                        None => iter
                            .next()
                            .ok_or_else(|| format!("--{name} needs a value"))?
                            .clone(),
                    };
                    args.options.insert(name.to_string(), value);
                } else if boolean.contains(&name) && inline.is_none() {
                    args.flags.insert(name.to_string());
                } else {
                    return Err(format!("unknown option: --{name}"));
                }
            } else {
                args.positional.push(arg.clone());
            }
        }
        Ok(args)
    }

    fn option(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    fn number(&self, name: &str, default: f64) -> Result<f64, String> {
        self.option(name)
            .map(|v| {
                v.parse::<f64>()
                    .map_err(|_| format!("--{name} must be a number"))
            })
            .unwrap_or(Ok(default))
    }

    fn effect(&self, name: &str) -> Result<String, String> {
        let effect = self.option(name).unwrap_or("solid");
        if effect_code(effect).is_none() {
            let names: Vec<&str> = EFFECTS.iter().map(|(n, _)| *n).collect();
            return Err(format!("--{name} must be one of: {}", names.join(", ")));
        }
        Ok(effect.to_string())
    }

    fn socket(&self) -> PathBuf {
        self.option("socket")
            .map(PathBuf::from)
            .unwrap_or_else(api::default_socket_path)
    }

    fn expect_positional(&self, count: usize, usage: &str) -> Result<(), String> {
        if self.positional.len() != count {
            return Err(format!("usage: codex-micro {usage}"));
        }
        Ok(())
    }
}

fn color(text: &str) -> Result<String, String> {
    parse_hex_color(text)
        .map(|c| format!("#{c:06x}"))
        .ok_or_else(|| format!("color must be #RRGGBB: {text}"))
}

fn selector(args: &Args) -> Result<Selector, String> {
    let mut selector = Selector::default();
    if let Some(transport) = args.option("transport") {
        selector.transport = transport.parse()?;
    }
    if let Some(name) = args.option("name") {
        selector.name = name.to_string();
    }
    Ok(selector)
}

/// Block termination signals in every thread and handle them on one thread.
fn handle_signals(socket: PathBuf, config_path: PathBuf, hub: std::sync::Arc<Hub>) {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
            libc::sigaddset(&mut set, signal);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        std::thread::spawn(move || loop {
            let mut signal = 0;
            if libc::sigwait(&set, &mut signal) != 0 {
                continue;
            }
            if signal == libc::SIGHUP {
                match Config::load(&config_path) {
                    Ok(config) => {
                        hub.set_mapper(Mapper::new(config.mappings));
                        hub.set_joystick(config.joystick);
                        log!(
                            "reloaded {} ([device] changes need a restart)",
                            config_path.display()
                        );
                    }
                    Err(error) => log!("config reload failed, keeping previous mappings: {error}"),
                }
            } else {
                let _ = std::fs::remove_file(&socket);
                log!("stopped");
                std::process::exit(0);
            }
        });
    }
}

fn daemon(args: &Args) -> Result<(), String> {
    args.expect_positional(0, "daemon [--config PATH] [--socket PATH] [--transport T]")?;
    let config_path = args
        .option("config")
        .map(PathBuf::from)
        .unwrap_or_else(config::default_config_path);
    let config = Config::load(&config_path)?;
    let mut selector = config.selector.clone();
    if let Some(transport) = args.option("transport") {
        selector.transport = transport.parse::<TransportPreference>()?;
    }
    let socket = args.socket();
    let hub = Hub::new(Mapper::new(config.mappings), config.joystick, None);
    // Signals must be blocked before any other thread starts.
    handle_signals(socket.clone(), config_path, hub.clone());
    let listener = api::bind(&socket)?;
    device::start(hub.clone(), selector);
    log!("listening on {}", socket.display());
    api::serve(listener, hub);
    Ok(())
}

fn agent_params(id: i64, color: String, args: &Args) -> Result<Value, String> {
    Ok(json!({
        "id": id,
        "color": color,
        "effect": args.effect("effect")?,
        "brightness": args.number("brightness", 0.5)?,
        "speed": args.number("speed", 0.4)?,
    }))
}

fn run(raw: &[String]) -> Result<(), String> {
    let Some((command, rest)) = raw.split_first() else {
        return Err(USAGE.trim_end().into());
    };
    let socket = |args: &Args| args.socket();
    let lighting_options = ["socket", "effect", "brightness", "speed"];
    match command.as_str() {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        "daemon" => daemon(&Args::parse(rest, &["config", "socket", "transport"], &[])?),
        "status" => {
            let args = Args::parse(rest, &["socket"], &[])?;
            args.expect_positional(0, "status")?;
            client::print_json(&client::request(&socket(&args), "device.status", None)?);
            Ok(())
        }
        "events" => {
            let args = Args::parse(rest, &["socket"], &[])?;
            client::events(&socket(&args), &args.positional)
        }
        "agent" => {
            let args = Args::parse(rest, &lighting_options, &[])?;
            args.expect_positional(
                2,
                "agent ID COLOR [--effect E] [--brightness N] [--speed N]",
            )?;
            let id = args.positional[0]
                .parse::<i64>()
                .ok()
                .filter(|id| (0..6).contains(id))
                .ok_or("agent ID must be from 0 through 5")?;
            let params = agent_params(id, color(&args.positional[1])?, &args)?;
            client::print_json(&client::request(
                &socket(&args),
                "lighting.agent.set",
                Some(params),
            )?);
            Ok(())
        }
        "agents" => {
            let args = Args::parse(rest, &lighting_options, &[])?;
            args.expect_positional(
                6,
                "agents C0 C1 C2 C3 C4 C5 [--effect E] [--brightness N] [--speed N]",
            )?;
            let agents = args
                .positional
                .iter()
                .enumerate()
                .map(|(id, c)| agent_params(id as i64, color(c)?, &args))
                .collect::<Result<Vec<_>, _>>()?;
            let params = json!({"agents": agents});
            client::print_json(&client::request(
                &socket(&args),
                "lighting.agents.set",
                Some(params),
            )?);
            Ok(())
        }
        "zones" => {
            let args = Args::parse(
                rest,
                &[
                    "socket",
                    "keys",
                    "ambient",
                    "keys-effect",
                    "ambient-effect",
                    "brightness",
                    "speed",
                ],
                &[],
            )?;
            args.expect_positional(0, "zones --keys COLOR --ambient COLOR [...]")?;
            let side = |name: &str| -> Result<Value, String> {
                let value = args.option(name).ok_or(format!("--{name} is required"))?;
                Ok(json!({
                    "color": color(value)?,
                    "effect": args.effect(&format!("{name}-effect"))?,
                    "brightness": args.number("brightness", 0.5)?,
                    "speed": args.number("speed", 0.4)?,
                }))
            };
            let params = json!({"keys": side("keys")?, "ambient": side("ambient")?});
            client::print_json(&client::request(
                &socket(&args),
                "lighting.zones.set",
                Some(params),
            )?);
            Ok(())
        }
        "probe" => {
            let args = Args::parse(rest, &["transport", "name"], &["all", "unsafe"])?;
            let selector = selector(&args)?;
            match args.positional.first().map(String::as_str) {
                Some("list") => client::probe_list(&selector, args.flags.contains("all")),
                Some("status") => {
                    client::print_json(&client::probe_rpc(selector, "device.status", None, false)?);
                    Ok(())
                }
                Some("monitor") => client::probe_monitor(selector),
                Some("rpc") => {
                    let method = args
                        .positional
                        .get(1)
                        .ok_or("usage: codex-micro probe rpc METHOD [JSON]")?;
                    let params = match args.positional.get(2) {
                        Some(text) => {
                            Some(serde_json::from_str(text).map_err(|e| format!("params: {e}"))?)
                        }
                        None => None,
                    };
                    let unsafe_ok = args.flags.contains("unsafe");
                    client::print_json(&client::probe_rpc(selector, method, params, unsafe_ok)?);
                    Ok(())
                }
                _ => Err("usage: codex-micro probe list|status|monitor|rpc".into()),
            }
        }
        other => Err(format!("unknown command: {other}\n\n{}", USAGE.trim_end())),
    }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match run(&raw) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_options_flags_and_positionals() {
        let args = Args::parse(
            &strings(&["0", "#FFFFFF", "--effect", "breath", "--speed=0.2", "--all"]),
            &["effect", "speed"],
            &["all"],
        )
        .unwrap();
        assert_eq!(args.positional, ["0", "#FFFFFF"]);
        assert_eq!(args.option("effect"), Some("breath"));
        assert_eq!(args.number("speed", 0.4), Ok(0.2));
        assert!(args.flags.contains("all"));
        assert!(Args::parse(&strings(&["--bogus"]), &[], &[]).is_err());
        assert!(Args::parse(&strings(&["--effect"]), &["effect"], &[]).is_err());
    }

    #[test]
    fn normalizes_colors_and_effects() {
        assert_eq!(color("A0b1C2"), Ok("#a0b1c2".into()));
        assert!(color("red").is_err());
        let args = Args::parse(&strings(&["--effect", "sparkles"]), &["effect"], &[]).unwrap();
        assert!(args.effect("effect").is_err());
    }
}
