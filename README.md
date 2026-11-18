# mchose-battery

A tiny Windows tray utility that reads the battery level of an **MCHOSE K7 Ultra**
wireless gaming mouse directly from its vendor HID interface.

No driver, no vendor software, no background service dependency, and no external
Rust crates — just direct Win32 HID/SetupAPI calls.

## Tested device

- **Mouse:** MCHOSE K7 Ultra
- **Connection:** 2.4 GHz USB dongle
- **USB IDs:** dongle `5253:1020`, wireless device `3837:4150`
- **Firmware reported by the mouse:** `328988`

Other MCHOSE mice using the same `0x11/0x06` feature-report protocol
(`0x5253` / `0x3837` vendor collection on usage page `0xFF01`) should work too,
but only the K7 Ultra has been verified.

## Features

- One-shot battery read, console polling, or a system-tray icon.
- Tray icon draws the battery percentage directly; hovering shows
  `MCHOSE 90% 2.4G` and right-click offers **Refresh** / **Exit**.
- Updates every 5 seconds.
- Runs with no console window (`windows_subsystem = "windows"`).
- Single-instance guard: launching it twice will not create a second tray icon.
- Optional autostart at logon via the per-user `Run` registry key.

## Build

With Visual Studio C++ build tools installed:

```powershell
cargo build --release
```

Binary: `target\release\mchose-battery.exe`

On a machine without the MSVC linker, use the GNU toolchain instead:

```powershell
cargo build --release --target x86_64-pc-windows-gnu
```

Binary: `target\x86_64-pc-windows-gnu\release\mchose-battery.exe`

## Usage

```powershell
# read the battery once
mchose-battery.exe

# poll every 5 seconds in the terminal
mchose-battery.exe --watch

# run as a background tray icon (no console window)
mchose-battery.exe --tray

# enable / disable autostart at logon
mchose-battery.exe --install
mchose-battery.exe --uninstall
```

Example output:

```
battery: 90%  link: 2.4G  model: 3837:4150  fw: 328988
```

Because the binary is a GUI-subsystem app, console modes print nothing when it
is launched from Explorer. Run it from a terminal if you want the text output.

Autostart writes `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\MCHOSE Battery`
pointing at the executable with `--tray`.

## How it works

Windows does not expose this mouse through the standard battery API. The tool
enumerates HID devices with VID `0x5253` or `0x3837` whose top-level collection
uses usage page `0xFF01`, then sends feature report `0x11` with command `0x06`.
The payload is XOR-masked with `0xFF`. The reply layout is:

```
vid:u16  pid:u16  firmware:u32  flags:u8  battery:u8  charging:u8
```

`flags & 0b111` is the link mode (`1` = 2.4 GHz dongle) and `flags & 8` means
connected. A 25 ms pacing floor between transactions keeps the mouse from
stalling its config channel.

Protocol reference: [mchose-linux/PROTOCOL.md](https://github.com/alexfrih/mchose-linux/blob/main/PROTOCOL.md)

## Notes

- The MCHOSE logo used by the Desktop shortcut is not included in this
  repository; it is copied locally from the MCHOSE HUB installation.
- Built and tested on Windows 11.

## License

MIT
