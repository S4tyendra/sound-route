use std::collections::HashMap;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

/// One hardware playback device.
#[derive(Clone, Debug)]
pub struct Device {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub state: String,
    pub muted: bool,
    /// Linear volume, 1.0 = 100%.
    pub volume: f64,
    pub latency_ms: Option<f64>,
    pub sample: String,
    pub ports: Vec<Port>,
    pub active_port: String,
    /// PipeWire object serial. WirePlumber uses this as `target.object`.
    pub serial: String,
    /// `pw-link` names for the sink's playback ports.
    pub playback_ports: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Port {
    pub name: String,
    pub description: String,
}

/// One application playback stream. Loopback legs are not included.
#[derive(Clone, Debug)]
pub struct Stream {
    pub index: u32,
    pub sink: u32,
    pub serial: String,
    pub title: String,
    pub detail: String,
    pub muted: bool,
    pub volume: f64,
    /// PipeWire node id. Metadata keys use this, not the Pulse serial.
    pub pw_id: u32,
    /// `pw-link` names for this stream's output ports.
    pub output_ports: Vec<String>,
    /// The output it was using is gone (headset disconnected, sink id unset).
    pub stranded: bool,
}

/// A `sound-route-*` loopback left behind by a previous run of this app.
#[derive(Clone, Debug)]
pub struct Orphan {
    pub pid: u32,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub default_sink: String,
    pub devices: Vec<Device>,
    pub streams: Vec<Stream>,
    pub orphans: Vec<Orphan>,
    /// Loopbacks whose device or stream disappeared. Safe to stop.
    pub stale_copies: Vec<Orphan>,
}

#[derive(Clone, Debug)]
pub struct Loopback {
    pub node_name: String,
    pub stream_serial: String,
    pub from_sink: String,
    /// When true, capture the main output's monitor (every app on it).
    /// When false, capture only this stream.
    pub whole_output: bool,
    pub to_sink: String,
    /// `pw-loopback --delay` target, in milliseconds.
    /// That target is the total capture-to-speaker delay. PipeWire subtracts
    /// the destination device's own latency, so the value to use is the main
    /// output's measured latency, not the difference between the two devices.
    pub delay_ms: f64,
}

pub fn load_snapshot() -> Result<Snapshot, String> {
    let info = pactl_json(&["info"])?;
    let default_sink = info
        .get("default_sink_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let (quantum, rate) = clock_quantum();
    let dump = pw_dump().unwrap_or(Value::Null);
    let graph = index_dump(&dump);

    let sinks = pactl_json(&["list", "sinks"])?;
    let mut devices = Vec::new();
    if let Some(list) = sinks.as_array() {
        for sink in list {
            let name = text(sink, "name");
            if name.starts_with("sound-route-bus-") {
                continue;
            }
            devices.push(Device {
                index: sink.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                description: {
                    let d = text(sink, "description");
                    if d.is_empty() { name.clone() } else { d }
                },
                latency_ms: node_latency_ms(&dump, &name, quantum, rate),
                name: name.clone(),
                state: text(sink, "state"),
                muted: sink.get("mute").and_then(|v| v.as_bool()).unwrap_or(false),
                volume: volume_of(sink.get("volume")),
                sample: text(sink, "sample_specification"),
                active_port: text(sink, "active_port"),
                ports: ports_of(sink.get("ports")),
                serial: graph.serials.get(&name).cloned().unwrap_or_default(),
                playback_ports: graph.playback_ports.get(&name).cloned().unwrap_or_default(),
            });
        }
    }

    let inputs = pactl_json(&["list", "sink-inputs"])?;
    let mut streams = Vec::new();
    let mut orphans = Vec::new();
    if let Some(list) = inputs.as_array() {
        for input in list {
            let props = input.get("properties").cloned().unwrap_or(Value::Null);
            let node_name = text(&props, "node.name");
            let media = text(&props, "media.name");
            let binary = text(&props, "application.process.binary");
            if is_helper(&node_name, &media, &binary) {
                if node_name.contains("sound-route") || media.contains("sound-route") {
                    if let Ok(pid) = text(&props, "application.process.id").parse::<u32>() {
                        orphans.push(Orphan {
                            pid,
                            label: if media.is_empty() { node_name } else { media },
                        });
                    }
                }
                continue;
            }
            let pw_id = props.get("object.id").and_then(json_u32).unwrap_or(0);
            let sink = input.get("sink").and_then(|v| v.as_u64()).map(|n| n as u32);
            let stranded = sink_is_gone(sink, &devices);
            let app = text(&props, "application.name");
            let title = if app.is_empty() {
                format!("Stream {}", input.get("index").and_then(|v| v.as_u64()).unwrap_or(0))
            } else {
                app
            };
            let role = text(&props, "media.role");
            let mut detail = media.clone();
            if !role.is_empty() {
                if detail.is_empty() {
                    detail = role;
                } else {
                    detail = format!("{detail} · {role}");
                }
            }
            if input.get("corked").and_then(|v| v.as_bool()).unwrap_or(false) {
                detail = if detail.is_empty() {
                    "paused".into()
                } else {
                    format!("{detail} · paused")
                };
            }
            if stranded {
                detail = if detail.is_empty() {
                    "no output device".into()
                } else {
                    format!("{detail} · no output device")
                };
            }
            streams.push(Stream {
                index: input.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                sink: sink.unwrap_or(u32::MAX),
                serial: text(&props, "object.serial"),
                title,
                detail,
                muted: input.get("mute").and_then(|v| v.as_bool()).unwrap_or(false),
                volume: volume_of(input.get("volume")),
                pw_id,
                output_ports: graph.output_ports.get(&pw_id).cloned().unwrap_or_default(),
                stranded,
            });
        }
    }

    orphans.sort_by_key(|o| o.pid);
    orphans.dedup_by_key(|o| o.pid);

    let mut stale_copies = graph
        .loopbacks
        .iter()
        .filter(|item| loopback_is_dead(&item.target, &devices, &streams))
        .map(|item| Orphan {
            pid: item.pid,
            label: item.label.clone(),
        })
        .collect::<Vec<_>>();
    stale_copies.extend(stale_loopbacks_from_ps(&devices, &streams));
    stale_copies.sort_by_key(|o| o.pid);
    stale_copies.dedup_by_key(|o| o.pid);
    stale_copies.retain(|o| o.pid != 0);

    Ok(Snapshot {
        default_sink,
        devices,
        streams,
        orphans,
        stale_copies,
    })
}

/// Move streams that lost their device onto a sink that is still present,
/// and stop loopbacks aimed at something that disappeared.
/// Healthy streams and healthy copies are left alone.
pub fn rescue(snap: &Snapshot) -> Result<Vec<String>, String> {
    if snap.streams.iter().all(|s| !s.stranded) && snap.stale_copies.is_empty() {
        return Ok(Vec::new());
    }
    let mut notes = Vec::new();
    for copy in &snap.stale_copies {
        let _ = stop_pid(copy.pid);
        notes.push(format!(
            "Stopped a copy aimed at a missing device ({})",
            copy.label
        ));
    }
    let Some(fallback) = fallback_device(&snap.devices, &snap.default_sink) else {
        if snap.streams.iter().any(|s| s.stranded) {
            notes.push("A device disconnected and no output is left.".into());
        }
        return Ok(notes);
    };
    for stream in snap.streams.iter().filter(|s| s.stranded) {
        attach_stream(stream, fallback)?;
        notes.push(format!(
            "{} lost its output and now plays on {}.",
            stream.title, fallback.description
        ));
    }
    Ok(notes)
}

fn attach_stream(stream: &Stream, device: &Device) -> Result<(), String> {
    if stream.pw_id != 0 {
        meta_delete(stream.pw_id, "target.object");
        meta_delete(stream.pw_id, "target.node");
        if !device.serial.is_empty() {
            meta_set(stream.pw_id, "target.object", &device.serial);
        }
    }
    let _ = move_stream(stream.index, &device.name);
    let pairs = port_pairs(&stream.output_ports, &device.playback_ports);
    if pairs.is_empty() {
        return Err(format!(
            "Could not find audio ports to connect {} to {}.",
            stream.title, device.description
        ));
    }
    for (from, to) in pairs {
        pw_link(&from, &to)?;
    }
    Ok(())
}

pub fn set_device_volume(name: &str, volume: f64) -> Result<(), String> {
    pactl(&[
        "set-sink-volume",
        name,
        &format!("{}%", (volume * 100.0).round() as i32),
    ])
}

pub fn set_device_mute(name: &str, mute: bool) -> Result<(), String> {
    pactl(&["set-sink-mute", name, if mute { "1" } else { "0" }])
}

pub fn set_default_sink(name: &str) -> Result<(), String> {
    pactl(&["set-default-sink", name])
}

pub fn set_sink_port(name: &str, port: &str) -> Result<(), String> {
    pactl(&["set-sink-port", name, port])
}

pub fn set_stream_volume(index: u32, volume: f64) -> Result<(), String> {
    pactl(&[
        "set-sink-input-volume",
        &index.to_string(),
        &format!("{}%", (volume * 100.0).round() as i32),
    ])
}

pub fn set_stream_mute(index: u32, mute: bool) -> Result<(), String> {
    pactl(&[
        "set-sink-input-mute",
        &index.to_string(),
        if mute { "1" } else { "0" },
    ])
}

pub fn move_stream(index: u32, sink_name: &str) -> Result<(), String> {
    check_token(sink_name)?;
    pactl(&["move-sink-input", &index.to_string(), sink_name])
}

pub fn start_loopback(opts: &Loopback) -> Result<Child, String> {
    check_token(&opts.node_name)?;
    check_token(&opts.from_sink)?;
    check_token(&opts.to_sink)?;
    if !opts.whole_output {
        check_token(&opts.stream_serial)?;
    }

    let capture = if opts.whole_output {
        format!(
            "stream.capture.sink=true target.object=\"{}\" node.dont-fallback=true node.dont-reconnect=true node.passive=true",
            opts.from_sink
        )
    } else {
        format!(
            "target.object=\"{}\" node.dont-fallback=true node.dont-reconnect=true node.passive=true",
            opts.stream_serial
        )
    };
    let playback = format!(
        "target.object=\"{}\" node.dont-fallback=true node.dont-reconnect=true node.passive=true",
        opts.to_sink
    );

    let mut cmd = Command::new("pw-loopback");
    cmd.arg("--name")
        .arg(&opts.node_name)
        .arg("--latency")
        .arg("21")
        .arg("--capture-props")
        .arg(capture)
        .arg("--playback-props")
        .arg(playback)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if opts.delay_ms >= 1.0 {
        cmd.arg("--delay").arg(format!("{:.3}", opts.delay_ms / 1000.0));
    }
    cmd.spawn().map_err(|e| format!("could not start pw-loopback: {e}"))
}

pub fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub fn stop_pid(pid: u32) -> Result<(), String> {
    let status = Command::new("kill")
        .arg(pid.to_string())
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("could not stop process {pid}"))
    }
}

/// Delay target that lines a copy up with the main output.
pub fn align_delay_ms(main_latency_ms: Option<f64>) -> f64 {
    main_latency_ms.unwrap_or(0.0).max(0.0)
}

pub fn device_by_index(snap: &Snapshot, index: u32) -> Option<&Device> {
    snap.devices.iter().find(|d| d.index == index)
}

pub fn device_by_name<'a>(snap: &'a Snapshot, name: &str) -> Option<&'a Device> {
    snap.devices.iter().find(|d| d.name == name)
}

pub fn sink_is_gone(sink: Option<u32>, devices: &[Device]) -> bool {
    match sink {
        None => true,
        Some(id) => !devices.iter().any(|d| d.index == id),
    }
}

pub fn fallback_device<'a>(devices: &'a [Device], default_sink: &str) -> Option<&'a Device> {
    if let Some(device) = devices.iter().find(|d| d.name == default_sink) {
        return Some(device);
    }
    devices
        .iter()
        .find(|d| d.name.starts_with("alsa_") || d.name.contains("analog"))
        .or_else(|| devices.first())
}

/// A loopback should be stopped when its target device or stream is gone.
/// An empty target is left alone: the process may still be connecting.
pub fn loopback_is_dead(target: &str, devices: &[Device], streams: &[Stream]) -> bool {
    if target.is_empty() {
        return false;
    }
    if target == "-1" {
        return true;
    }
    let device_here = devices
        .iter()
        .any(|d| d.name == target || d.serial == target);
    let stream_here = streams.iter().any(|s| {
        !s.stranded && (s.serial == target || s.pw_id.to_string() == target)
    });
    !(device_here || stream_here)
}

pub fn port_pairs(outputs: &[String], inputs: &[String]) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for output in outputs {
        let channel = channel_suffix(output);
        if let Some(input) = inputs.iter().find(|port| channel_suffix(port) == channel) {
            pairs.push((output.clone(), input.clone()));
        }
    }
    pairs
}

fn channel_suffix(port: &str) -> &str {
    port.rsplit('_').next().unwrap_or(port)
}

struct LoopTarget {
    pid: u32,
    label: String,
    target: String,
}

struct DumpIndex {
    serials: HashMap<String, String>,
    playback_ports: HashMap<String, Vec<String>>,
    output_ports: HashMap<u32, Vec<String>>,
    loopbacks: Vec<LoopTarget>,
}

fn index_dump(dump: &Value) -> DumpIndex {
    let mut index = DumpIndex {
        serials: HashMap::new(),
        playback_ports: HashMap::new(),
        output_ports: HashMap::new(),
        loopbacks: Vec::new(),
    };
    let Some(list) = dump.as_array() else {
        return index;
    };

    struct NodeRec {
        name: String,
        class: String,
        serial: String,
        binary: String,
        pid: u32,
        target: String,
        media: String,
    }
    let mut nodes: HashMap<u32, NodeRec> = HashMap::new();
    // direction, port.name. Link targets are `node.name:port.name`.
    // `port.alias` is a different label and is not the endpoint pw-link used.
    let mut ports: HashMap<u32, Vec<(String, String)>> = HashMap::new();

    for obj in list {
        let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let Some(props) = obj.get("info").and_then(|info| info.get("props")) else {
            continue;
        };
        if kind == "PipeWire:Interface:Node" {
            let id = obj.get("id").and_then(json_u32).unwrap_or(0);
            nodes.insert(
                id,
                NodeRec {
                    name: text(props, "node.name"),
                    class: text(props, "media.class"),
                    serial: json_string(props, "object.serial"),
                    binary: text(props, "application.process.binary"),
                    pid: props
                        .get("application.process.id")
                        .and_then(json_u32)
                        .unwrap_or(0),
                    target: json_string(props, "target.object"),
                    media: text(props, "media.name"),
                },
            );
        } else if kind == "PipeWire:Interface:Port" {
            let node_id = props.get("node.id").and_then(json_u32).unwrap_or(0);
            let name = text(props, "port.name");
            let direction = text(props, "port.direction");
            ports.entry(node_id).or_default().push((direction, name));
        }
    }

    for (id, node) in &nodes {
        let node_ports = ports.get(id).map(Vec::as_slice).unwrap_or(&[]);
        if node.class == "Audio/Sink" {
            index.serials.insert(node.name.clone(), node.serial.clone());
            let playback = node_ports
                .iter()
                .filter(|(direction, name)| *direction == "in" && name.starts_with("playback_"))
                .map(|(_, name)| format!("{}:{name}", node.name))
                .collect();
            index.playback_ports.insert(node.name.clone(), playback);
        } else if node.class == "Stream/Output/Audio" {
            let outputs = node_ports
                .iter()
                .filter(|(direction, name)| *direction == "out" && name.starts_with("output_"))
                .map(|(_, name)| format!("{}:{name}", node.name))
                .collect();
            index.output_ports.insert(*id, outputs);
        }
        let is_loopback = node.binary == "pw-loopback"
            || node.name.starts_with("input.")
            || node.name.starts_with("output.");
        if is_loopback && node.pid != 0 {
            let label = if node.media.is_empty() {
                node.name.clone()
            } else {
                node.media.clone()
            };
            index.loopbacks.push(LoopTarget {
                pid: node.pid,
                label,
                target: node.target.clone(),
            });
        }
    }
    index
}

fn stale_loopbacks_from_ps(devices: &[Device], streams: &[Stream]) -> Vec<Orphan> {
    let Ok(out) = Command::new("ps")
        .args(["-C", "pw-loopback", "-o", "pid=,args="])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((pid_str, args)) = line.split_once(|c: char| c.is_whitespace()) else {
            continue;
        };
        let Ok(pid) = pid_str.trim().parse::<u32>() else {
            continue;
        };
        let targets = quoted_targets(args);
        if let Some(missing) = targets.iter().find(|t| loopback_is_dead(t, devices, streams)) {
            found.push(Orphan {
                pid,
                label: missing.clone(),
            });
        }
    }
    found
}

fn quoted_targets(args: &str) -> Vec<String> {
    let key = "target.object=\"";
    let mut rest = args;
    let mut targets = Vec::new();
    while let Some(start) = rest.find(key) {
        let after = &rest[start + key.len()..];
        let Some(end) = after.find('"') else {
            break;
        };
        targets.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    targets
}

fn meta_delete(id: u32, key: &str) {
    let _ = Command::new("pw-metadata")
        .args(["-d", &id.to_string(), key])
        .status();
}

fn meta_set(id: u32, key: &str, value: &str) {
    let _ = Command::new("pw-metadata")
        .args([&id.to_string(), key, value, "Spa:Id"])
        .status();
}

fn pw_link(from: &str, to: &str) -> Result<(), String> {
    if from.is_empty() || to.is_empty() || from.contains('\n') || to.contains('\n') {
        return Err("refusing to link an empty audio port".into());
    }
    let out = Command::new("pw-link")
        .arg(from)
        .arg(to)
        .output()
        .map_err(|e| format!("pw-link: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.to_lowercase().contains("exist") {
        return Ok(());
    }
    let err = err.trim();
    if err.is_empty() {
        Err(format!("could not link {from} to {to}"))
    } else {
        Err(err.to_string())
    }
}

fn json_u32(value: &Value) -> Option<u32> {
    if let Some(n) = value.as_u64() {
        return Some(n as u32);
    }
    value.as_str()?.parse().ok()
}

fn json_string(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

fn is_helper(node_name: &str, media: &str, binary: &str) -> bool {
    binary == "pw-loopback"
        || node_name.starts_with("output.")
        || node_name.starts_with("input.")
        || node_name.contains("sound-route")
        || media.contains("sound-route")
        || media.contains("vlc-speaker-sync")
}

fn pactl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("pactl")
        .args(args)
        .output()
        .map_err(|e| format!("pactl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        if err.is_empty() {
            Err(format!("pactl {} failed", args.first().unwrap_or(&"")))
        } else {
            Err(err.to_string())
        }
    }
}

fn pactl_json(args: &[&str]) -> Result<Value, String> {
    let out = Command::new("pactl")
        .arg("-f")
        .arg("json")
        .args(args)
        .output()
        .map_err(|e| format!("pactl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("pactl json: {e}"))
}

fn pw_dump() -> Result<Value, String> {
    let out = Command::new("pw-dump")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

fn clock_quantum() -> (f64, f64) {
    let mut quantum = 1024.0;
    let mut rate = 48000.0;
    let Ok(out) = Command::new("pw-metadata").arg("-n").arg("settings").output() else {
        return (quantum, rate);
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(v) = meta_value(line, "clock.quantum") {
            quantum = v;
        }
        if let Some(v) = meta_value(line, "clock.rate") {
            rate = v;
        }
    }
    (quantum, rate)
}

fn meta_value(line: &str, key: &str) -> Option<f64> {
    let needle = format!("key:'{key}'");
    if !line.contains(&needle) {
        return None;
    }
    let rest = line.split("value:'").nth(1)?;
    let raw = rest.split('\'').next()?;
    raw.parse().ok()
}

fn node_latency_ms(dump: &Value, node_name: &str, quantum: f64, rate: f64) -> Option<f64> {
    let list = dump.as_array()?;
    for obj in list {
        if obj.get("type")?.as_str()? != "PipeWire:Interface:Node" {
            continue;
        }
        let props = obj.get("info")?.get("props")?;
        if props.get("node.name")?.as_str()? != node_name {
            continue;
        }
        if props.get("media.class")?.as_str()? != "Audio/Sink" {
            continue;
        }
        let latency = obj.get("info")?.get("params")?.get("Latency")?.as_array()?;
        for entry in latency {
            if entry.get("direction")?.as_str()? != "Input" {
                continue;
            }
            let ns = entry.get("minNs").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let quanta = entry
                .get("minQuantum")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let ms = ns / 1.0e6 + quanta * quantum / rate * 1000.0;
            return Some(ms);
        }
    }
    None
}

fn ports_of(value: Option<&Value>) -> Vec<Port> {
    let Some(list) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|p| {
            let name = text(p, "name");
            if name.is_empty() {
                return None;
            }
            let description = text(p, "description");
            Some(Port {
                description: if description.is_empty() { name.clone() } else { description },
                name,
            })
        })
        .collect()
}

fn volume_of(value: Option<&Value>) -> f64 {
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return 1.0;
    };
    let mut sum = 0.0;
    let mut n = 0.0;
    for channel in obj.values() {
        let Some(pct) = channel.get("value_percent").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(v) = parse_percent(pct) {
            sum += v;
            n += 1.0;
        }
    }
    if n == 0.0 { 1.0 } else { sum / n }
}

fn parse_percent(text: &str) -> Option<f64> {
    let number = text.trim().trim_end_matches('%').trim();
    number.parse::<f64>().ok().map(|v| v / 100.0)
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn check_token(value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!("unexpected audio name: {value}"));
    }
    Ok(())
}

pub fn fmt_ms(ms: Option<f64>) -> String {
    match ms {
        None => "latency unknown".into(),
        Some(v) if v >= 10.0 => format!("{:.0} ms", v),
        Some(v) => format!("{:.1} ms", v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_and_delay() {
        assert!((parse_percent("42%").unwrap() - 0.42).abs() < 1e-9);
        assert!((parse_percent("100.00%").unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(align_delay_ms(Some(138.333)), 138.333);
        assert_eq!(align_delay_ms(None), 0.0);
    }

    #[test]
    fn latency_uses_codec_delay_plus_quantum() {
        let dump = serde_json::json!([
            {
                "type": "PipeWire:Interface:Node",
                "info": {
                    "props": {
                        "node.name": "bluez_output.test",
                        "media.class": "Audio/Sink"
                    },
                    "params": {
                        "Latency": [{
                            "direction": "Input",
                            "minQuantum": 0.0,
                            "minNs": 138333332
                        }]
                    }
                }
            },
            {
                "type": "PipeWire:Interface:Node",
                "info": {
                    "props": {
                        "node.name": "alsa_output.speakers",
                        "media.class": "Audio/Sink"
                    },
                    "params": {
                        "Latency": [{
                            "direction": "Input",
                            "minQuantum": 1.0,
                            "minNs": 0
                        }]
                    }
                }
            }
        ]);
        let bt = node_latency_ms(&dump, "bluez_output.test", 1024.0, 48000.0).unwrap();
        let sp = node_latency_ms(&dump, "alsa_output.speakers", 1024.0, 48000.0).unwrap();
        assert!((bt - 138.333332).abs() < 0.01);
        assert!((sp - 21.333333).abs() < 0.01);
    }

    #[test]
    fn helper_streams_are_hidden() {
        assert!(is_helper("output.vlc-speaker-sync", "vlc-speaker-sync output", "pw-loopback"));
        assert!(!is_helper("VLC media player", "audio stream", "vlc"));
    }

    fn dev(index: u32, name: &str) -> Device {
        Device {
            index,
            name: name.into(),
            description: name.into(),
            state: "RUNNING".into(),
            muted: false,
            volume: 1.0,
            latency_ms: None,
            sample: String::new(),
            ports: Vec::new(),
            active_port: String::new(),
            serial: index.to_string(),
            playback_ports: vec![
                format!("{name}:playback_FL"),
                format!("{name}:playback_FR"),
            ],
        }
    }

    fn stream_on(sink: u32, serial: &str) -> Stream {
        Stream {
            index: 1,
            sink,
            serial: serial.into(),
            title: "VLC".into(),
            detail: String::new(),
            muted: false,
            volume: 1.0,
            pw_id: 118,
            output_ports: vec!["VLC:output_FL".into(), "VLC:output_FR".into()],
            stranded: false,
        }
    }

    #[test]
    fn disconnect_leaves_stream_stranded_and_kills_headset_copy() {
        let speakers = dev(62, "alsa_output.speakers");
        let devices = vec![speakers];
        assert!(sink_is_gone(Some(u32::MAX), &devices));
        assert!(!sink_is_gone(Some(62), &devices));
        let fallback = fallback_device(&devices, "bluez_output.gone").unwrap();
        assert_eq!(fallback.name, "alsa_output.speakers");

        let mut stranded = stream_on(u32::MAX, "816");
        stranded.stranded = true;
        let healthy = stream_on(62, "900");
        assert!(loopback_is_dead("bluez_output.headset", &devices, &[healthy.clone()]));
        assert!(loopback_is_dead("-1", &devices, &[healthy.clone()]));
        assert!(!loopback_is_dead("alsa_output.speakers", &devices, &[healthy.clone()]));
        assert!(!loopback_is_dead("900", &devices, &[healthy.clone()]));
        assert!(loopback_is_dead("816", &devices, &[stranded]));
        assert!(!loopback_is_dead("", &devices, &[healthy]));

        let pairs = port_pairs(
            &["VLC:output_FL".into(), "VLC:output_FR".into(), "VLC:output_LFE".into()],
            &["alsa:playback_FL".into(), "alsa:playback_FR".into()],
        );
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].1, "alsa:playback_FL");
    }

    #[test]
    fn quoted_process_targets_are_found() {
        let args = r#"pw-loopback --capture-props target.object="bluez_output.headset" --playback-props target.object="alsa_output.speakers""#;
        let targets = quoted_targets(args);
        assert_eq!(targets, ["bluez_output.headset", "alsa_output.speakers"]);
    }
}
