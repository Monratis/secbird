//! Only the keyboard's game-controller HID child can be disabled. Never a USB parent,
//! keyboard, mouse, lighting interface, or another physical unit's child.

use crate::{Device, Error, Result};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Gamepad {
    pub instance: String,
    pub disabled: bool,
    pub present: bool,
}

fn is_gamepad(class: &str, ids: &[String]) -> bool {
    class.eq_ignore_ascii_case("HIDClass") && ids.iter().any(|id| {
        matches!(id.to_ascii_uppercase().as_str(), "HID_DEVICE_UP:0001_U:0004" | "HID_DEVICE_UP:0001_U:0005")
    })
}

#[cfg(windows)]
pub(crate) fn container(path: &str) -> Result<String> {
    use winreg::{enums::*, RegKey};
    let path = path.strip_prefix(r"\\?\").ok_or_else(|| Error::System("no Windows HID instance path".into()))?;
    let parts: Vec<_> = path.split('#').collect();
    if parts.len() != 4 || !parts[0].eq_ignore_ascii_case("hid") { return Err(Error::System("invalid HID path".into())); }
    RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(format!(r"SYSTEM\CurrentControlSet\Enum\HID\{}\{}", parts[1], parts[2]))
        .and_then(|key| key.get_value("ContainerID")).map_err(|e| Error::System(e.to_string()))
}
#[cfg(not(windows))]
pub(crate) fn container(_: &str) -> Result<String> { Ok(String::new()) }

#[cfg(windows)]
pub fn gamepads(dev: &Device) -> Result<Vec<Gamepad>> {
    use winreg::{enums::*, RegKey};
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
    if !dev.spec.as_ref().is_some_and(|s| s.kind == crate::Kind::Keyboard) { return Ok(Vec::new()); }
    let path = dev.path.to_string_lossy();
    let path = path.strip_prefix(r"\\?\").ok_or_else(|| Error::System("no Windows HID instance path".into()))?;
    let parts: Vec<_> = path.split('#').collect();
    if parts.len() != 4 || !parts[0].eq_ignore_ascii_case("hid") { return Err(Error::System("invalid HID path".into())); }
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    let parent = root.open_subkey(format!(r"SYSTEM\CurrentControlSet\Enum\HID\{}\{}", parts[1], parts[2]))
        .map_err(|e| Error::System(e.to_string()))?;
    let container: String = parent.get_value("ContainerID").map_err(|e| Error::System(e.to_string()))?;
    let hid = root.open_subkey(r"SYSTEM\CurrentControlSet\Enum\HID").map_err(|e| Error::System(e.to_string()))?;
    let prefix = format!("VID_1532&PID_{:04X}&", dev.pid);
    let mut out = Vec::new();
    for name in hid.enum_keys().flatten().filter(|n| n.to_ascii_uppercase().starts_with(&prefix)) {
        let Ok(model) = hid.open_subkey(&name) else { continue };
        for unit in model.enum_keys().flatten() {
            let Ok(key) = model.open_subkey(&unit) else { continue };
            let ids: Vec<String> = key.get_value("HardwareID").unwrap_or_default();
            let class_guid: String = key.get_value("ClassGUID").unwrap_or_default();
            let class = if class_guid.eq_ignore_ascii_case("{745a17a0-74d3-11d0-b6fe-00a0c90f57da}") { "HIDClass" } else { "" };
            let other: String = key.get_value("ContainerID").unwrap_or_default();
            if !other.eq_ignore_ascii_case(&container) || !is_gamepad(class, &ids) { continue; }
            let instance = format!(r"HID\{name}\{unit}");
            let mut wide: Vec<u16> = instance.encode_utf16().chain(Some(0)).collect();
            let mut node = 0;
            let present = unsafe { CM_Locate_DevNodeW(&mut node, wide.as_mut_ptr(), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS;
            if !present && unsafe { CM_Locate_DevNodeW(&mut node, wide.as_mut_ptr(), CM_LOCATE_DEVNODE_PHANTOM) } != CR_SUCCESS { continue; }
            let mut status = 0;
            let mut problem = 0;
            let known = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, node, 0) } == CR_SUCCESS;
            let flags: u32 = key.get_value("ConfigFlags").unwrap_or(0);
            out.push(Gamepad { instance, disabled: if known { problem == CM_PROB_DISABLED } else { flags & 1 != 0 }, present });
        }
    }
    Ok(out)
}

#[cfg(not(windows))]
pub fn gamepads(_: &Device) -> Result<Vec<Gamepad>> { Ok(Vec::new()) }

#[cfg(windows)]
pub fn set_gamepad(dev: &Device, enabled: bool) -> Result<()> {
    let targets = gamepads(dev)?;
    if targets.is_empty() { return Err(Error::Unsupported("no separate game-controller HID child found".into())); }
    if targets.iter().any(|g| !g.present && g.disabled == enabled) {
        return Err(Error::Unsupported("the game-controller interface is currently disconnected; reconnect it before changing its state".into()));
    }
    let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32");
    for target in targets.iter().filter(|g| g.disabled == enabled) {
        // Registry-derived ID, re-validated here; no browser-supplied instance or command.
        if !target.instance.bytes().all(|b| b.is_ascii_alphanumeric() || b"\\&_-.".contains(&b)) {
            return Err(Error::System("unsafe device instance ID".into()));
        }
        let verb = if enabled { "/enable-device" } else { "/disable-device" };
        let exe = system.join("pnputil.exe").to_string_lossy().replace('\'', "''");
        let script = format!("$ErrorActionPreference='Stop'; $p=Start-Process -FilePath '{exe}' -ArgumentList '{verb}', '\"{}\"' -Verb RunAs -Wait -PassThru; exit $p.ExitCode", target.instance);
        let output = std::process::Command::new(system.join(r"WindowsPowerShell\v1.0\powershell.exe"))
            .args(["-NoProfile", "-NonInteractive", "-Command", &script]).output().map_err(|e| Error::System(e.to_string()))?;
        if !output.status.success() {
            return Err(Error::System(format!("gamepad change failed or UAC cancelled: {}", String::from_utf8_lossy(&output.stderr))));
        }
    }
    let after = gamepads(dev)?;
    if after.iter().any(|g| g.disabled == enabled && g.present) {
        return Err(Error::System("gamepad state did not change; reconnect the keyboard or restart Windows".into()));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_gamepad(_: &Device, _: bool) -> Result<()> { Err(Error::Unsupported("Windows only".into())) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_controller_hid_children_are_selected() {
        assert!(is_gamepad("HIDClass", &["HID_DEVICE_UP:0001_U:0004".into()]));
        assert!(is_gamepad("HIDClass", &["HID_DEVICE_UP:0001_U:0005".into()]));
        for (class, id) in [("Keyboard", "HID_DEVICE_UP:0001_U:0004"), ("USB", "HID_DEVICE_UP:0001_U:0004"),
            ("HIDClass", "HID_DEVICE_UP:0001_U:0006"), ("HIDClass", "HID_DEVICE_UP:0059_U:0001")] {
            assert!(!is_gamepad(class, &[id.into()]));
        }
    }
}
