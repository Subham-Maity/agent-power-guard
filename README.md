# Agent Power Guard 🛡️⚡

> A lightweight, 100% native Windows desktop utility written in Rust (`egui` + `glow`) that automatically puts your PC to sleep **only when you AND your background AI coding agents are truly idle**.

---

## ⚡ The Problem

Most Windows sleep settings fail developers running local AI agents, language servers, or builds:
1. **Windows Standby Timers** either put the PC to sleep mid-prompt (killing long LLM reasoning, code generation, or multi-minute builds), or...
2. **Audio/Browser Driver Locks** (like Discord, YouTube in Chrome/Edge, virtual audio cables, or IDM) prevent the PC from ever going to sleep, leaving high-power GPUs and workstations running hot overnight.
3. **Legacy scripts** (batch / PowerShell) run blind in the background without real-time visibility or diagnostics when sleep triggers fail.

**Agent Power Guard** replaces scripts and heavy Electron/Webview wrappers with a single, native executable running at **< 5 MB RAM** with real-time process and I/O threshold graphs.

---

## 💡 The Staged Power-Saving Philosophy (Why Monitor Off First?)

Instead of dropping the entire PC into sleep mode all at once, **Agent Power Guard** uses a **Two-Stage Power Strategy**:

```
[ User Steps Away ]
       │
       ▼ (Stage 1: After 2 mins)
[ Turn Off Display / Monitor ] ──► Saves 50–100W immediately
       │                          Protects OLED & prevents screen burn-in
       │                          CPU, GPU & Network STAY ACTIVE
       │                          Coding agents (Cursor, Claude, Antigravity) continue working!
       ▼ (Stage 2: After 3 mins total idle)
[ Check All 9 Guard Conditions ]
       ├── If Agent is still generating code ──► Stays Awake (Monitor stays off)
       └── If Human AND Agent are idle ────────► PC Enters S3 / Modern Standby (< 1W)
```

1. **Stage 1 (Monitor Turn-Off):** Turning off your display saves 50–100 Watts of electricity instantly and eliminates OLED burn-in, but keeps Windows, CPU, GPU, and network alive so long AI agent reasoning or cargo builds finish uninterrupted in the dark.
2. **Stage 2 (Deep Sleep):** Once the monitor is off and your coding agent finishes its work (CPU/Disk/Net drop below threshold), the app safely elevates `SeShutdownPrivilege` and suspends the PC into true low-power sleep (< 1W).

---

## ⏱️ Quick Presets & Staged Timers Guide

Inside the application's Left Panel, you can click any of the **Quick Preset** buttons to configure both stages automatically:

| Preset Button | Sleep Countdown | Monitor Turn-Off | Best For & Why This Setting |
|---|---|---|---|
| **`[ 15s Test ]`** | **15 seconds** | **1 min** | **Instant Testing:** Fast hands-off test mode. Lets you verify the automatic sleep countdown in 15 seconds without waiting. *(Tip: Uncheck Cond 5 Disk if compiling!)* |
| **`[ 3m ★ ]`** *(Default)* | **3 minutes** (180s) | **2 minutes** | **Best All-Rounder (Recommended):** Perfect balance. Monitor turns off at 2 min to save power; if coding agents finish within that minute, PC sleeps at 3 min. |
| **`[ 5m ]`** | **5 minutes** (300s) | **3 minutes** | **Light Coding & Browsing:** Gives you extra time to read docs or think between prompts without the screen turning off. |
| **`[ 10m ]`** | **10 minutes** (600s) | **5 minutes** | **Medium Workloads:** Good when testing local Docker containers, medium Rust/C++ compiles, or longer LLM chains. |
| **`[ 15m ]`** | **15 minutes** (900s) | **10 minutes** | **Office & Multitasking:** Display stays on for 10 min during phone calls or quick breaks. |
| **`[ 30m ]`** | **30 minutes** (1800s) | **15 minutes** | **Long Batch Tasks:** For running medium dataset training, database migrations, or test suites. |
| **`[ 45m ]`** | **45 minutes** (2700s) | **20 minutes** | **Extended Autonomous Agents:** For multi-step autonomous agent runs (e.g. multi-agent SWE benchmarks). |
| **`[ 1h ]`** | **1 hour** (3600s) | **30 minutes** | **Heavy Local Builds:** For long compilation, packaging, or 4K video renders. |
| **`[ 2h ]`** | **2 hours** (7200s) | **45 minutes** | **Overnight Training / Render:** Leaves PC awake for up to 2 hours of inactivity, then sleeps once done. |
| **`[ 3h ]`** | **3 hours** (10800s) | **60 minutes** | **Maximum Standby Window:** For very long background tasks while ensuring the PC does not stay on all weekend. |
| **Custom Slider** | **10s – 3 hours** | **1m – 3 hours** | **Fine-Tuning:** Drag the *Idle Timer* slider (10s to 10800s) and *Monitor Timeout* slider (1m to 180m) to any custom value you want. Settings auto-save. |

---

## 🧪 How to Test Immediately (Step-by-Step)

### Method 1: Instant Sleep Test (1-Click)
1. Click the **`[ ⚡ Test Sleep ]`** button in the top bar.
2. Your PC will immediately enter sleep mode via the Win32 `SeShutdownPrivilege` + `SetSuspendState` call.
3. Move your mouse or press a key on your keyboard to wake the PC up.
4. When the PC wakes up, the app automatically detects the wall-clock time jump (> 5s) and restarts a fresh grace period.

### Method 2: Full Automatic Sleep Trigger Test (15 Seconds)
1. In your app, look at the condition badges on the left.
2. If any condition shows red `[ BLOCKING SLEEP ]` (for example, Condition 5 Whole-PC Disk was high in your screenshot because files were being written):
   - Simply uncheck **`[ ] 5. Whole-PC Disk I/O`** (or slide its threshold up to `80,000 KB/s`).
3. Click the **`[ 15s Test ]`** preset button.
4. **Take your hands off the mouse and keyboard.**
5. Watch the top progress bar:
   - Condition 1 (Human Idle) badge turns green: `[ PASS (Quiet) ]` after 10s of inactivity.
   - The top progress bar counts down: `Time until sleep: 15s... 10s... 3s... 0s`.
   - The bottom banner turns bright green: `ALL CONDITIONS PASS -> PC Sleeping in Xs`.
6. At `0s`, the app puts the PC to sleep automatically!

---

## 📖 What is What? (Clear & Concise Reference)

### 1. Top Control Bar & Buttons

| Control / Button | What It Is | When To Use / What Setting |
|---|---|---|
| **`GUARD: [ ON / OFF ]`** | **Master Power Guard Switch.** When `ON`, the app actively monitors all 9 conditions and puts the PC to sleep when the countdown reaches 0. When `OFF`, automatic sleep is paused completely. | Keep **`ON`** for automatic sleep protection; click **`OFF`** if you want your PC to stay awake indefinitely. |
| **`[ ⚡ Test Sleep ]`** | **Instant Hardware Sleep Verification.** Executes the native Win32 `SeShutdownPrivilege` token privilege elevation and calls `SetSuspendState(false, false, false)` directly. | Click once when first installing to verify your PC supports native S3/Modern Standby cleanly. |
| **`[ ⏱ 15s Test ]`** | **Quick 15-Second Test Mode.** Instantly sets the idle countdown timer to 15 seconds. | Click whenever you want to test your active condition thresholds live. |
| **`[ ⚙ Power Plan ]`** | **Optimal Windows Power Plan.** Runs hidden `powercfg` commands: CPU Min = 5%, CPU Max = 100%, PCIe ASPM power savings = Enabled, Wake Timers = Disabled, Windows Standby Timeout = 0 (disabled, so only Agent Power Guard controls sleep). | Click once after installing to configure Windows for low idle wattage. |
| **`[ 🧹 Clean v3 ]`** | **Legacy Script Cleanup.** Unregisters the old `AgentPowerGuard` scheduled task and deletes `C:\ProgramData\AgentPowerGuard` from the earlier batch/PowerShell v3 script. | Click once to clean up old conflicting background tasks. |
| **`[ ☀ Light / 🌙 Dark ]`** | **UI Theme Switch.** Toggles between egui Dark Mode and Light Mode instantly. | Set to your visual preference. |

---

### 2. The 9 Interactive Guard Conditions

Every condition has an **`[x] Enable`** checkbox. If a condition is blocking sleep and you want to bypass it temporarily, simply uncheck it!

#### 1. Human Input Idle
- **What it is:** Measures how many seconds have elapsed since you last moved your mouse or pressed a key, using Win32 `GetLastInputInfo`.
- **Why it blocks:** If you are actively using the PC (idle seconds < threshold), it displays `BLOCKING SLEEP` so your PC never sleeps while you are working.
- **Recommended Setting:** `180s` (3 minutes). Set to `15s` during testing.

#### 2. Watched Agent CPU %
- **What it is:** Normalized CPU usage across all logical CPU cores for monitored AI/developer processes. Click *"Edit Process Keywords"* to view or edit the list: `antigravity, language_server, cursor, claude, gemini, code, node, npm, pnpm, bun, tsc, esbuild, vite, next, nest, git, cargo, rustc, python, docker, wsl`.
- **How it works:** If you have an 8-core CPU (800% max in Task Manager), a process taking 16% in Task Manager is normalized to `2.0%` of your whole system.
- **Why it blocks:** Keeps the PC awake while Claude, Cursor, Antigravity, Gemini, or a Cargo/Vite compile is generating code or thinking.
- **Recommended Setting:** `2.0%`.

#### 3. Active Download / IDM
- **What it is:** Specifically watches download managers (`IDMan.exe`, `aria2c`, `qbittorrent`, `fdm`) and measures their active disk I/O in KB/s.
- **Why it matters:** `IDMan.exe` sitting idle in the Windows system tray uses 0 KB/s and will **NOT** block sleep. Only active, transferring downloads block sleep.
- **Recommended Setting:** `100 KB/s`.

#### 4. Whole-PC CPU %
- **What it is:** Global system CPU load across all processes on your entire PC (`sys.global_cpu_usage()`).
- **Why it blocks:** Fallback safety net. If an unmonitored tool (like 7-Zip compressing a huge archive or video rendering) is running, sleep is blocked.
- **Recommended Setting:** `30.0%`.

#### 5. Whole-PC Disk I/O
- **What it is:** Sum of all disk read and write activity across all drives in KB/s.
- **Why it blocks:** Prevents sleep during active file copy, disk indexing, database operations, or builds.
- **Recommended Setting:** `5,000 KB/s` (5 MB/s). If background drives perform periodic indexing, increase to `10,000 KB/s`–`15,000 KB/s`.

#### 6. Whole-PC Network
- **What it is:** Sum of all network interface traffic (received + transmitted) across all adapters in KB/s.
- **Why it blocks:** Prevents sleep while large uploads, downloads, or cloud backups are actively transferring.
- **Recommended Setting:** `100 KB/s`.

#### 7. Sustained Spike Filter
- **What it is:** Consecutive seconds Whole-PC CPU, Disk, or Network must **remain continuously above threshold** before resetting the sleep countdown.
- **Why it matters:** Windows constantly has 1-second blips (e.g., Microsoft Edge loading a tab, Windows Search indexing a small file, Wallpaper Engine cycling wallpapers). Without this filter, those 1-second blips would reset your sleep timer indefinitely.
- **Recommended Setting:** `15s`.

#### 8. Monitor Timeout
- **What it is:** Syncs the Windows AC screen turn-off timeout with your sleep timer (`powercfg /change monitor-timeout-ac X`).
- **Recommended Setting:** Set to turn off 1–2 minutes before sleep mode kicks in (e.g. `2 min` for a 3 min sleep timer).

#### 9. Power Requests & Audio Override
- **What it is:** Monitors `powercfg /requests` for active `DISPLAY:` locks (e.g., watching a fullscreen YouTube video or VLC movie).
- **Audio/Browser Override:** The **`[ 🔇 Override Audio/Browser Locks ]`** button tells Windows to ignore driver locks from virtual audio cables (VB-Audio, FxSound) or idle browser media threads so they cannot keep your PC awake.
- **Recommended Setting:** Checked `[x]` with `Respect DISPLAY Requests` enabled.

---

## 📦 Multi-Platform Support & Downloads

Agent Power Guard runs natively across **Windows**, **Linux**, and **macOS**:

### Which File Should I Download? (Extension Guide)

| Operating System | Recommended Download | Extension | Target Architecture | Description |
|---|---|---|---|---|
| **Windows 10 / 11** | `AgentPowerGuard-windows-x64.msi` | `.msi` | x86_64 (64-bit) | **Recommended for Windows.** Installs into Program Files with Start Menu, Desktop shortcuts, and Add/Remove Programs support. |
| **Windows (Portable)** | `agent-power-guard-windows-x64.exe` | `.exe` | x86_64 (64-bit) | Portable standalone binary. Runs directly without installation. |
| **Linux (Ubuntu / Debian / Fedora / Arch)** | `agent-power-guard-linux-x86_64.tar.gz` | `.tar.gz` | x86_64 (64-bit) | Standalone Linux executable using X11 / Wayland and systemd suspend. |
| **macOS (Apple Silicon)** | `agent-power-guard-macos-aarch64.tar.gz` | `.tar.gz` | ARM64 (M1 / M2 / M3 / M4) | Native Apple Silicon binary. Uses `pmset` and `ioreg` for sleep and display control. |
| **macOS (Intel)** | `agent-power-guard-macos-x86_64.tar.gz` | `.tar.gz` | x86_64 (Intel) | Native Intel Mac binary. |

---

## 🐧 Linux Setup & Prerequisites

On Linux, Agent Power Guard uses:
- **Sleep Trigger:** `systemctl suspend` (or `loginctl suspend` / DBus)
- **Human Inactivity Idle:** `xprintidle` (for X11 desktop sessions)
- **Display Off:** `xset dpms` (X11) or DBus power manager
- **Autostart:** `~/.config/autostart/agent-power-guard.desktop`

### Prerequisites (Ubuntu / Debian / Mint):
```bash
sudo apt update
sudo apt install -y libgl1-mesa-dev libx11-dev libxext-dev libxcursor-dev libxinerama-dev libxi-dev xprintidle
```

### Running on Linux:
```bash
tar -xzvf agent-power-guard-linux-x86_64.tar.gz
cd agent-power-guard
./agent-power-guard
```

---

## 🍎 macOS Setup & Notes

On macOS, Agent Power Guard uses:
- **Sleep Trigger:** `pmset sleepnow`
- **Human Inactivity Idle:** `ioreg -c IOHIDSystem` (reads `HIDIdleTime` in nanoseconds)
- **Display Off:** `pmset -a displaysleep <minutes>`
- **Autostart:** `~/Library/LaunchAgents/com.agentpowerguard.app.plist`

### Running on macOS:
```bash
tar -xzvf agent-power-guard-macos-aarch64.tar.gz
./agent-power-guard
```
*(If prompted by macOS Gatekeeper on first run, go to System Settings -> Privacy & Security -> click "Open Anyway".)*

---

## 🤖 Automated GitHub Actions CI/CD Bot

This repository includes fully automated GitHub Actions workflows:
- **`.github/workflows/ci.yml`:** Automatically tests and compiles every pull request and push across Windows, Linux (`ubuntu-latest`), and macOS (`macos-latest`).
- **`.github/workflows/release.yml`:** When a tag like `v1.0.0` is pushed, GitHub Actions compiles:
  1. Windows `.exe` and `.msi` (with WiX toolset)
  2. Linux `.tar.gz`
  3. macOS Apple Silicon & Intel `.tar.gz`
  4. Computes `SHA256SUMS.txt` checksums
  5. Publishes a complete GitHub Release page with release notes and the download extension guide!

---

## 📄 License
This project is licensed under the [MIT License](LICENSE).
