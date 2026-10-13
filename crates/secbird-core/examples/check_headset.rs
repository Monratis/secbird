//! Opt-in hardware test: change one safe setting at a time, verify, restore.
//! cargo run -p secbird-core --example check_headset -- --write
use secbird_core::{discover, hidapi::HidApi, Db, Error, Result};

fn main() -> Result<()> {
    if !std::env::args().any(|a| a == "--write") {
        return Err(Error::InvalidArg("pass --write to allow temporary audio changes".into()));
    }
    let api = HidApi::new()?;
    let db = Db::builtin()?;
    let devices = discover(&api, &db);
    let dev = devices.iter().find(|d| d.pid == secbird_core::headset::PID)
        .ok_or_else(|| Error::NotFound("BlackShark V3 Pro".into()))?;
    let (original, errors) = dev.audio_state();
    if !errors.is_empty() { return Err(Error::System(format!("baseline reads failed: {errors:?}"))); }
    // Record baseline before any SET, independently of the user's profiles.
    std::fs::write("target/headset-before-check.json", serde_json::to_string_pretty(&original).unwrap())
        .map_err(|e| Error::System(e.to_string()))?;
    let result = (|| -> Result<()> {
        for (name, values) in &original {
            if name == "mic-mute" { continue; }
            let mut changed = values.clone();
            match name.as_str() {
                "anc" => changed[1] = if values[1] == 3 { 2 } else { 3 },
                "sidetone" => changed[0] = if values[0] == 0 { 1 } else { 0 },
                "mic-mute" => changed[0] = 1 - values[0],
                "eq-preset" => changed[0] = (values[0] + 1) % 6,
                "mic-preset" => changed[0] = 32 + (values[0] - 32 + 1) % 4,
                "mic-eq" => changed[0] = if values[0] == 0 { -1 } else { 0 },
                n if n.starts_with("eq-") => changed[0] = if values[0] == 0 { -1 } else { 0 },
                _ => {}
            }
            dev.audio_set(name, &changed)?;
            dev.audio_set(name, values)?;
            println!("ok: {name} change/read-back/restore");
        }
        Ok(())
    })();
    // Restore even after a failed check. Presets first (they can reset the mic EQ).
    let mut failed = Vec::new();
    for (name, values) in original.iter().filter(|(n, _)| n.ends_with("preset"))
        .chain(original.iter().filter(|(n, _)| !n.ends_with("preset"))) {
        if name == "mic-mute" { continue; }
        if let Err(e) = dev.audio_set(name, values) { failed.push(format!("{name}: {e}")); }
    }
    if !failed.is_empty() { return Err(Error::System(format!("RESTORE FAILED: {failed:?}; baseline in target/headset-before-check.json"))); }
    let (after, errors) = dev.audio_state();
    if after != original || !errors.is_empty() { return Err(Error::System("final baseline verification failed".into())); }
    println!("All original audio settings restored; profiles untouched.");
    result
}
