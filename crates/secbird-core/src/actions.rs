//! User-facing changes: send a setting to the device, then record it in the active
//! profile and save. The CLI and the editor both go through here, so they behave alike.

use crate::device::Device;
use crate::error::{Error, Result};
use crate::features::Rgb;
use crate::profile::{Store, Zone};

fn zone<'a>(store: &'a mut Store, dev: &Device, name: &str) -> &'a mut Zone {
    store.for_device(dev).active_mut().lighting.entry(name.to_string()).or_default()
}

pub fn set_audio(dev: &Device, store: &mut Store, name: &str, values: Vec<i16>) -> Result<()> {
    dev.audio_set(name, &values)?;
    if name == "mic-preset" {
        let bands = dev.audio_read("mic-eq")?;
        store.for_device(dev).active_mut().audio.insert("mic-eq".into(), bands);
    }
    store.for_device(dev).active_mut().audio.insert(name.into(), values);
    store.save()
}

fn own_lighting(dev: &Device, store: &mut Store) -> Result<()> {
    if store.for_device(dev).windows_lighting {
        return Err(Error::Unsupported("lighting handed to Windows for this device".into()));
    }
    Ok(())
}

pub fn set_windows_lighting(dev: &Device, store: &mut Store, enabled: bool) -> Result<()> {
    if enabled {
        let lamp = crate::lamp::for_device(dev)?.ok_or_else(|| Error::Unsupported("this device has no connected Windows LampArray with LEDs".into()))?;
        crate::lamp::release(&lamp.id)?;
    }
    store.for_device(dev).windows_lighting = enabled;
    store.save()
}

/// Returns the DPI actually set, after rounding to the device's step.
pub fn set_dpi(dev: &Device, store: &mut Store, x: u16, y: u16) -> Result<(u16, u16)> {
    let applied = dev.set_dpi(x, y)?;
    store.for_device(dev).active_mut().dpi = Some(applied);
    store.save()?;
    Ok(applied)
}

pub fn set_polling(dev: &Device, store: &mut Store, hz: u32) -> Result<()> {
    dev.set_polling_rate(hz)?;
    store.for_device(dev).active_mut().polling = Some(hz);
    store.save()
}

pub fn set_color(dev: &Device, store: &mut Store, zone_name: &str, color: Rgb) -> Result<()> {
    own_lighting(dev, store)?;
    dev.set_static_color(dev.led_id(zone_name)?, color)?;
    store.for_device(dev).active_mut().keys.clear();
    let z = zone(store, dev, zone_name);
    z.color = Some(color);
    z.off = false;
    store.save()
}

/// Brightness in percent, clamped to 0-100.
pub fn set_brightness(dev: &Device, store: &mut Store, zone_name: &str, percent: u8) -> Result<()> {
    own_lighting(dev, store)?;
    let percent = percent.min(100);
    dev.set_brightness(dev.led_id(zone_name)?, percent)?;
    zone(store, dev, zone_name).brightness = Some(percent);
    store.save()
}

pub fn lights_off(dev: &Device, store: &mut Store, zone_name: &str) -> Result<()> {
    own_lighting(dev, store)?;
    dev.lights_off(dev.led_id(zone_name)?)?;
    store.for_device(dev).active_mut().keys.clear();
    zone(store, dev, zone_name).off = true;
    store.save()
}

/// Per-key colors (row-major). They replace any whole-zone color or off state;
/// brightness is kept.
pub fn set_keys(dev: &Device, store: &mut Store, keys: Vec<Rgb>) -> Result<()> {
    own_lighting(dev, store)?;
    dev.show_frame(&keys)?;
    let profile = store.for_device(dev).active_mut();
    for z in profile.lighting.values_mut() {
        z.color = None;
        z.off = false;
    }
    profile.keys = keys;
    store.save()
}

/// Remap a button (`None` puts back its saved setting). `button` is a name from
/// [`crate::buttons::BUTTONS`].
pub fn set_button(
    dev: &Device,
    store: &mut Store,
    button: &str,
    layer: crate::buttons::Layer,
    action: Option<crate::buttons::Action>,
) -> Result<()> {
    let key = crate::buttons::setting_key(button, layer);
    let (id, _) = crate::buttons::parse_setting_key(&key)?;
    let buttons = &mut store.for_device(dev).active_mut().buttons;
    match action {
        Some(action) => {
            dev.set_button(id, layer, action)?;
            buttons.insert(key, action.to_string());
        }
        None => {
            dev.reset_button(id, layer)?;
            buttons.remove(&key);
        }
    }
    store.save()
}

/// One zone's outcome from [`sync_color`].
pub struct Synced {
    pub pid: u16,
    /// [`Device::key`]: tells identical models apart.
    pub key: String,
    pub device: String,
    pub zone: String,
    pub result: Result<()>,
}

/// The same static color on every lighting zone of every device, each recorded in
/// that device's active profile. Devices without lighting are skipped.
pub fn sync_color(devices: &[Device], store: &mut Store, color: Rgb) -> Vec<Synced> {
    let mut out = Vec::new();
    for dev in devices {
        if store.for_device(dev).windows_lighting { continue; }
        let zones: Vec<String> = dev.spec.as_ref().map(|s| s.leds.iter().map(|l| l.name.clone()).collect()).unwrap_or_default();
        for zone in zones {
            let result = set_color(dev, store, &zone, color);
            out.push(Synced { pid: dev.pid, key: dev.key(), device: dev.name(), zone, result });
        }
    }
    out
}
