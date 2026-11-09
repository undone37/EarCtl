# sonyctl

Control your **Sony WH-1000XM4** headphones from Linux: noise cancelling, ambient sound, equalizer, DSEE Extreme, Speak-to-Chat and battery, from a tray icon or the command line. No phone needed.

Sony's Sound Connect app only exists for Android and iOS. sonyctl talks to the headphones directly over Bluetooth, using the same protocol as the app.

## Features

- **Tray icon** that runs in the background, reconnects on its own and picks up changes you make with the buttons on the headphones
- **Panel** (left click on the tray icon):
  - noise cancelling, ambient sound or off
  - ambient level slider and Focus on Voice
  - Speak-to-Chat and DSEE Extreme
  - equalizer presets and a 6-band equalizer (Clear Bass, 400, 1k, 2.5k, 6.3k, 16k)
- **Quick menu** (right click) for switching modes
- **Command line** for scripts and keyboard shortcuts
- **German and English**, picked from your system language

## Supported headphones

| Model | Status |
|---|---|
| WH-1000XM4 | Tested |
| WH-1000XM3 | Should work (same protocol), untested |

Newer models (WH-1000XM5, WF-1000XM4 and later) use a different protocol version and are not supported. [aecca/sound-connect](https://github.com/aecca/sound-connect) supports them.

## Install

Download the package for your distribution from the [latest release](../../releases/latest):

```sh
# Debian / Ubuntu
sudo apt install ./sonyctl_*_amd64.deb

# Fedora / openSUSE
sudo dnf install ./sonyctl-*.x86_64.rpm

# Arch Linux
sudo pacman -U sonyctl-*-x86_64.pkg.tar.zst
```

The packages add sonyctl to autostart. Log out and back in, or run `sonyctl` once to start it right away.

### Build from source

You need Rust (1.85 or newer), `pkg-config` and the D-Bus development files (`libdbus-1-dev` on Debian/Ubuntu, `dbus-devel` on Fedora, `dbus` on Arch).

```sh
cargo install --path .
cp sonyctl.desktop ~/.config/autostart/   # optional: start on login
```

## Usage

1. Pair and connect your headphones in your system's Bluetooth settings, as usual.
2. Run `sonyctl`. A headphones icon appears in the system tray.
3. Left click it to open the panel, right click it for the quick menu.

The tray icon hides itself while the headphones are disconnected.

### Command line

```
sonyctl status                       battery, mode, equalizer, ...
sonyctl anc nc|off                   noise cancelling / off
sonyctl anc ambient [1-20] [voice]   ambient sound, optionally with Focus on Voice
sonyctl eq <preset>                  off rock pop jazz dance edm rnb acoustic bright excited
                                     mellow relaxed vocal treble bass speech custom user1-3
sonyctl eq <preset> <6 values>       preset with bands: Clear Bass 400 1k 2.5k 6.3k 16k (-10..10)
sonyctl dsee on|off                  DSEE Extreme
sonyctl stc on|off                   Speak-to-Chat
sonyctl autooff 5|30|60|180|removed|off
sonyctl poweroff
sonyctl raw <hex bytes>              send a raw protocol message (for exploring)
```

For example, bind `sonyctl anc ambient 20 voice` and `sonyctl anc nc` to keyboard shortcuts.

## Known limitations

- **One connection at a time.** While the tray icon is running, the command line can't connect ("UUID already registered"). Quit the tray first.
- **Panel position on Wayland.** Wayland doesn't let apps place their windows, so the panel opens where your desktop puts it, not right next to the icon.
- **No firmware updates.** Sony delivers updates through its own signed process. Rebuilding that risks bricking the headphones, so sonyctl leaves it out. Use the phone app for updates.
- `autooff` values come from [Gadgetbridge](https://gadgetbridge.org/) and haven't been checked on a WH-1000XM4 yet.

## How it works

The headphones speak Sony's "MDR" protocol over a Bluetooth RFCOMM channel (service UUID `96CC203E-5068-46AD-B32D-E316F5E069BA`). The message layout comes from Sony Sound Connect 13.3.1 for Android (`com.sony.songpal.tandemfamily.message.mdr.v1`) and was checked against the real headphones. Everything lives in `src/main.rs`: the protocol, the command line and the tray. The panel is in `src/panel.rs`.

## License

[MIT](LICENSE)

This project is not affiliated with, endorsed by or supported by Sony. Sony, WH-1000XM4 and related names are trademarks of Sony Group Corporation.
