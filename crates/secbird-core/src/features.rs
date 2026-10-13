//! Typed device settings built on [`Device::transact`].

use crate::commands as cmd;
use crate::commands::BACKLIGHT_LED;
use crate::db::{Lighting, Matrix, Polling};
use crate::device::Device;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

impl std::str::FromStr for Rgb {
    type Err = Error;

    /// `RRGGBB` or `#RRGGBB`.
    fn from_str(s: &str) -> Result<Self> {
        let hex = s.trim_start_matches('#');
        let bad = || Error::InvalidArg(format!("color '{s}' is not RRGGBB"));
        if hex.len() != 6 {
            return Err(bad());
        }
        let v = u32::from_str_radix(hex, 16).map_err(|_| bad())?;
        Ok(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}

impl Device {
    pub fn firmware(&self) -> Result<(u8, u8)> {
        let r = self.get(cmd::FIRMWARE, cmd::FIRMWARE_SIZE, &[])?;
        Ok((r.args()[0], r.args()[1]))
    }

    pub fn serial(&self) -> Result<String> {
        let r = self.get(cmd::SERIAL, cmd::SERIAL_SIZE, &[])?;
        let raw = &r.args()[..cmd::SERIAL_SIZE as usize];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        Ok(String::from_utf8_lossy(&raw[..end]).trim().to_string())
    }

    pub fn dpi(&self) -> Result<(u16, u16)> {
        let r = self.get(cmd::DPI, cmd::DPI_SIZE, &[cmd::NOSTORE])?;
        let a = r.args();
        Ok((u16::from_be_bytes([a[1], a[2]]), u16::from_be_bytes([a[3], a[4]])))
    }

    /// Set DPI and store it in the device's memory, rounded to the device's step.
    pub fn set_dpi(&self, x: u16, y: u16) -> Result<(u16, u16)> {
        let (x, y) = match self.spec.as_ref().and_then(|s| s.dpi.as_ref()) {
            Some(r) => (snap(x, r.min, r.max, r.step), snap(y, r.min, r.max, r.step)),
            None if (100..=30000).contains(&x) && (100..=30000).contains(&y) => (x, y),
            None => return Err(Error::InvalidArg(format!("DPI {x}x{y} out of range"))),
        };
        let [xh, xl] = x.to_be_bytes();
        let [yh, yl] = y.to_be_bytes();
        self.transact(cmd::DPI, cmd::DPI_SIZE, &[cmd::VARSTORE, xh, xl, yh, yl, 0, 0])?;
        Ok((x, y))
    }

    fn polling_kind(&self) -> Polling {
        self.spec.as_ref().and_then(|s| s.polling).unwrap_or(Polling::Standard)
    }

    pub fn polling_rate(&self) -> Result<u32> {
        match self.polling_kind() {
            Polling::Standard => {
                let r = self.get(cmd::POLLING, cmd::POLLING_SIZE, &[])?;
                cmd::polling_hz(r.args()[0])
            }
            Polling::High => {
                let r = self.get(cmd::POLLING_HS, cmd::POLLING_HS_SIZE, &[])?;
                cmd::polling_hs_hz(r.args()[1])
            }
        }
        .ok_or_else(|| Error::Unsupported("unknown polling rate code".into()))
    }

    pub fn set_polling_rate(&self, hz: u32) -> Result<()> {
        let unsupported = || Error::InvalidArg(format!("{hz} Hz is not supported by this device"));
        match self.polling_kind() {
            Polling::Standard => {
                let code = cmd::polling_code(hz).ok_or_else(unsupported)?;
                self.transact(cmd::POLLING, cmd::POLLING_SIZE, &[code])?;
            }
            Polling::High => {
                let code = cmd::polling_hs_code(hz).ok_or_else(unsupported)?;
                self.transact(cmd::POLLING_HS, cmd::POLLING_HS_SIZE, &[0x00, code])?;
                // HyperPolling writes both selectors (live and stored), as Synapse and
                // OpenRazer do; writing only 0 leaves the stored rate unchanged.
                if self.pid == 0x00B3 {
                    self.transact(cmd::POLLING_HS, cmd::POLLING_HS_SIZE, &[0x01, code])?;
                }
            }
        }
        Ok(())
    }

    /// Battery percentage, or `None` for wired-only devices.
    pub fn battery(&self) -> Result<Option<u8>> {
        if self.pid == crate::headset::PID {
            return self.headset_query(crate::headset::BATTERY).map(Some);
        }
        if !self.spec.as_ref().is_some_and(|s| s.wireless) {
            return Ok(None);
        }
        let r = self.get(cmd::BATTERY, cmd::POWER_SIZE, &[])?;
        Ok(Some(((r.args()[1] as u32 * 100 + 127) / 255) as u8))
    }

    pub fn charging(&self) -> Result<bool> {
        if self.pid == crate::headset::PID {
            return self.headset_query(crate::headset::CHARGING).map(|n| n != 0);
        }
        let r = self.get(cmd::CHARGING, cmd::POWER_SIZE, &[])?;
        Ok(r.args()[1] != 0)
    }

    /// Resolve a lighting zone by name ("logo") or numeric id.
    pub fn led_id(&self, zone: &str) -> Result<u8> {
        if let Some(led) = self.spec.as_ref().and_then(|s| s.leds.iter().find(|l| l.name == zone)) {
            return Ok(led.id);
        }
        zone.parse::<u8>().map_err(|_| Error::InvalidArg(format!("unknown lighting zone '{zone}'")))
    }

    fn lighting(&self) -> Lighting {
        self.spec.as_ref().map_or(Lighting::Extended, |s| s.lighting)
    }

    /// Old-style grids light the whole device with one effect instead of per-LED commands.
    fn is_grid_backlight(&self, led: u8) -> bool {
        led == BACKLIGHT_LED && self.matrix().is_some()
    }

    fn no_lighting(&self) -> Error {
        Error::Unsupported(format!("lighting on {} uses a protocol Secretarybird doesn't speak yet", self.name()))
    }

    /// Brightness 0-100 %.
    pub fn set_brightness(&self, led: u8, percent: u8) -> Result<()> {
        let level = (percent.min(100) as u32 * 255 / 100) as u8;
        let command = match self.lighting() {
            Lighting::Extended => cmd::MATRIX_BRIGHTNESS,
            Lighting::Standard | Lighting::MouseExtended => cmd::LED_BRIGHTNESS,
            Lighting::Kraken => return Err(self.no_lighting()),
        };
        self.transact(command, 3, &[cmd::VARSTORE, led, level])?;
        Ok(())
    }

    pub fn set_static_color(&self, led: u8, Rgb(r, g, b): Rgb) -> Result<()> {
        match self.lighting() {
            Lighting::Extended => {
                let args = [cmd::VARSTORE, led, cmd::EFFECT_STATIC, 0x00, 0x00, 0x01, r, g, b];
                self.transact(cmd::MATRIX_EFFECT, args.len() as u8, &args)?;
            }
            Lighting::Standard if self.is_grid_backlight(led) => {
                self.transact(cmd::GRID_EFFECT, 4, &[cmd::GRID_STATIC, r, g, b])?;
            }
            Lighting::Standard => {
                self.transact(cmd::LED_RGB, 5, &[cmd::VARSTORE, led, r, g, b])?;
                self.transact(cmd::LED_EFFECT, 3, &[cmd::VARSTORE, led, cmd::CLASSIC_STATIC])?;
                self.transact(cmd::LED_STATE, 3, &[cmd::VARSTORE, led, 1])?;
            }
            Lighting::MouseExtended => {
                self.transact(cmd::MOUSE_LED_EFFECT, 6, &[cmd::VARSTORE, led, cmd::GRID_STATIC, r, g, b])?;
            }
            Lighting::Kraken => return Err(self.no_lighting()),
        }
        Ok(())
    }

    pub fn lights_off(&self, led: u8) -> Result<()> {
        match self.lighting() {
            Lighting::Extended => {
                let args = [cmd::VARSTORE, led, cmd::EFFECT_OFF, 0x00, 0x00, 0x00];
                self.transact(cmd::MATRIX_EFFECT, args.len() as u8, &args)?;
            }
            Lighting::Standard if self.is_grid_backlight(led) => {
                self.transact(cmd::GRID_EFFECT, 1, &[cmd::GRID_OFF])?;
            }
            Lighting::Standard => {
                self.transact(cmd::LED_STATE, 3, &[cmd::VARSTORE, led, 0])?;
            }
            Lighting::MouseExtended => {
                self.transact(cmd::MOUSE_LED_EFFECT, 3, &[cmd::VARSTORE, led, 0x00])?;
            }
            Lighting::Kraken => return Err(self.no_lighting()),
        }
        Ok(())
    }

    /// Per-key lighting grid, for keyboards and keypads.
    pub fn matrix(&self) -> Option<Matrix> {
        self.spec.as_ref().and_then(|s| s.matrix)
    }

    /// Send colors for keys `start..start + colors.len()` of one row. Shown once the
    /// custom-frame effect is active (see [`Device::show_frame`]); not saved on the device.
    pub fn set_row(&self, row: u8, start: u8, colors: &[Rgb]) -> Result<()> {
        let m = self.matrix().ok_or_else(|| Error::Unsupported("no per-key lighting".into()))?;
        let end = start as usize + colors.len();
        if colors.is_empty() || row >= m.rows || end > m.cols as usize {
            return Err(Error::InvalidArg(format!("row {row}, keys {start}..{end} outside {}x{}", m.rows, m.cols)));
        }
        let lighting = self.lighting();
        for (i, chunk) in colors.chunks(cmd::MATRIX_FRAME_MAX_KEYS).enumerate() {
            let first = (start as usize + i * cmd::MATRIX_FRAME_MAX_KEYS) as u8;
            let last = first + chunk.len() as u8 - 1;
            let rgb = chunk.iter().flat_map(|c| [c.0, c.1, c.2]);
            match lighting {
                Lighting::Extended => {
                    let mut args = vec![0x00, 0x00, row, first, last];
                    args.extend(rgb);
                    self.transact(cmd::MATRIX_FRAME, args.len() as u8, &args)?;
                }
                Lighting::Standard => {
                    let mut args = vec![0xFF, row, first, last];
                    args.extend(rgb);
                    self.transact(cmd::GRID_FRAME, cmd::GRID_FRAME_SIZE, &args)?;
                }
                Lighting::MouseExtended | Lighting::Kraken => return Err(self.no_lighting()),
            }
        }
        Ok(())
    }

    /// Show a full frame of per-key colors, row-major (`rows * cols` entries).
    pub fn show_frame(&self, frame: &[Rgb]) -> Result<()> {
        let m = self.matrix().ok_or_else(|| Error::Unsupported("no per-key lighting".into()))?;
        let cols = m.cols as usize;
        if frame.len() != m.rows as usize * cols {
            return Err(Error::InvalidArg(format!(
                "frame has {} colors, expected {} ({}x{})",
                frame.len(),
                m.rows as usize * cols,
                m.rows,
                m.cols
            )));
        }
        for (row, colors) in frame.chunks(cols).enumerate() {
            self.set_row(row as u8, 0, colors)?;
        }
        match self.lighting() {
            // Size 0x0C as Razer sends it; the trailing arguments are zero.
            Lighting::Extended => {
                self.transact(cmd::MATRIX_EFFECT, 0x0C, &[cmd::NOSTORE, 0x00, cmd::EFFECT_CUSTOM_FRAME])?
            }
            _ => self.transact(cmd::GRID_EFFECT, 2, &[cmd::GRID_CUSTOM_FRAME, cmd::VARSTORE])?,
        };
        Ok(())
    }
}

/// Clamp to `[min, max]` and round to the nearest multiple of `step`.
fn snap(v: u16, min: u16, max: u16, step: u16) -> u16 {
    let v = v.clamp(min, max);
    let step = step.max(1);
    let rounded = (v + step / 2) / step * step;
    rounded.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_rounds_and_clamps() {
        assert_eq!(snap(1026, 100, 26000, 50), 1050);
        assert_eq!(snap(1020, 100, 26000, 50), 1000);
        assert_eq!(snap(50, 100, 26000, 50), 100);
        assert_eq!(snap(30000, 100, 26000, 50), 26000);
    }

    #[test]
    fn rgb_parses_hex() {
        assert_eq!("#FF8000".parse::<Rgb>().unwrap(), Rgb(255, 128, 0));
        assert!("12345".parse::<Rgb>().is_err());
    }
}
