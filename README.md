<picture>
  <source media="(prefers-color-scheme: light)" srcset="docs/banner-light.png">
  <img src="docs/banner.png" alt="Secretarybird: your Razer gear, minus the bloat." width="100%">
</picture>

A small, lightweight replacement for Razer Synapse on Windows. Set your Razer mouse,
keyboard and other peripherals: lighting, DPI, polling rate, button remapping and profiles, without
Synapse's background services, account, ads, pop-ups or restart prompts.

- **Light:** a 780 KB tray app using about 11 MB of RAM, next to the hundreds of MB Synapse keeps running.
- **Quiet:** no services, no updater, no telemetry, no internet. It sits in the tray and
  applies your settings when you sign in, plug a device in or wake the PC.
- **Built for many devices:** one screen for all of them, and one click to sync a color everywhere.

> **Beta.** Only the Basilisk V3 has been tested on real hardware so far. Other devices are
> supported from public protocol facts and may not work yet. [Tell us how yours does](#help-us-support-your-device).

### [⬇ Download for Windows](https://github.com/infinityInfinite/secbird/releases/latest)

Free · Windows 10 and 11 · on the page that opens, click `Secretarybird-Setup-<version>.exe`.

![All devices](docs/overview.png)

## Install

1. Go to the [**latest release**](https://github.com/infinityInfinite/secbird/releases/latest) and download
   `Secretarybird-Setup-<version>.exe`.
2. Run it. It installs for your Windows account only, so it doesn't ask for administrator rights.
3. The bird appears in the tray (click `^` next to the clock if it's hidden).
   Click it and choose **Open editor**.

**"Windows protected your PC"?** The installer isn't code-signed yet (signing costs money),
so Windows SmartScreen doesn't know it. Click **More info → Run anyway**. To check your download
matches the release, compare `Get-FileHash Secretarybird-Setup-<version>.exe` in PowerShell
with `SHA256SUMS.txt` on the release page.

Prefer no installer? Download `Secretarybird-<version>-portable.zip` instead, unzip it anywhere
and run `secbird-tray.exe`.

### Local hardware support changes

- HyperPolling receivers identify their connected mouse with a read-only device query,
  rather than showing only the dongle name. The receiver's profile key is kept.
- BlackShark V3 Pro (`1532:0577`) is discovered on its HID audio interface. Battery percentage
  and charging status, ANC/ambient mode and level, six speaker EQ slots and active slot,
  microphone EQ/presets, sidetone and auto-off are supported. Writes are read back from
  hardware before being saved to a profile. The microphone's physical mute switch is
  read-only. **THX spatial audio, OS volume and Synapse software voice enhancements are
  not implemented**; this is hardware control, not a replacement for the THX audio driver.
- HyperPolling rate changes write both live and stored selectors.

The editor has a **Let Windows control this device's lighting** switch on each device.
It appears only where Windows reports a LampArray for that device — that is, the device
has LEDs and Windows can drive them. Turning it on hands that one device's lighting to
Windows and stops Secretarybird writing it, including profiles and Sync; dragging the
device in Windows **Settings → Personalization → Dynamic lighting** then chooses its
colour and effects. The choice is stored per device and survives profile switches.
`secbird lamps` prints the native LampArray inventory.

Background (app-driven) control needs package identity and the
`com.microsoft.windows.lighting` extension: build the registered version with
`installer/lighting/build.ps1` after a release build. The generated MSIX is unsigned; it
must be signed with a trusted certificate before installation. The build script does not
change certificate trust or install anything. Then prioritize Secretarybird in Windows
Background light control. Secretarybird never bypasses Windows by writing raw HID
lighting to a device Windows owns, and never alters undocumented Windows registry
priorities.

Keyboards with a separate game-controller HID child have a disable/re-enable button.
Only game-controller collections of the selected physical keyboard are targeted, never
its keyboard/mouse/lighting interfaces or USB parent. Windows asks for administrator
approval. A disconnected/remembered controller is shown separately; Windows may require
it to be reconnected before disabling it.

CLI examples: `secbird -d 0577 audio`, `secbird -d 0577 audio anc 1 3`,
`secbird -d 0577 audio mic-eq -5 -4 -4 -3 -2 1 2 3 3 3`,
`secbird -d 02A7 windows-lighting on`, `secbird -d 02A7 gamepad off`.

**Updating:** run the newer installer; your settings are kept.
**Uninstalling:** Windows Settings → Apps → Secretarybird. It asks whether to keep your profiles.

## Using it

![A mouse with three profiles side by side](docs/mouse.png)

| Remapping a button | Keyboard, per-key |
|---|---|
| ![Button editor](docs/buttons.png) | ![Keyboard page](docs/keyboard.png) |

<sub>Screenshots: the simulated devices built into the app, next to a real Basilisk V3.</sub>

- **Tray icon:** switch profiles per device, re-apply settings, Start with Windows. Hover it to see
  each device's profile and battery.
- **Editor:** opens in your browser but runs only on your PC (`127.0.0.1`), protected by a
  one-time key. Each device is one sheet: settings are rows, your profiles are columns, and the
  active profile is lit. Click any cell to change it: colors and brightness per zone, DPI, polling
  rate, buttons, and paint-by-key on keyboards with per-key lighting.
- **Button remapping** (mice with on-board button memory): keys and shortcuts, turbo, sniper/DPI
  clutch, DPI and profile buttons, media keys, double click, disable, and a **HyperShift** layer for
  a second function while a HyperShift button is held. Changes go to the mouse's live memory, so
  what Synapse or the factory saved stays put, and Reset brings it back. Verified on the Basilisk V3.
- **Popups:** a small popup above the clock when you change DPI with the mouse's button, switch
  its on-board profile, or when a per-app profile kicks in. The editor follows DPI changes too.
- **Profiles** are saved per physical device, so two identical mice keep their own settings.
  Back them up with `secbird export backup.json`.
- **Per-app profiles:** tie a profile to games or apps (in its column header in the editor, or
  `secbird profile apps Gaming cs2.exe`). It switches while that program is in front and goes
  back when you leave it, button remaps included. Windows tells the tray when the window in front
  changes, so nothing polls.

## Moving from Synapse

1. Close Synapse (tray icon → Exit). If both run, Synapse overwrites your settings, and
   Secretarybird warns you about that.
2. Optional: turn off Razer's background services and startup entries with `secbird debloat`
   (below), or uninstall Synapse completely. Your devices keep working: Secretarybird talks to
   them directly and doesn't need Razer's services.

## Command line

The installer can add `secbird` to PATH (tick the box); otherwise run it from the install folder,
`%LOCALAPPDATA%\Programs\Secretarybird`.

```
secbird list                    connected devices
secbird info                    firmware, serial, DPI, polling rate, battery
secbird dpi 1600                set DPI
secbird poll 1000               set polling rate
secbird color logo FF8800       static color for a zone
secbird keys 00FFAA             every key one color (per-key keyboards)
secbird sync 00FFAA             same color on every device
secbird profile new gaming      profiles per device
secbird profile apps gaming cs2.exe   switch to a profile while a program is in front
secbird button                  what each mouse button does
secbird button side-forward key:ctrl+c   remap a button (add --hypershift for that layer)
secbird button side-forward reset        back to what is saved in the mouse
secbird export backup.json      save all profiles to a file
secbird report                  read-only description of your hardware, for issues
secbird --version
```

Run `secbird help` for everything. With several devices connected, pick one with `-d PID` or
`-d SERIAL`.

### Debloat

- `secbird debloat` only **lists** what Razer runs in the background.
- `secbird debloat apply` turns it off. It needs a terminal opened with **Run as administrator**,
  prints every Windows command before running it, and saves a backup.
- `secbird debloat undo` puts back exactly what `apply` changed.

Drivers are never touched.

## Help us support your device

Run `secbird report` and [open a device report](https://github.com/infinityInfinite/secbird/issues/new?template=device-report.yml),
saying what works and what doesn't. The report only reads from your devices and shortens serial
numbers. Reports are how devices move from "listed" to "verified".

| Status | Devices |
|---|---|
| Verified on real hardware (by us) | Basilisk V3 |
| Reported working by users | Cobra (wired), Basilisk V3 Pro 35K Phantom Green Edition, BlackWidow V3 Mini HyperSpeed, Goliathus Chroma, Thunderbolt 4 Dock Chroma |
| Listed, waiting for reports | 268 more mice, keyboards, keypads, mousepads, laptops and accessories |
| Not supported yet | Kraken headsets, laptop cooling pads, the Chroma Addressable RGB Controller (shows up, but has no setup screen yet) |

"Reported working" means a user said so; a `secbird report` from them moves a device to verified.

## Files

Settings live in `%APPDATA%\Secretarybird`: `profiles.json`, `secbird.log` (helpful for bug
reports) and an optional `devices.json` override. The tray menu has **Open settings folder**.

## Build from source

Needs Rust (stable, MSVC) on Windows 10/11.

```
cargo build --release
```

The installer is built with [Inno Setup 6](https://jrsoftware.org/isinfo.php):
`ISCC.exe /DAppVersion=0.1.0 installer\secbird.iss`. Pushing a `v*` tag builds the installer,
portable zip and checksums on GitHub into a draft release.

To try the editor without hardware, start the tray with simulated devices:

```
$env:SECBIRD_SIM="mouse,wireless,keyboard,old-keyboard"
.\target\release\secbird-tray.exe
```

## Legal

Secretarybird contains no code or assets from Razer. Device facts are restated from the OpenRazer
project; see [NOTICE](NOTICE). Razer, Synapse and Chroma are trademarks of Razer Inc.; this project
is not affiliated with or endorsed by Razer.

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
