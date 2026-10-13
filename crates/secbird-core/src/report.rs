//! `secbird report`: a read-only description of connected Razer hardware that users
//! can share, so devices we don't own can be added to the database.
//!
//! Only GET commands are sent. Nothing on the device changes.

use crate::commands as cmd;
use crate::db::{Db, RAZER_VID};
use crate::device::{Device, TXID_CANDIDATES};
use crate::error::Error;
use crate::proto::Command;
use hidapi::HidApi;
use serde::Serialize;

/// One Razer HID interface as Windows lists it.
#[derive(Debug, Clone, Serialize)]
pub struct Interface {
    pub pid: String,
    pub interface: i32,
    pub usage_page: String,
    pub usage: String,
    /// Transaction id this interface answered Protocol 2.5 with, if any.
    pub txid: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub name: &'static str,
    pub command: String,
    /// "ok", or why it failed ("NotSupported", "no reply", ...).
    pub status: String,
    /// Reply arguments in hex, trailing zeros trimmed.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reply: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceReport {
    pub pid: String,
    pub name: String,
    pub in_database: bool,
    pub simulated: bool,
    pub interface: i32,
    pub txid: String,
    /// Shortened for privacy.
    pub serial: String,
    pub probes: Vec<Probe>,
    /// LED ids that answered a brightness GET.
    pub leds: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub secbird: &'static str,
    pub os: String,
    pub interfaces: Vec<Interface>,
    pub devices: Vec<DeviceReport>,
}

/// Read-only GETs: (name, command, size, args).
const PROBES: &[(&str, Command, u8, &[u8])] = &[
    ("firmware", cmd::FIRMWARE.get(), cmd::FIRMWARE_SIZE, &[]),
    ("device mode", cmd::DEVICE_MODE.get(), cmd::DEVICE_MODE_SIZE, &[]),
    ("polling rate", cmd::POLLING.get(), cmd::POLLING_SIZE, &[]),
    ("polling rate (high speed)", cmd::POLLING_HS.get(), cmd::POLLING_HS_SIZE, &[]),
    ("dpi", cmd::DPI.get(), cmd::DPI_SIZE, &[cmd::NOSTORE]),
    ("dpi stages", Command::new(0x04, 0x86), 0x26, &[cmd::VARSTORE]),
    ("battery", cmd::BATTERY.get(), cmd::POWER_SIZE, &[]),
    ("charging", cmd::CHARGING.get(), cmd::POWER_SIZE, &[]),
    ("idle time", Command::new(0x07, 0x83), 0x02, &[]),
];

/// LED ids tried with a brightness GET. Razer uses 0x00-0x14 across its products.
const LED_IDS: std::ops::RangeInclusive<u8> = 0x00..=0x14;

pub fn build(api: &HidApi, db: &Db) -> Report {
    let mut interfaces = Vec::new();
    let mut devices = Vec::new();

    let mut infos: Vec<_> = api.device_list().filter(|d| d.vendor_id() == RAZER_VID).collect();
    infos.sort_by_key(|d| (d.product_id(), d.interface_number(), d.usage_page(), d.usage()));
    infos.dedup_by(|a, b| a.path() == b.path());

    for info in infos {
        let pid = info.product_id();
        let spec = db.get(pid).cloned();
        let audio_interface = pid != crate::headset::PID || (info.usage_page() == 0xFF14 && info.usage() == 1);
        let dev = audio_interface.then(|| api.open_path(info.path()).ok()).flatten().and_then(|hid| {
            Device::open(Box::new(hid), pid, info.interface_number(), info.path().to_owned(), &TXID_CANDIDATES, spec)
        });
        interfaces.push(Interface {
            pid: format!("{pid:04X}"),
            interface: info.interface_number(),
            usage_page: format!("{:04X}", info.usage_page()),
            usage: format!("{:04X}", info.usage()),
            txid: dev.as_ref().map(|d| format!("{:02X}", d.txid)),
        });
        // One full probe per product, from the first interface that answers.
        if let Some(mut dev) = dev {
            dev.identify_receiver(db);
            if !devices.iter().any(|d: &DeviceReport| d.pid == format!("{pid:04X}")) {
                devices.push(probe(&dev, false));
            }
        }
    }
    devices.extend(crate::sim::from_env().iter().map(|d| probe(d, true)));

    Report {
        secbird: env!("CARGO_PKG_VERSION"),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        interfaces,
        devices,
    }
}

/// Send every read-only probe to one device.
pub fn probe(dev: &Device, simulated: bool) -> DeviceReport {
    if dev.pid == crate::headset::PID {
        let probes = [("battery", crate::headset::BATTERY), ("charging", crate::headset::CHARGING)]
            .into_iter()
            .map(|(name, command)| match dev.headset_query(command) {
                Ok(value) => Probe { name, command: format!("HID64:{command:02X} GET"), status: "ok".into(), reply: format!("{value:02X}") },
                Err(e) => Probe { name, command: format!("HID64:{command:02X} GET"), status: e.to_string(), reply: String::new() },
            })
            .collect();
        return DeviceReport {
            pid: format!("{:04X}", dev.pid), name: dev.name(), in_database: dev.spec.is_some(),
            simulated, interface: dev.interface, txid: format!("{:02X}", dev.txid),
            serial: String::new(), probes, leds: Vec::new(),
        };
    }
    let probes = PROBES
        .iter()
        .map(|&(name, command, size, args)| {
            let (status, reply) = match dev.transact(command, size, args) {
                Ok(r) => ("ok".to_string(), hex(&r.args()[..size as usize])),
                Err(Error::Device { status, .. }) => (format!("{status:?}"), String::new()),
                Err(Error::NoReply(_)) => ("no reply".to_string(), String::new()),
                Err(e) => (e.to_string(), String::new()),
            };
            Probe { name, command: command.to_string(), status, reply }
        })
        .collect();
    let leds = LED_IDS
        .filter(|&led| {
            dev.get(cmd::MATRIX_BRIGHTNESS, cmd::MATRIX_BRIGHTNESS_SIZE, &[cmd::VARSTORE, led, 0]).is_ok()
        })
        .collect();
    DeviceReport {
        pid: format!("{:04X}", dev.pid),
        name: dev.name(),
        in_database: dev.spec.is_some(),
        simulated,
        interface: dev.interface,
        txid: format!("{:02X}", dev.txid),
        serial: dev.serial().map(|s| shorten(&s)).unwrap_or_default(),
        probes,
        leds,
    }
}

fn hex(bytes: &[u8]) -> String {
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    bytes[..end].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// Keep the first 4 and last 2 characters: enough to tell devices apart.
fn shorten(serial: &str) -> String {
    let chars: Vec<char> = serial.chars().collect();
    if chars.len() <= 6 {
        return "*".repeat(chars.len());
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 2..].iter().collect();
    format!("{head}...{tail}")
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Secretarybird report (secbird {}, {})", self.secbird, self.os)?;
        writeln!(f, "Only read (GET) commands were sent; nothing on the device was changed.")?;
        writeln!(f, "Serial numbers are shortened for privacy.\n")?;

        writeln!(f, "Razer HID interfaces:")?;
        if self.interfaces.is_empty() {
            writeln!(f, "  (none)")?;
        }
        for i in &self.interfaces {
            let protocol = if i.pid == "0577" { "HID audio" } else { "Protocol 2.5" };
            let answers = i.txid.as_ref().map(|t| format!("  <- answers {protocol} (id {t})")).unwrap_or_default();
            writeln!(f, "  {}  interface {:<2} usage {}:{}{answers}", i.pid, i.interface, i.usage_page, i.usage)?;
        }

        for d in &self.devices {
            let known = match (d.simulated, d.in_database) {
                (true, _) => "simulated",
                (false, true) => "in database",
                (false, false) => "NOT in database",
            };
            writeln!(f, "\nDevice {} {} ({known})", d.pid, d.name)?;
            writeln!(f, "  interface {}, txid {}, serial {}", d.interface, d.txid, d.serial)?;
            for p in &d.probes {
                writeln!(f, "  {:<26} {}  {:<12} {}", p.name, p.command, p.status, p.reply)?;
            }
            let leds: Vec<String> = d.leds.iter().map(|l| l.to_string()).collect();
            let leds = if leds.is_empty() { "none".to_string() } else { leds.join(", ") };
            writeln!(f, "  {:<26} 0F:84  {leds}", "lighting zones (LED ids)")?;
        }
        if self.devices.is_empty() {
            writeln!(f, "\nNo interface answered Protocol 2.5.")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Sim, KEYBOARD_PID, MOUSE_PID, WIRELESS_MOUSE_PID};

    fn report_for(sim: &Sim, pid: u16) -> DeviceReport {
        probe(&sim.open(pid, Sim::spec_for(pid)).unwrap(), true)
    }

    fn status<'a>(r: &'a DeviceReport, name: &str) -> &'a str {
        &r.probes.iter().find(|p| p.name == name).unwrap().status
    }

    #[test]
    fn finds_mouse_zones_and_settings() {
        let r = report_for(&Sim::mouse(), MOUSE_PID);
        assert_eq!(r.leds, [1, 4, 10]);
        assert_eq!(status(&r, "dpi"), "ok");
        assert_eq!(r.probes.iter().find(|p| p.name == "dpi").unwrap().reply, "00 03 20 03 20");
        assert_eq!(status(&r, "battery"), "NotSupported");
        assert_eq!(r.serial, "SIMM...01");
    }

    #[test]
    fn wireless_and_keyboard_differ() {
        let w = report_for(&Sim::wireless_mouse(), WIRELESS_MOUSE_PID);
        assert_eq!(status(&w, "battery"), "ok");
        assert_eq!(w.txid, "3F");
        let k = report_for(&Sim::keyboard(), KEYBOARD_PID);
        assert_eq!(k.leds, [5]);
        assert_eq!(status(&k, "dpi"), "NotSupported");
    }

    #[test]
    fn report_only_reads() {
        let sim = Sim::mouse();
        report_for(&sim, MOUSE_PID);
        assert!(sim.state().log.iter().all(|c| c.id & 0x80 != 0), "only GET commands");
        assert_eq!(sim.state().dpi, Some((800, 800)));
    }

    #[test]
    fn hex_and_shorten() {
        assert_eq!(hex(&[1, 0, 0x20, 0, 0]), "01 00 20");
        assert_eq!(hex(&[0, 0]), "");
        assert_eq!(shorten("AB1234C56789012"), "AB12...12");
        assert_eq!(shorten("ABC"), "***");
    }
}
