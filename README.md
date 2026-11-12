Agent Power Guard

**Sleep your PC only when you and your background coding agents are idle.**

Agent Power Guard is a native utility written in Rust that replaces the fixed OS sleep timer with one that knows what the machine is doing. It watches input activity, developer tooling, GPU load, disk, network and meetings, and suspends the system only when every enabled condition is quiet. Long builds, local LLM inference and autonomous agent runs (Claude Code, Cursor, Antigravity, Ollama) are no longer killed by a power plan that cannot tell idle from busy.

[![Release](https://img.shields.io/github/v/release/Subham-Maity/agent-power-guard?label=release)](https://github.com/Subham-Maity/agent-power-guard/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/Subham-Maity/agent-power-guard/ci.yml?label=build)](https://github.com/Subham-Maity/agent-power-guard/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

<img width="1069" height="1149" alt="Agent Power Guard GUI" src="https://github.com/user-attachments/assets/8d286957-530a-43a6-90ae-dbd3d4641a6c" />

Supported platforms: Windows 10/11 (x64), Linux (x86_64, X11 and Wayland), macOS (Apple Silicon and Intel).

## Contents

- [Why](#why)
- [Features](#features)
- [Installation](#installation)
- [Quick start](#quick-start)
- [How it works](#how-it-works)
- [Guard conditions](#guard-conditions)
- [Presets, power modes and schedules](#presets-power-modes-and-schedules)
- [Configuration and data files](#configuration-and-data-files)
- [Usage](#usage)
- [Remote dashboard](#remote-dashboard)
- [Background service and packaging](#background-service-and-packaging)
- [Energy tracking](#energy-tracking)
- [Block-reason history](#block-reason-history)
- [Alerts and update checks](#alerts-and-update-checks)
- [Architecture](#architecture)
- [Platform support](#platform-support)
- [Project structure](#project-structure)
- [Building from source](#building-from-source)
- [Development, CI and releases](#development-ci-and-releases)
- [Security](#security)
- [Troubleshooting](#troubleshooting)
- [Contributing](#contributing)
- [License](#license)

## Why

- **Fixed timers sleep at the wrong moment.** A 15-minute OS timer will suspend the machine halfway through `cargo build`, `npm install`, a fine-tuning run or a multi-step agent task.
- **Stuck power requests keep it awake all night.** Audio drivers, paused browser tabs and download managers often hold a `SYSTEM_REQUIRED` lock, leaving the machine (and its GPU) at full power for no reason.
- **Displays burn power and panels.** Leaving a monitor on wastes electricity and risks OLED burn-in, even when the machine still has work to do.

Agent Power Guard evaluates 11 independent conditions every second and suspends the system only when every enabled one passes.

## Features

- **Intent-aware sleep engine & signal conditioning:** 11 conditions filtered through a rolling-window duty-cycle & hysteresis layer so 1-second background spikes never reset the sleep timer.
- **Three clear top-level modes:** **Smart Guard** (watchdog manages display-off & sleep), **Guard Off** (watchdog off, normal OS power plan), and **Best Performance** (explicit OS wake lock via dedicated thread keeping display & PC awake, with optional `1h / 2h / ∞` timers).
- **Two-stage power saving:** turns the display off first (with HDMI/monitor speaker audio protection), suspends the system later.
- **Passive media detection & prompt:** CoreAudio render + WinRT SMTC detection surfaces a non-focus-stealing prompt (`Keep Awake / Sleep Now / Hide`) locked to the playback session identity and auto-cleared after pause grace.
- **Process-tree & noise attribution:** separates idle IDE hosts (`code.exe`, `cursor.exe`) from active worker children; attributes Disk I/O per process minus background noise (`MsMpEng`, `SearchIndexer`); filters Network traffic to default-route interfaces (ignoring WSL/Docker/loopback).
- **Adaptive GPU baseline & NVML power:** learns idle desktop compositor baseline (`floor + margin`) across all NVML adapters and board power draw, without triggering on static VRAM allocation.
- **Meeting & creative app intelligence:** combines OS mic/camera hooks with live CoreAudio capture signal-presence (`Dead / Ambient / Active`) — protects muted Zoom/Teams/Meet calls while ignoring silent DAW/Premiere standby.
- **Zombie lease & wake classification:** detects idle PID-bound leases (~0% subtree CPU/disk), alerts before optional expiry, and classifies resumes (`User Input`, `Scheduled Timer`, `Unattended`) with a short `120s` unattended return-to-sleep timer.
- **Transparent blocker table & built-in `(i)` guide:** shows exact condition, offending process/owner, live metric, hold duration, and 1-tap `+ Ignore` in both the desktop UI and web dashboard.
- **Agent leases, alerts & remote fleet:** `run` wrapper, stdio MCP server, ntfy/Telegram/webhook notifications, block history timeline, QR-paired web dashboard, Wake-on-LAN relay, energy/CO2 accounting, quiet hours, battery profiles, and system tray memory trimming (~5 MB working set hidden).
- **No Electron, no WebView, no runtime:** one native executable built on `egui`.

## Installation

Every release on the [Releases page](https://github.com/Subham-Maity/agent-power-guard/releases) includes the files below and a `SHA256SUMS.txt` for verification.

| Platform | File | Notes |
|---|---|---|
| Windows 10 / 11 (x64) | `AgentPowerGuard-windows-x64.msi` | Installer with Start Menu and Desktop shortcuts, installs to `C:\Program Files\AgentPowerGuard`. Recommended. |
| Windows 10 / 11 (x64) | `agent-power-guard-windows-x64.exe` | Portable single executable. |
| Linux (x86_64) | `agent-power-guard-linux-x86_64.tar.gz` | X11 and Wayland. Contains `agent-power-guard/` with the binary, `README.md` and `LICENSE`. |
| Linux (amd64) | `agent-power-guard_<version>_amd64.deb` | Debian/Ubuntu package. Declares no dependencies; install `xprintidle` and the GTK/AppIndicator libraries yourself. |
| macOS (Apple Silicon) | `agent-power-guard-macos-aarch64.tar.gz` | M1 and newer. |
| macOS (Intel) | `agent-power-guard-macos-x86_64.tar.gz` | |
| macOS (universal) | `AgentPowerGuard-macos-universal.zip` | `Agent Power Guard.app` bundle (arm64 + x86_64 via `lipo`). |

### Package managers

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

Package manager manifests (winget, Scoop, Chocolatey, Homebrew tap, AUR) are maintained in the [`packaging/`](packaging/) directory and automatically published upon GitHub Releases tags via CI. Direct installation via GitHub Releases (`.msi`, `.deb`, macOS universal `.zip`) and `cargo install` is available immediately.

### Windows

Run the `.msi` and follow the wizard, then start **Agent Power Guard** from the Start Menu. For a silent install:

```powershell
msiexec /i AgentPowerGuard-windows-x64.msi /qn
```

The executable embeds an application manifest configured for standard user execution (`ExecutionLevel::AsInvoker` in `build.rs`). CLI subcommands (`status`, `lease`, `wol`, `run`, `mcp`, `postpone`) and the background `daemon` run immediately without UAC elevation prompts. On Windows, system sleep privileges (`SeShutdownPrivilege`) are acquired at runtime when an interactive session triggers sleep.

### Linux

```bash
tar -xzf agent-power-guard-linux-x86_64.tar.gz
cd agent-power-guard
./agent-power-guard
```

Input idle time is read through `xprintidle` under X11 (Wayland sessions query D-Bus first, see [Background service and packaging](#background-service-and-packaging)). The tray icon needs GTK 3 and AppIndicator libraries, which most desktop distributions already ship:

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
sha256sum -c SHA256SUMS.txt                       # Linux (shasum -a 256 -c on macOS)
Get-FileHash .\AgentPowerGuard-windows-x64.msi    # Windows (PowerShell)
```

Release binaries are not signed with a paid code-signing certificate (see [Troubleshooting](#troubleshooting)).

## Quick start

1. Launch the app and select the **15s Test** preset.
2. Take your hands off the mouse and keyboard.
3. Watch the countdown. If a condition shows **BLOCKING SLEEP**, its reason is listed (recent input, disk activity, an active download, and so on).
4. When every condition passes and the countdown reaches zero, the machine suspends. Move the mouse to wake it; the watchdog restarts its cycle automatically.
5. Switch to a real preset (the default is 3 minutes) once you are happy with the thresholds.

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

Each condition can be switched off and tuned independently. The numbering below matches the `condN_pass` fields of the HTTP API.

| # | Condition | Blocks sleep when | Default |
|---|---|---|---|
| 1 | Human input idle | Last mouse or keyboard input is more recent than the threshold | 180 s |
| 2 | Watched agent CPU | Process-tree worker children matching watch list exceed threshold (sustained) | 2.0 % |
| 3 | Active downloads | Download managers (`idman`, `aria2c`, `qbittorrent`, `fdm`) exceed threshold | 100 KB/s |
| 4 | Whole-PC CPU | Total CPU minus Agent Power Guard self-CPU exceeds threshold (sustained) | 30 % |
| 5 | Attributed disk I/O | Per-process disk I/O bounded by physical SSD throughput (`IOCTL_DISK_PERFORMANCE`), excluding background noise (`MsMpEng`, `SearchIndexer`, etc.) and browser/WebView IPC, exceeds threshold | 10,000 KB/s |
| 6 | Default-route network | Default-route (`0.0.0.0/0`, `::/0`) throughput exceeds threshold (ignores WSL/Docker/loopback) | 200 KB/s |
| 7 | Signal-conditioning filter | Rolling-window duty cycle + hysteresis across analog checks (`Busy` >= 8/15s, clears <= 2/15s) | 15 s (8/2) |
| 8 | Monitor timeout & speaker protection | Syncs OS display timeout with Stage 1; skips display-off if HDMI/monitor speakers play audio | 2 min |
| 9 | Power requests | Active `DISPLAY` locks (suppresses paused silent browsers >60s; degrades cleanly if unprivileged) | on |
| 10 | Adaptive GPU activity | GPU utilization exceeds `max(threshold, idle_floor + margin)` across NVML adapters + power draw | 5 % (+12 % floor margin) |
| 11 | Mic & cam + signal presence | Strict on calls (`Zoom`/`Teams`/`Meet`/`Discord`); creative tools (`Premiere`/`OBS`) require active capture signal | on |

How the intent engine works:

- **Signal-conditioning layer:** Transient 1-second spikes enter `Pending` without resetting the sleep countdown or blocking Stage 1 display-off. Only sustained `Busy` duty cycles block sleep, and hysteresis clears smoothly when quiet.
- **Condition 2** traverses parent-child process trees so idle IDE windows (`code.exe`, `cursor.exe`) don't block sleep by themselves while active spawned workers/compilers (`cargo`, `node`, `rustc`) do.
- **Conditions 5 & 6** bound per-process disk I/O by physical SSD throughput (`IOCTL_DISK_PERFORMANCE`) while excluding antivirus/indexing noise, browser/WebView Mojo IPC, and virtual adapters (WSL vEthernet, Hyper-V, Docker, loopback), showing both effective and raw metrics (`proc_rate (Disk total)`) plus top-5 raw I/O diagnostics.
- **Condition 8 & Passive Media:** Audio-only playback allows Stage 1 display-off while keeping the machine awake (unless playing through monitor/HDMI speakers). Fullscreen/windowed media idle triggers a non-focus-stealing topmost prompt (**Keep Awake / Sleep Now / Hide**) locked to session identity (`app + session`) until pause grace expires.
- **Condition 9 & 10 Probe Health:** When standard-user privileges (`AsInvoker`) block `powercfg /requests` or `nvml.dll` is absent, Agent Power Guard surfaces an explicit **Probe Unavailable** badge instead of silently misreporting idle.
- **Condition 11** pairs OS mic/camera checks (`CapabilityAccessManager` / PulseAudio) with live CoreAudio peak capture classification (`Dead / Ambient / Active`) so DAWs in standby don't block sleep while muted calls still stay protected.

## Presets, power modes and schedules

### Top-level operational modes

Choose directly from the top segmented control, system tray, or `POST /api/operational_mode`:

- **Smart Guard (`smart_guard`):** Active intent-aware watchdog managing Stage 1 display-off and Stage 2 sleep.
- **Guard Off (`guard_off`):** Pauses the watchdog so your normal operating system power plan governs sleep and display timeout.
- **Best Performance (`best_performance`):** Explicitly locks display and system awake on a dedicated OS wake-lock thread (`SetThreadExecutionState` on Windows, `systemd-inhibit` on Linux, `caffeinate` on macOS) with optional `1h`, `2h`, or `∞` timers.

### Timeout presets

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

### Timeout profiles

Switch timeout profiles without reconfiguring sliders (`POST /api/mode`):

| Mode | Idle | Display timeout | Agent CPU |
|---|---|---|---|
| Balanced (default) | 3 min | 2 min | 2.0 % |
| Work | 10 min | 5 min | 1.0 % |
| Night | 1 min | 1 min | 1.0 % |
| Away | 30 s | 1 min | not specified |
| Custom | user-defined thresholds are preserved | | |

The tray icon's right-click menu switches modes, starts a 1-hour Keep Awake, or postpones sleep (+15 min).

### Quiet hours

- Configure automated quiet periods (for example `23:00` to `07:00`).
- During quiet hours the watchdog downshifts to the aggressive `Night` or `Away` thresholds.
- **Sleep After Lease Ends** puts the machine to sleep the moment an agent task or wrapped build completes, even overnight.

### Laptop battery awareness

- Detects AC versus battery discharge via `GetSystemPowerStatus` (Windows), `/sys/class/power_supply` (Linux) and `pmset -g batt` (macOS). Desktops without a battery are unaffected.
- **Battery profile:** shorter idle timeout (default 60 s) and tighter CPU limit (default 1.0 %) when unplugged.
- **Emergency low-battery sleep:** forces sleep or hibernate below a critical cutoff (default 15 %).

### Scheduled wake timers

Schedule recurring or one-off wake events (for example `02:00` for a backup or CI job) with native OS facilities: Windows Task Scheduler (`-WakeToRun`), Linux `rtcwake`, macOS `pmset schedule wake`. After waking, the watchdog resumes monitoring and returns the machine to sleep once conditions are idle. Also available as `POST /api/wake_timer/add`.

## Configuration and data files

Agent Power Guard runs with zero mandatory environment variables. All watchdog settings, condition thresholds, power profiles, and the `ignored_processes` list are automatically persisted to `config.json` in the user configuration directory.

| File | Purpose | Windows | Linux | macOS |
|---|---|---|---|---|
| `config.json` | Watchdog thresholds, profiles & settings | `%APPDATA%\AgentPowerGuard\` | `~/.config/agent-power-guard/` | `~/Library/Application Support/AgentPowerGuard/` |
| `api_token.txt` | 256-bit API auth token (0600 on Unix) | `%APPDATA%\AgentPowerGuard\` | `~/.config/agent-power-guard/` | `~/Library/Application Support/AgentPowerGuard/` |
| `energy_history.json` | Energy tracking data | `%APPDATA%\AgentPowerGuard\` | `~/.config/agent-power-guard/` | `~/Library/Application Support/AgentPowerGuard/` |
| `block_history.jsonl` | Block-reason history (JSON Lines) | `%APPDATA%\AgentPowerGuard\` | `~/.config/agent-power-guard/` | `~/Library/Application Support/AgentPowerGuard/` |

Network defaults:

| Item | Value |
|---|---|
| HTTP API and dashboard port | `49281` |
| Bind address | `127.0.0.1` (LAN access is opt-in from the **Web & Mobile Remote** tab) |
| Wake relay port | passed as an argument (`agent-power-guard relay 49282` in the examples) |
| Wake-on-LAN default | broadcast `255.255.255.255`, UDP port `9` |

## Usage

### 1. Process wrapper (`run`, recommended)

Runs any agent or long task and ties the lease to the child's PID. The lease is released when the process exits or crashes (the watchdog notices a dead PID within 1-2 seconds), `Ctrl+C` terminates the child gracefully and releases the lease, and the child's exit code is forwarded.

```bash
# Wrap Claude Code
agent-power-guard run -- claude

# Wrap a long build or test command
agent-power-guard run -- cargo test --workspace

# Custom reason and a maximum safety timeout of 120 minutes
agent-power-guard run --max-mins 120 --reason "LoRA fine-tuning" -- python train.py
```

### 2. Model Context Protocol (MCP) server

`agent-power-guard mcp` is a stdio MCP server exposing:

| Tool | Description |
|---|---|
| `keep_awake(minutes, reason)` | Request a keep-awake lease |
| `release()` | Release the active agent lease |
| `status()` | Countdown, failing blockers and lease details |
| `postpone(minutes)` | Defer automated sleep |

Claude Code:

```bash
claude mcp add agent-power-guard -- agent-power-guard mcp
```

Claude Code (`~/.claude.json` or `.claude/config.json`), Cursor (`.cursor/mcp.json`) and Antigravity all accept the same entry:

```json
{
  "mcpServers": {
    "agent-power-guard": {
      "command": "agent-power-guard",
      "args": ["mcp"]
    }
  }
}
```

In Cursor you can instead use **Settings > Features > MCP > + Add New MCP Server** with type `command` and command `agent-power-guard mcp`. Shell-hook alternative for any agent: run `agent-power-guard lease 60 "Claude agent running"` before a task and `agent-power-guard release` afterwards. See [`docs/agents.md`](docs/agents.md) for the full guide.

### 3. Agent profiles and noise filtering

Select a profile under **Conditions > Watched Agent Processes**:

| Profile | Process names |
|---|---|
| Claude Code | `claude, node` |
| Cursor | `cursor, node, code` |
| Antigravity | `antigravity, python, node` |
| Ollama / Local LLM | `ollama, llama-box, text-generation-launcher` |
| Docker | `docker, dockerd, com.docker.backend` |
| Rust / Cargo | `cargo, rustc, rust-analyzer` |
| Node / JS | `node, bun, deno, pnpm` |
| Python ML | `python, python3, jupyter, torchrun` |

`ignored_processes` excludes noisy background processes (language-server spikes, Git background indexers, telemetry agents) from resetting the timer. It can also be changed with `POST /api/ignore/add` and `/api/ignore/remove`, or with the **+ Ignore** button in the dashboard.

### 4. CLI reference

```bash
agent-power-guard lease 30 "Compiling LLVM release"   # hold for 30 minutes
agent-power-guard release                             # release immediately
agent-power-guard status                              # current state as JSON
agent-power-guard postpone 15                         # delay sleep by 15 minutes
agent-power-guard sleep                               # suspend now
agent-power-guard wol 00:11:22:33:44:55               # send Wake-on-LAN magic packet
agent-power-guard wol 00:11:22:33:44:55 192.168.1.255 # ... to a specific broadcast address
agent-power-guard daemon                              # headless watchdog (no GUI)
agent-power-guard service install                     # register background autostart service/task
agent-power-guard service uninstall                   # unregister background service
agent-power-guard service status                      # inspect background service status
agent-power-guard relay 49282                         # Wake-on-LAN micro-relay server
agent-power-guard mcp                                 # stdio MCP server
agent-power-guard run [--max-mins N] [--reason TEXT] -- <command>
```

`wol` accepts standard MAC formats (`00:11:22:33:44:55`, `00-11-22-33-44-55`, or `001122334455`) with an optional custom broadcast target (e.g. `192.168.1.255:9`). Additional commands include `daemon` (headless background watchdog), `service [install|uninstall|status|run]` (system service registration), and `relay [port]` (standalone Wake-on-LAN micro-relay).

### 5. HTTP API

The app serves a local API on port `49281`. It binds to `127.0.0.1` and generates a random 256-bit token on first run.

**Authentication.** All `/api/*` endpoints require the token in `Authorization: Bearer <token>` or `X-APG-Token: <token>`. The CLI reads the token from `api_token.txt` (see [Configuration](#configuration-and-data-files)). Missing or invalid tokens return `401 Unauthorized`; IPs exceeding 10 failed authentication attempts per minute return `429 Too Many Requests` (`Retry-After: 60`). Constant-time comparison is used to prevent timing attacks. When LAN access is enabled, `/api/*` endpoints and `OPTIONS` preflights emit `Access-Control-Allow-Origin: *` for multi-machine Fleet View polling; otherwise cross-origin browser requests are rejected with `403`.

| Endpoint | Method | Parameters | Description |
|---|---|---|---|
| `/` | GET | `#t=<token>` | Remote dashboard (token read from the URL fragment) |
| `/api/status` | GET | | Countdown, operational mode, intent, structured blockers, media/mic/GPU/power-request telemetry, leases |
| `/api/operational_mode` | POST | `mode=smart_guard\|guard_off\|best_performance&hours=N` | Switch top-level operational mode (with optional expiry hours for Best Performance) |
| `/api/media_prompt` | POST | `action=keep\|sleep\|hide` | Respond to an active passive media playback prompt |
| `/api/guide` | GET | | Built-in `(i)` Guide sections explaining modes, intent, and signal conditioning |
| `/api/sleep` | POST | | Suspend immediately |
| `/api/postpone` | POST | `mins=15` | Postpone sleep |
| `/api/toggle_guard` | POST | | Toggle between `Smart Guard` and `Guard Off` |
| `/api/lease` | POST | `mins=30&reason=Build&pid=1234` | Grant a sleep lease (optionally PID-bound with zombie detection) |
| `/api/release` | POST | | Clear all leases |
| `/api/mode` | POST | `name=balanced\|work\|night\|away` | Switch timeout preset profile |
| `/api/schedule/stay_awake` | POST | `mins=60` | Schedule temporary keep-awake |
| `/api/wake_timer/add` | POST | `time=02:00&label=Backup` | Schedule an OS RTC / Task Scheduler wake timer |
| `/api/wake_timer/delete` | POST | `id=<timer_id>` | Delete a scheduled wake timer |
| `/api/history` | GET | | Recent block-reason history and wakeup records |
| `/api/history/clear` | POST | | Clear block-reason history |
| `/api/ignore/add` | POST | `process=<name>&kind=process\|disk_noise\|power_owner` | Add target to process ignore, disk noise, or power-request ignore list |
| `/api/ignore/remove` | POST | `process=<name>` | Remove a process keyword from the ignore list |
| `/api/peers` | GET | | Configured fleet peers |
| `/api/peers/add` | POST | `name=&url=&token=&permission=&mac=` | Add or update a fleet peer |
| `/api/peers/remove` | POST | `id=<peer_id>` | Remove a fleet peer |
| `/api/wol` | POST | `mac=&bcast=` | Broadcast a Wake-on-LAN magic packet |
| `/api/notify/test` | POST | | Send a test notification on all configured channels |
| `/api/energy/clear` | POST | | Clear energy history |

Abridged `/api/status` response:

```json
{
  "guard_enabled": true,
  "operational_mode": "smart_guard",
  "intent": "ActiveWork",
  "intent_label": "Active Work (cargo.exe)",
  "all_conditions_pass": false,
  "countdown_secs": 180,
  "countdown_max": 180,
  "blocker_banner_text": "Blocked by Attributed Disk I/O (cargo.exe)",
  "structured_blockers": [
    { "condition": 5, "name": "Disk I/O", "who": "cargo.exe", "value": "14.2 MB/s", "since_secs": 18, "ignore_target": "cargo.exe", "ignore_kind": "disk_noise" }
  ],
  "live_idle_secs": 42,
  "live_agent_cpu": 4.8,
  "agent_lease_active": false
}
```

Other fields consumed by the bundled dashboard: `operational_mode_note`, `stage1_display_off_allowed`, `defer_display_off_for_monitor_audio`, `media` (`prompt_active`, `app`, `title`, `lock_active`, `lock_app`), `mic_cam` (`mic_active`, `cam_active`, `app`, `signal_state`, `capture_peak`), `power_requests` (`display_active`, `suppressed_browser`, `unavailable`, `owners`), `gpu` (`util_pct`, `floor_pct`, `effective_threshold_pct`, `power_mw`, `unavailable`), `raw_pc_disk`, `raw_pc_net`, `top_disk_proc`, `top_disk_kbps`, `top_io_debug`, `suggested_thresholds` (`pc_cpu`, `disk_kbps`, `net_kbps`), `agent_lease_reason`, `agent_lease_remaining_secs`, `cond1_pass` through `cond11_pass`, `live_agent_proc`, `live_gpu`, `live_download`, `live_mic_cam_active`, `live_mic_cam_app`, `live_pc_cpu`, `live_pc_disk`, `live_pc_net`, `cond7_text`, `spike_counter`, `spike_limit`, `power_request_active`, `power_mode`, `local_ip`, `mac_address`, `currency_symbol`, `co2_factor`, `history` (`agent_cpu`, `gpu`, `pc_cpu`, `pc_disk`, `pc_net` arrays), `top_processes` and `energy` (`cost_saved`, `kwh_saved`, `duration_secs`, `co2_saved`).

curl:

```bash
TOKEN=$(cat ~/.config/agent-power-guard/api_token.txt)
curl -X POST -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:49281/api/lease?mins=45&reason=Build"
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:49281/api/status
curl -X POST -H "Authorization: Bearer $TOKEN" http://127.0.0.1:49281/api/release
```

Python (Windows uses `%APPDATA%`; elsewhere `~/.config`):

```python
import os
import urllib.request
from pathlib import Path

appdata = os.environ.get("APPDATA")
token_path = (
    Path(appdata) / "AgentPowerGuard" / "api_token.txt"
    if appdata
    else Path.home() / ".config" / "agent-power-guard" / "api_token.txt"
)
token = token_path.read_text().strip()

base = "http://127.0.0.1:49281"
headers = {"Authorization": f"Bearer {token}"}

req = urllib.request.Request(f"{base}/api/lease?mins=45&reason=PyTorch+Training", data=b"", headers=headers)
urllib.request.urlopen(req)

# ... run long job ...

req = urllib.request.Request(f"{base}/api/release", data=b"", headers=headers)
urllib.request.urlopen(req)
```

## Remote dashboard

The embedded web server serves a responsive, mobile-first dashboard (`assets/dashboard.html`, a single static file with no build step and no external scripts or CDNs). LAN access is enabled from the **Web & Mobile Remote** tab.

- **QR pairing.** The tab renders a QR code (generated by the built-in encoder in `src/qr.rs`: versions 1-10, error-correction levels L and M, byte mode) that opens the dashboard with the token pre-loaded in the URL fragment (`#t=<token>`). The dashboard stores the token in browser `localStorage` and removes it from the address bar. The pairing URL grants full remote control; do not share or publish screenshots of the QR code. The QR encoder formats standard Version 1–10 matrices with 7×7 finder patterns and 1-module white separators across all three corners (top-left, top-right, bottom-left), verified by unit tests.
- **Installable web app.** A web app manifest (`/manifest.json`), icons (`/icon.png`, `/apple-touch-icon.png`) and mobile viewport tags allow **Add to Home Screen** on Safari and Chrome. Over plain `http://192.168.x.x` it works as a standalone web app. For HTTPS outside your LAN, use Tailscale Serve: `tailscale serve https / http://127.0.0.1:49281`.
- **Live sparklines.** 120-second rolling trends for agent CPU, GPU, whole-PC CPU and network, drawn as inline SVG polylines.
- **Active processes and 1-tap ignore.** The top 8 processes by CPU are listed; agent-keyword matches are badged. **+ Ignore** appends a process to the ignore list with no restart.
- **Multi-machine fleet.** The **Multi-Machine Fleet** tab lists configured peers with state badges (`ARMED`, `BLOCKED`, `PAUSED`, `OFFLINE`), countdowns, blockers, CPU/GPU and mode, refreshed every 3 seconds. Peers are added with name, URL, API token, permission (`control` or `view`) and an optional MAC address. `control` peers expose +15m, Sleep and Toggle; peers with a MAC expose Wake-on-LAN. Peers are stored through `/api/peers` (the dashboard falls back to browser storage if that call fails), and the browser queries each peer's `/api/status` directly. All `/api/*` endpoints and OPTIONS preflights emit `Access-Control-Allow-Origin: *`, allowing the browser dashboard to poll fleet peers seamlessly without CORS blocks.
- **Wake-on-LAN info.** The dashboard shows the PC's LAN IP and MAC address for use with any phone WoL app.

### Remote wake

A sleeping PC cannot serve web traffic. Send a magic packet from another machine on the LAN:

```bash
agent-power-guard wol 00:11:22:33:44:55 192.168.1.255
```

To wake a machine from outside your network, use a Tailscale subnet router, an always-on node (Raspberry Pi, NAS, router) running the relay, or your router's WoL feature. Requirements: WoL enabled in BIOS/UEFI and in the adapter properties, wired Ethernet (Wi-Fi radios power down during sleep), and Windows Fast Startup disabled. The full guide is in [`docs/remote-wake.md`](docs/remote-wake.md).

**Wake relay.** `agent-power-guard relay 49282` turns a lightweight device into an authenticated relay serving `GET /relay/status` and token-protected `POST /relay/wake?mac=XX:XX:XX:XX:XX:XX&bcast=...`, broadcasting magic packets onto the home LAN. The standalone relay runs without GUI, egui, or sysinfo overhead, maintaining an active footprint of ~2–3 MB of RAM.

## Background service and packaging

### Headless service and UI split

```bash
agent-power-guard daemon              # run the watchdog without the GUI
agent-power-guard service install     # Windows Scheduled Task / Linux systemd / macOS launchd
agent-power-guard service uninstall
agent-power-guard service status
```

In service mode, closing the GUI does not stop the watchdog; protection persists across logoff and window close. (On Windows the executable manifest requests elevation; see [Installation](#windows).)

### Linux and Wayland

- Idle detection queries `org.freedesktop.ScreenSaver` over D-Bus and `systemd-logind` session hints on GNOME, KDE Plasma and wlroots, falling back to `xprintidle` under X11.
- Systemd user unit: `packaging/systemd/agent-power-guard.service` runs `%h/.local/bin/agent-power-guard daemon`. Enable with `systemctl --user enable --now agent-power-guard`. For a `.deb` install (binary in `/usr/bin`), adjust `ExecStart` or symlink the binary into `~/.local/bin`.
- `packaging/build-deb.sh` builds the `.deb` (binary, XDG desktop entry, 32x32 icon, systemd unit).

### macOS

- `packaging/launchd/com.agentpowerguard.watchdog.plist` runs `/usr/local/bin/agent-power-guard daemon` at load with `KeepAlive`; install the binary there first. Logs go to `/tmp/agent-power-guard.stdout.log` and `.stderr.log`.
- `packaging/build-macos-app.sh` bundles `Agent Power Guard.app` (minimum macOS 11.0) and creates a universal binary with `lipo` when both architecture builds exist.

## Energy tracking

The **Energy & Cost Savings** tab estimates what suspend and display-off saved: sleep sessions, monitor-off events, active versus sleep hours, kWh, money and CO2. Filter by today, 7 days, this month, this year or all time.

- Defaults: 120 W active system, 45 W display, 2 W sleep; wattage and tariff are configurable.
- Recording can be turned off at any time. Data is plain local JSON (`energy_history.json`, see [Configuration](#configuration-and-data-files)) that can be inspected, backed up or deleted without affecting the app. Nothing is sent anywhere.

**Regional carbon intensity.** Select a grid profile or enter a custom g/kWh value:

| Profile | g CO2 / kWh |
|---|---|
| US Average | 368 |
| US California (CAMX) | 205 |
| US Texas (ERCO) | 380 |
| European Union | 230 |
| United Kingdom | 148 |
| Germany | 380 |
| France | 52 |
| India | 713 |
| Australia | 510 |
| Global Average | 436 |
| Custom | user-defined |

Currency symbols: `$`, `€`, `£`, `₹`, `¥`, `C$`, `A$`. The desktop app and the remote dashboard both use the `co2_saved` value computed from the configured factor.

## Block-reason history

The **Block History** tab shows what prevented sleep:

- **Top blockers leaderboard:** blocking processes and conditions ranked by cumulative hold time (for example `Agent CPU: 2h 15m`).
- **"Last Night" summary:** the overnight 12-hour window, with total blocked time, the dominant blocker and the timestamp sleep was reached.
- **Event timeline:** start time, duration, primary condition and concurrent blockers per event.
- **Export:** CSV or JSON in one click; history can be cleared at any time (`POST /api/history/clear`).
- **Storage:** JSON Lines in `block_history.jsonl`, size-based rotation at a 5 MB cap, configurable retention (default 14 days).

## Alerts and update checks

The **Alerts & Updates** tab configures notifications. Everything is off by default, and outbound requests go only to destinations you configure. Network calls run on a dedicated worker thread so the 1 Hz watchdog loop is never delayed.

| Channel | Configuration |
|---|---|
| ntfy | ntfy.sh or a self-hosted server; optional authentication token |
| Telegram | `bot_token` and `chat_id` (Telegram Bot API) |
| Generic webhook | HTTP POST with a JSON payload (Discord, Slack, Home Assistant, custom servers) |

| Event | Trigger |
|---|---|
| Sleep imminent | 30-second pre-sleep warning when all conditions pass |
| Machine blocked long | One condition or agent holds the PC awake longer than N hours (default 2) |
| Lease ended | An agent task or build lease concludes |
| PC resumed | The system wakes from sleep |
| Guard paused long | The master guard stayed disabled longer than N hours (default 2 hours, configurable 1–24h) |

`POST /api/notify/test` sends a test notification to all configured channels.

**Update check (opt-in).** When enabled, the app checks GitHub Releases once per 24 hours. No machine identifiers or analytics are sent. A newer release shows a banner in the top bar linking to the release notes.

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
- **Native OS APIs, no helper processes.** Windows uses `windows-sys`; Linux and macOS use the platform tools listed below. Release builds use fat LTO, one codegen unit, `panic = "abort"` and symbol stripping.
- **No frontend build step.** The remote dashboard is a single static HTML file served by the embedded HTTP server.

| Component | Crate |
|---|---|
| GUI | `eframe` / `egui` (glow backend), `egui_plot` |
| System sampling | `sysinfo` |
| Tray | `tray-icon` |
| Serialization | `serde`, `serde_json` |
| Images | `image` (png, ico) |
| Windows APIs | `windows-sys`, `windows` (CoreAudio & WinRT SMTC) |
| Windows packaging | `embed-manifest`, `winres`, WiX Toolset v3 |
| QR codes | in-tree `src/qr.rs` (no dependency) |

## Platform support

| Capability | Windows | Linux | macOS |
|---|---|---|---|
| System sleep | `SetSuspendState` (never disables wake events) | `systemctl suspend` | `pmset sleepnow` |
| Wake locks (`Best Performance`) | Dedicated thread `SetThreadExecutionState` | `systemd-inhibit` | `caffeinate -d -i` |
| Input idle | `GetLastInputInfo` | `xprintidle` / X11, Wayland D-Bus | `ioreg` (`HIDIdleTime`) |
| Wake classification | `PowerRegisterSuspendResumeNotification` | Resume wall-clock jump + idle heuristic | Resume wall-clock jump + idle heuristic |
| Display timeout | `powercfg` + HDMI speaker form-factor check | `xset dpms` / Wayland DBus | `pmset displaysleep` |
| GPU monitoring | Multi-adapter NVML (`nvml.dll` + board power) | DRM sysfs | IOAccelerator |
| Audio / media / meeting | CoreAudio render & capture RMS, WinRT SMTC, `ConsentStore` | PulseAudio / PipeWire | CoreAudio |
| Network filtering | `GetIpForwardTable2` + `GetAdaptersAddresses` (default route) | `sysinfo` filtered NICs | `sysinfo` filtered NICs |
| Autostart | Task Scheduler / Startup | `~/.config/autostart` | `~/Library/LaunchAgents` |

## Project structure

```
.
├── Cargo.toml                 package manifest (binary: src/app.rs)
├── build.rs                   Windows manifest (AsInvoker) and icon resources
├── build-msi.ps1              builds the Windows MSI with WiX v3.14
├── generate-icon.ps1          regenerates assets\app.ico (Windows PowerShell, System.Drawing)
├── src/
│   ├── app.rs                 binary entry point, UI, tray, web server, watchdog orchestrator
│   ├── signal.rs              rolling-window signal conditioning, hysteresis, GPU baseline & mic signal classifier
│   ├── intent.rs              operational modes, UserIntent state machine, blocker transparency & (i) Guide content
│   ├── evaluator.rs           pure intent-aware decision evaluator and 15 scenario replay tests (T1-T15)
│   ├── probes.rs              OS probes (CoreAudio/SMTC worker, wake-lock thread, wake classifier, NVML, NICs, tree sampler)
│   ├── qr.rs                  dependency-free QR encoder (versions 1-10, EC L/M)
│   └── main.rs                legacy single-module reference (superseded by src/app.rs)
├── assets/
│   ├── dashboard.html         remote dashboard (single static file)
│   └── ...                    app.ico, icon_32.png and other icons
├── docs/
│   ├── agents.md              agent integration guide
│   └── remote-wake.md         Wake-on-LAN guide
├── packaging/                 .deb, macOS .app, systemd, launchd, winget, Scoop,
│                              Chocolatey, Homebrew and AUR manifests
├── wix/                       main.wxs and license.rtf for the MSI
└── .github/workflows/         ci.yml and release.yml
```

## Building from source

Requires a stable Rust toolchain at or above the `rust-version` declared in `Cargo.toml` (currently 1.95).

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

Packaging (run after `cargo build --release`):

```powershell
# Windows MSI: requires WiX Toolset v3.14 at C:\Program Files (x86)\WiX Toolset v3.14\bin
.\build-msi.ps1          # writes target\installer\AgentPowerGuard-<version>-x64.msi
```

```bash
mkdir -p dist
bash packaging/build-deb.sh          # Linux: dist/agent-power-guard_<version>_amd64.deb (needs dpkg-deb)
bash packaging/build-macos-app.sh    # macOS: dist/AgentPowerGuard-macos-universal.zip (uses lipo)
```

## Development, CI and releases

CI (`.github/workflows/ci.yml`) runs on pushes and pull requests to `main` (README, LICENSE and `.gitignore` changes are ignored) and on manual dispatch. It runs the following on `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`:

```bash
cargo check --target <target>
cargo test --target <target>
cargo build --release --target <target>
```

Unit tests cover the pure signal-conditioning filters (`src/signal.rs`), intent classification (`src/intent.rs`), T1-T17 scenario replay tests & evaluator edge cases (`src/evaluator.rs`, `src/app.rs`), OS request/NIC/IPC-browser parsers (`src/probes.rs`), and QR encoding (`src/qr.rs`). CI does not build the Intel macOS target, run `clippy` or check formatting.

**Releases.** Pushing a tag matching `v*`, or running the workflow manually with `tag_name`, triggers `.github/workflows/release.yml`. It builds the Windows MSI and portable EXE, the Linux `.tar.gz` and `.deb`, and the macOS `aarch64` and `x86_64` archives plus the universal `.app` zip. It then generates `SHA256SUMS.txt` and publishes a GitHub Release. A signing step exists as a placeholder and does not sign anything yet.

**Version bump checklist.** The version string is duplicated in `Cargo.toml`, `wix/main.wxs` (`Version="x.y.z.0"`), `packaging/macos/Info.plist`, `packaging/build-deb.sh`, and the winget, Scoop, Chocolatey, Homebrew and AUR manifests. `build-msi.ps1` reads `Cargo.toml` and falls back to `1.2.3`. Package-manager manifests also need real SHA-256 values after release checksums are published.

## Security

- The API and dashboard bind to `127.0.0.1` by default. Enabling LAN access exposes the dashboard over plain HTTP; use a VPN or Tailscale Serve for access outside your LAN.
- Every `/api/*` request needs the 256-bit token. Treat `api_token.txt`, the pairing URL and the QR code as credentials: they grant control over sleep, leases and power modes.
- `Host` validation protects against DNS rebinding, and cross-origin browser requests are rejected with `403` when LAN access is off (LAN mode permits authenticated fleet polling via CORS).
- The dashboard keeps the token (and, if the peers API is unavailable, fleet peer tokens) in browser `localStorage`. Pair only browsers you trust.
- On Windows the application embeds an `AsInvoker` manifest so GUI, CLI commands, and background daemons launch without UAC elevation prompts. Suspend (`SetSuspendState`) requests `SeShutdownPrivilege` cleanly as a standard user when allowed by local policy; `powercfg /requests` probing degrades transparently to a **Probe Unavailable** badge when unprivileged rather than hiding errors.
- No telemetry. Outbound requests go only to the notification destinations you configure and, if enabled, GitHub Releases.
- Release binaries are not signed with a paid code-signing certificate.

## Troubleshooting

**The machine never sleeps.** Open the app and look for the condition marked **BLOCKING SLEEP**; it shows the reason. The usual causes are a stuck power request (enable the override in condition 9), a background process matching the watch list (the defaults include broad names such as `code`, `node` and `git`), or a threshold that is too tight for your machine's idle noise. The **Block History** tab and the dashboard's **+ Ignore** button help identify and exclude false positives.

**Antivirus flags or removes the executable.** The app suspends the machine, acquires `SeShutdownPrivilege`, writes a startup entry and polls hardware and registry state. Those are legitimate for a power tool but look similar to suspicious behaviour to heuristic scanners, and the binary is not signed with a paid code-signing certificate. The project is open source and makes no network calls to external services other than those you configure. To resolve it, add an exclusion for the install folder:

- **Microsoft Defender:**
  1. Open **Windows Security > Virus & threat protection > Manage settings**.
  2. Under **Exclusions**, choose **Add or remove exclusions** and add the folder `C:\Program Files\AgentPowerGuard`.
- **Bitdefender Total Security:**
  1. Open Bitdefender -> **Protection** tab.
  2. Under **Antivirus**, open **Settings** -> **Manage exceptions** -> **Add an Exception** -> add folder `C:\Program Files\AgentPowerGuard` (check *On-access*, *On-demand*, *Embedded scripts*).
  3. Under **Advanced Threat Defense**, open **Settings** -> **Manage exceptions** -> add folder `C:\Program Files\AgentPowerGuard` (check *Advanced Threat Defense*).
  4. If files were quarantined, go to **Protection** -> **Antivirus** -> **Quarantine** / **Notifications** and choose **Restore**.

**Linux: idle time is never detected.** Install `xprintidle` (X11). On Wayland the app uses D-Bus and `systemd-logind` hints first.

**Linux: no tray icon.** Install GTK 3 and the AppIndicator libraries (`libgtk-3-dev`, `libayatana-appindicator3-dev` for building; the runtime equivalents otherwise).

**macOS: binary blocked on first launch.** Open **System Settings > Privacy & Security** and choose **Open Anyway**.

**Wake-on-LAN doesn't work.** Check that it is enabled in both the BIOS/UEFI and the adapter properties, that Windows Fast Startup is disabled, and that the PC is on Ethernet rather than Wi-Fi. See [`docs/remote-wake.md`](docs/remote-wake.md).

**Dashboard shows "Authentication Required".** Open the dashboard through the link or QR code from the **Web & Mobile Remote** tab, or paste the contents of `api_token.txt` into the token field.

## Contributing

Bug reports, ideas and pull requests are welcome.

1. Fork the repository.
2. Create a branch: `git checkout -b feat/my-feature`.
3. Commit your changes: `git commit -m "feat: add my feature"`.
4. Push the branch and open a pull request.

CI builds every pull request on Windows (x64), Linux (x86_64) and macOS (Apple Silicon).

## License

Released under the [MIT License](LICENSE).
``