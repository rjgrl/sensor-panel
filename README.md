# DS916 Sensor Panel

A fully open-source sensor panel system for the **Jonsbo DS916** (and compatible ArtInChip-based USB LCD screens), built by reverse-engineering the device's USB protocol from scratch.

Design custom themes visually, stream live hardware sensor data from [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor), and run everything silently in the background — no proprietary software required. Don't want to design from scratch? The built-in **✨ AI Theme Generator** can build a complete, good-looking theme for you in one click.

> All hardware sensor data comes from [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor) (free, open source, MPL-2.0) — one monitoring app, no time limits, nothing to restart.

---

## Background — How This Was Discovered

The Jonsbo DS916 ships with the **JONSBO-AIO** application, which is the only officially supported way to use the screen. There was no public documentation of how the device communicates with a PC.

Through USB traffic analysis using Wireshark and USBPcap, we reverse-engineered the complete communication protocol:

### Device Identification

The DS916 enumerates as two USB devices:

| Device | VID | PID | Description |
|--------|-----|-----|-------------|
| Composite | `0x33C3` | `0xF101` | ArtInChip USB composite device |
| Serial | `0x33C3` | `0xF101 MI_00` | Virtual COM port (CDC ACM) |

The chip inside is made by **ArtInChip** (广东匠芯创科技有限公司), a Chinese semiconductor company. Their SDK (`Luban-Lite`) is open source and uses **CherryUSB** as the USB stack. The screen presents itself as a USB CDC serial device (`COM3` on most systems).

The manufacturer DLL bundled with JONSBO-AIO — `MSDISPLAYSDKWRRAPER.dll` — contains an embedded copy of **libjpeg-turbo**, which confirmed our hypothesis that the screen receives JPEG frames rather than raw pixel data.

### Protocol

The DS916 accepts a continuous stream of **JPEG frames** over the CDC serial COM port. Each frame consists of a **60-byte header** followed by the raw JPEG data.

**Frame structure:**
```
[60-byte header][JPEG data]
```

**Header format (60 bytes, little-endian):**
```
Offset  Size  Value         Description
------  ----  -----         -----------
0       1     0x03/0x00     Frame type: 0x03 on first frame, 0x00 thereafter
1       1     0x3C (60)     Header length
2-4     3     0x000000      Reserved
5       1     0x06          Channel/endpoint identifier
6-7     2     0x0000        Reserved
8-11    4     uint32 LE     Total packet size (JPEG size + 60)
12-16   5     0x00...       Reserved
17      1     0x4F ('O')    \
18      1     0x54 ('T')     > Magic bytes: "OT\x06"
19      1     0x06          /
20-23   4     (varies)      Timestamp/counter — device ignores this, zero is fine
24      1     0x1B          Constant
25-31   7     (varies)      Sequence-related — device ignores, zero is fine
32      1     0x00          Reserved
33      1     0x1B          Constant
34      1     0x00          Reserved
35-38   4     (varies)      Frame group counter — device ignores, zero is fine
39-43   5     89 B3 FF FF   Constant
              00
44-55   12    00 00 00 09   Constant protocol flags
              00 00 01 00
              03 00 02 03
56-59   4     uint32 LE     JPEG payload size (confirmed — must be exact)
```

**Key findings:**
- The baud rate setting is irrelevant — the device operates at USB 2.0 speeds regardless
- Only bytes 0, 8-11, and 56-59 need to be set correctly; all timestamp/counter fields can be zeroed
- Frames should be sent continuously at 5-10 fps to keep the display active
- Screen resolution is **462 × 1920 pixels** (portrait) or **1920 × 462** (landscape)
- JPEG quality of 85-92 gives a good balance of quality and throughput

**Minimum working Python example:**
```python
import serial, struct, io
from PIL import Image

def make_frame(jpeg_bytes, first=False):
    header = bytearray(60)
    header[0]  = 0x03 if first else 0x00
    header[1]  = 0x3C
    header[5]  = 0x06
    header[17] = 0x4F  # 'O'
    header[18] = 0x54  # 'T'
    header[19] = 0x06
    header[24] = 0x1B
    header[33] = 0x1B
    header[39:44] = bytes([0x89, 0xB3, 0xFF, 0xFF, 0x00])
    header[44:56] = bytes([0x00,0x00,0x00,0x09,0x00,0x00,0x01,0x00,0x03,0x00,0x02,0x03])
    struct.pack_into('<I', header, 56, len(jpeg_bytes))
    struct.pack_into('<I', header,  8, len(jpeg_bytes) + 60)
    return bytes(header) + jpeg_bytes

img = Image.new('RGB', (462, 1920), color=(20, 20, 40))
buf = io.BytesIO()
img.save(buf, format='JPEG', quality=88)
jpeg = buf.getvalue()

port = serial.Serial('COM3', baudrate=115200, timeout=2)
for i in range(60):
    port.write(make_frame(jpeg, first=(i==0)))
    port.flush()
port.close()
```

---

## Project Structure

```
sensor-panel/
├── theme_builder.html    # Visual theme designer (open in Chrome or Edge)
├── ds916_tray.py         # Background renderer + system tray app
├── build.bat             # Build + install script — compiles the exe, closes
│                         # any running instance, installs it and
│                         # theme_builder.html to %APPDATA%\DS916Tray\, creates
│                         # Desktop/Start Menu shortcuts (first run only), and
│                         # offers to relaunch the app immediately
└── README.md
```

After running `build.bat`, this entire folder is no longer needed for normal use — everything the app uses lives in `%APPDATA%\DS916Tray\` from that point on. Keep the folder around if you plan to make further code changes.

**You should not need to use Uninstall as part of a normal edit/rebuild loop.** `build.bat` already closes any running instance before overwriting it, so simply re-running `build.bat` after a code change is enough — no need to uninstall first. Uninstall (tray icon → **🗑 Uninstall…**) is for when you actually want to remove the app from a machine, not as a routine step before every rebuild.

---

## Step-by-Step Installation

### Step 1 — Install Python

Download and install Python 3.9 or later from https://www.python.org/downloads/

> **Important:** During installation, check **"Add Python to PATH"**

Verify it works by opening a Command Prompt and running:
```
python --version
```

### Step 2 — Install LibreHardwareMonitor

1. Download the latest release from https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases and extract it to a permanent folder (e.g. `C:\Program Files\LibreHardwareMonitor`)
2. Run `LibreHardwareMonitor.exe` — it asks for administrator rights, which it needs to read CPU, motherboard and fan sensors
3. In the **Options** menu, enable:

| Setting | Why |
|---------|-----|
| ✅ **Remote Web Server → Run** | **Required** — this is how the tray app reads sensor data (`http://127.0.0.1:8085/data.json`) |
| ✅ **Run On Windows Startup** | LHM starts automatically with Windows |
| ✅ **Start Minimized** | Keeps LHM out of the way at startup |
| ✅ **Minimize To Tray** / **Minimize On Close** | Prevents accidentally stopping sensor data |

4. Leave the web server port at its default **8085** (or set the same port in DS916 Settings → LibreHardwareMonitor tab)
5. LHM will now start minimized to the system tray on every boot. There's no time limit — it can run indefinitely

> **Switching from HWiNFO64?** You can uninstall HWiNFO64 (or just turn off its Auto Start). Running both at the same time isn't needed, and two monitoring tools polling the same sensor chips can occasionally conflict.

### Step 3 — Download the Sensor Panel Files

Download or clone this repository:
```
git clone https://github.com/mike-novotny/sensor-panel.git
```
Or download the ZIP from the GitHub page and extract it to a folder of your choice.

### Step 4 — Build and Install the Tray App

Open a Command Prompt **in the folder where you extracted the files** and run:
```
build.bat
```

This will:
1. Install all Python dependencies (`pillow`, `pyserial`, `pystray`, `pyinstaller`)
2. Compile `ds916_tray.py` into a standalone `DS916Tray.exe`
3. Install `DS916Tray.exe` and `theme_builder.html` together into `%APPDATA%\DS916Tray\` — keeping the app, the theme builder, your config, discovered sensors, and saved themes all in one place for easy cleanup later
4. Create a **Desktop shortcut** and a **Start Menu shortcut**, both named "DS916 Tray", pointing at the installed copy

> The build takes 1-3 minutes. Once it finishes, the folder you extracted the project into (and its `dist\`/`build\` subfolders) is no longer needed — everything the app uses going forward lives in `%APPDATA%\DS916Tray\`. You can delete the original extracted folder if you'd like.

### Step 5 — First Launch

1. Launch **DS916 Tray** from the Desktop shortcut or Start Menu
2. A small icon appears in your system tray (bottom-right of the taskbar)
3. The app will:
   - Add itself to Windows startup automatically
   - Connect to LibreHardwareMonitor's web server
   - **Discover all your sensors** and save them to `%APPDATA%\DS916Tray\lhm_sensors.json`
   - Create a `%APPDATA%\DS916Tray\Themes\` folder for storing your theme files

> **If you've ever used JONSBO-AIO, make sure it's fully closed** (including from the system tray) before running DS916Tray — both cannot use the COM port at the same time. If you've never installed or used JONSBO-AIO, you can skip this.

### Step 6 — Design a Theme

1. Right-click the DS916 tray icon and choose **🎨 Open Theme Builder** — this opens `theme_builder.html` directly from its installed location in your browser (you no longer need to find the file manually)
2. When prompted, click **Browse for file…** and navigate to `%APPDATA%\DS916Tray\lhm_sensors.json` to load your system's sensor list
3. Design your theme (see [Theme Builder](#theme-builder) section below)
4. Click **💾 Save Theme** and save to `%APPDATA%\DS916Tray\Themes\`

### Step 7 — Load the Theme

1. Right-click the DS916 tray icon
2. Click **📂 Load Theme…**
3. Navigate to `%APPDATA%\DS916Tray\Themes\` and select your `.ds916theme` file
4. The screen should immediately start displaying your theme

---

## Preparing Image Assets

If you plan to use a background image or image layers, they must match the screen's resolution exactly.

### Canvas Dimensions

| Orientation | Width | Height |
|-------------|-------|--------|
| **Vertical** (default) | 462 px | 1920 px |
| **Horizontal** | 1920 px | 462 px |

Images that don't match these dimensions will be stretched to fit. For best results, create your background at exactly 462×1920 px (or 1920×462 px for landscape).

### Recommended Tools

Any image editor works — Photoshop, GIMP, Affinity Photo, or even Paint.NET. Create a new canvas at the correct dimensions, design your background, and export as PNG or JPG.

For animated backgrounds, MP4 video files are supported (AVI and WMV also work). The video loops automatically and plays silently.

### Background vs Image Layers

The theme builder has two separate ways to add visual assets:

**🖼 Background button** — sets the canvas background. This can be:
- A static image (PNG, JPG, GIF)
- A video file (MP4, AVI, WMV) — plays looped and silent behind all elements

**+ Image button** — adds a transparent image layer **on top of** the background and behind or in front of sensor elements (controlled by z-order). Use this for:
- Decorative overlays (borders, frames, logos)
- Semi-transparent panels behind groups of sensors
- Static artwork that sits above a video background

You can add multiple image layers. Each behaves like any other element — drag to reposition, resize with the handle, adjust z-order in the Layers panel.

---

## Tray App — Right-Click Menu

| Option | Description |
|--------|-------------|
| **▶ Start Display** | Start streaming the current theme to the screen |
| **⏹ Stop Display** | Stop streaming |
| **📂 Load Theme…** | Load a `.ds916theme` file |
| **🎨 Open Theme Builder** | Opens `theme_builder.html` in your browser |
| **🔍 Discover Sensors** | Re-scan LibreHardwareMonitor and update `lhm_sensors.json` |
| **ℹ Status…** | Live status: sensor source, COM port, sensor readings |
| **⚙ Settings…** | COM port, FPS, LibreHardwareMonitor connection, RTSS, logging |
| **🗑 Uninstall…** | Remove from startup and delete app data |
| **❌ Exit** | Close the tray app |

---

## Settings

Open via tray icon → **⚙ Settings…**

### General Tab

| Setting | Description |
|---------|-------------|
| **COM Port** | Serial port for the DS916 — click **Auto-detect** to find it automatically |
| **FPS** | Frames per second to stream (default: 6, max: 30) |
| **Theme File** | Path to the last loaded theme (auto-remembered) |
| **Start with Windows** | Adds/removes the app from Windows startup |

> **COM port auto-detection:** the app identifies the DS916 by its USB VID/PID (`33C3:F101`) and automatically updates the COM port setting if it changes between sessions.

### LibreHardwareMonitor Tab

- **Host / Port** — where LHM's Remote Web Server is listening. Default `127.0.0.1` : `8085`; only change it if you changed the port in LHM
- **↻ Test Connection** — shows whether LHM is reachable, its version, how many sensors it reports, and how many standard sensor keys were matched on your hardware
- If the connection fails, check that LHM is running and **Options → Remote Web Server → Run** is ticked

### RTSS (FPS) Tab

Configures the optional RivaTuner Statistics Server framerate source (see [Framerate Sensors](#framerate-sensors) above):

- **Connection status** — shows whether RTSS is currently running and how many active 3D applications it's tracking
- **Auto-detect active 3D app** (default) — automatically uses whichever hooked game most recently rendered a frame
- **Pin a specific process** — choose an exact process from a live dropdown (populated via **↻ Refresh List**) instead of relying on auto-detection, useful if you regularly run multiple games/3D apps at once

---

## Theme Builder

Open `theme_builder.html` in **Chrome or Edge** (not Firefox — system font loading requires Chrome/Edge).

### Interface Layout

| Area | Description |
|------|-------------|
| **Left panel** | Element palette — drag or click to add to canvas |
| **Center** | Canvas — drag elements to move, resize handle to resize |
| **Right panel** | Properties for the selected element |
| **Bottom** | Layers panel — visibility, z-order, delete |
| **Top bar** | Orientation, zoom, background, export controls |

### Adding a Background

1. Click **🖼 Background** in the top bar
2. Select a PNG, JPG, GIF, or video file (MP4, AVI, WMV)
3. The background appears behind all elements on the canvas
4. The background is embedded into the `.ds916theme` file on export — no separate file needed

To add decorative image layers on top of the background, use **+ Image** instead. These layers can be positioned, resized, and z-ordered like any other element.

### Loading Your Sensor List

Click **🔍 Sensors** in the top bar to import `lhm_sensors.json`. This loads all sensors from your specific hardware into the palette. Without this file, only standard sensor keys are available.

The sensor list is generated automatically by the tray app on every startup. If the file doesn't exist yet, run DS916Tray.exe first.

### Element Types

| Type | Description |
|------|-------------|
| **Clock** | Live time — 12h (no seconds, no leading zero) or 24h |
| **Clock (seconds)** | Live time with seconds — 12h zero-padded for stable AM/PM position |
| **Date** | Live date — configurable format (DD-MM-YYYY, MM/DD/YYYY, etc.) |
| **Day of Week** | Full (Tuesday) or short (Tue) |
| **Sensor Value** | Live sensor reading with optional prefix and unit |
| **Static Label** | Fixed text — double-click to edit inline |
| **Bar** | Horizontal progress bar |
| **Ring Gauge** | Circular gauge |
| **Line Graph** | Scrolling history graph |
| **Preset** (⊞) | Drops a label + value + bar group in one click |
| **Rectangle** | Decorative divider or block |
| **Image** | PNG/JPG overlay layer (above background) |

### Sensor Palette

The palette shows a curated default set of sensors. Use the **＋ Sensors** button to open the full picker with checkboxes — select any sensor to add it to the palette. Importing a `lhm_sensors.json` file adds your hardware-specific sensors (custom fans, liquid cooling temps, per-core data, etc.) to the picker.

### Resizing Elements

- **Text elements** — drag the resize handle (bottom-right corner) to scale **font size**
- **Bars, graphs, rings, rectangles** — drag the resize handle to change **width/height**
- **Arrow keys** — nudge 1px; Shift+arrow nudges 10px

### Fonts

In the Font section of the Properties panel:
- **🔍 System Fonts** — loads all fonts installed on your PC (Chrome/Edge only)
- **📁 Font File…** — loads a `.ttf` or `.otf` file; embedded in the theme on export so it works on any system

### Colors

Each color field has a color picker, an alpha (0-255) transparency field, a **+** button to save to the palette, and saved color swatches. Click any swatch to apply it.

### Export & Import

- **💾 Save Theme** — saves a single `.ds916theme` file with all assets (background, fonts, image layers) embedded as base64. The file picker defaults to `%APPDATA%\DS916Tray\Themes\` after first use
- **📂 Import** — loads a `.ds916theme` file. The file picker remembers the Themes folder

---

## ✨ AI Theme Generator

Don't want to design a theme by hand? Click **✨ AI Generate** in the top bar to instantly build a complete, good-looking theme — no design skills required.

This is a fully offline, rule-based generator (no internet connection, no API calls, no cost). It works by combining:

- **12 curated visual styles** ("vibes"), each with its own color palette, font, corner-radius personality, and a procedurally generated SVG background (gradients, starfields, scanlines, grid horizons, aurora bands, etc. — all drawn in code, no external image files)
- **7 distinct layout templates** (4 vertical, 3 horizontal), covering different arrangements: ring gauges up top, stacked progress bars, a featured line graph, or a clean single column
- **Randomization** — every time you generate, the layout template, sensor display types (ring vs. bar), color tone, and element ordering vary slightly, so generating twice with the same vibe won't produce an identical result

### How to Use It

1. Click **✨ AI Generate**
2. Choose **vertical** or **horizontal** orientation
3. Click a vibe thumbnail — Space, Cyberpunk, Minimal, Nature, Racing, Anime, Synthwave, Industrial, Ocean, Monochrome, Volcanic, or Aurora
4. Check or uncheck sensors in the list (a sensible default selection is pre-checked: clock, date, CPU usage/temp/fan, GPU usage/temp, motherboard temp, and chassis fans)
5. Click **Generate Theme**

The canvas is populated instantly with a complete layout — clock, date, labeled ring gauges or bars for usage-type sensors, and label+value+bar rows for everything else. Every ring/bar/graph element includes a label so it's always clear which sensor it represents.

From there, treat it like any other theme: drag elements to fine-tune positions, change colors, swap fonts, or just save it as-is with **💾 Save Theme**.

> Generating again will clear the current canvas (you'll be asked to confirm), so if you like a particular result, save it before trying another vibe or generating again.

---

## Sensor Discovery

When the tray app starts and LibreHardwareMonitor is reachable, it automatically scans all available sensors and saves them to:

```
%APPDATA%\DS916Tray\lhm_sensors.json
```

This file contains every sensor LHM exposes, including hardware-specific sensors like additional fan headers, liquid cooling temperatures, per-core clocks/temps/loads and per-drive data. Sensors are labelled with their device name where needed (e.g. `Samsung SSD 990 PRO: Temperature`) so duplicates are easy to tell apart.

**In the Theme Builder**, click **🔍 Sensors** to import this file. All discovered sensors appear in the element palette via the **＋ Sensors** picker.

**Re-run discovery** at any time via tray icon → **🔍 Discover Sensors** — for example after adding new hardware or updating LHM.

> Standard keys (`CPU_USAGE`, `GPU_TEMP`, …) are matched by sensor name and hardware type on every read, and sensors you pick from the list are stored by LHM's stable sensor ID (e.g. `/amdcpu/0/temperature/2`) — so nothing goes stale if the sensor order changes.
>
> **Themes made with the old HWiNFO sensor list:** standard keys keep working unchanged. Elements bound to a *custom* sensor from the old `hwinfo_sensors.json` (`CUSTOM_<number>`) can't be translated automatically — re-import `lhm_sensors.json` in the Theme Builder, re-pick those sensors and re-save the theme. The tray app logs a warning naming each one.

### Framerate Sensors

There are two sources for FPS data, with very different reliability:

**`RTSS_FPS` — "Framerate — Live (RTSS)"** is the recommended sensor. It uses RivaTuner Statistics Server, which hooks directly into the game's DirectX/OpenGL/Vulkan present calls via a continuously-updating ring buffer. The framerate is reliably attributed to the specific game process, with no background-app confusion. This sensor is **included in the default palette** and requires no configuration beyond having RTSS installed and running.

Three additional RTSS sensors are available via the **＋ Sensors** picker but **not in the default palette**:
- `RTSS_FPS_MIN` — lowest framerate recorded during the current RTSS benchmark session
- `RTSS_FPS_MAX` — highest framerate recorded during the current RTSS benchmark session
- `RTSS_FPS_AVG` — average framerate across the current RTSS benchmark session

These three are **session-based** — they only populate while an RTSS benchmark/recording session is actively running. They will show 0 at all other times. Start a session in RTSS via its OSD benchmark controls if you want these values.

**`FRAMERATE`** is a legacy key from the HWiNFO version and is no longer populated — LibreHardwareMonitor has no framerate sensor. Use the RTSS keys for FPS.

To set up RTSS:
1. Install [RivaTuner Statistics Server](https://www.guru3d.com/download/rtss-rivatuner-statistics-server-download/) (also bundled with MSI Afterburner) and make sure it's running
2. Launch your game — RTSS will automatically hook into it
3. `RTSS_FPS` will now update in the default palette automatically — no additional configuration needed
4. Optionally, go to **Settings → RTSS (FPS)** to pin a specific process instead of relying on auto-detection

RTSS is fully optional — if it isn't installed or running, the framerate sensors simply stay unavailable and everything else continues working normally. No administrator privileges are required.

**If `RTSS_FPS` shows 0 or no data for a specific game**, RTSS likely hasn't hooked that game yet. Try raising RTSS's **Application Detection Level** (Options → General) from Low to Medium or High. RTSS's on-screen display can be left **off** and **Stealth Mode** can be left **on** — neither affects RTSS's shared memory availability.

---

## GPU Vendor Support

NVIDIA, AMD and Intel GPUs expose slightly different sensors in LibreHardwareMonitor. This project's standard sensor keys (`GPU_USAGE`, `GPU_TEMP`, `GPU_FAN1`, `GPU_POWER`, `VRAM_USED`, `VRAM_USAGE`) automatically check the right sensor for whichever GPU is installed — you should never need to change which sensor an element is bound to after a GPU swap. A discrete GPU is always preferred over integrated graphics.

A few notes worth knowing:

- **`GPU_POWER`** uses LHM's "GPU Package" sensor — total board/ASIC power on NVIDIA and AMD — and only falls back to AMD's GFX core rail ("GPU Core") if nothing else exists, since that rail alone under-reports.
- **`VRAM_USED`** prefers "D3D Dedicated Memory Used" (what Windows Task Manager shows, and accurate on AMD), falling back to the driver's "GPU Memory Used".
- **`VRAM_USAGE`** (percentage) is computed from `VRAM_USED` ÷ LHM's "GPU Memory Total" for the same card. If LHM doesn't report a total for your GPU, a small built-in table of known card capacities (`GPU_VRAM_GB` in `ds916_tray.py`) is used as a fallback.

If a standard key doesn't update on your hardware, open LHM and look up the exact sensor name, then add it to that key's candidate list in `STANDARD_SENSOR_RULES` near the top of `ds916_tray.py` (or just pick the sensor directly from the ＋ Sensors list). Open an issue if you find a name that should be added.

---

## Sensor Update Speed

LibreHardwareMonitor refreshes its sensors once per second by default (Options → Update Interval). The tray app re-reads LHM at most twice a second, so the panel is never more than one LHM refresh behind. If fast-changing sensors like CPU/GPU usage feel sluggish, lower LHM's update interval — at a small CPU cost to LHM itself.

---

## Status Window

Right-click tray icon → **ℹ Status…** to see a live dashboard:

- **Display** — streaming status, COM port, FPS, active theme name and resolution
- **Sensor Source — LibreHardwareMonitor** — web server address `✅`, LHM version and how many sensors / standard keys were found, or an unavailable warning if LHM isn't running or its web server is off
- **Live Sensor Snapshot** — current values for CPU/GPU usage and temperature, motherboard temp, CPU fan
- **System** — Windows startup status

The window auto-sizes to its content (capped at 90% of your screen height) and has a **↻ Refresh** button to re-read all values.

---

## Logging

The tray app writes a log file to:
```
%APPDATA%\DS916Tray\ds916_tray_log.txt
```

Configurable in **Settings → General → Logging**:
- **Off** — nothing is written at all
- **Normal** (default) — startup, theme loads, connection status, settings changes, and errors — useful detail for troubleshooting without being noisy
- **Verbose** — adds per-frame and per-sensor-read diagnostics, for actively chasing down a specific problem

The log file is automatically capped at roughly 1MB (with one rotated backup kept), so it can never grow unbounded no matter how long the app has been running. Use **📁 Open Log Folder** in Settings for quick access, especially useful when reporting an issue.

---

## How JONSBO-AIO Works (Internals)

Based on our reverse engineering:

1. Reads a **`Setting.txt`** layout file defining element positions, sensor bindings, fonts and colors
2. Reads hardware sensor values via Windows APIs at a configurable polling interval
3. Composites all elements onto a canvas using **SkiaSharp**
4. Encodes the result as JPEG using **libjpeg-turbo**
5. Sends the JPEG to COM3 continuously at ~6fps using the protocol documented above

The `MSDISPLAYSDKWRRAPER.dll` is the PC-side SDK wrapper from ArtInChip. The device firmware runs on their **Luban-Lite** RTOS with CherryUSB.

---

## Theme File Format

Themes are saved as a single **`.ds916theme`** file — a JSON document with all assets embedded as base64 data URLs.

```json
{
  "name": "MyTheme",
  "width": 462,
  "height": 1920,
  "background": "#111114",
  "backgroundImage": "data:image/png;base64,...",
  "sensorMap": { "CPU_USAGE": 46, "CPU_TEMP": 94 },
  "themeColors": ["#00b4ffff", "#ff0000ff"],
  "visibleSensors": ["CLOCK", "CPU_USAGE", "CPU_TEMP"],
  "customFonts": [{"family": "MyFont", "filename": "MyFont.ttf", "data": "data:font/ttf;base64,..."}],
  "elements": [ ... ]
}
```

### Element Types Reference

| Type | Key properties |
|------|----------------|
| `clock` | `clockFormat` (12h/24h), `clockSeconds` (true/false) |
| `date` | `dateFormat` (DD-MM-YYYY etc.) |
| `weekday` | `weekdayFormat` (full/short) |
| `text` | `sensorKey`, `prefix`, `unit`, `manualSize` |
| `static` | `customText`, `manualSize` |
| `bar` | `sensorKey`, `maxValue`, `fillColor`, `bgColor`, `cornerRadius` |
| `ring` | `sensorKey`, `maxValue`, `arcColor`, `trackColor`, `ringWidth` |
| `linegraph` | `sensorKey`, `maxValue`, `lineColor`, `historySeconds` |
| `rect` | `fillColor`, `cornerRadius` |
| `image` | `data` (base64 data URL) |

> **`manualSize`**: `false` (default for text) = box auto-sizes to content, resize handle scales font size. `true` = fixed dimensions, resize handle scales the widget.

### Standard Sensor Keys

| Key | LibreHardwareMonitor source (first match wins) |
|-----|-------------------|
| `CPU_USAGE` | CPU › Load › CPU Total |
| `CPU_TEMP` | CPU › Core (Tctl/Tdie) — AMD, *or* CPU Package — Intel (motherboard "CPU" sensor as fallback) |
| `CPU_FREQ` | CPU › Cores (Average), else Core #1 |
| `CPU_POWER` | CPU › Package |
| `CPU_VOLTAGE` | CPU › Core (SVI2 TFN) — AMD, *or* CPU Core — Intel (motherboard Vcore as fallback) |
| `CPU_FAN` | Motherboard › CPU Fan |
| `GPU_USAGE` / `GPU_TEMP` / `GPU_FREQ` | GPU › GPU Core (load / temperature / clock) |
| `GPU_FAN1` / `GPU_FAN2` | GPU › GPU Fan 1 / GPU Fan 2 (or GPU Fan on single-fan-sensor cards) |
| `GPU_POWER` | GPU › GPU Package |
| `VRAM_USED` | GPU › D3D Dedicated Memory Used, else GPU Memory Used (GB) |
| `VRAM_USAGE` | Computed: VRAM_USED ÷ GPU Memory Total |
| `MB_TEMP` | Motherboard › Motherboard / System |
| `CHASSIS_FAN1/2/3` | Motherboard › System Fan #1/#2/#3 or Chassis Fan #1/#2/#3 |
| `RAM_USAGE` | Memory › Load › Memory |
| `RAM_USED_GB` / `RAM_FREE_GB` | Memory › Memory Used / Memory Available |
| `RAM_TOTAL` | Computed: used + available |
| `DISK_USAGE` / `DISK_USED` / `DISK_FREE` | Windows system drive (read directly from Windows, not LHM) |
| `DISK_TEMP` | First drive › Temperature |
| `DISK_READ` / `DISK_WRITE` | First drive › Read Rate / Write Rate (MB/s) |
| `NET_DOWN` / `NET_UP` | Busiest network adapter › Download Speed / Upload Speed (MB/s) |
| `BATTERY` | Battery › Charge Level |
| `NET_PING` | Not available from LHM |
| `FRAMERATE` | Not available from LHM — use RTSS |
| `RTSS_FPS` | **Framerate — Live (RTSS)** — recommended, continuously-updating ring buffer (default palette) |
| `RTSS_FPS_MIN` | Framerate Min — Session (RTSS) — opt-in, only populated during active benchmark session |
| `RTSS_FPS_MAX` | Framerate Max — Session (RTSS) — opt-in, only populated during active benchmark session |
| `RTSS_FPS_AVG` | Framerate Avg — Session (RTSS) — opt-in, only populated during active benchmark session |
| `CUSTOM_…` | Any sensor picked from `lhm_sensors.json`, stored by its LHM sensor ID |

---

## Uninstalling

Right-click the tray icon → **🗑 Uninstall…**

> **If you're just rebuilding after a code change, you don't need this.** Just re-run `build.bat` — it closes the running instance and overwrites it automatically. Uninstall is for removing the app from a machine entirely.

This will:
1. Remove DS916Tray from Windows startup
2. Remove the Desktop and Start Menu shortcuts
3. Delete `theme_builder.html`, config, and discovered sensor data from `%APPDATA%\DS916Tray\` (your `.ds916theme` theme files are **not** deleted)
4. Close the app

`DS916Tray.exe` itself can't delete itself while running, so it's left behind in `%APPDATA%\DS916Tray\` — delete it manually once the app has fully closed, or just leave it; it's harmless and inert on its own.

---

## Compatible Devices

Confirmed working:
- **Jonsbo DS916** ✅

If you get this working on another device, please open an issue or PR.

---

## Contributing

Pull requests welcome. Areas that would benefit most:

- **More compatible devices** — test on other ArtInChip-based panels
- **Theme gallery** — share your `.ds916theme` files in Discussions
- **Linux/Mac support** — protocol is the same; the LibreHardwareMonitor reader would need replacing (e.g. `lm-sensors`)
- **More sensor keys** — per-core temps, disk activity, GPU power, etc.

---

## License

MIT — do whatever you want with it.

---

## Acknowledgements

Protocol reverse-engineered using Wireshark + USBPcap on Windows 11.
ArtInChip Luban-Lite SDK: https://github.com/artinchip/luban-lite
CherryUSB: https://github.com/cherry-embedded/CherryUSB
LibreHardwareMonitor: https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
