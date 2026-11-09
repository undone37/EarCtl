# earctl

Control your **Sony WH-1000XM4** headphones and **Jabra Elite 10** earbuds from Linux: noise cancelling, ambient sound, equalizer, DSEE Extreme, Speak-to-Chat and battery, from a tray icon or the command line. No phone needed.

The vendor apps (Sony Sound Connect, Jabra Sound+/Moments) only exist for Android and iOS. earctl talks to the headphones directly over Bluetooth, using the same protocols as those apps.

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

| Model | Status | Features |
|---|---|---|
| Sony WH-1000XM4 | Tested | everything listed above |
| Sony WH-1000XM3 | Should work (same protocol), untested | |
| Jabra Elite 10 | Tested | battery (each earbud and the case), ANC / HearThrough / off, 5-band equalizer |

Newer Sony models (WH-1000XM5, WF-1000XM4 and later) use a different protocol version and are not supported. [aecca/sound-connect](https://github.com/aecca/sound-connect) supports them. Other recent Jabra earbuds probably work too, but haven't been tested.

If several supported headphones are connected, earctl uses the first one it finds. Pick one with `EARCTL_DEVICE`, e.g. `EARCTL_DEVICE=Jabra earctl status`.

## Install

Download the package for your distribution from the [latest release](../../releases/latest):

```sh
# Debian / Ubuntu
sudo apt install ./earctl_*_amd64.deb

# Fedora / openSUSE
sudo dnf install ./earctl-*.x86_64.rpm

# Arch Linux
sudo pacman -U earctl-*-x86_64.pkg.tar.zst
```

The packages add earctl to autostart. Log out and back in, or run `earctl` once to start it right away.

### Build from source

You need Rust (1.85 or newer), `pkg-config` and the D-Bus development files (`libdbus-1-dev` on Debian/Ubuntu, `dbus-devel` on Fedora, `dbus` on Arch).

```sh
cargo install --path .
cp earctl.desktop ~/.config/autostart/   # optional: start on login
```

## Usage

1. Pair and connect your headphones in your system's Bluetooth settings, as usual.
2. Run `earctl`. A headphones icon appears in the system tray.
3. Left click it to open the panel, right click it for the quick menu.

The tray icon hides itself while the headphones are disconnected.

### Command line

```
earctl status                       battery, mode, equalizer, ...
earctl anc nc|off                   noise cancelling / off
earctl anc ambient [1-20] [voice]   ambient sound, optionally with Focus on Voice
earctl eq <preset>                  off rock pop jazz dance edm rnb acoustic bright excited
                                     mellow relaxed vocal treble bass speech custom user1-3
earctl eq <preset> <6 values>       Sony: preset with bands Clear Bass 400 1k 2.5k 6.3k 16k (-10..10)
earctl eq <gains...>                bands of the active preset (Jabra: dB per band, see status)
earctl dsee on|off                  DSEE Extreme
earctl stc on|off                   Speak-to-Chat
earctl autooff 5|30|60|180|removed|off
earctl poweroff
earctl raw <hex bytes>              send a raw protocol message (for exploring)
```

For example, bind `earctl anc ambient 20 voice` and `earctl anc nc` to keyboard shortcuts.

## Known limitations

- **One connection at a time.** While the tray icon is running, the command line can't connect ("UUID already registered"). Quit the tray first.
- **Panel position on Wayland.** Wayland doesn't let apps place their windows, so the panel opens where your desktop puts it, not right next to the icon.
- **No firmware updates.** Sony and Jabra deliver updates through their own signed processes. Rebuilding that risks bricking the headphones, so earctl leaves it out. Use the phone app for updates.
- `autooff` values come from [Gadgetbridge](https://gadgetbridge.org/) and haven't been checked on a WH-1000XM4 yet.

## How it works

- **Sony** headphones speak the "MDR" protocol over a Bluetooth RFCOMM channel (service UUID `96CC203E-5068-46AD-B32D-E316F5E069BA`). The message layout comes from Sony Sound Connect 13.3.1 for Android (`com.sony.songpal.tandemfamily.message.mdr.v1`). See `src/sony.rs`.
- **Jabra** earbuds speak the GN Protocol (GNP) over the standard serial port service (SPP). The packet layout and commands come from the native library in Jabra Moments 5.21 (`libjabralib.so`). See `src/jabra.rs`.

Both were checked against real devices. `src/main.rs` holds the command line, the tray and the device-independent status; the panel is in `src/panel.rs`.

## License

[MIT](LICENSE)

This project is not affiliated with, endorsed by or supported by Sony or GN Audio (Jabra). Sony, WH-1000XM4, Jabra, Elite 10 and related names are trademarks of their respective owners.
