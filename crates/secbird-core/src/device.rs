//! Discovery and request/reply transport over HID feature reports.

use crate::commands as cmd;
use crate::db::{Db, DeviceSpec, Kind, RAZER_VID};
use crate::error::{Error, Result};
use crate::proto::{Command, Report, Status, REPORT_LEN};
use crate::transport::Transport;
use hidapi::HidApi;
use std::ffi::CString;
use std::thread::sleep;

/// Synapse retries reads up to 30 times (5 ms apart, see [`Transport::read_delay`]).
const READ_RETRIES: u32 = 30;

/// Transaction ids seen across Razer devices, tried in order when the database has none.
pub const TXID_CANDIDATES: [u8; 4] = [0x1F, 0x3F, 0xFF, 0x9F];

pub struct Device {
    transport: Box<dyn Transport>,
    pub pid: u16,
    /// Serial number read during discovery; empty if the device reports none.
    pub serial: String,
    pub interface: i32,
    pub path: CString,
    pub txid: u8,
    pub spec: Option<DeviceSpec>,
}

impl Device {
    pub fn name(&self) -> String {
        match &self.spec {
            Some(s) => s.name.clone(),
            None => format!("Unknown Razer device {:04X}", self.pid),
        }
    }

    /// Keep the receiver's transport, capabilities and profile key, but label the mouse
    /// it actually reports. Unknown/offline devices leave the receiver name unchanged.
    pub(crate) fn identify_receiver(&mut self, db: &Db) {
        if self.pid != 0x00B3 {
            return;
        }
        let Ok(reply) = self.get(cmd::WIRELESS_DEVICES, cmd::WIRELESS_DEVICES_SIZE, &[]) else { return };
        let Some(pid) = connected_mouse_pid(reply.args()) else { return };
        let Some(mouse) = db.get(pid).filter(|s| s.kind == Kind::Mouse) else { return };
        if let Some(spec) = &mut self.spec {
            spec.name = format!("{} (via HyperPolling)", mouse.name);
        }
    }

    /// Send one command and wait for its reply. `size` is the argument length the
    /// device expects for this command.
    pub fn transact(&self, command: Command, size: u8, args: &[u8]) -> Result<Report> {
        if self.pid == crate::headset::PID {
            return Err(Error::Unsupported("this headset uses HID audio reports, not Protocol 2.5".into()));
        }
        transact(self.transport.as_ref(), self.txid, command, size, args)
    }

    pub(crate) fn headset_query(&self, command: u8) -> Result<u8> {
        if self.pid != crate::headset::PID {
            return Err(Error::Unsupported("HID audio reports on this device".into()));
        }
        crate::headset::query(self.transport.as_ref(), command)
    }

    pub(crate) fn headset_transact(&self, command: u8, args: &[u8]) -> Result<Vec<u8>> {
        if self.pid != crate::headset::PID {
            return Err(Error::Unsupported("HID audio reports on this device".into()));
        }
        crate::headset::transact(self.transport.as_ref(), command, args)
    }

    pub fn get(&self, command: Command, size: u8, args: &[u8]) -> Result<Report> {
        self.transact(command.get(), size, args)
    }

    /// Stable identity of this physical device: `PID:SERIAL`, or just `PID` when the
    /// device has no usable serial. Profiles are stored under this key, so two
    /// identical mice keep separate settings.
    pub fn key(&self) -> String {
        if self.serial.is_empty() {
            format!("{:04X}", self.pid)
        } else {
            format!("{:04X}:{}", self.pid, self.serial)
        }
    }
}

/// Serial from a GET serial reply: printable ASCII only; placeholders count as none.
fn parse_serial(args: &[u8]) -> String {
    let raw = &args[..(cmd::SERIAL_SIZE as usize).min(args.len())];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let s: String = raw[..end].iter().map(|&b| b as char).collect::<String>().trim().to_string();
    let usable = !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && s.chars().any(|c| c != '0' && c != 'F' && c != 'f');
    if usable { s } else { String::new() }
}

fn connected_mouse_pid(args: &[u8]) -> Option<u16> {
    let count = *args.first()? as usize;
    let entries = args.get(1..1 + count * 3)?;
    let mut connected = entries.chunks_exact(3).filter_map(|entry| {
        let pid = u16::from_be_bytes([entry[1], entry[2]]);
        (entry[0] == 1 && pid != 0 && pid != 0xFFFF).then_some(pid)
    });
    let pid = connected.next()?;
    // This receiver supports one mouse. Don't guess if a future firmware reports more.
    connected.next().is_none().then_some(pid)
}

fn transact(hid: &dyn Transport, txid: u8, command: Command, size: u8, args: &[u8]) -> Result<Report> {
    let req = Report::request(txid, command, size, args);
    let mut buf = [0u8; REPORT_LEN + 1]; // byte 0 is the HID report id (0)
    buf[1..].copy_from_slice(&req.0);
    hid.send_feature_report(&buf)?;

    for _ in 0..READ_RETRIES {
        sleep(hid.read_delay());
        let mut reply = [0u8; REPORT_LEN + 1];
        let n = hid.get_feature_report(&mut reply)?;
        if n < REPORT_LEN {
            continue;
        }
        let mut body = [0u8; REPORT_LEN];
        body.copy_from_slice(&reply[n - REPORT_LEN..n]);
        let rep = Report(body);
        if rep.command() != command {
            continue;
        }
        match rep.status() {
            Status::Success => return Ok(rep),
            Status::Busy | Status::New => continue,
            status => return Err(Error::Device { cmd: command, status }),
        }
    }
    Err(Error::NoReply(command))
}

/// A Razer HID interface seen during enumeration, before we know if it speaks Protocol 2.5.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub pid: u16,
    pub interface: i32,
    pub path: CString,
}

pub fn candidates(api: &HidApi) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = api
        .device_list()
        .filter(|d| d.vendor_id() == RAZER_VID)
        .filter(|d| d.product_id() != crate::headset::PID || (d.usage_page() == 0xFF14 && d.usage() == 1))
        .map(|d| Candidate {
            pid: d.product_id(),
            interface: d.interface_number(),
            path: d.path().to_owned(),
        })
        .collect();
    out.sort_by(|a, b| (a.pid, a.interface, &a.path).cmp(&(b.pid, b.interface, &b.path)));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// Find every connected Razer device that answers a supported protocol, one handle per physical
/// device (several units of the same model are told apart by serial number).
///
/// Simulated devices named in `SECBIRD_SIM` (e.g. `mouse,keyboard`) are added at the end.
pub fn discover(api: &HidApi, db: &Db) -> Vec<Device> {
    let mut found = discover_hid(api, db);
    for dev in &mut found {
        dev.identify_receiver(db);
    }
    found.extend(crate::sim::from_env());
    found
}

fn discover_hid(api: &HidApi, db: &Db) -> Vec<Device> {
    let all = candidates(api);
    let mut pids: Vec<u16> = all.iter().map(|c| c.pid).collect();
    pids.dedup();

    pids.into_iter()
        .filter_map(|pid| -> Option<Vec<Device>> {
            let spec = db.get(pid).cloned();
            let mut ifaces: Vec<&Candidate> = all.iter().filter(|c| c.pid == pid).collect();
            // Known interface first; the rest still get a chance.
            if let Some(want) = spec.as_ref().and_then(|s| s.interface) {
                ifaces.sort_by_key(|c| c.interface != want);
            }
            let txids: Vec<u8> = match spec.as_ref().and_then(|s| s.txid) {
                Some(t) => std::iter::once(t).chain(TXID_CANDIDATES).collect(),
                None => TXID_CANDIDATES.to_vec(),
            };
            let mut iter = ifaces.into_iter();
            let first = iter.by_ref().find_map(|c| probe(api, c, &txids, spec.clone()))?;
            // More units of the same model answer on the same interface number.
            let mut units = vec![first];
            let rest: Vec<&Candidate> = iter.filter(|c| c.interface == units[0].interface).collect();
            for c in rest {
                if let Some(dev) = probe(api, c, &[units[0].txid], spec.clone()) {
                    if !dev.serial.is_empty() && units.iter().all(|u| u.serial != dev.serial) {
                        units.push(dev);
                    }
                }
            }
            Some(units)
        })
        .flatten()
        .collect()
}

fn probe(api: &HidApi, c: &Candidate, txids: &[u8], spec: Option<DeviceSpec>) -> Option<Device> {
    let hid = api.open_path(&c.path).ok()?;
    Device::open(Box::new(hid), c.pid, c.interface, c.path.clone(), txids, spec)
}

impl Device {
    /// Wrap a transport, keeping the first txid that answers a harmless GET (serial number).
    pub fn open(
        transport: Box<dyn Transport>,
        pid: u16,
        interface: i32,
        path: CString,
        txids: &[u8],
        spec: Option<DeviceSpec>,
    ) -> Option<Device> {
        if pid == crate::headset::PID {
            let dev = Device { transport, pid, serial: String::new(), interface, path, txid: 0x60, spec };
            dev.headset_query(crate::headset::BATTERY).ok()?;
            return Some(dev);
        }
        let (txid, reply) = txids.iter().copied().find_map(|t| {
            transact(transport.as_ref(), t, cmd::SERIAL.get(), cmd::SERIAL_SIZE, &[]).ok().map(|r| (t, r))
        })?;
        let serial = parse_serial(reply.args());
        Some(Device { transport, pid, serial, interface, path, txid, spec })
    }
}

/// Razer products that are plugged in but didn't answer Protocol 2.5 (e.g. Kraken
/// headsets), so the user sees them instead of wondering where they went.
pub fn unanswered(api: &HidApi, found: &[Device]) -> Vec<u16> {
    let mut pids: Vec<u16> = candidates(api).iter().map(|c| c.pid).filter(|p| !found.iter().any(|d| d.pid == *p)).collect();
    pids.dedup();
    pids
}

#[cfg(test)]
mod tests {
    use super::{connected_mouse_pid, parse_serial};

    #[test]
    fn receiver_reply_requires_one_connected_device() {
        assert_eq!(connected_mouse_pid(&[2, 1, 0, 0xB7, 0, 0xFF, 0xFF]), Some(0x00B7));
        for args in [
            &[][..], &[0], &[2, 1, 0, 0xB7], &[255, 1, 0, 0xB7],
            &[1, 0, 0, 0xB7], &[1, 1, 0xFF, 0xFF], &[1, 1, 0, 0],
            &[2, 1, 0, 0xB7, 1, 0, 0xA6],
        ] {
            assert_eq!(connected_mouse_pid(args), None, "{args:?}");
        }
    }

    #[test]
    fn serials_are_cleaned() {
        assert_eq!(parse_serial(b"AB1234C56789012\0\0\0\0\0\0\0"), "AB1234C56789012");
        assert_eq!(parse_serial(b"  ABC123  \0"), "ABC123");
        assert_eq!(parse_serial(&[0u8; 22]), "");
        assert_eq!(parse_serial(b"000000000000\0"), "");
        assert_eq!(parse_serial(&[0xFF; 22]), "");
        assert_eq!(parse_serial(b"AB\x01CD\0"), "");
    }
}
