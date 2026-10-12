# LightSync

Your Govee lights follow what's on your screen, like DreamView, but for **every** Govee light with
Wi-Fi (LAN Control) or Bluetooth, not just the handful the Govee app allows. Everything stays on your
network: no cloud, no account. One small Windows app, open source and free.

```
screen ──DXGI Desktop Duplication──▶ GPU averages it down (mipmaps) ──▶ ~160 px picture
                                                                          │
                          zones per light (edges, grid, bands, whole screen), letterbox aware
                                                                          │
                          colour: saturation, gamma, white balance, black level, smoothing
                                                                          │
              ┌───────────────────────────────┬───────────────────────────┘
   Wi-Fi: UDP "razer" frames (per segment)    Bluetooth: GATT writes (whole light or segments)
```

- **Any Govee light.** Wi-Fi lights stream every segment with the `razer` protocol the Govee app
  uses for DreamView; any LAN light can also take one colour for the whole light. Bluetooth lights
  follow with one colour, or per segment on RGBIC models.
- **Frugal.** The screen is averaged down on the GPU and only a tiny picture reaches the CPU. The
  capture sleeps until something on screen actually changes, a light only gets a packet when its
  colour really moved, and pausing lets go of everything. Measured on a 5120 × 1440 screen: 0.4% of
  one core on a still screen, about 2% while a video plays to three Wi-Fi lights (Bluetooth lights add
  roughly 2% each), 45 MB of memory.
- **Configurable.** Per light: which part of the screen it follows, segment count, direction, start,
  depth, update rate, its own colour tuning, and what happens when LightSync stops. For everything:
  display, analysis rate, detail, black bar detection and shared colour tuning. Changes apply live.
- **Polite.** When sync pauses or the app quits, each light goes back to the colour and power it had.

## Install

Grab `LightSync-<version>-windows-x64.zip`, unzip it and double click `LightSync.exe`.
**Set Up LightSync** walks you through it:

1. **LightSync.** Installs for you only, in `%LOCALAPPDATA%\Programs\LightSync`, starts quietly in
   the tray when you sign in, and adds a Start menu entry. No administrator needed.
2. **Local network.** Adds one Windows Firewall rule so your lights' replies reach LightSync (only for
   LightSync, only UDP port 4002). Windows asks for permission once.
3. **Your lights.** Shows what it found over Wi-Fi and Bluetooth.

For Wi-Fi lights, switch on **LAN Control** first: Govee app → the light → gear icon → LAN Control.

**Uninstall** from Settings > Apps > Installed apps > LightSync, or the tray menu. It lists exactly what
it removes, can keep your settings for next time, and hands every light back first.

## Use

Click the tray icon to open LightSync:

- **Sync with screen** turns it all on or off (also in the tray menu).
- **Add lights** scans Wi-Fi and Bluetooth and adds a light with one click. A light that shows up on
  both is marked, since Wi-Fi is far faster.
- **A light's page** shows the screen with each zone painted in the colour its segment is showing
  right now. Pick what it follows:
  - *Screen edges*: the sides it runs along, clockwise or not, how many segments, how deep into the
    screen. *Reverse* and *Shift start by* line the zones up with where the strip begins.
  - *Whole screen* (or just the middle), *Grid* (panels, curtain lights, zigzag wiring) and *Bands*.
  - **Identify** lights the first segment red and the last blue with a white dot walking between
    them, so you can match segment count and direction.
- **Screen & colour** picks the display, how often to analyse it, black bar detection, and the colour
  tuning every light shares unless it has its own.

### From the terminal

```powershell
LightSync status                   # sync state and every light
LightSync pause | resume           # lights go back to how they were while paused
LightSync discover                 # every Govee light over Wi-Fi and Bluetooth
LightSync identify "H6046 471C"    # red first segment, blue last
LightSync start                    # run the engine in the terminal instead of the tray
LightSync quit                     # quit the running app, handing every light back
LightSync install | uninstall      # --quiet for no dialogs, --window for the checklist
```

Settings live in `%APPDATA%\LightSync\settings.json`, the log in `%LOCALAPPDATA%\LightSync\lightsync.log`.

## Which mode for my light?

| Light | Use |
| --- | --- |
| Wi-Fi with LAN Control | **Per segment**. If it ignores that, **Whole light** works on every LAN model. |
| Bluetooth only | **Whole light**. If the colour doesn't change, try another *Colour command*. RGBIC models can try **Per segment**. |

Verified on H6046, H6056 and H618A over Wi-Fi (per segment) and H6008 over Bluetooth. Reports for other
models are welcome.

## How it talks to the lights

The wire formats are in [`crates/lightsync-core/src/govee.rs`](crates/lightsync-core/src/govee.rs), with tests.

- **LAN API** (UDP JSON): a multicast scan to `239.255.255.250:4001`, replies on port `4002`, commands
  to `<ip>:4003`. Lights only answer `devStatus` to a computer that scanned them recently, and drop
  control commands sent back to back, so LightSync scans first and spaces commands ~200 ms apart.
- **razer** (DreamView streaming): `{"cmd":"razer","data":{"pt":base64(packet)}}` with
  `packet = BB <length16> B0 <gradient> <n> r g b … <xor>`, switched on and off with
  `BB 00 01 B1 01|00 <xor>`.
- **Bluetooth**: 20 byte packets written to `00010203-0405-0607-0809-0a0b0c0d2b11`, the payload padded
  to 19 bytes plus an XOR checksum: `33 01` power, `33 05 0D|02 r g b` colour,
  `33 05 15 01 r g b 00×5 <mask>` segment colour, `AA 01` keepalive.

Thanks to OpenRGB (`GoveeController`), Home Assistant (`govee_light_local`, `govee-ble`) and
`wez/govee2mqtt`, whose reverse engineering this builds on.

## Crates

| Crate | What it does |
| --- | --- |
| `lightsync-core` | Govee wire formats, screen zones, letterbox detection, colour processing and settings. No OS code. |
| `lightsync` | `LightSync.exe`: DXGI capture, the LAN and Bluetooth senders, the engine, the tray, the windows (egui), the control pipe, setup and every command. |

The tray hosts the engine; each window is its own process that talks to it over a named pipe
(`\\.\pipe\LightSync-<session>`), so closing a window gives all its memory back while sync carries on.

## Build

Needs Rust (`winget install Rustlang.Rustup`) and the Visual Studio C++ build tools with a Windows SDK.

```powershell
cargo test
cargo build --release                                         # target\release\LightSync.exe
powershell -ExecutionPolicy Bypass -File scripts\package.ps1  # dist\LightSync-<version>-windows-x64.zip
```

## Not yet

- HDR desktops come through tone mapped to 8 bits, so colours can look a little flat.
- Rotated (portrait) displays aren't accounted for in the zones.
- Windows only for now; the core crate has no OS code, so other platforms need only capture and senders.

An independent project, not affiliated with or endorsed by Govee. MIT licensed.
