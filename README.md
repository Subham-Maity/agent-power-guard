# Agent Power Guard

<img width="1069" height="1149" alt="Agent Power Guard GUI" src="https://github.com/user-attachments/assets/8d286957-530a-43a6-90ae-dbd3d4641a6c" />

A native, high-performance power guardian for developers and power users that puts your PC to sleep only when **you AND your background coding agents / AI workflows are truly idle**.

Written in Rust (`egui` + `glow`). Single lightweight executable, trims memory to under 5 MB of physical RAM in the system tray, zero Electron, zero WebView, zero heavy runtimes.

[![Release](https://img.shields.io/github/v/release/Subham-Maity/agent-power-guard?label=release)](https://github.com/Subham-Maity/agent-power-guard/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/Subham-Maity/agent-power-guard/ci.yml?label=build)](https://github.com/Subham-Maity/agent-power-guard/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey)

> **Quick Download:** Grab the latest installer or archive for your operating system from the **[Releases Page](https://github.com/Subham-Maity/agent-power-guard/releases)**.

---

## Table of Contents

- [Why Agent Power Guard?](#why-agent-power-guard)
- [How It Works: Two-Stage Sleep](#how-it-works-two-stage-sleep)
- [Core Features](#core-features)
- [Presets & Idle Timer Guide](#presets--idle-timer-guide)
- [Guard Conditions & Thresholds (11-Point Verification)](#guard-conditions--thresholds-11-point-verification)
- [Pre-Sleep Grace Period Notification](#pre-sleep-grace-period-notification)
- [Agent Heartbeat API & CLI](#agent-heartbeat-api--cli)
- [Web & Mobile Remote Dashboard (Over Wi-Fi)](#web--mobile-remote-dashboard-over-wi-fi)
- [Energy & Cost Savings Dashboard](#energy--cost-savings-dashboard)
- [System Tray & Memory Trimming](#system-tray--memory-trimming)
- [Installation & Multi-Platform Support](#installation--multi-platform-support)
- [Platform Implementation Details](#platform-implementation-details)
- [Contributing & License](#contributing--license)

---

## Why Agent Power Guard?

Default operating system power plans are blind to developer workflows:
- **Premature Standby During Builds & LLM Inference:** Fixed OS timers can put your computer to sleep right in the middle of a long `cargo build`, `npm install`, PyTorch fine-tuning, or a multi-step autonomous coding agent task (Cursor, Claude Code, Antigravity, Ollama).
- **Stuck Sleep Locks:** Audio drivers (VB-Audio, FxSound), paused browser video tabs, or tray download tools often request `SYSTEM_REQUIRED` power locks, leaving your workstation and hungry GPUs running at full power all night.
- **Power Waste & Burn-In:** Leaving monitors on burns electricity and risks OLED burn-in. Conversely, sleeping prematurely destroys unfinished agent runs.

Agent Power Guard watches what is **actually occurring** across 11 distinct system metrics (human input, watched dev processes, active downloads, global CPU/disk/network, sustained spikes, display locks, GPU compute, and microphone/camera activity). It sleeps your machine only when everything is genuinely quiet.

---

## How It Works: Two-Stage Sleep

Instead of an abrupt sleep, Agent Power Guard manages power in two coordinated stages:

```
User steps away
   │
   ├─► Stage 1: Display Off (default: 2 min idle)
   │   └─► Turns off monitors, saving 40-120 W and preventing OLED burn-in.
   │       CPU, GPU, network, and background agents continue working uninterrupted.
   │
   └─► Stage 2: Deep System Sleep (default: 3 min idle)
       ├─► Evaluates all 11 Guard Conditions + Agent Leases
       ├─► If any condition blocks ──► System stays awake (display stays dark)
       ├─► At ≤ 30s remaining ───────► Grace Notification banner with [Postpone (+15m)]
       └─► When all conditions pass ─► Enters S3 / Modern Standby (< 1 W draw)
```

---

## Core Features

- 🎯 **11 Real-Time Guard Conditions:** Per-condition toggles, thresholds, and live pass/fail indicators.
- 📉 **5 Rolling Real-Time Graphs (120s History):**
  1. Agent CPU %
  2. Whole-PC CPU %
  3. Total Disk I/O (KB/s)
  4. Total Network I/O (KB/s)
  5. GPU Compute % (1 Hz sampling)
- 🎮 **GPU Activity Monitoring:** Native NVIDIA NVML dynamic linking on Windows, `/sys/class/drm` on Linux, and IOAccelerator on macOS. Blocks sleep when Ollama, CUDA, Stable Diffusion, or PyTorch is running.
- 🎙️ **Microphone, Camera & Meeting Detection:** Prevents sleep during active calls on Zoom, Microsoft Teams, Google Meet, Discord, Slack, and OBS.
- ⏳ **Pre-Sleep Grace Period Notification:** Visual countdown alert at $\le 30\text{s}$ before sleep with **[ ⏳ Postpone (+15m) ]** and **[ ⚡ Sleep Immediately ]** quick actions.
- 🔌 **Agent Heartbeat REST API & CLI:** Embedded HTTP API on port `49281` and CLI commands (`lease`, `release`, `status`, `sleep`, `postpone`) allowing autonomous AI agents to lease sleep prevention.
- 🌐 **Web & Mobile Remote Dashboard (over Wi-Fi):** Control your PC from your smartphone or tablet browser over local network. Includes Wake-on-LAN (WoL) MAC broadcast instructions.
- 💰 **Energy & Cost Savings Dashboard:** Historical analytics tracking sleep sessions, monitor-off time, estimated kWh saved, financial cost saved, and $\text{CO}_2$ emissions avoided. Includes date filters, recording toggle, and direct JSON data controls.
- 📥 **System Tray with Memory Trimming:** Minimize or close to system tray. Automatically trims memory working set (`K32EmptyWorkingSet`) to maintain $< 5\text{ MB}$ RAM footprint.

---

## Presets & Idle Timer Guide

Agent Power Guard provides quick-access presets tailored for different tasks:

| Preset | Sleep After | Display Off | Recommended Use Case |
|---|---|---|---|
| **15s Test** | 15 seconds | 1 minute | Instant verification of sleep readiness and threshold tuning |
| **3m (Default)** | 3 minutes | 2 minutes | General coding and everyday web browsing |
| **5m** | 5 minutes | 3 minutes | Reading documentation, drafting emails, light code review |
| **10m** | 10 minutes | 5 minutes | Docker container launches, medium test suites, LLM queries |
| **15m** | 15 minutes | 10 minutes | Short meetings, coffee breaks, medium compilation jobs |
| **30m** | 30 minutes | 15 minutes | Large database migrations, heavy package installs, small training runs |
| **45m** | 45 minutes | 20 minutes | Multi-agent autonomous workflows and git monorepo builds |
| **1h** | 1 hour | 30 minutes | Extended compilation, video rendering, full test matrices |
| **2h / 3h** | 2 - 3 hours | 45 - 60 min | Overnight autonomous agent executions and massive dataset processing |
| **Custom** | 10s - 3 hours | 1m - 3 hours | Fully custom sliders for tailored power policies |

### How to Test Immediately

1. Click **15s Test** preset in the UI.
2. If any condition shows <span style="color:#e05252">**BLOCKING SLEEP**</span>, inspect its reason (e.g. human idle, disk activity, or an active download).
3. Take your hands off the mouse and keyboard.
4. Watch the countdown timer reach `0s`. When all conditions pass, the machine suspends smoothly.
5. Move your mouse or press a key to wake the machine up. Agent Power Guard detects the sleep resume and restarts its watchdog cycle.

---

## Guard Conditions & Thresholds (11-Point Verification)

Every condition can be toggled on or off and tuned with its own dedicated threshold slider:

| # | Condition | What It Measures | Default Threshold |
|---|---|---|---|
| **1** | **Human Input Idle** | Time since last physical mouse move, click, or keyboard keystroke | 180 s (3 min) |
| **2** | **Watched Agent CPU** | Normalized CPU % across all cores for monitored developer and AI tools | 2.0 % |
| **3** | **Active Download / IDM** | Active disk I/O of download managers (`idman`, `aria2c`, `qbittorrent`, `fdm`) | 100 KB/s |
| **4** | **Whole-PC CPU** | Global aggregate CPU utilization across all system threads | 30.0 % |
| **5** | **Whole-PC Disk I/O** | Combined read and write throughput across all physical storage drives | 5,000 KB/s |
| **6** | **Whole-PC Network** | Combined send/receive throughput across all network interfaces | 100 KB/s |
| **7** | **Sustained Spike Filter** | Duration a CPU/disk/network blip must persist before resetting idle counter | 15 s |
| **8** | **Monitor Timeout** | Synchronizes system display-off timeout with Stage 1 power policy | 2 min |
| **9** | **Power Requests** | Honors active `DISPLAY:` locks (e.g. fullscreen video) with audio lock override | Enabled |
| **10** | **GPU Activity Monitoring** | GPU compute load for active AI inference, CUDA shaders, and local LLMs | 5.0 % |
| **11** | **Mic & Camera Meetings** | Detects active recording by Zoom, Teams, Meet, Discord, Slack, OBS | Enabled |

### In-Depth Condition Notes

- **Condition 2 (Watched Agent Processes):** Normalizes process CPU across total logical threads. An 8-core CPU process using 16% in Task Manager evaluates to 2.0% global load. Default watch list: `antigravity, language_server, cursor, claude, gemini, code, node, npm, pnpm, bun, tsc, esbuild, vite, next, nest, git, cargo, rustc, python, docker, wsl`. Edit anytime via **Edit Process Keywords**.
- **Condition 7 (Sustained Spike Filter):** Prevents background OS noise (Windows Search indexing, anti-malware blips, Edge tab refreshes) from permanently aborting sleep countdowns.
- **Condition 9 (Override Audio / Browser Locks):** Automatically tells Windows powercfg to ignore wake locks from virtual audio loopbacks (VB-Audio, voicemeeter) and paused browser media tabs.
- **Condition 10 (GPU Compute):** Dynamically loads `nvml.dll` at runtime without CUDA SDK dependencies; gracefully handles AMD and Intel integrated graphics via OS hardware counters.
- **Condition 11 (Meeting Detection):** Queries Windows `CapabilityAccessManager\ConsentStore` registry keys (`LastUsedTimeStop == 0`) and Linux PulseAudio/PipeWire capture streams for instant call detection.

---

## Pre-Sleep Grace Period Notification

When the countdown reaches $\le 30\text{s}$ before sleep, Agent Power Guard triggers an interactive Grace Period banner in the UI and Web Remote:

```
⚠️ SYSTEM SLEEP IMMINENT — Suspending in 24s. Click 'Postpone' to delay sleep.
[ ⏳ Postpone (+15m) ]    [ ⚡ Sleep Immediately ]
```

- **[ ⏳ Postpone (+15m) ]**: Instantly delays sleep by 15 minutes, allowing you to wrap up your work or extend an active coding session.
- **[ ⚡ Sleep Immediately ]**: Bypasses remaining countdown and suspends the PC right away.

---

## Agent Heartbeat API & CLI

Agent Power Guard embeds a lightweight, zero-dependency HTTP server on port `49281` (`http://localhost:49281`). Any terminal script, Python job, or autonomous agent (Cursor Composer, Claude Code, Antigravity, AutoGen) can acquire sleep-prevention leases.

### CLI Subcommands

Agent Power Guard can be invoked directly from your command line:

```bash
# Acquire a 30-minute lease for a background task
agent-power-guard lease 30 "Compiling LLVM release"

# Release the active lease immediately
agent-power-guard release

# Query current watchdog status in JSON format
agent-power-guard status

# Postpone sleep by 15 minutes
agent-power-guard postpone 15

# Trigger immediate system sleep
agent-power-guard sleep
```

### REST API Endpoints

| Endpoint | Method | Parameters | Description |
|---|---|---|---|
| `/` | `GET` | — | Serves the responsive Web & Mobile Remote Dashboard |
| `/api/status` | `GET` | — | Returns JSON status (countdown, conditions, blocking reasons, leases) |
| `/api/sleep` | `POST` | — | Triggers immediate system sleep |
| `/api/postpone` | `POST` | `?mins=15` | Postpones sleep by specified minutes |
| `/api/toggle_guard` | `POST` | — | Toggles Master Guard ON/OFF |
| `/api/lease` | `POST` | `?mins=30&reason=Build` | Grants an agent heartbeat sleep lease |
| `/api/release` | `POST` | — | Clears all active agent leases |
| `/api/energy/clear` | `POST` | — | Clears all historical energy records |

#### Python Agent Heartbeat Example

```python
import urllib.request

# Prevent sleep for 45 minutes while training
urllib.request.urlopen("http://localhost:49281/api/lease?mins=45&reason=PyTorch+Training", data=b"")

# ... run training code ...

# Release lease when finished
urllib.request.urlopen("http://localhost:49281/api/release", data=b"")
```

---

## Web & Mobile Remote Dashboard (Over Wi-Fi)

Control your computer from your smartphone, tablet, or another laptop on the same local Wi-Fi network without installing any apps!

1. Open Agent Power Guard and switch to the **🌐 Web & Mobile Remote** tab.
2. Note your Local Network URL (e.g. `http://192.168.1.15:49281`). Click **Copy URL** or open directly in your mobile browser.
3. You can remotely:
   - Monitor live sleep countdown and blocking conditions.
   - Click **⚡ Sleep Now** to put your PC into deep sleep from across the room.
   - Click **⏳ Postpone 15m** to extend awake time.
   - Toggle Master Guard ON or OFF.
   - Grant or revoke agent leases.

### Remote Wake-on-LAN (WoL)

The Web Remote tab displays your primary Network Interface Name and MAC Address (e.g. `00:1A:2B:3C:4D:5E`). To turn your PC back on remotely from your phone:
1. Ensure **Wake-on-LAN (Magic Packet)** is enabled in your BIOS/UEFI and network card properties.
2. Install any free Wake-on-LAN app on your iOS or Android device.
3. Enter your PC's MAC address and broadcast IP (`255.255.255.255` or subnet broadcast).
4. Send the packet to power on your PC over Wi-Fi!

---

## Energy & Cost Savings Dashboard

Switch to the **💰 Energy & Cost Savings** tab to view your environmental and financial savings:

- **Metrics Tracked:**
  - Total Sleep Sessions & Monitor-Off Events.
  - Active Hours vs Sleep Hours.
  - Cumulative Energy Saved (kWh).
  - Financial Savings ($ / ₹ / € based on configurable electricity rates).
  - Estimated $\text{CO}_2$ Emissions Avoided ($\text{kg CO}_2$).
- **Customizable Wattage Parameters:**
  - Active System Power (default: 120 W).
  - Monitor Display Power (default: 45 W).
  - Deep Sleep Power (default: 2 W).
  - Electricity Rate ($/kWh).
- **Date Filtering:** Filter statistics by **Today**, **Last 7 Days**, **This Month**, **This Year**, or **All-Time**.
- **Data Privacy & Storage:**
  - Toggle recording ON/OFF at any time.
  - Click **Clear Filtered** or **Clear All Data** in the UI.
  - All metrics are stored locally in standard JSON format:
    - **Windows:** `%APPDATA%\AgentPowerGuard\energy_history.json`
    - **Linux:** `~/.config/agent-power-guard/energy_history.json`
    - **macOS:** `~/Library/Application Support/agent-power-guard/energy_history.json`
  - You can inspect, back up, or delete `energy_history.json` at any time without affecting the application.

---

## System Tray & Memory Trimming

- **Close to Tray:** Clicking the window close button **[X]** hides Agent Power Guard into your OS system tray while keeping the watchdog active in the background.
- **Ultra-Low Memory:** Upon minimizing or hiding, Agent Power Guard trims its working set (`K32EmptyWorkingSet`), dropping RAM consumption to **under 5 MB**.
- **Tray Menu Actions:** Right-click the tray icon to:
  - Open / Show GUI
  - Toggle Guard ON / OFF
  - ⚡ Test Sleep Now
  - Exit Agent Power Guard completely
- **Quick Restore:** Single left-click or double-click the tray icon anytime to unhide and focus the window.

---

## Installation & Multi-Platform Support

Download the appropriate binary or installer from the **[Releases Page](https://github.com/Subham-Maity/agent-power-guard/releases)**.

| Platform | Recommended Package | Description |
|---|---|---|
| **Windows 10 / 11 (x64)** | `AgentPowerGuard-1.0.2-x64.msi` | Native WiX MSI Installer with Start Menu shortcut & uninstaller |
| **Windows (Portable)** | `agent-power-guard-windows-x64.exe` | Zero-dependency standalone executable |
| **Linux (x86_64)** | `agent-power-guard-linux-x86_64.tar.gz` | Supports both X11 & Wayland compositors |
| **macOS (Apple Silicon)** | `agent-power-guard-macos-aarch64.tar.gz` | Universal binary for Apple M1, M2, M3, M4 |
| **macOS (Intel x86_64)** | `agent-power-guard-macos-x86_64.tar.gz` | For Intel-based Macs |

### Windows Setup

Run `AgentPowerGuard-1.0.2-x64.msi` and follow the installer wizard. After installation, launch **Agent Power Guard** from your Start Menu.

### Linux Setup

Install GUI and idle query prerequisites:

```bash
# Debian / Ubuntu / Mint
sudo apt update
sudo apt install -y pkg-config libx11-dev libxext-dev libxrandr-dev libxinerama-dev libxcursor-dev libxi-dev libgl1-mesa-dev libglu1-mesa-dev libxss-dev libxkbcommon-dev libwayland-dev xprintidle libgtk-3-dev libayatana-appindicator3-dev

# Extract and launch
tar -xzvf agent-power-guard-linux-x86_64.tar.gz
cd agent-power-guard
./agent-power-guard
```

### macOS Setup

```bash
tar -xzvf agent-power-guard-macos-aarch64.tar.gz
./agent-power-guard
```

*Note: If Gatekeeper displays an untrusted developer prompt on first launch, open **System Settings > Privacy & Security** and click **Open Anyway**.*

---

## Platform Implementation Details

| Capability | Windows | Linux | macOS |
|---|---|---|---|
| **System Sleep** | `SetSuspendState(false, false, false)` | `systemctl suspend` (or `loginctl` / DBus) | `pmset sleepnow` |
| **Input Idle** | `GetLastInputInfo` | `xprintidle` / X11 extension | `ioreg -c IOHIDSystem` (`HIDIdleTime`) |
| **Display Timeout** | `powercfg /change monitor-timeout-ac` | `xset dpms` (X11) or Wayland DBus | `pmset -a displaysleep` |
| **GPU Monitoring** | Dynamically loads `nvml.dll` | DRM sysfs (`/sys/class/drm`) | `IOAccelerator` counters |
| **Meeting Detection** | `CapabilityAccessManager\ConsentStore` | PulseAudio / PipeWire capture nodes | CoreAudio active IO procs |
| **Autostart** | Windows Task Scheduler / Startup folder | `~/.config/autostart/*.desktop` | `~/Library/LaunchAgents/*.plist` |

---

## Contributing & License

Contributions, bug reports, and suggestions are warmly welcome!
1. Fork the repository.
2. Create your feature branch (`git checkout -b feat/my-feature`).
3. Commit your changes (`git commit -m "feat: add feature"`).
4. Push to branch (`git push origin feat/my-feature`).
5. Open a Pull Request.

This project is licensed under the **[MIT License](LICENSE)**.
