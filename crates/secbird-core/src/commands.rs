//! Command ids with their argument sizes. Each SET command's GET variant is `cmd.get()`.
//!
//! Only commands that agree between Synapse's middleware and public OpenRazer docs live
//! here; `verified` notes the ones confirmed on real hardware.

use crate::proto::Command;

/// Storage byte used as the first argument by many SET commands.
pub const NOSTORE: u8 = 0x00;
pub const VARSTORE: u8 = 0x01;

/// Whole-keyboard backlight LED id.
pub const BACKLIGHT_LED: u8 = 0x05;

/// Firmware version: args `[major, minor]`. Verified (Basilisk V3).
pub const FIRMWARE: Command = Command::new(0x00, 0x01);
pub const FIRMWARE_SIZE: u8 = 0x02;

/// Serial number: 22 ASCII bytes. Verified (Basilisk V3).
pub const SERIAL: Command = Command::new(0x00, 0x02);
pub const SERIAL_SIZE: u8 = 0x16;

/// Receiver's wireless devices: `[count, status, pid_hi, pid_lo, ...]`.
/// GET 00:BF verified with a HyperPolling dongle and DeathAdder V3 Pro.
pub const WIRELESS_DEVICES: Command = Command::new(0x00, 0x3F);
pub const WIRELESS_DEVICES_SIZE: u8 = 0x50;

/// Device mode: `[mode, param]`; 0x00 = normal, 0x03 = driver mode.
pub const DEVICE_MODE: Command = Command::new(0x00, 0x04);
pub const DEVICE_MODE_SIZE: u8 = 0x02;

/// Polling rate (up to 1000 Hz): `[code]`, see [`polling_code`].
pub const POLLING: Command = Command::new(0x00, 0x05);
pub const POLLING_SIZE: u8 = 0x01;

/// Polling rate for 2000-8000 Hz devices: `[0x00, code]`.
pub const POLLING_HS: Command = Command::new(0x00, 0x40);
pub const POLLING_HS_SIZE: u8 = 0x02;

/// Active DPI: `[store, x_hi, x_lo, y_hi, y_lo, 0, 0]`. GET verified (Basilisk V3).
pub const DPI: Command = Command::new(0x04, 0x05);
pub const DPI_SIZE: u8 = 0x07;

/// Battery level: reply `[_, level 0..=255]`.
pub const BATTERY: Command = Command::new(0x07, 0x00);
/// Charging state: reply `[_, 0|1]`.
pub const CHARGING: Command = Command::new(0x07, 0x04);
pub const POWER_SIZE: u8 = 0x02;

/// Extended matrix brightness: `[store, led, 0..=255]`.
pub const MATRIX_BRIGHTNESS: Command = Command::new(0x0F, 0x04);
pub const MATRIX_BRIGHTNESS_SIZE: u8 = 0x03;

/// Extended matrix effect: `[store, led, effect, flags, rate, colors, r, g, b, ...]`.
pub const MATRIX_EFFECT: Command = Command::new(0x0F, 0x02);
pub const EFFECT_OFF: u8 = 0x00;
pub const EFFECT_STATIC: u8 = 0x01;
/// Show the colors last sent with [`MATRIX_FRAME`]. Not yet verified on hardware.
pub const EFFECT_CUSTOM_FRAME: u8 = 0x08;

/// One row of per-key colors: `[profile, region, row, start_col, end_col, r, g, b, ...]`,
/// size `5 + 3 * keys`. Profile 0 is the live (unsaved) frame. Layout from Synapse's
/// `setChromaFrame`; not yet verified on hardware.
pub const MATRIX_FRAME: Command = Command::new(0x0F, 0x03);
/// Colors per frame packet: (80 argument bytes - 5 header bytes) / 3.
pub const MATRIX_FRAME_MAX_KEYS: usize = 25;

// Older ("standard") lighting commands, used by devices from before ~2019.
// Packet layouts as documented by OpenRazer.

/// LED on/off: `[store, led, 0|1]`.
pub const LED_STATE: Command = Command::new(0x03, 0x00);
/// LED color: `[store, led, r, g, b]`.
pub const LED_RGB: Command = Command::new(0x03, 0x01);
/// LED effect: `[store, led, effect]`, effect 0 = static.
pub const LED_EFFECT: Command = Command::new(0x03, 0x02);
/// LED brightness: `[store, led, 0..=255]`.
pub const LED_BRIGHTNESS: Command = Command::new(0x03, 0x03);
pub const CLASSIC_STATIC: u8 = 0x00;
/// Whole-grid effect: `[effect, ...]`: off `[0x00]`, static `[0x06, r, g, b]`,
/// custom frame `[0x05, store]`.
pub const GRID_EFFECT: Command = Command::new(0x03, 0x0A);
pub const GRID_OFF: u8 = 0x00;
pub const GRID_CUSTOM_FRAME: u8 = 0x05;
pub const GRID_STATIC: u8 = 0x06;
/// One row of a custom frame: `[0xFF, row, start_col, end_col, r, g, b, ...]`, size 0x46.
pub const GRID_FRAME: Command = Command::new(0x03, 0x0B);
pub const GRID_FRAME_SIZE: u8 = 0x46;
/// Per-LED effect on a few Naga models: off `[store, led, 0x00]`, static `[store, led, 0x06, r, g, b]`.
pub const MOUSE_LED_EFFECT: Command = Command::new(0x03, 0x0D);

/// What one button does: `[slot, button, layer, function, size, data0..data4]`, size 0x50.
/// Layout from Synapse's `setSingleButtonAssignment`; GET and SET verified (Basilisk V3).
/// Replies always carry 5 data bytes but report size 1, so readers take all 5.
pub const BUTTON: Command = Command::new(0x02, 0x0C);
pub const BUTTON_SIZE: u8 = 0x50;
/// Button slot 0 is live and unsaved, and wins over the saved profile slots 1-5.
/// Verified (Basilisk V3): a remap in slot 0 took effect while slot 1 was active.
pub const BUTTON_LIVE_SLOT: u8 = 0x00;
/// Active saved on-board profile slot: `[slot]`. GET verified (Basilisk V3: 1 of 5).
pub const ACTIVE_SLOT: Command = Command::new(0x05, 0x04);

/// Polling rate code (sent to the device) for a rate in Hz.
pub fn polling_code(hz: u32) -> Option<u8> {
    match hz {
        1000 => Some(0x01),
        500 => Some(0x02),
        125 => Some(0x08),
        _ => None,
    }
}

pub fn polling_hz(code: u8) -> Option<u32> {
    match code {
        0x01 => Some(1000),
        0x02 => Some(500),
        0x08 => Some(125),
        _ => None,
    }
}

/// High-speed polling codes (8 kHz-capable devices).
pub fn polling_hs_code(hz: u32) -> Option<u8> {
    match hz {
        8000 => Some(0x01),
        4000 => Some(0x02),
        2000 => Some(0x04),
        1000 => Some(0x08),
        500 => Some(0x10),
        250 => Some(0x20),
        125 => Some(0x40),
        _ => None,
    }
}

pub fn polling_hs_hz(code: u8) -> Option<u32> {
    match code {
        0x01 => Some(8000),
        0x02 => Some(4000),
        0x04 => Some(2000),
        0x08 => Some(1000),
        0x10 => Some(500),
        0x20 => Some(250),
        0x40 => Some(125),
        _ => None,
    }
}
