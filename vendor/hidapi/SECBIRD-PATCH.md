# Local hidapi 2.6.7 patch

Source: crates.io `hidapi` 2.6.7, unchanged except the Windows native metadata reader.
Original license: `LICENSE.txt`.

The connected HyperPolling receiver (1532:00B3) stalls indefinitely on
`HidD_GetProductString` on its mouse collection. Avoid USB string descriptor queries
for that VID/PID in the shared `get_device_info` path; Windows ConfigManager still
provides interface/bus metadata. Secretarybird reads the mouse identity and serial
through its existing Razer protocol. No HID report behavior is changed.

Regression: `cargo test --manifest-path vendor/hidapi/Cargo.toml --no-default-features --features windows-native hyperpolling_never_reads_usb_string_descriptors`.
The crate's other tests need `tests/pp_data` fixtures that are not shipped with the
published crate, so they fail here regardless of this patch.
Remove the patch when upstream metadata enumeration no longer stalls on this firmware.
