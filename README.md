# Agent Power Guard

**Sleep your PC only when you and your background coding agents are idle.**

Agent Power Guard is a small native utility written in Rust that replaces the fixed OS sleep timer with one that knows what the machine is actually doing. It watches input activity, developer tooling, GPU load, disk, network and meetings, and suspends the system only when all of them are quiet. Long builds, local LLM inference and autonomous agent runs (Claude Code, Cursor, Antigravity, Ollama) are no longer killed by a power plan that can't tell idle from busy.

[![Release](https://img.shields.io/github/v/release/Subham-Maity/agent-power-guard?label=release)](https://github.com/Subham-Maity/agent-power-guard/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/Subham-Maity/agent-power-guard/ci.yml?label=build)](https://github.com/Subham-Maity/agent-power-guard/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey)

<img width="1069" height="1149" alt="Agent Power Guard GUI" src="https://github.com/user-attachments/assets/8d286957-530a-43a6-90ae-dbd3d4641a6c" />

> **Installation:** download the installer or archive for your platform from the [Releases page](https://github.com/Subham-Maity/agent-power-guard/releases). Step-by-step instructions are in [Installation](#installation).

## Why

- **Fixed timers sleep at the wrong moment.** A 15-minute OS timer will happily suspend the machine halfway through `cargo build`, `npm install`, a fine-tuning run or a multi-step agent task.
- **Stuck power requests keep it awake all night.** Audio drivers, paused browser tabs and download managers often hold a `SYSTEM_REQUIRED` lock, leaving the machine (and its GPU) running at full power for no reason.
- **Displays burn power and panels.** Leaving a monitor on wastes electricity and risks OLED burn-in, even when the machine itself still has work to do.

Agent Power Guard evaluates 11 independent conditions every second and only suspends the system when every enabled one passes.

## Features

- **11 guard conditions** with individual toggles, thresholds and live pass/fail status
- **Two-stage power saving:** turn the display off first, suspend the system later
- **Agent leases:** coding agents and scripts can hold sleep off through a CLI or local HTTP API
- **GPU awareness:** NVIDIA (NVML), Linux DRM and macOS IOAccelerator
- **Meeting detection:** microphone and camera use by Zoom, Teams, Meet, Discord, Slack and OBS
- **Pre-sleep grace period:** a 30-second warning with postpone and sleep-now actions
- **Block-reason history:** "What kept my PC awake" timeline, overnight summary, top blockers ranking, and CSV/JSON export
- **Alerts & webhooks:** ntfy, Telegram and custom webhook notifications on imminent sleep, stuck blockers, or completed leases
- **Opt-in update check:** daily check for new GitHub releases with zero telemetry
- **Remote dashboard:** control and monitor the machine from a phone or another computer on your network, with Wake-on-LAN details
- **Energy tracking:** estimated kWh, cost and CO2 avoided, stored locally
- **Live graphs:** agent CPU, whole-PC CPU, disk, network and GPU over a rolling 120-second window
- **Tray mode:** runs hidden in the system tray and trims its working set to under 5 MB of RAM
- **No Electron, no WebView, no runtime:** one native executable built on `egui`

## Installation

Every release on the [Releases page](https://github.com/Subham-Maity/agent-power-guard/releases) includes the files below and a `SHA256SUMS.txt` for verification.

| Platform | File | Notes |
|---|---|---|
| Windows 10 / 11 (x64) | `AgentPowerGuard-windows-x64.msi` | Installer with Start Menu and Desktop shortcuts. Recommended. |
| Windows 10 / 11 (x64) | `agent-power-guard-windows-x64.exe` | Portable single executable. |
| Linux (x86_64) | `agent-power-guard-linux-x86_64.tar.gz` | X11 and Wayland. |
| macOS (Apple Silicon) | `agent-power-guard-macos-aarch64.tar.gz` | M1 and newer. |
| macOS (Intel) | `agent-power-guard-macos-x86_64.tar.gz` | |

### Package managers

Install Agent Power Guard directly via your preferred package manager:

```bash
# Windows (WinGet)
winget install SubhamMaity.AgentPowerGuard

# Windows (Scoop)
scoop install agent-power-guard

# Windows (Chocolatey)
choco install agent-power-guard

# macOS & Linux (Homebrew)
brew install subham-maity/tap/agent-power-guard

# Arch Linux (AUR)
yay -S agent-power-guard-bin
```

### Windows

Run the `.msi` and follow the wizard, then start **Agent Power Guard** from the Start Menu. For a silent install:

```powershell
msiexec /i AgentPowerGuard-windows-x64.msi /qn
```

The Windows build requests administrator rights at launch (UAC prompt). It needs them to change power settings and acquire the shutdown privilege required to suspend the machine.

### Linux

```bash
tar -xzf agent-power-guard-linux-x86_64.tar.gz
cd agent-power-guard
./agent-power-guard
```

Input idle time is read through `xprintidle`, and the tray icon needs GTK 3 and AppIndicator libraries, which most desktop distributions already ship:

```bash
sudo apt install xprintidle   # Debian / Ubuntu
```

### macOS

```bash
tar -xzf agent-power-guard-macos-aarch64.tar.gz
./agent-power-guard
```

On first launch Gatekeeper may block the binary. Open **System Settings > Privacy & Security** and choose **Open Anyway**.

### Verify a download

```bash
sha256sum -c SHA256SUMS.txt          # Linux / macOS (shasum -a 256 -c on macOS)
Get-FileHash .\AgentPowerGuard-windows-x64.msi   # Windows (PowerShell)
```

## Quick start

1. Launch the app and select the **15s Test** preset.
2. Take your hands off the mouse and keyboard.
3. Watch the countdown. If a condition shows **BLOCKING SLEEP**, its reason is listed (recent input, disk activity, an active download, and so on).
4. When every condition passes and the countdown reaches zero, the machine suspends. Move the mouse to wake it; the watchdog restarts its cycle automatically.
5. Switch to a real preset (the default is 3 minutes) once you're happy with the thresholds.

## How it works

```
User steps away
   |
   +--> Stage 1: Display off            (default: 2 min idle)
   |      Monitors go dark. CPU, GPU, network and agents keep running.
   |
   +--> Stage 2: System sleep           (default: 3 min idle)
          |
          +-- Evaluate all guard conditions and active agent leases
          +-- Any condition blocking  -->  stay awake (display stays off)
          +-- 30 s or less remaining  -->  grace banner: Postpone (+15 min) / Sleep now
          +-- All conditions pass     -->  suspend (S3 / Modern Standby)
```

Any blocking condition resets the countdown. Resuming from sleep also resets it, so the machine never suspends again immediately after waking.

## Guard conditions

Each condition can be switched off and tuned independently.

| # | Condition | Blocks sleep when | Default |
|---|---|---|---|
| 1 | Human input idle | Last mouse or keyboard input is more recent than the threshold | 180 s |
| 2 | Watched agent CPU | Processes matching the watch list use more than the threshold | 2.0 % |
| 3 | Active downloads | Download managers (`idman`, `aria2c`, `qbittorrent`, `fdm`) are doing disk I/O | 100 KB/s |
| 4 | Whole-PC CPU | Total CPU utilisation exceeds the threshold | 30 % |
| 5 | Whole-PC disk I/O | Combined read and write throughput exceeds the threshold | 5,000 KB/s |
| 6 | Whole-PC network | Combined send and receive throughput exceeds the threshold | 100 KB/s |
| 7 | Sustained-spike filter | Activity from 4-6 persists longer than this window | 15 s |
| 8 | Monitor timeout | Keeps the OS display timeout in sync with Stage 1 | 2 min |
| 9 | Power requests | A `DISPLAY` power request is active (for example fullscreen video) | on |
| 10 | GPU activity | GPU compute load exceeds the threshold (inference, CUDA, local LLMs) | 5 % |
| 11 | Mic and camera | A meeting app is actively recording | on |

Notes on the less obvious ones:

- **Condition 2** normalises per-process CPU against total logical cores, so a process showing 16% in Task Manager on an 8-core machine counts as 2.0%. The default watch list is `antigravity, language_server, cursor, claude, gemini, code, node, npm, pnpm, bun, tsc, esbuild, vite, next, nest, git, cargo, rustc, python, docker, wsl` and can be edited from **Edit Process Keywords**.
- **Condition 7** stops short background noise (search indexing, antivirus scans, browser refreshes) from resetting the countdown indefinitely.
- **Condition 9** can override wake locks held by virtual audio devices (VB-Audio, Voicemeeter) and paused browser media, which are a common cause of machines that never sleep.
- **Condition 10** loads `nvml.dll` at runtime, so there is no CUDA SDK dependency. AMD and Intel GPUs fall back to OS counters.
- **Condition 11** reads the Windows `CapabilityAccessManager\ConsentStore` registry keys and PulseAudio / PipeWire capture streams on Linux.

## Presets

| Preset | Sleep after | Display off |
|---|---|---|
| 15s Test | 15 s | 1 min |
| 3m (default) | 3 min | 2 min |
| 5m | 5 min | 3 min |
| 10m | 10 min | 5 min |
| 15m | 15 min | 10 min |
| 30m | 30 min | 15 min |
| 45m | 45 min | 20 min |
| 1h | 1 h | 30 min |
| 2h / 3h | 2-3 h | 45-60 min |
| Custom | 10 s - 3 h | 1 min - 3 h |

## Agent integration

Autonomous agents and long-running scripts can hold sleep off dynamically. For a complete guide covering copy-paste setup for **Claude Code**, **Cursor**, and **Antigravity**, see [`docs/agents.md`](docs/agents.md).

### 1. Process Wrapper (`run` - Recommended)

The simplest way to run any agent or long task without worrying about stale leases. It automatically tracks the process ID (PID) and releases power hold immediately when the process completes or crashes:

```bash
# Wrap Claude Code
agent-power-guard run -- claude

# Wrap long build or test command
agent-power-guard run -- cargo test --workspace

# Specify a custom reason and maximum safety timeout (e.g., 120 minutes)
agent-power-guard run --max-mins 120 --reason "LoRA fine-tuning" -- python train.py
```

### 2. Model Context Protocol (MCP) Server

Agent Power Guard provides a native stdio MCP server (`agent-power-guard mcp`) allowing AI agents to request sleep prevention autonomously:

- **Exposed tools:** `keep_awake(minutes, reason)`, `release()`, `status()`, `postpone(minutes)`.
- **Claude Code:** `claude mcp add agent-power-guard -- agent-power-guard mcp`
- **Cursor & Antigravity:** Configure `agent-power-guard mcp` as a stdio command in `.cursor/mcp.json` or agent settings.
- See detailed configurations in [`docs/agents.md`](docs/agents.md).

### 3. Agent Profiles & Noise Filtering

Quickly switch presets for your specific developer environment under **Conditions > Watched Agent Processes**:
- **Built-in profiles:** Claude Code, Cursor, Antigravity, Ollama, Docker, Rust/Cargo, Node/JS, Python ML.
- **Ignore list (`ignored_processes`):** Excludes noisy background daemon processes (e.g. language servers, telemetry) from resetting the sleep timer.

### 4. CLI Commands

```bash
agent-power-guard lease 30 "Compiling LLVM release"   # hold for 30 minutes
agent-power-guard release                             # release immediately
agent-power-guard status                              # current state as JSON
agent-power-guard postpone 15                         # delay sleep by 15 minutes
agent-power-guard sleep                               # suspend now
```

### 5. HTTP API & Security

The app serves a local API on port `49281`. By default, it binds exclusively to `127.0.0.1` and generates a secure random 256-bit token on first run.

#### Authentication & Headers
- All `/api/*` endpoints require the token passed via header:
  `Authorization: Bearer <token>` or `X-APG-Token: <token>`
- The CLI reads the token automatically from `%APPDATA%\AgentPowerGuard\api_token.txt` (`~/.config/agent-power-guard/api_token.txt` on Unix).
- DNS rebinding and cross-origin attacks are prevented with strict `Host` and `Origin` validation.
- LAN access can be enabled from the **Web & Mobile Remote** tab.

| Endpoint | Method | Parameters | Description |
|---|---|---|---|
| `/` | GET | `#t=<token>` | Remote dashboard (token loaded in URL fragment) |
| `/api/status` | GET | | Countdown, conditions, blocking reasons, leases |
| `/api/sleep` | POST | | Suspend immediately |
| `/api/postpone` | POST | `mins=15` | Postpone sleep |
| `/api/toggle_guard` | POST | | Turn the master guard on or off |
| `/api/lease` | POST | `mins=30&reason=Build&pid=1234` | Grant a sleep lease (optionally PID-bound) |
| `/api/release` | POST | | Clear all leases |
| `/api/mode` | POST | `mode=balanced\|work\|night\|away` | Switch operational power mode |
| `/api/schedule/stay_awake` | POST | `mins=60` | Schedule temporary keep-awake |
| `/api/wake_timer/add` | POST | `time=02:00&label=Backup` | Schedule an OS RTC / Task Scheduler wake timer |
| `/api/wake_timer/delete` | POST | `id=<timer_id>` | Delete a scheduled wake timer |
| `/api/history` | GET | | Retrieve recent block-reason history records |
| `/api/history/clear` | POST | | Clear block-reason history |
| `/api/ignore/add` | POST | `process=<name>` | Add a process keyword to the ignore list |
| `/api/ignore/remove` | POST | `process=<name>` | Remove a process keyword from the ignore list |
| `/api/notify/test` | POST | | Send a test notification across configured channels |
| `/api/energy/clear` | POST | | Clear energy history |

Abridged `/api/status` response:

```json
{
  "guard_enabled": true,
  "all_conditions_pass": false,
  "countdown_secs": 180,
  "countdown_max": 180,
  "blocker_banner_text": "Blocked by agent CPU (node 4.8%)",
  "live_idle_secs": 42,
  "live_agent_cpu": 4.8,
  "agent_lease_active": false
}
```

Python example:

```python
import os
import urllib.request
from pathlib import Path

# Load secure token from config
token_path = Path(os.environ.get("APPDATA", "")) / "AgentPowerGuard" / "api_token.txt"
token = token_path.read_text().strip()

base = "http://127.0.0.1:49281"
req = urllib.request.Request(
    f"{base}/api/lease?mins=45&reason=PyTorch+Training",
    data=b"",
    headers={"Authorization": f"Bearer {token}"}
)
urllib.request.urlopen(req)

# ... run long job ...

req = urllib.request.Request(
    f"{base}/api/release",
    data=b"",
    headers={"Authorization": f"Bearer {token}"}
)
urllib.request.urlopen(req)
```

## Remote dashboard

The built-in web server exposes a responsive, mobile-first dashboard designed for phones, tablets, and remote browsers on your local network.

### 1. Instant QR Code Pairing
- Open the **Web & Mobile Remote** tab in the desktop app.
- A high-resolution QR code is rendered natively using a zero-dependency QR generator.
- Point your phone's camera at the screen to immediately open the dashboard with your secure authentication token pre-loaded in the URL fragment (`#t=<token>`). No manual token typing required.
- *Security Note:* The pairing URL grants full remote control over sleep, leases, and power modes; do not share or publish screenshots of the QR code.

### 2. Installable Mobile Dashboard (PWA)
- Equipped with a web app manifest (`/manifest.json`), high-resolution icons (`/icon.png`, `/apple-touch-icon.png`), and mobile viewport tags.
- Tap **Add to Home Screen** on Safari (iOS) or Chrome (Android) to launch Agent Power Guard as a standalone, distraction-free app without browser address bars.
- *Network Limitation:* Service workers require a secure context (HTTPS or `localhost`). Over local LAN (`http://192.168.x.x`), the dashboard functions as an installable standalone web app. For remote HTTPS outside your LAN, pair with Tailscale Serve (`tailscale serve https / http://127.0.0.1:49281`).

### 3. Live Sparklines
- 120-second rolling activity trends for **Agent CPU**, **GPU compute**, **Whole-PC CPU**, **Disk I/O**, and **Network throughput**.
- Rendered using lightweight, inline SVG `<polyline>` graphics with zero external charting libraries or third-party CDNs to ensure zero memory overhead and instant rendering on low-power devices.

### 4. Active Processes & 1-Tap Ignore
- Inspect the top 8 running processes ranked by CPU load directly in the dashboard and desktop GUI.
- Processes matching agent keywords are clearly badged.
- Tap **+ Ignore** next to any false-positive process (e.g. system indexing, background backup tools) to immediately append it to the ignore list and unblock sleep with zero restarts needed.

### 5. Remote Wake-on-LAN (WoL)
A sleeping PC cannot serve web traffic. To wake your workstation remotely:
- Send a magic packet from any client or another machine on the LAN using the built-in CLI:
  ```bash
  agent-power-guard wol 00:11:22:33:44:55 192.168.1.255
  ```
- For waking your workstation from outside your home network (via Tailscale, VPN, or a Raspberry Pi subnet router), follow the comprehensive guide in [`docs/remote-wake.md`](docs/remote-wake.md).

## Energy tracking

The **Energy & Cost Savings** tab estimates what suspending and display-off saved you: sleep sessions, monitor-off events, active versus sleep hours, kWh, money and CO2. Filter by today, 7 days, this month, this year or all time.

Estimates use configurable wattage and tariff values (defaults: 120 W active system, 45 W display, 2 W sleep). Recording can be turned off at any time, and everything is stored locally as plain JSON:

| OS | Location |
|---|---|
| Windows | `%APPDATA%\AgentPowerGuard\energy_history.json` |
| Linux | `~/.config/agent-power-guard/energy_history.json` |
| macOS | `~/Library/Application Support/agent-power-guard/energy_history.json` |

The file can be inspected, backed up or deleted without affecting the app. Nothing is sent anywhere.

### Accurate Regional Carbon Grid & Currency

Electricity generation carbon intensity varies dramatically across power grids:
- **Bundled grid-intensity factors:** Select from 11 verified regional profiles including **US Average (368 g/kWh)**, **US California CAMX (205 g/kWh)**, **US Texas ERCO (380 g/kWh)**, **European Union (230 g/kWh)**, **United Kingdom (148 g/kWh)**, **Germany (380 g/kWh)**, **France (52 g/kWh)**, **India (713 g/kWh)**, **Australia (510 g/kWh)**, **Global Average (436 g/kWh)**, or enter a **Custom** g/kWh intensity.
- **Multi-currency support:** Choose from `$`, `€`, `£`, `₹`, `¥`, `C$`, `A$` for accurate localized financial reporting.
- **Dynamic API reporting:** Both the desktop app and the remote web dashboard consume dynamic `co2_saved` values directly calculated from the configured regional factor, completely eliminating inaccurate hardcoded estimates.

## Power modes, scheduling & laptop battery profiles

### 1. Operational Power Modes
Quickly change power strategies without manually reconfiguring sliders:
- **Balanced (default):** 3-minute idle, 2-minute display timeout, 2.0% agent CPU.
- **Work:** 10-minute idle, 5-minute display timeout, 1.0% agent CPU — perfect for long reading or active pair programming sessions.
- **Night:** 1-minute idle, 1-minute display timeout, 1.0% agent CPU — aggressive power reduction.
- **Away:** 30-second idle, 1-minute display timeout — immediate standby when leaving your workstation.
- **Custom:** Preserve user-customized condition thresholds.
- **System Tray Quick Actions:** Right-click the tray icon to switch modes, activate 1-hour Keep Awake, or postpone sleep (+15m) instantly.

### 2. Quiet Hours & Night Schedules
- Configure automated quiet periods (e.g. `23:00` to `07:00`).
- The watchdog automatically downshifts to aggressive thresholds (`Night` or `Away`) during quiet hours.
- **Sleep After Lease Ends:** Automatically put the machine into deep sleep the moment an autonomous agent task or wrapped build completes, even in the middle of the night.

### 3. Laptop Battery Awareness
- Detects AC mains vs DC battery discharge across Windows (`GetSystemPowerStatus`), Linux (`/sys/class/power_supply`), and macOS (`pmset -g batt`).
- **Automatic Battery Profile:** Applies shorter idle timeouts (default: 60s) and tighter CPU limits (default: 1.0%) when unplugged. Desktops without a battery are unaffected.
- **Emergency Low-Battery Sleep:** Automatically forces sleep or hibernate when battery drops below a critical safety cutoff (default: 15%) to prevent unexpected shutdown and lost work.

### 4. Scheduled Wake Timers
- Schedule recurring or one-off wake events at specific times (e.g. `02:00 AM` for scheduled backups or CI jobs).
- Uses native OS facilities: Windows Task Scheduler (`-WakeToRun`), Linux `rtcwake`, or macOS `pmset schedule wake`.
- Once awake, Agent Power Guard resumes active monitoring and returns the machine to deep sleep as soon as conditions become idle.

## Block-reason history ("What kept my PC awake")

When your machine doesn't sleep as expected, the **Block History** tab provides an exact timeline of what prevented it:

- **Top Blockers Leaderboard:** Ranks blocking processes and conditions by total cumulative hold duration (e.g. `Agent CPU: 2h 15m`, `Active Download: 45m`).
- **"Last Night" Overnight Summary:** Focuses on the overnight 12-hour window, summarizing total blocked time, the dominant blocking process, and the exact timestamp sleep was reached.
- **Event Timeline:** Inspect individual block events with start time, duration, primary condition, and other concurrent blockers.
- **Data Export:** Export logs with one click to CSV or JSON, or clear history anytime.
- **Bounded Storage & Rotation:** Stored locally in JSON Lines format (`block_history.jsonl`) with automatic size-based rotation (5 MB cap) and configurable retention days (default: 14 days).

| OS | Location |
|---|---|
| Windows | `%APPDATA%\AgentPowerGuard\block_history.jsonl` |
| Linux | `~/.config/agent-power-guard/block_history.jsonl` |
| macOS | `~/Library/Application Support/agent-power-guard/block_history.jsonl` |

## Alerts & notifications

Stay informed about power events on your phone or desktop via the **Alerts & Updates** tab.

### Supported Channels
- **ntfy:** Real-time push notifications to [ntfy.sh](https://ntfy.sh) or self-hosted ntfy servers (optional authentication token supported).
- **Telegram:** Push alerts to your private chat or team channel via Telegram Bot API (`bot_token` and `chat_id`).
- **Generic Webhook:** HTTP POST JSON payload to Discord, Slack, Home Assistant, or custom automation servers.

### Configurable Events
- **Sleep Imminent:** 30-second pre-sleep warning when all conditions pass.
- **Machine Blocked Long:** Alerts if a single condition or agent holds the PC awake for longer than $N$ hours (default: 2 hours).
- **Lease Ended:** Notifies when an agent task or build lease concludes.
- **PC Resumed:** Alerts when the system wakes up from sleep.
- **Guard Paused Long:** Reminds you if master guard protection was left disabled for longer than $N$ hours.

*Zero Telemetry Guarantee:* Off by default. Outbound requests are made only to the destinations you configure. Network calls run asynchronously in a dedicated worker thread, ensuring the 1 Hz watchdog loop is never delayed.

## Opt-in software updates

- **Private & Non-intrusive:** When enabled in **Alerts & Updates**, Agent Power Guard checks GitHub Releases once per 24 hours.
- **No telemetry:** No machine identifiers, tracking cookies, or analytics are sent.
- **Direct notifications:** If a newer release exists, a non-intrusive banner appears in the top navigation bar with a direct link to the release notes.

## Architecture

```
+------------------------------------------------------------------------+
|                           agent-power-guard                            |
|                                                                        |
|  Platform layer              Watchdog thread (1 Hz)       Frontends    |
|  ---------------             ----------------------       ----------   |
|  input idle         ------>  sample metrics               egui window  |
|  sysinfo: CPU,               evaluate guard conditions    system tray  |
|    disk, net, procs          apply leases / postpones     HTTP API     |
|  GPU, mic / camera           run the countdown            CLI          |
|  power requests              trigger display-off / sleep  web dashboard|
|  suspend, display  <------                                             |
|                                        ^                               |
|                                        |                               |
|                          Arc<Mutex<SharedState>>                       |
+------------------------------------------------------------------------+
```

Key decisions:

- **The watchdog is independent of the UI.** `eframe` stops running UI code when the window is minimized or the display is off, which is exactly when the sleep timer matters. All timing, sampling and decisions run on a dedicated thread; the GUI, tray, HTTP server and CLI only read and mutate shared state.
- **Small memory footprint by design.** The renderer is `glow` (OpenGL) rather than `wgpu`, repaints are throttled to 1 Hz, and the working set is trimmed when the window is hidden in the tray.
- **Allocation-free sampling.** A single `System`, `Networks` and `Disks` instance is created once and refreshed selectively each second instead of calling `refresh_all()`.
- **Native OS APIs, no helper processes.** Windows uses `windows-sys`; Linux and macOS use the platform tools listed below. Release builds use fat LTO, a single codegen unit, `panic = "abort"` and symbol stripping.
- **No frontend build step.** The remote dashboard is a single static HTML file served by the embedded HTTP server.

| Component | Crate |
|---|---|
| GUI | `eframe` / `egui` (glow backend), `egui_plot` |
| System sampling | `sysinfo` |
| Tray | `tray-icon` |
| Serialization | `serde`, `serde_json` |
| Windows APIs | `windows-sys` |
| Windows packaging | `embed-manifest`, `winres`, WiX Toolset v3 |

## Platform support

| Capability | Windows | Linux | macOS |
|---|---|---|---|
| System sleep | `SetSuspendState` | `systemctl suspend` | `pmset sleepnow` |
| Input idle | `GetLastInputInfo` | `xprintidle` / X11 | `ioreg` (`HIDIdleTime`) |
| Display timeout | `powercfg` | `xset dpms` / Wayland DBus | `pmset displaysleep` |
| GPU monitoring | NVML (dynamic) | DRM sysfs | IOAccelerator |
| Meeting detection | `ConsentStore` registry | PulseAudio / PipeWire | CoreAudio |
| Autostart | Task Scheduler / Startup | `~/.config/autostart` | `~/Library/LaunchAgents` |

## Building from source

Requires a recent stable Rust toolchain (`rust-version` is declared in `Cargo.toml`).

```bash
git clone https://github.com/Subham-Maity/agent-power-guard.git
cd agent-power-guard
cargo build --release
```

On Linux, install the build dependencies first:

```bash
sudo apt install -y pkg-config libx11-dev libxext-dev libxrandr-dev libxinerama-dev \
  libxcursor-dev libxi-dev libgl1-mesa-dev libglu1-mesa-dev libxss-dev libxkbcommon-dev \
  libwayland-dev xprintidle libgtk-3-dev libayatana-appindicator3-dev
```

To build the Windows MSI, install [WiX Toolset v3.14](https://wixtoolset.org/) and run:

```powershell
.\build-msi.ps1
```

The installer is written to `target\installer`. `generate-icon.ps1` regenerates `assets\app.ico`.

## Troubleshooting

**The machine never sleeps.** Open the app and look for the condition marked **BLOCKING SLEEP**; it shows the reason. The usual causes are a stuck power request (enable the override in condition 9), a background process matching the watch list (the defaults include broad names such as `code`, `node` and `git`), or a threshold that is too tight for your machine's idle noise.

**Antivirus flags or removes the executable.** The app suspends the machine, acquires `SeShutdownPrivilege`, writes a startup entry and polls hardware and registry state. Those are legitimate for a power tool but look similar to suspicious behaviour to heuristic scanners, and the binary is not signed with a paid code-signing certificate. The project is open source and makes no network calls to external services. To resolve it, add an exclusion for the install folder:

- **Microsoft Defender:**
  1. Open **Windows Security > Virus & threat protection > Manage settings**.
  2. Under **Exclusions**, choose **Add or remove exclusions** and add the folder `C:\Program Files\AgentPowerGuard`.
- **Bitdefender Total Security:**
  1. Open Bitdefender -> **Protection** tab.
  2. Under **Antivirus**, open **Settings** -> **Manage exceptions** -> **Add an Exception** -> add folder `C:\Program Files\AgentPowerGuard` (check *On-access*, *On-demand*, *Embedded scripts*).
  3. Under **Advanced Threat Defense**, open **Settings** -> **Manage exceptions** -> add folder `C:\Program Files\AgentPowerGuard` (check *Advanced Threat Defense*).
  4. If files were quarantined, go to **Protection** -> **Antivirus** -> **Quarantine** / **Notifications** and choose **Restore**.

**Wake-on-LAN doesn't work.** Check that it is enabled in both the BIOS/UEFI and the adapter properties, and that the PC is on Ethernet rather than Wi-Fi.

## Contributing

Bug reports, ideas and pull requests are welcome.

1. Fork the repository.
2. Create a branch: `git checkout -b feat/my-feature`.
3. Commit your changes: `git commit -m "feat: add my feature"`.
4. Push the branch and open a pull request.

CI builds every pull request on Windows, Linux and macOS.

## License

Released under the [MIT License](LICENSE).