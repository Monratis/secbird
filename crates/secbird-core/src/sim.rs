//! Simulated Razer devices that answer Protocol 2.5 the way real hardware does.
//!
//! Tests use them to exercise discovery, retries, lighting and profiles without a
//! device on the desk. Setting `SECBIRD_SIM=mouse,wireless,keyboard,old-keyboard` also makes the
//! CLI and tray show them, which is handy for working on the editor.
//!
//! The simulated product ids (`FFF1`-`FFF3`) are outside Razer's range, so they
//! can't be confused with real devices or their saved profiles.

use crate::commands as cmd;
use crate::db::DeviceSpec;
use crate::device::{Device, TXID_CANDIDATES};
use crate::error::{Error, Result};
use crate::proto::{checksum, Command, Report, Status, REPORT_LEN};
use crate::transport::Transport;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub const MOUSE_PID: u16 = 0xFFF1;
pub const WIRELESS_MOUSE_PID: u16 = 0xFFF2;
pub const KEYBOARD_PID: u16 = 0xFFF3;
pub const OLD_KEYBOARD_PID: u16 = 0xFFF4;

const MOUSE_SPEC: &str = r#"{"pid":"FFF1","name":"Simulated Mouse","kind":"mouse","interface":0,
  "dpi":{"min":100,"max":26000,"step":50},"polling":"standard",
  "leds":[{"name":"scroll","id":1},{"name":"logo","id":4},{"name":"underglow","id":10}],
  "source":"simulator"}"#;

const WIRELESS_MOUSE_SPEC: &str = r#"{"pid":"FFF2","name":"Simulated Wireless Mouse","kind":"mouse",
  "interface":0,"dpi":{"min":100,"max":30000,"step":50},"polling":"standard",
  "leds":[{"name":"logo","id":4}],"wireless":true,"source":"simulator"}"#;

const OLD_KEYBOARD_SPEC: &str = r#"{"pid":"FFF4","name":"Simulated Keyboard (older)","kind":"keyboard",
  "interface":0,"polling":"standard","lighting":"standard",
  "leds":[{"name":"backlight","id":5},{"name":"logo","id":4}],
  "matrix":{"rows":6,"cols":22},"source":"simulator"}"#;

const KEYBOARD_SPEC: &str = r#"{"pid":"FFF3","name":"Simulated Keyboard","kind":"keyboard",
  "interface":0,"polling":"standard","leds":[{"name":"backlight","id":5}],
  "matrix":{"rows":6,"cols":22},"source":"simulator"}"#;

/// Everything the simulated device remembers. Tests read and tweak it via [`Sim::state`].
#[derive(Debug, Clone)]
pub struct SimState {
    /// The only transaction id the device answers; others get no reply.
    pub txid: u8,
    pub serial: String,
    pub firmware: (u8, u8),
    /// `None` = the device has no DPI sensor (keyboards).
    pub dpi: Option<(u16, u16)>,
    pub polling_code: u8,
    pub polling_hs: Option<[u8; 2]>,
    pub battery: Option<u8>,
    pub charging: bool,
    /// Mouse reported by a HyperPolling receiver; None = command unsupported.
    pub paired_mouse: Option<u16>,
    /// LED ids that exist on the device.
    pub leds: Vec<u8>,
    pub brightness: BTreeMap<u8, u8>,
    /// Last effect per LED: `[effect, flags, rate, ...colors]`.
    pub effects: BTreeMap<u8, Vec<u8>>,
    /// Per-key colors `rows x cols`, row-major.
    pub matrix: Option<(u8, u8, Vec<[u8; 3]>)>,
    /// Speaks the older `03:xx` lighting commands instead of `0F:xx`.
    pub standard: bool,
    /// Older command set: per-LED on/off and color, and the whole-grid effect.
    pub led_on: BTreeMap<u8, bool>,
    pub led_rgb: BTreeMap<u8, [u8; 3]>,
    pub grid_effect: Vec<u8>,
    /// Button assignments by (slot, button id, layer): (function, data).
    pub buttons: BTreeMap<(u8, u8, u8), (u8, [u8; 5])>,
    /// Answer this many reads with Busy before the real reply.
    pub busy_reads: u32,
    /// Every well-formed command received, in order.
    pub log: Vec<Command>,
    reply: Option<Report>,
}

impl SimState {
    fn new(serial: &str) -> Self {
        SimState {
            txid: 0x1F,
            serial: serial.into(),
            firmware: (1, 2),
            dpi: None,
            polling_code: 0x01,
            polling_hs: None,
            battery: None,
            charging: false,
            paired_mouse: None,
            leds: Vec::new(),
            brightness: BTreeMap::new(),
            effects: BTreeMap::new(),
            matrix: None,
            standard: false,
            led_on: BTreeMap::new(),
            led_rgb: BTreeMap::new(),
            grid_effect: Vec::new(),
            buttons: BTreeMap::new(),
            busy_reads: 0,
            log: Vec::new(),
            reply: None,
        }
    }

    /// Per-key color at `(row, col)`.
    pub fn key(&self, row: u8, col: u8) -> Option<[u8; 3]> {
        let (rows, cols, frame) = self.matrix.as_ref()?;
        (row < *rows && col < *cols).then(|| frame[row as usize * *cols as usize + col as usize])
    }

    fn handle(&mut self, c: Command, a: &[u8], size: u8) -> (Status, Vec<u8>) {
        use Status::{Failed, NotSupported, Success};
        let ok = |v: Vec<u8>| (Success, v);
        // Each device speaks one lighting generation and ignores the other.
        let old_lighting = c.class == 0x03 && c.id & 0x80 == 0;
        if (self.standard && c.class == 0x0F) || (!self.standard && old_lighting) {
            return (NotSupported, vec![]);
        }
        let known = |s: &Self, led: u8| s.leds.contains(&led);
        match c {
            c if c == cmd::WIRELESS_DEVICES.get() => match self.paired_mouse {
                Some(pid) => {
                    let [hi, lo] = pid.to_be_bytes();
                    ok(vec![2, 1, hi, lo, 0, 0xFF, 0xFF])
                }
                None => (NotSupported, vec![]),
            },
            c if c == cmd::FIRMWARE.get() => ok(vec![self.firmware.0, self.firmware.1]),
            c if c == cmd::SERIAL.get() => {
                let mut v = self.serial.as_bytes().to_vec();
                v.resize(cmd::SERIAL_SIZE as usize, 0);
                ok(v)
            }
            c if c == cmd::DPI.get() => match self.dpi {
                Some((x, y)) => {
                    let ([xh, xl], [yh, yl]) = (x.to_be_bytes(), y.to_be_bytes());
                    ok(vec![a[0], xh, xl, yh, yl, 0, 0])
                }
                None => (NotSupported, vec![]),
            },
            cmd::DPI => {
                let x = u16::from_be_bytes([a[1], a[2]]);
                let y = u16::from_be_bytes([a[3], a[4]]);
                match self.dpi {
                    None => (NotSupported, vec![]),
                    Some(_) if x == 0 || y == 0 => (Failed, vec![]),
                    Some(_) => {
                        self.dpi = Some((x, y));
                        ok(a[..7].to_vec())
                    }
                }
            }
            c if c == cmd::BUTTON.get() => match self.buttons.get(&(a[0], a[1], a[2])) {
                // Like the real mouse: size byte 1 whatever the function's data length.
                Some(&(function, d)) => ok(vec![a[0], a[1], a[2], function, 1, d[0], d[1], d[2], d[3], d[4]]),
                None => (Failed, vec![]),
            },
            cmd::BUTTON => {
                if !self.buttons.contains_key(&(1, a[1], a[2])) {
                    return (Failed, vec![]);
                }
                self.buttons.insert((a[0], a[1], a[2]), (a[3], [a[5], a[6], a[7], a[8], a[9]]));
                ok(a[..10].to_vec())
            }
            c if c == cmd::ACTIVE_SLOT.get() => match self.buttons.is_empty() {
                true => (NotSupported, vec![]),
                false => ok(vec![1]),
            },
            c if c == cmd::POLLING.get() => ok(vec![self.polling_code]),
            c if c == cmd::POLLING_HS.get() && a[0] < 2 => match self.polling_hs {
                Some(rates) => ok(vec![a[0], rates[a[0] as usize]]),
                None => (NotSupported, vec![]),
            },
            cmd::POLLING_HS if a[0] < 2 && cmd::polling_hs_hz(a[1]).is_some() => match &mut self.polling_hs {
                Some(rates) => { rates[a[0] as usize] = a[1]; ok(a[..2].to_vec()) }
                None => (NotSupported, vec![]),
            },
            cmd::POLLING => {
                if cmd::polling_hz(a[0]).is_none() {
                    return (Failed, vec![]);
                }
                self.polling_code = a[0];
                ok(vec![a[0]])
            }
            c if c == cmd::BATTERY.get() => match self.battery {
                Some(level) => ok(vec![0, level]),
                None => (NotSupported, vec![]),
            },
            c if c == cmd::CHARGING.get() => match self.battery {
                Some(_) => ok(vec![0, self.charging as u8]),
                None => (NotSupported, vec![]),
            },
            c if c == cmd::MATRIX_BRIGHTNESS.get() => match self.brightness.get(&a[1]) {
                Some(&v) => ok(vec![a[0], a[1], v]),
                None if self.leds.contains(&a[1]) => ok(vec![a[0], a[1], 255]),
                None => (Failed, vec![]),
            },
            cmd::MATRIX_BRIGHTNESS => {
                if !self.leds.contains(&a[1]) {
                    return (Failed, vec![]);
                }
                self.brightness.insert(a[1], a[2]);
                ok(a[..3].to_vec())
            }
            cmd::MATRIX_EFFECT => {
                // LED 0 addresses the whole device (used by the custom-frame effect).
                let led = a[1];
                if led != 0 && !self.leds.contains(&led) {
                    return (Failed, vec![]);
                }
                if a[2] == cmd::EFFECT_CUSTOM_FRAME && self.matrix.is_none() {
                    return (NotSupported, vec![]);
                }
                self.effects.insert(led, a[2..size as usize].to_vec());
                ok(a[..size as usize].to_vec())
            }
            cmd::MATRIX_FRAME => {
                let Some((rows, cols, frame)) = self.matrix.as_mut() else {
                    return (NotSupported, vec![]);
                };
                let (row, start, end) = (a[2], a[3] as usize, a[4] as usize);
                let keys = end.wrapping_sub(start).wrapping_add(1);
                if row >= *rows || end < start || end >= *cols as usize || size as usize != 5 + 3 * keys {
                    return (Failed, vec![]);
                }
                let base = row as usize * *cols as usize;
                for (i, rgb) in a[5..5 + 3 * keys].chunks(3).enumerate() {
                    frame[base + start + i] = [rgb[0], rgb[1], rgb[2]];
                }
                ok(vec![])
            }
            cmd::LED_STATE | cmd::LED_RGB | cmd::LED_EFFECT | cmd::LED_BRIGHTNESS if !known(self, a[1]) => {
                (Failed, vec![])
            }
            cmd::LED_STATE => {
                self.led_on.insert(a[1], a[2] != 0);
                ok(a[..3].to_vec())
            }
            cmd::LED_RGB => {
                self.led_rgb.insert(a[1], [a[2], a[3], a[4]]);
                ok(a[..5].to_vec())
            }
            cmd::LED_EFFECT => {
                self.effects.insert(a[1], vec![a[2]]);
                ok(a[..3].to_vec())
            }
            cmd::LED_BRIGHTNESS => {
                self.brightness.insert(a[1], a[2]);
                ok(a[..3].to_vec())
            }
            cmd::GRID_EFFECT => {
                if self.matrix.is_none() {
                    return (NotSupported, vec![]);
                }
                self.grid_effect = a[..size as usize].to_vec();
                ok(self.grid_effect.clone())
            }
            cmd::GRID_FRAME => {
                let Some((rows, cols, frame)) = self.matrix.as_mut() else {
                    return (NotSupported, vec![]);
                };
                let (row, start, end) = (a[1], a[2] as usize, a[3] as usize);
                let keys = end.wrapping_sub(start).wrapping_add(1);
                if size != cmd::GRID_FRAME_SIZE || a[0] != 0xFF || row >= *rows || end < start
                    || end >= *cols as usize || 4 + 3 * keys > 80
                {
                    return (Failed, vec![]);
                }
                let base = row as usize * *cols as usize;
                for (i, rgb) in a[4..4 + 3 * keys].chunks(3).enumerate() {
                    frame[base + start + i] = [rgb[0], rgb[1], rgb[2]];
                }
                ok(vec![])
            }
            _ => (NotSupported, vec![]),
        }
    }
}

/// A simulated device. Clones share state, so a test can keep one and hand another
/// to a [`Device`].
#[derive(Clone)]
pub struct Sim(Arc<Mutex<SimState>>);

impl Sim {
    pub fn new(state: SimState) -> Self {
        Sim(Arc::new(Mutex::new(state)))
    }

    /// Wired mouse shaped like a Basilisk V3: DPI, polling, three lighting zones.
    pub fn mouse() -> Self {
        let mut s = SimState::new("SIMMOUSE0001");
        s.dpi = Some((800, 800));
        s.leds = vec![1, 4, 10];
        // The Basilisk V3's factory buttons, as read from the real mouse, in the live
        // slot and the active saved slot, both layers.
        let factory: [(u8, u8, [u8; 5]); 9] = [
            (1, 1, [1, 0, 0, 0, 0]),
            (2, 1, [2, 0, 0, 0, 0]),
            (3, 1, [3, 0, 0, 0, 0]),
            (4, 1, [4, 0, 0, 0, 0]),
            (5, 1, [5, 0, 0, 0, 0]),
            (14, 7, [4, 0, 0, 0, 0]),
            (15, 6, [5, 0x01, 0x90, 0x01, 0x90]),
            (52, 14, [0x68, 0, 0x14, 0, 0]),
            (96, 6, [6, 0, 0, 0, 0]),
        ];
        for slot in [0, 1] {
            for layer in [0, 1] {
                for (id, function, data) in factory {
                    s.buttons.insert((slot, id, layer), (function, data));
                }
            }
        }
        Sim::new(s)
    }

    pub fn wireless_mouse() -> Self {
        let mut s = SimState::new("SIMWLMOUSE01");
        s.txid = 0x3F;
        s.dpi = Some((1600, 1600));
        s.battery = Some(191); // 75 %
        s.charging = true;
        s.leds = vec![4];
        Sim::new(s)
    }

    /// Full-size keyboard: one backlight zone and a 6 x 22 per-key grid.
    pub fn keyboard() -> Self {
        let mut s = SimState::new("SIMKEYBOARD1");
        s.txid = 0xFF;
        s.leds = vec![5];
        s.matrix = Some((6, 22, vec![[0; 3]; 6 * 22]));
        Sim::new(s)
    }

    /// Pre-2019 style keyboard: older `03:xx` lighting commands, a logo LED and a 6 x 22 grid.
    pub fn old_keyboard() -> Self {
        let mut s = SimState::new("SIMOLDKEYBD1");
        s.txid = 0xFF;
        s.standard = true;
        s.leds = vec![5, 4];
        s.matrix = Some((6, 22, vec![[0; 3]; 6 * 22]));
        Sim::new(s)
    }

    pub fn state(&self) -> MutexGuard<'_, SimState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The database entry this simulator pretends to be, if it's one of the built-ins.
    pub fn spec_for(pid: u16) -> Option<DeviceSpec> {
        let json = match pid {
            MOUSE_PID => MOUSE_SPEC,
            WIRELESS_MOUSE_PID => WIRELESS_MOUSE_SPEC,
            KEYBOARD_PID => KEYBOARD_SPEC,
            OLD_KEYBOARD_PID => OLD_KEYBOARD_SPEC,
            _ => return None,
        };
        Some(serde_json::from_str(json).expect("built-in simulator spec"))
    }

    /// Open as a [`Device`], probing the txid like real discovery does.
    pub fn open(&self, pid: u16, spec: Option<DeviceSpec>) -> Option<Device> {
        let path = CString::new(format!("sim:{pid:04X}")).expect("no NUL");
        Device::open(Box::new(self.clone()), pid, 0, path, &TXID_CANDIDATES, spec)
    }
}

impl Transport for Sim {
    fn send_feature_report(&self, data: &[u8]) -> Result<()> {
        if data.len() != REPORT_LEN + 1 || data[0] != 0 {
            return Err(Error::InvalidArg(format!("simulator: bad report ({} bytes)", data.len())));
        }
        let mut body = [0u8; REPORT_LEN];
        body.copy_from_slice(&data[1..]);
        let req = Report(body);
        let mut s = self.state();
        let command = req.command();

        if req.txid() != s.txid {
            s.reply = None; // a real device ignores other transaction ids
            return Ok(());
        }
        let (status, args) = if checksum(&body) != body[88] {
            (Status::Failed, Vec::new())
        } else {
            s.log.push(command);
            s.handle(command, req.args(), body[5])
        };
        let mut reply = Report::request(req.txid(), command, body[5], &args);
        reply.0[0] = match status {
            Status::New => 0,
            Status::Busy => 1,
            Status::Success => 2,
            Status::Failed => 3,
            Status::Timeout => 4,
            Status::NotSupported => 5,
            Status::Unknown(b) => b,
        };
        reply.0[88] = checksum(&reply.0);
        s.reply = Some(reply);
        Ok(())
    }

    fn get_feature_report(&self, buf: &mut [u8]) -> Result<usize> {
        let mut s = self.state();
        let mut out = s.reply.clone().map(|r| r.0).unwrap_or([0; REPORT_LEN]);
        if s.reply.is_some() && s.busy_reads > 0 {
            s.busy_reads -= 1;
            out[0] = 1;
        }
        let n = buf.len().min(REPORT_LEN + 1);
        buf[0] = 0;
        buf[1..n].copy_from_slice(&out[..n - 1]);
        Ok(n)
    }

    fn read_delay(&self) -> Duration {
        Duration::ZERO
    }
}

/// Simulated devices listed in `SECBIRD_SIM` (comma-separated: mouse, wireless, keyboard, old-keyboard).
pub fn from_env() -> Vec<Device> {
    let Ok(list) = std::env::var("SECBIRD_SIM") else { return Vec::new() };
    list.split(',')
        .filter_map(|name| {
            let (sim, pid) = match name.trim() {
                "mouse" => (Sim::mouse(), MOUSE_PID),
                "wireless" => (Sim::wireless_mouse(), WIRELESS_MOUSE_PID),
                "keyboard" => (Sim::keyboard(), KEYBOARD_PID),
                "old-keyboard" => (Sim::old_keyboard(), OLD_KEYBOARD_PID),
                _ => return None,
            };
            sim.open(pid, Sim::spec_for(pid))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::Rgb;
    use crate::profile::{Profile, Zone};

    fn open(sim: &Sim, pid: u16) -> Device {
        sim.open(pid, Sim::spec_for(pid)).expect("simulator answers")
    }

    #[test]
    fn discovery_finds_each_txid() {
        for (sim, pid, txid) in [
            (Sim::mouse(), MOUSE_PID, 0x1F),
            (Sim::wireless_mouse(), WIRELESS_MOUSE_PID, 0x3F),
            (Sim::keyboard(), KEYBOARD_PID, 0xFF),
        ] {
            let dev = open(&sim, pid);
            assert_eq!(dev.txid, txid);
            assert!(dev.serial().unwrap().starts_with("SIM"));
        }
    }

    #[test]
    fn unknown_txid_is_not_a_device() {
        let sim = Sim::mouse();
        sim.state().txid = 0x42;
        assert!(sim.open(MOUSE_PID, None).is_none());
    }

    #[test]
    fn busy_replies_are_retried() {
        let sim = Sim::mouse();
        let dev = open(&sim, MOUSE_PID);
        sim.state().busy_reads = 5;
        assert_eq!(dev.firmware().unwrap(), (1, 2));
        sim.state().busy_reads = 100;
        assert!(matches!(dev.firmware(), Err(Error::NoReply(_))));
    }

    #[test]
    fn bad_checksum_is_rejected() {
        let sim = Sim::mouse();
        let mut req = Report::request(0x1F, cmd::FIRMWARE.get(), 2, &[]);
        req.0[88] ^= 0xFF;
        let mut data = vec![0u8];
        data.extend_from_slice(&req.0);
        sim.send_feature_report(&data).unwrap();
        let mut buf = [0u8; REPORT_LEN + 1];
        sim.get_feature_report(&mut buf).unwrap();
        assert_eq!(buf[1], 3, "status Failed");
        assert!(sim.state().log.is_empty());
    }

    #[test]
    fn mouse_settings_round_trip() {
        let sim = Sim::mouse();
        let dev = open(&sim, MOUSE_PID);
        assert_eq!(dev.set_dpi(1234, 1234).unwrap(), (1250, 1250));
        assert_eq!(dev.dpi().unwrap(), (1250, 1250));
        dev.set_polling_rate(500).unwrap();
        assert_eq!(dev.polling_rate().unwrap(), 500);
        assert!(dev.set_polling_rate(333).is_err());
        assert_eq!(dev.battery().unwrap(), None);

        dev.set_static_color(4, Rgb(0x12, 0x34, 0x56)).unwrap();
        dev.set_brightness(4, 50).unwrap();
        dev.lights_off(10).unwrap();
        let s = sim.state();
        assert_eq!(s.effects[&4], [cmd::EFFECT_STATIC, 0, 0, 1, 0x12, 0x34, 0x56]);
        assert_eq!(s.brightness[&4], 127);
        assert_eq!(s.effects[&10][0], cmd::EFFECT_OFF);
    }

    #[test]
    fn buttons_remap_reset_and_follow_profiles() {
        use crate::buttons::{Action, Layer};
        use crate::profile::DeviceProfiles;
        let sim = Sim::mouse();
        let dev = open(&sim, MOUSE_PID);
        let found: Vec<u8> = dev.buttons().iter().map(|b| b.0).collect();
        assert_eq!(found, [1, 2, 3, 4, 5, 14, 15, 52, 96]);
        assert_eq!(dev.button(15, Layer::Normal).unwrap(), Action::DpiClutch { x: 400, y: 400 });

        let key_a: Action = "key:a".parse().unwrap();
        assert!(dev.set_button(5, Layer::Normal, key_a).unwrap());
        assert!(!dev.set_button(5, Layer::Normal, key_a).unwrap(), "unchanged buttons are not rewritten");
        assert_eq!(dev.button(5, Layer::Normal).unwrap(), key_a);
        assert_eq!(sim.state().buttons[&(1, 5, 0)].0, 1, "saved slot untouched");
        dev.reset_button(5, Layer::Normal).unwrap();
        assert_eq!(dev.button(5, Layer::Normal).unwrap(), Action::Mouse(5));
        assert!(dev.set_button(77, Layer::Normal, key_a).is_err(), "unknown button");

        // Switching to a profile without the remap puts the button back.
        let mut p = DeviceProfiles::default();
        p.create("Gaming").unwrap();
        p.profiles.get_mut("Gaming").unwrap().buttons.insert("side-forward/hypershift".into(), "turbo:left@10".into());
        assert!(p.apply("Gaming", &dev).iter().all(|a| a.result.is_ok()));
        assert_eq!(dev.button(5, Layer::HyperShift).unwrap(), Action::TurboMouse { button: 1, rate: 10 });
        assert!(p.apply("Default", &dev).iter().all(|a| a.result.is_ok()));
        assert_eq!(dev.button(5, Layer::HyperShift).unwrap(), Action::Mouse(5));
    }

    #[test]
    fn wireless_reports_battery() {
        let dev = open(&Sim::wireless_mouse(), WIRELESS_MOUSE_PID);
        assert_eq!(dev.battery().unwrap(), Some(75));
        assert!(dev.charging().unwrap());
    }

    #[test]
    fn hyperpolling_identifies_mouse_without_changing_transport_or_profiles() {
        let db = crate::Db::builtin().unwrap();
        let sim = Sim::wireless_mouse();
        sim.state().polling_hs = Some([0x08; 2]);
        let mut dev = sim.open(0x00B3, db.get(0x00B3).cloned()).unwrap();
        let key = dev.key();
        dev.identify_receiver(&db);
        assert_eq!(dev.name(), "HyperPolling Wireless Dongle");
        sim.state().paired_mouse = Some(0x1234); // unknown mouse: no guessed name
        dev.identify_receiver(&db);
        assert_eq!(dev.name(), "HyperPolling Wireless Dongle");
        sim.state().paired_mouse = Some(0x00B7);
        dev.identify_receiver(&db);
        assert_eq!(dev.name(), "DeathAdder V3 Pro (Wireless) (via HyperPolling)");
        assert_eq!(dev.key(), key);
        assert_eq!(dev.pid, 0x00B3);
        assert_eq!(dev.spec.as_ref().unwrap().polling, Some(crate::db::Polling::High));
        assert_eq!(dev.battery().unwrap(), Some(75));
        assert!(sim.state().log.iter().all(|c| c.id & 0x80 != 0));
        assert_eq!(dev.polling_rate().unwrap(), 1000);
        dev.set_polling_rate(2000).unwrap();
        assert_eq!(dev.polling_rate().unwrap(), 2000);
        assert_eq!(sim.state().polling_hs, Some([0x04; 2]));
        assert!(dev.set_polling_rate(333).is_err());
        assert_eq!(sim.state().polling_hs, Some([0x04; 2]));
    }

    #[test]
    fn unknown_led_and_unsupported_commands_fail() {
        let sim = Sim::mouse();
        let dev = open(&sim, MOUSE_PID);
        assert!(matches!(dev.set_brightness(7, 50), Err(Error::Device { status: Status::Failed, .. })));
        let kb = open(&Sim::keyboard(), KEYBOARD_PID);
        assert!(matches!(kb.dpi(), Err(Error::Device { status: Status::NotSupported, .. })));
    }

    #[test]
    fn keyboard_shows_a_full_frame() {
        let sim = Sim::keyboard();
        let dev = open(&sim, KEYBOARD_PID);
        let m = dev.matrix().unwrap();
        let frame: Vec<Rgb> = (0..m.rows as usize * m.cols as usize)
            .map(|i| Rgb((i / m.cols as usize) as u8 * 40, (i % m.cols as usize) as u8 * 10, 7))
            .collect();
        dev.show_frame(&frame).unwrap();

        let s = sim.state();
        assert_eq!(s.key(0, 0), Some([0, 0, 7]));
        assert_eq!(s.key(5, 21), Some([200, 210, 7]));
        assert_eq!(s.effects[&0][0], cmd::EFFECT_CUSTOM_FRAME);
        // 6 rows + 1 effect command after the serial probe.
        assert_eq!(s.log.iter().filter(|c| **c == cmd::MATRIX_FRAME).count(), 6);
    }

    #[test]
    fn frame_size_is_checked() {
        let dev = open(&Sim::keyboard(), KEYBOARD_PID);
        assert!(matches!(dev.show_frame(&[Rgb(1, 2, 3); 10]), Err(Error::InvalidArg(_))));
        assert!(matches!(dev.set_row(6, 0, &[Rgb(1, 2, 3)]), Err(Error::InvalidArg(_))));
        assert!(matches!(dev.set_row(0, 20, &[Rgb(1, 2, 3); 3]), Err(Error::InvalidArg(_))));
        let mouse = open(&Sim::mouse(), MOUSE_PID);
        assert!(matches!(mouse.show_frame(&[]), Err(Error::Unsupported(_))));
    }

    #[test]
    fn wide_rows_are_split_into_packets() {
        let mut s = SimState::new("SIMWIDE00001");
        s.leds = vec![5];
        s.matrix = Some((1, 30, vec![[0; 3]; 30]));
        let sim = Sim::new(s);
        let mut spec = Sim::spec_for(KEYBOARD_PID).unwrap();
        spec.matrix = Some(crate::db::Matrix { rows: 1, cols: 30 });
        let dev = sim.open(KEYBOARD_PID, Some(spec)).unwrap();
        let row: Vec<Rgb> = (0..30).map(|i| Rgb(i, i, i)).collect();
        dev.set_row(0, 0, &row).unwrap();
        let s = sim.state();
        assert_eq!(s.log.iter().filter(|c| **c == cmd::MATRIX_FRAME).count(), 2);
        assert_eq!(s.key(0, 29), Some([29, 29, 29]));
        assert_eq!(s.key(0, 24), Some([24, 24, 24]));
    }

    #[test]
    fn profile_applies_to_simulated_mouse() {
        let sim = Sim::mouse();
        let dev = open(&sim, MOUSE_PID);
        let mut p = Profile { dpi: Some((3200, 3200)), polling: Some(125), ..Default::default() };
        p.lighting.insert("logo".into(), Zone { color: Some(Rgb(255, 0, 0)), off: false, brightness: Some(100) });
        p.lighting.insert("underglow".into(), Zone { color: None, off: true, brightness: None });
        let results = p.apply(&dev);
        assert!(results.iter().all(|r| r.result.is_ok()), "{:?}", results.iter().map(|r| &r.result).collect::<Vec<_>>());
        let s = sim.state();
        assert_eq!(s.dpi, Some((3200, 3200)));
        assert_eq!(s.polling_code, 0x08);
        assert_eq!(s.effects[&4][4..7], [255, 0, 0]);
        assert_eq!(s.effects[&10][0], cmd::EFFECT_OFF);
    }

    #[test]
    fn keyboard_backlight_zone_works() {
        let sim = Sim::keyboard();
        let dev = open(&sim, KEYBOARD_PID);
        let led = dev.led_id("backlight").unwrap();
        dev.set_static_color(led, Rgb(0, 255, 0)).unwrap();
        dev.set_brightness(led, 100).unwrap();
        assert_eq!(sim.state().brightness[&5], 255);
    }

    #[test]
    fn windows_lighting_blocks_actions_sync_and_profile_lighting_only() {
        use crate::{actions, Store};
        let sim = Sim::mouse();
        let dev = open(&sim, MOUSE_PID);
        let mut store = Store::default();
        assert!(actions::set_windows_lighting(&dev, &mut store, true).is_err());
        assert!(!store.for_device(&dev).windows_lighting);
        store.for_device(&dev).windows_lighting = true;
        sim.state().log.clear();
        assert!(actions::set_color(&dev, &mut store, "logo", Rgb(1, 2, 3)).is_err());
        assert!(actions::set_brightness(&dev, &mut store, "logo", 40).is_err());
        assert!(actions::lights_off(&dev, &mut store, "logo").is_err());
        assert!(actions::set_keys(&dev, &mut store, Vec::new()).is_err());
        assert!(actions::sync_color(std::slice::from_ref(&dev), &mut store, Rgb(1, 2, 3)).is_empty());
        assert!(sim.state().log.is_empty());
        let mut p = Profile { dpi: Some((1600, 1600)), ..Default::default() };
        p.lighting.insert("logo".into(), Zone { color: Some(Rgb(1, 2, 3)), ..Default::default() });
        assert!(p.apply_with_lighting(&dev, false).iter().all(|r| r.result.is_ok()));
        assert_eq!(sim.state().dpi, Some((1600, 1600)));
        assert!(sim.state().log.iter().all(|c| *c == cmd::DPI));
        store.for_device(&dev).create("Gaming").unwrap();
        store.for_device(&dev).switch("Gaming").unwrap();
        let text = serde_json::to_string(&store).unwrap();
        let mut restored: Store = serde_json::from_str(&text).unwrap();
        assert!(restored.for_device(&dev).windows_lighting);
    }

    #[test]
    fn painted_keys_are_saved_and_replace_zone_color() {
        use crate::{actions, Store};
        let path = std::env::temp_dir().join(format!("secbird-test-{}.json", std::process::id()));
        let mut store = Store::load(&path).unwrap();
        let sim = Sim::keyboard();
        let dev = open(&sim, KEYBOARD_PID);

        actions::set_color(&dev, &mut store, "backlight", Rgb(0, 0, 255)).unwrap();
        actions::set_brightness(&dev, &mut store, "backlight", 60).unwrap();
        let mut keys = vec![Rgb(0, 0, 0); 6 * 22];
        keys[23] = Rgb(255, 0, 0);
        actions::set_keys(&dev, &mut store, keys.clone()).unwrap();

        let saved = Store::load(&path).unwrap().for_device(&dev).active().clone();
        assert_eq!(saved.keys, keys);
        assert_eq!(saved.lighting["backlight"].color, None);
        assert_eq!(saved.lighting["backlight"].brightness, Some(60));
        assert_eq!(sim.state().key(1, 1), Some([255, 0, 0]));

        // Re-applying on a fresh device repaints the keys.
        let fresh = Sim::keyboard();
        let results = saved.apply(&open(&fresh, KEYBOARD_PID));
        assert!(results.iter().all(|r| r.result.is_ok()));
        assert_eq!(fresh.state().key(1, 1), Some([255, 0, 0]));

        // A whole-zone color clears the painted keys.
        actions::set_color(&dev, &mut store, "backlight", Rgb(9, 9, 9)).unwrap();
        assert!(Store::load(&path).unwrap().for_device(&dev).active().keys.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn older_keyboard_uses_the_03_command_set() {
        let sim = Sim::old_keyboard();
        let dev = open(&sim, OLD_KEYBOARD_PID);
        dev.set_static_color(5, Rgb(1, 2, 3)).unwrap();
        assert_eq!(sim.state().grid_effect, [cmd::GRID_STATIC, 1, 2, 3]);
        dev.lights_off(5).unwrap();
        assert_eq!(sim.state().grid_effect, [cmd::GRID_OFF]);

        dev.set_static_color(4, Rgb(9, 8, 7)).unwrap();
        dev.set_brightness(4, 100).unwrap();
        {
            let s = sim.state();
            assert_eq!(s.led_rgb[&4], [9, 8, 7]);
            assert_eq!(s.effects[&4], [cmd::CLASSIC_STATIC]);
            assert!(s.led_on[&4]);
            assert_eq!(s.brightness[&4], 255);
        }
        dev.lights_off(4).unwrap();
        assert!(!sim.state().led_on[&4]);

        let mut frame = vec![Rgb(0, 0, 0); 6 * 22];
        frame[22 * 5 + 21] = Rgb(255, 255, 255);
        dev.show_frame(&frame).unwrap();
        let s = sim.state();
        assert_eq!(s.key(5, 21), Some([255, 255, 255]));
        assert_eq!(s.grid_effect, [cmd::GRID_CUSTOM_FRAME, cmd::VARSTORE]);
        assert!(s.log.iter().all(|c| c.class != 0x0F), "no 0F:xx commands sent");
    }

    #[test]
    fn kraken_lighting_is_refused_cleanly() {
        let mut spec = Sim::spec_for(MOUSE_PID).unwrap();
        spec.lighting = crate::db::Lighting::Kraken;
        let dev = Sim::mouse().open(MOUSE_PID, Some(spec)).unwrap();
        assert!(matches!(dev.set_static_color(4, Rgb(1, 1, 1)), Err(Error::Unsupported(_))));
    }

    #[test]
    fn sync_colors_every_zone_on_every_device() {
        use crate::{actions, Store};
        let path = std::env::temp_dir().join(format!("secbird-sync-{}.json", std::process::id()));
        let mut store = Store::load(&path).unwrap();
        let sims = [
            (Sim::mouse(), MOUSE_PID),
            (Sim::wireless_mouse(), WIRELESS_MOUSE_PID),
            (Sim::keyboard(), KEYBOARD_PID),
            (Sim::old_keyboard(), OLD_KEYBOARD_PID),
        ];
        let devices: Vec<Device> = sims.iter().map(|(s, p)| open(s, *p)).collect();
        let results = actions::sync_color(&devices, &mut store, Rgb(0x11, 0x22, 0x33));
        assert_eq!(results.len(), 3 + 1 + 1 + 2);
        assert!(results.iter().all(|r| r.result.is_ok()));

        assert_eq!(sims[0].0.state().effects[&10][4..7], [0x11, 0x22, 0x33]);
        assert_eq!(sims[2].0.state().effects[&5][4..7], [0x11, 0x22, 0x33]);
        assert_eq!(sims[3].0.state().grid_effect, [cmd::GRID_STATIC, 0x11, 0x22, 0x33]);
        assert_eq!(sims[3].0.state().led_rgb[&4], [0x11, 0x22, 0x33]);
        assert_eq!(Store::load(&path).unwrap().devices.len(), 4);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn identical_devices_get_separate_keys() {
        let a = Sim::mouse();
        let b = Sim::mouse();
        b.state().serial = "SIMMOUSE0002".into();
        let (da, db) = (open(&a, MOUSE_PID), open(&b, MOUSE_PID));
        assert_eq!(da.key(), "FFF1:SIMMOUSE0001");
        assert_eq!(db.key(), "FFF1:SIMMOUSE0002");
        let mut store = crate::Store::default();
        store.for_device(&da).active_mut().dpi = Some((400, 400));
        assert_eq!(store.for_device(&db).active().dpi, None);
    }
}
