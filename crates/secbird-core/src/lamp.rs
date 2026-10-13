//! Windows-owned LampArray access; never sends raw HID lighting reports behind Windows.
use crate::{Device, Error, Result};
use serde::Serialize;

#[derive(Serialize)]
pub struct Info {
    pub id: String,
    pub name: String,
    pub lamps: i32,
    pub available: bool,
    pub controlled: bool,
    pub error: Option<String>,
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{collections::BTreeMap, sync::{Mutex, OnceLock}};
    use windows::{core::HSTRING, Devices::{Enumeration::DeviceInformation, Lights::LampArray}};
    static HELD: OnceLock<Mutex<BTreeMap<String, LampArray>>> = OnceLock::new();
    fn held() -> &'static Mutex<BTreeMap<String, LampArray>> { HELD.get_or_init(Default::default) }
    fn err(e: windows::core::Error) -> Error { Error::System(format!("Windows LampArray: {e}")) }
    struct Apartment(bool);
    impl Drop for Apartment {
        fn drop(&mut self) { if self.0 { unsafe { windows::Win32::System::WinRT::RoUninitialize(); } } }
    }
    thread_local! { static APARTMENT: std::cell::RefCell<Option<Apartment>> = const { std::cell::RefCell::new(None) }; }
    fn init() -> Result<()> {
        // Every caller thread needs WinRT. An already initialized apartment is fine.
        APARTMENT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                let hr = unsafe { windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED) };
                *slot = Some(match hr {
                    Ok(()) => Apartment(true),
                    Err(e) if e.code().0 == 0x80010106u32 as i32 => Apartment(false),
                    Err(e) => return Err(err(e)),
                });
            }
            Ok(())
        })
    }
    pub fn list() -> Result<Vec<Info>> {
        let _apartment = init()?;
        let selector = LampArray::GetDeviceSelector().map_err(err)?;
        let devices = DeviceInformation::FindAllAsyncAqsFilter(&selector).map_err(err)?.get().map_err(err)?;
        let mut out = Vec::new();
        let mut handles = held().lock().unwrap_or_else(|p| p.into_inner());
        for device in devices {
            let id = device.Id().map_err(err)?.to_string();
            let name = device.Name().map_err(err)?.to_string();
            let controlled = handles.contains_key(&id);
            let array = if let Some(array) = handles.get(&id) { Ok(array.clone()) }
                else { LampArray::FromIdAsync(&HSTRING::from(&id)).and_then(|op| op.get()) };
            let (lamps, available, error) = match array {
                Ok(array) => (array.LampCount().unwrap_or(0), array.IsAvailable().unwrap_or(false), None),
                Err(e) => (0, false, Some(e.to_string())),
            };
            out.push(Info { id, name, lamps, available, controlled, error });
        }
        handles.retain(|id, _| out.iter().any(|info| &info.id == id));
        Ok(out)
    }
    pub fn release(id: &str) -> Result<()> {
        held().lock().unwrap_or_else(|p| p.into_inner()).remove(id);
        Ok(())
    }
}

#[cfg(windows)]
pub use native::{list, release};
#[cfg(not(windows))]
pub fn list() -> Result<Vec<Info>> { Ok(Vec::new()) }
#[cfg(not(windows))]
pub fn release(_: &str) -> Result<()> { Ok(()) }

pub fn for_device(dev: &Device) -> Result<Option<Info>> {
    let container = crate::windows_device::container(&dev.path.to_string_lossy())?;
    if container.is_empty() { return Ok(None); }
    Ok(list()?.into_iter().find(|info| info.lamps > 0 &&
        crate::windows_device::container(&info.id).is_ok_and(|other| !other.is_empty() && other.eq_ignore_ascii_case(&container))))
}
