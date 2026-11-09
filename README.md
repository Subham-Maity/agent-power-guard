# Agent Power Guard

<img width="1069" height="1149" alt="image" src="https://github.com/user-attachments/assets/8d286957-530a-43a6-90ae-dbd3d4641a6c" />

A small native utility that puts your PC to sleep only when **you and your background AI coding agents are both idle**.

Written in Rust (`egui` + `glow`). Single executable, under 5 MB of RAM, no Electron, no WebView.

[![Release](https://img.shields.io/github/v/release/Subham-Maity/agent-power-guard?label=release)](https://github.com/Subham-Maity/agent-power-guard/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/Subham-Maity/agent-power-guard/ci.yml?label=build)](https://github.com/Subham-Maity/agent-power-guard/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey)

> **Installation:** download the latest installer or binary for your OS from the
> **[Releases page](https://github.com/Subham-Maity/agent-power-guard/releases)**.
> Setup instructions are [below](#installation).

---

## Why

Default OS sleep timers don't work well for developers running local agents, language servers, or long builds:

- A fixed standby timer can suspend the machine in the middle of an LLM run or a multi-minute build.
- Audio drivers and browser media threads (Discord, a paused YouTube tab, virtual audio cables, IDM) often hold wake locks, so the machine never sleeps and a hot GPU runs all night.
- Batch and PowerShell workarounds are blind. When sleep fails, there is no way to see why.

Agent Power Guard watches what is actually happening on the machine (your input, agent processes, CPU, disk and network) and sleeps the PC only when all of it is quiet. It also shows live graphs and per-condition status, so you can see exactly what is blocking sleep.

## How it works

Sleep happens in two stages instead of one:

```
User steps away
   |
   |  Stage 1 (default: 2 min idle)
   v
Display off ........ saves 50-100 W, avoids OLED burn-in
                     CPU, GPU and network stay active, agents keep working
   |
   |  Stage 2 (default: 3 min idle)
   v
Check all guard conditions
   |-- agent still busy ......... stay awake (display stays off)
   '-- user and agent idle ...... suspend (S3 / Modern Standby, < 1 W)
```

Turning the display off first means long agent runs and builds can finish in the dark, and the PC sleeps as soon as they are done.

## Features

- Sleeps only when human input, watched processes, and whole-system load are all below your thresholds
- Per-process watching for common dev tools (Cursor, Claude, Gemini, Antigravity, cargo, node, python, docker, and more). The keyword list is editable
- Download-aware: an idle `IDMan.exe` in the tray does not block sleep, an active transfer does
- Sustained spike filter, so one-second system blips don't reset the timer
- Override for stuck audio and browser power requests
- Live CPU, disk and network graphs
- Every condition can be toggled and tuned from the UI. Settings are saved automatically
- Light and dark themes

## Installation

Get the build for your platform from the [Releases page](https://github.com/Subham-Maity/agent-power-guard/releases).

| Platform | File | Notes |
|---|---|---|
| Windows 10 / 11 (x64) | `AgentPowerGuard-windows-x64.msi` | Recommended. Installs to Program Files with Start Menu entry and Add/Remove Programs support |
| Windows (portable) | `agent-power-guard-windows-x64.exe` | Standalone, no install needed |
| Linux (x86_64) | `agent-power-guard-linux-x86_64.tar.gz` | X11 or Wayland, uses systemd for suspend |
| macOS (Apple Silicon) | `agent-power-guard-macos-aarch64.tar.gz` | M1 and newer |
| macOS (Intel) | `agent-power-guard-macos-x86_64.tar.gz` | |

Each release includes a `SHA256SUMS.txt` file for verifying downloads.

### Windows

Run the `.msi` installer, or launch the portable `.exe` directly.

On first run, it's worth clicking **Test Sleep** once to confirm your hardware suspends cleanly, then **Power Plan** to apply the recommended settings (see [Controls](#controls)).

### Linux

Install the runtime dependencies (Debian / Ubuntu / Mint):

```bash
sudo apt update
sudo apt install -y libgl1-mesa-dev libx11-dev libxext-dev libxcursor-dev libxinerama-dev libxi-dev xprintidle
```

Extract and run:

```bash
tar -xzvf agent-power-guard-linux-x86_64.tar.gz
cd agent-power-guard
./agent-power-guard
```

### macOS

```bash
tar -xzvf agent-power-guard-macos-aarch64.tar.gz
./agent-power-guard
```

If Gatekeeper blocks the first launch, go to **System Settings > Privacy & Security** and click **Open Anyway**.

## Quick start

1. Launch the app and make sure **Guard** is `ON`.
2. Pick a preset in the left panel. `3m` is a good default.
3. Leave it running. When you and your agents go quiet, the display turns off, then the PC sleeps.

### Verifying it works

**Instant check:** click **Test Sleep**. The PC suspends immediately. Move the mouse to wake it. After wake-up the app detects the clock jump and starts a fresh grace period.

**Full automatic check:**

1. Click the **15s Test** preset.
2. If any condition shows `BLOCKING SLEEP`, uncheck it or raise its threshold. For example, uncheck *Whole-PC Disk I/O* if files are being written.
3. Take your hands off the mouse and keyboard.
4. Watch the countdown. When every condition passes, the status banner turns green and the PC sleeps at `0s`.

## Presets

| Preset | Sleep after | Display off after | Suited for |
|---|---|---|---|
| 15s Test | 15 s | 1 min | Verifying setup |
| **3m** (default) | 3 min | 2 min | General use |
| 5m | 5 min | 3 min | Light coding, reading docs |
| 10m | 10 min | 5 min | Docker, medium compiles, longer LLM chains |
| 15m | 15 min | 10 min | Calls and short breaks |
| 30m | 30 min | 15 min | Test suites, migrations, small training runs |
| 45m | 45 min | 20 min | Multi-step autonomous agent runs |
| 1h | 1 h | 30 min | Long builds, video renders |
| 2h | 2 h | 45 min | Overnight training or rendering |
| 3h | 3 h | 60 min | Longest standby window |

The sliders allow custom values: idle timer from 10 s to 3 h, monitor timeout from 1 min to 3 h.

## Guard conditions

Sleep triggers only when every enabled condition passes. Each one has its own checkbox, so you can bypass a condition that is giving false positives.

| # | Condition | What it measures | Default |
|---|---|---|---|
| 1 | Human input idle | Time since last mouse or keyboard input | 180 s |
| 2 | Watched agent CPU | Normalized CPU use of monitored dev and AI processes | 2.0 % |
| 3 | Active download | Disk I/O of download managers (`IDMan`, `aria2c`, `qbittorrent`, `fdm`) | 100 KB/s |
| 4 | Whole-PC CPU | Total CPU load across all processes | 30 % |
| 5 | Whole-PC disk I/O | Combined read and write across all drives | 5,000 KB/s |
| 6 | Whole-PC network | Combined traffic across all adapters | 100 KB/s |
| 7 | Sustained spike filter | How long CPU, disk or network must stay above threshold before the timer resets | 15 s |
| 8 | Monitor timeout | Syncs the Windows screen-off timeout with the sleep timer | 2 min |
| 9 | Power requests | Respects active `DISPLAY:` requests from `powercfg /requests` (fullscreen video, etc.) | On |

**Notes**

- **Condition 2** normalizes per-process CPU across all logical cores. On an 8-core machine, a process at 16% in Task Manager counts as 2.0% of the whole system. The default watch list is `antigravity, language_server, cursor, claude, gemini, code, node, npm, pnpm, bun, tsc, esbuild, vite, next, nest, git, cargo, rustc, python, docker, wsl`, and it can be edited from *Edit Process Keywords*.
- **Condition 5:** if background indexing keeps tripping it, raise the threshold to 10,000-15,000 KB/s.
- **Condition 7** exists because Windows produces constant one-second blips (a browser tab loading, Search indexing a file). Without the filter they would reset the countdown forever.
- **Condition 9** includes an **Override Audio/Browser Locks** button that tells Windows to ignore wake locks from virtual audio drivers (VB-Audio, FxSound) and idle browser media threads.

## Controls

| Control | Description |
|---|---|
| **Guard ON / OFF** | Master switch. When off, automatic sleep is disabled and the PC stays awake |
| **Test Sleep** | Elevates `SeShutdownPrivilege` and calls `SetSuspendState` to verify the PC can suspend |
| **15s Test** | Sets the idle countdown to 15 seconds for testing thresholds |
| **Power Plan** | Applies a low-idle-power Windows plan via `powercfg`: CPU min 5% / max 100%, PCIe ASPM on, wake timers off, Windows standby timeout disabled so only Agent Power Guard controls sleep |
| **Clean v3** | Removes the legacy `AgentPowerGuard` scheduled task and `C:\ProgramData\AgentPowerGuard` left by the old batch / PowerShell version |
| **Light / Dark** | Switches the UI theme |

## Platform details

| | Windows | Linux | macOS |
|---|---|---|---|
| Sleep | `SetSuspendState` | `systemctl suspend` (or `loginctl` / DBus) | `pmset sleepnow` |
| Input idle | `GetLastInputInfo` | `xprintidle` (X11) | `ioreg -c IOHIDSystem` (`HIDIdleTime`) |
| Display off | `powercfg` | `xset dpms` (X11) or DBus | `pmset -a displaysleep` |
| Autostart | Installer / Task Scheduler | `~/.config/autostart/agent-power-guard.desktop` | `~/Library/LaunchAgents/com.agentpowerguard.app.plist` |

## CI and releases

GitHub Actions handles builds and releases:

- `ci.yml` builds and tests every push and pull request on Windows, Ubuntu and macOS.
- `release.yml` runs when a version tag such as `v1.0.0` is pushed. It builds the Windows `.exe` and `.msi` (WiX), the Linux `.tar.gz`, and both macOS `.tar.gz` archives, generates `SHA256SUMS.txt`, and publishes the GitHub Release.

## Contributing

Issues and pull requests are welcome. For larger changes, please open an issue first to discuss the approach.

## License

[MIT](LICENSE)
