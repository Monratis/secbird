//! Local editor: one embedded page plus a small JSON API on 127.0.0.1.
//!
//! Any website can send requests to localhost, so every request must:
//! - carry a Host header naming this server (blocks DNS rebinding),
//! - have no Origin, or our own origin,
//! - include the per-session token (page: `?t=` query, API: `X-Secbird-Token` header;
//!   a custom header also forces a CORS preflight, which we never grant).

use crate::engine::Engine;
use secbird_core::{actions, config_dir, Device, Error, Kind, Rgb, Store};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const PAGE: &str = include_str!("editor.html");
const LOGO_PNG: &[u8] = include_bytes!("../../../assets/logo-64.png");
/// Product photos, keyed by the HID pid the device keeps even when it renames itself
/// (the HyperPolling receiver answers for its paired mouse). WebP only where the
/// vendor asset is WebP; every browser that runs this editor supports it.
const PHOTOS: &[(u16, &str, &[u8])] = &[
    (0x0577, "image/png", include_bytes!("../../../assets/blackshark-v3-pro.png")),
    (0x02A7, "image/webp", include_bytes!("../../../assets/huntsman-v3-pro-tkl.webp")),
    (0x00B3, "image/png", include_bytes!("../../../assets/deathadder-v3-pro.png")),
    (0x00B6, "image/png", include_bytes!("../../../assets/deathadder-v3-pro.png")),
    (0x00C2, "image/png", include_bytes!("../../../assets/deathadder-v3-pro.png")),
    (0x00C3, "image/png", include_bytes!("../../../assets/deathadder-v3-pro.png")),
];
const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;

pub struct Editor {
    pub url: String,
}

/// Bind a random local port and serve on a background thread.
pub fn start(engine: Arc<Mutex<Engine>>) -> std::io::Result<Editor> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let token = new_token();
    let url = format!("http://127.0.0.1:{port}/?t={token}");
    let server = Server { port, token, engine };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            server.handle(stream);
        }
    });
    Ok(Editor { url })
}

/// 128 bits from std's OS-seeded hasher keys; no extra crate needed.
fn new_token() -> String {
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut token = String::with_capacity(32);
    for i in 0..2u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(nanos);
        token.push_str(&format!("{:016x}", h.finish()));
    }
    token
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn query_param(&self, name: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let header_end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEADER {
            return None;
        }
    };
    let head = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();

    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return None;
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < len {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Some(Request { method, path: path.to_string(), query: query.to_string(), headers, body })
}

struct Response {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Response {
    fn json(v: Value) -> Self {
        Response { status: "200 OK", content_type: "application/json", body: v.to_string().into_bytes() }
    }

    fn error(status: &'static str, msg: impl Into<String>) -> Self {
        let body = json!({ "error": msg.into() }).to_string().into_bytes();
        Response { status, content_type: "application/json", body }
    }

    fn text(status: &'static str, msg: &str) -> Self {
        Response { status, content_type: "text/plain; charset=utf-8", body: msg.as_bytes().to_vec() }
    }
}

struct Server {
    port: u16,
    token: String,
    engine: Arc<Mutex<Engine>>,
}

impl Server {
    fn handle(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let Some(req) = read_request(&mut stream) else { return };
        let resp = self.route(&req);
        let head = format!(
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
             X-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n\
             Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src data:\r\n\
             Connection: close\r\n\r\n",
            resp.status,
            resp.content_type,
            resp.body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&resp.body);
    }

    fn origin_ok(&self, req: &Request) -> bool {
        let hosts = [format!("127.0.0.1:{}", self.port), format!("localhost:{}", self.port)];
        let host_ok = req.header("host").is_some_and(|h| hosts.iter().any(|x| x == h));
        let origin_ok = req
            .header("origin")
            .is_none_or(|o| hosts.iter().any(|x| o == format!("http://{x}")));
        host_ok && origin_ok
    }

    fn route(&self, req: &Request) -> Response {
        if !self.origin_ok(req) {
            return Response::text("403 Forbidden", "Forbidden");
        }
        if req.method == "GET" && req.path == "/" {
            return if req.query_param("t") == Some(self.token.as_str()) {
                let logo = format!("data:image/png;base64,{}", base64(LOGO_PNG));
                let page = PAGE.replace("{{LOGO}}", &logo);
                Response { status: "200 OK", content_type: "text/html; charset=utf-8", body: page.into_bytes() }
            } else {
                Response::text("403 Forbidden", "Open the editor from the Secretarybird tray menu.")
            };
        }
        if !req.path.starts_with("/api/") {
            return Response::text("404 Not Found", "Not found");
        }
        if req.header("x-secbird-token") != Some(self.token.as_str()) {
            return Response::error("403 Forbidden", "bad token");
        }
        let mut engine = match self.engine.lock() {
            Ok(e) => e,
            Err(poisoned) => poisoned.into_inner(),
        };
        let result = match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/api/state") => Ok(state(&engine)),
            ("GET", "/api/apps") => Ok(json!({ "apps": crate::apps::running() })),
            ("GET", "/api/live") => {
                let (seq, dpi) = engine.live_dpi();
                Ok(json!({ "seq": seq, "dpi": dpi.iter().map(|(k, (x, y))| (k.clone(), json!([x, y]))).collect::<serde_json::Map<_, _>>() }))
            }
            ("POST", "/api/rescan") => {
                engine.rescan();
                Ok(state(&engine))
            }
            ("POST", "/api/windows-lighting") => {
                #[cfg(windows)]
                {
                    let explorer = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("explorer.exe");
                    std::process::Command::new(explorer).arg("ms-settings:personalization-lighting").spawn()
                        .map(|_| json!({})).map_err(|e| Error::System(e.to_string()))
                }
                #[cfg(not(windows))]
                { Err(Error::Unsupported("Windows only".into())) }
            }
            ("POST", path) => post(&mut engine, path, &req.body),
            _ => return Response::text("404 Not Found", "Not found"),
        };
        match result {
            Ok(v) => Response::json(v),
            Err(e) => Response::error("400 Bad Request", e.to_string()),
        }
    }
}

#[derive(Deserialize)]
struct Body {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    values: Vec<i16>,
    /// Device key (`PID:SERIAL`); older clients send only `pid`.
    #[serde(default)]
    id: String,
    #[serde(default)]
    pid: String,
    #[serde(default)]
    x: u16,
    #[serde(default)]
    y: u16,
    #[serde(default)]
    hz: u32,
    #[serde(default)]
    zone: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    value: u8,
    #[serde(default)]
    action: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default)]
    apps: Vec<String>,
    /// Button name, layer ("normal" / "hypershift") and action text ("" = reset).
    #[serde(default)]
    button: String,
    #[serde(default)]
    layer: String,
    #[serde(default)]
    assign: String,
}

/// Every button of a mouse with what it does on each layer, and which ones the active
/// profile remaps.
fn buttons_json(dev: &Device, store: &mut Store) -> Value {
    use secbird_core::buttons::{button_label, button_name, setting_key, Layer};
    let remapped = &store.for_device(dev).active().buttons;
    let list: Vec<Value> = dev
        .buttons()
        .into_iter()
        .map(|(id, normal)| {
            let name = button_name(id).unwrap_or("?");
            let hyper = dev.button(id, Layer::HyperShift).map(|a| a.to_string()).ok();
            let is_remapped = |layer| remapped.contains_key(&setting_key(name, layer));
            // What the mouse itself holds: the live value unless the active profile remapped it.
            let saved = |layer, live: Option<String>| match is_remapped(layer) {
                true => dev.saved_button(id, layer).map(|a| a.to_string()).ok(),
                false => live,
            };
            json!({
                "name": name,
                "label": button_label(id),
                "normal": normal.to_string(),
                "hypershift": hyper,
                "remapped": { "normal": is_remapped(Layer::Normal), "hypershift": is_remapped(Layer::HyperShift) },
                "saved": {
                    "normal": saved(Layer::Normal, Some(normal.to_string())),
                    "hypershift": saved(Layer::HyperShift, hyper.clone()),
                },
            })
        })
        .collect();
    json!({ "buttons": list })
}

fn post(engine: &mut Engine, path: &str, body: &[u8]) -> Result<Value, Error> {
    let b: Body = serde_json::from_slice(body).map_err(|e| Error::InvalidArg(e.to_string()))?;
    if path == "/api/sync" {
        return sync(engine, &b.color);
    }
    let target = if b.id.is_empty() { b.pid.to_ascii_uppercase() } else { b.id.clone() };

    // A device can disappear between requests; rescan once and retry before giving up.
    let attempt = |engine: &Engine| -> Result<Value, Error> {
        let dev = engine
            .devices()
            .iter()
            .find(|d| d.key() == target || format!("{:04X}", d.pid) == target)
            .ok_or_else(|| Error::NotFound(format!("device {target}")))?;
        let mut store = Store::load(&config_dir().join("profiles.json"))?;
        match path {
            "/api/lighting-owner" => {
                actions::set_windows_lighting(dev, &mut store, b.enabled)?;
                let failures = if b.enabled { Vec::new() } else {
                    let saved = store.for_device(dev).active();
                    let lighting = secbird_core::Profile { lighting: saved.lighting.clone(), keys: saved.keys.clone(), ..Default::default() };
                    lighting.apply(dev).into_iter()
                        .filter_map(|a| a.result.err().map(|e| e.to_string())).collect::<Vec<_>>()
                };
                Ok(json!({ "device": device_json(dev), "failures": failures }))
            }
            "/api/gamepad" => {
                secbird_core::windows_device::set_gamepad(dev, b.enabled)?;
                Ok(json!({ "device": device_json(dev) }))
            }
            "/api/audio" => {
                actions::set_audio(dev, &mut store, &b.name, b.values.clone())?;
                Ok(json!({ "device": device_json(dev) }))
            }
            "/api/dpi" => {
                let (x, y) = actions::set_dpi(dev, &mut store, b.x, b.y)?;
                Ok(json!({ "dpi": [x, y] }))
            }
            "/api/polling" => actions::set_polling(dev, &mut store, b.hz).map(|_| json!({})),
            "/api/color" => {
                actions::set_color(dev, &mut store, &b.zone, b.color.parse::<Rgb>()?).map(|_| json!({}))
            }
            "/api/brightness" => actions::set_brightness(dev, &mut store, &b.zone, b.value).map(|_| json!({})),
            "/api/off" => actions::lights_off(dev, &mut store, &b.zone).map(|_| json!({})),
            "/api/keys" => {
                let keys = b.keys.iter().map(|k| k.parse::<Rgb>()).collect::<Result<Vec<_>, _>>()?;
                actions::set_keys(dev, &mut store, keys).map(|_| json!({}))
            }
            "/api/buttons" => Ok(buttons_json(dev, &mut store)),
            "/api/button" => {
                let layer = match b.layer.as_str() {
                    "hypershift" => secbird_core::buttons::Layer::HyperShift,
                    _ => secbird_core::buttons::Layer::Normal,
                };
                let action = match b.assign.trim() {
                    "" => None,
                    text => Some(text.parse()?),
                };
                actions::set_button(dev, &mut store, &b.button, layer, action)?;
                Ok(buttons_json(dev, &mut store))
            }
            "/api/profile" => {
                let profiles = store.for_device(dev);
                match b.action.as_str() {
                    "new" => profiles.create(b.name.trim())?,
                    "delete" => profiles.delete(&b.name)?,
                    "use" => profiles.switch(&b.name)?,
                    "apps" => profiles.set_apps(&b.name, &b.apps)?,
                    other => return Err(Error::InvalidArg(format!("unknown action '{other}'"))),
                }
                let switched = b.action == "use";
                let profiles = profiles.clone();
                store.save()?;
                let failures: Vec<String> = if switched {
                    profiles
                        .apply(&profiles.active, dev)
                        .into_iter()
                        .filter_map(|a| a.result.err().map(|e| format!("{}: {e}", a.setting)))
                        .collect()
                } else {
                    Vec::new()
                };
                Ok(json!({ "device": device_json(dev), "failures": failures }))
            }
            _ => Err(Error::NotFound(path.to_string())),
        }
    };

    match attempt(engine) {
        Err(Error::Hid(_)) | Err(Error::NotFound(_)) | Err(Error::NoReply(_)) => {
            engine.rescan();
            attempt(engine)
        }
        other => other,
    }
}

/// One color on every zone of every connected device.
fn sync(engine: &mut Engine, color: &str) -> Result<Value, Error> {
    let color: Rgb = color.parse()?;
    let mut store = Store::load(&config_dir().join("profiles.json"))?;
    let results = actions::sync_color(engine.devices(), &mut store, color);
    let zones = results.iter().filter(|r| r.result.is_ok()).count();
    let mut pids: Vec<&str> = results.iter().filter(|r| r.result.is_ok()).map(|r| r.key.as_str()).collect();
    pids.dedup();
    let failed: Vec<String> = results
        .iter()
        .filter_map(|r| r.result.as_ref().err().map(|e| format!("{} {}: {e}", r.device, r.zone)))
        .collect();
    Ok(json!({ "zones": zones, "devices": pids.len(), "failed": failed, "state": state(engine) }))
}

fn state(engine: &Engine) -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "devices": engine.devices().iter().map(device_json).collect::<Vec<_>>(),
        "unsupported": engine.unsupported().iter().map(|u| json!({
            "pid": format!("{:04X}", u.pid), "name": u.name, "reason": u.reason,
        })).collect::<Vec<_>>(),
        "synapse": engine.synapse(),
    })
}

fn device_json(dev: &Device) -> Value {
    let (audio, audio_errors) = dev.audio_state();
    let spec = dev.spec.as_ref();
    let (profile_names, active) = Engine::profiles(dev);
    let all = Store::load(&config_dir().join("profiles.json")).ok().map(|mut s| s.for_device(dev).clone());
    let profile = all.as_ref().and_then(|p| serde_json::to_value(p.active()).ok()).unwrap_or(Value::Null);
    // Every profile's settings, so the editor can show them side by side.
    let windows_lighting = all.as_ref().map(|p| p.windows_lighting).unwrap_or(true);
    let profiles_data = all.and_then(|p| serde_json::to_value(p.profiles).ok()).unwrap_or(Value::Null);
    let dpi_range = spec.and_then(|s| s.dpi.as_ref());
    let polling_options: &[u32] = match spec.and_then(|s| s.polling) {
        Some(secbird_core::db::Polling::High) => &[125, 250, 500, 1000, 2000, 4000, 8000],
        _ => &[125, 500, 1000],
    };
    let kind = match spec.map(|s| s.kind) {
        Some(Kind::Mouse) => "mouse",
        Some(Kind::Keyboard) => "keyboard",
        Some(Kind::Headset) => "headset",
        Some(Kind::Mousepad) => "mousepad",
        Some(Kind::Keypad) => "keypad",
        Some(Kind::Laptop) => "laptop",
        Some(Kind::Accessory) => "accessory",
        None => "unknown",
    };
    json!({
        "image": PHOTOS.iter().find(|(pid, _, _)| *pid == dev.pid)
            .map(|(_, mime, bytes)| format!("data:{mime};base64,{}", base64(bytes))),
        "id": dev.key(),
        "pid": format!("{:04X}", dev.pid),
        "name": dev.name(),
        "kind": kind,
        "known": spec.is_some(),
        "firmware": dev.firmware().ok().map(|(a, b)| format!("{a}.{b}")),
        "serial": dev.serial().ok(),
        "dpi": dev.dpi().ok().map(|(x, y)| json!({
            "x": x, "y": y,
            "min": dpi_range.map_or(100, |r| r.min),
            "max": dpi_range.map_or(20000, |r| r.max),
            "step": dpi_range.map_or(50, |r| r.step),
        })),
        // Mousepads and docks answer the polling question too, but have no polling rate to
        // set: only show it where the device list says there is one (or the model is unknown).
        "polling": spec
            .is_none_or(|s| s.polling.is_some())
            .then(|| dev.polling_rate().ok())
            .flatten()
            .map(|hz| json!({ "hz": hz, "options": polling_options })),
        "battery": dev.battery().ok().flatten(),
        "matrix": spec.and_then(|s| s.matrix).map(|m| json!({ "rows": m.rows, "cols": m.cols })),
        "leds": spec.map(|s| s.leds.iter().map(|l| json!({ "name": l.name, "id": l.id })).collect::<Vec<_>>()).unwrap_or_default(),
        "profiles": profile_names,
        "active": active,
        "profile": profile,
        "profilesData": profiles_data,
        "audio": audio,
        "audio_errors": audio_errors,
        "windows_lighting": windows_lighting,
        "lamp": secbird_core::lamp::for_device(dev).ok().flatten(),
        "gamepads": secbird_core::windows_device::gamepads(dev).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::base64;

    /// Manual smoke test, separate from the user's tray instance. Never applies profiles.
    /// cargo test -p secbird-tray editor_browser -- --ignored --nocapture
    #[test]
    #[ignore]
    fn editor_browser() {
        let mut engine = crate::engine::Engine::new().unwrap();
        engine.rescan();
        let editor = super::start(std::sync::Arc::new(std::sync::Mutex::new(engine))).unwrap();
        println!("EDITOR_TEST_URL={}", editor.url);
        loop { std::thread::park(); }
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
