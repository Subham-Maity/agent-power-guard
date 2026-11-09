# Agent Power Guard — Gemini Implementation Plan

> [!IMPORTANT]
> This plan is written for **Gemini** to implement from scratch. Read all sections before writing a single line of code. The previous Claude attempt produced code with **17 compile errors** — all documented below so you don't repeat them.

---

## 1. What We're Building

A **100% native Rust + egui Windows desktop app** that automatically puts the PC to sleep only when the user AND their AI coding agents are truly idle. No Tauri, No Electron, no WebView.

### Key Files to Produce
| File | Purpose |
|------|---------|
| `Cargo.toml` | Exact pinned dependency versions |
| `build.rs` | UAC `RequireAdministrator` manifest embedding |
| `src/main.rs` | Complete application (single-file OK) |

---

## 2. Exact Crate Versions (DO NOT CHANGE)

Verified against crates.io on 2026-11-09:

```toml
[dependencies]
eframe       = { version = "0.37.0", default-features = false, features = ["glow", "default_fonts"] }
egui_plot    = "0.38.0"
sysinfo      = { version = "0.40.1", default-features = false, features = ["system", "disk", "network"] }
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
windows-sys  = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_System_Power",
    "Win32_System_ProcessStatus",
    "Win32_System_SystemInformation",
    "Win32_System_Threading",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_UI_WindowsAndMessaging",
] }

[build-dependencies]
embed-manifest = "1.5.1"

[profile.release]
opt-level     = 3
lto           = "fat"
codegen-units = 1
panic         = "abort"
strip         = true
```

> NOTE: `egui_plot = "0.38.0"` requires `egui = "^0.37.0"` — this version mismatch is intentional and confirmed working. Do NOT "fix" it.

---

## 3. Known API Pitfalls from Claude's Failed Attempt (MUST READ)

These are the **exact 17 compile errors** Claude got. Fix them preemptively:

### 3.1 `sysinfo::System::new()` returns `Result<Self, Error>` in 0.40.x

```rust
// WRONG (Claude's mistake)
let mut sys = sysinfo::System::new();
sys.refresh_cpu_usage();

// CORRECT
let mut sys = sysinfo::System::new().expect("sysinfo init failed");
sys.refresh_cpu_usage();
```

**All** method calls on `sys` will fail with `no method found for enum Result<T,E>` if you forget `.expect()` or `.unwrap()`.

### 3.2 `SetSuspendState` takes `bool`, not integer

```rust
// WRONG (Claude's mistake)
SetSuspendState(0, 0, 0)   // arguments to this function are incorrect

// CORRECT (windows-sys 0.61.2)
SetSuspendState(false, false, false)
// Return type is `bool`, not BOOL integer
let ok: bool = SetSuspendState(false, false, false);
if !ok { /* failed */ }
```

### 3.3 `Disks` has no `.list()` method in sysinfo 0.40.x

```rust
// WRONG (Claude's mistake)
disks.list()   // no method named `list` found for struct `Disks`

// CORRECT — iterate directly
for disk in &disks {
    let usage = disk.usage();
}
// Or: use sysinfo::Disks::new_with_refreshed_list_specifics(refreshes)
```

### 3.4 `ViewportBuilder` has no `.with_minimized()` in egui 0.37

```rust
// WRONG (Claude's mistake)
viewport.with_minimized(false)   // no method named `with_minimized`

// CORRECT — just omit it
egui::ViewportBuilder::default()
    .with_title("Agent Power Guard")
    .with_min_inner_size([900.0, 600.0])
    .with_inner_size([1200.0, 750.0])
```

### 3.5 `OsString` has no `.contains()` method

```rust
// WRONG
process.name().contains("node")   // OsStr has no contains()

// CORRECT — convert first
process.name().to_string_lossy().to_lowercase().contains("node")
```

### 3.6 eframe 0.37 App trait: `ui()` not `update()`

```rust
// WRONG — older eframe API
impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) { ... }
}

// CORRECT — eframe 0.37 split the trait
impl eframe::App for MyApp {
    // Called even when window is hidden (use for logic, NOT painting):
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) { ... }

    // Called only when visible (use for all UI rendering):
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) { ... }
}
```

### 3.7 Panel API in egui 0.37

```rust
// CORRECT panel layout (use unified Panel API)
egui::Panel::top("top_bar").resizable(false).show(ui, |ui| { ... });
egui::Panel::left("conditions").default_size(380.0).show(ui, |ui| { ... });
egui::CentralPanel::default_margins().show(ui, |ui| { ... });
```

### 3.8 egui_plot 0.38 Line API

```rust
// Line constructor: Line::new(name, series)  — name is first, then PlotPoints
use egui_plot::{Line, PlotPoints, HLine, Plot};

let points: PlotPoints = data_vec.iter().enumerate()
    .map(|(i, &v)| [i as f64, v as f64])
    .collect::<Vec<[f64; 2]>>()
    .into();

Plot::new("cpu_plot")
    .height(100.0)
    .allow_drag(false)
    .allow_zoom(false)
    .show(ui, |plot_ui| {
        plot_ui.line(Line::new("CPU", points).color(egui::Color32::GREEN));
        plot_ui.hline(HLine::new("threshold", threshold_value)
            .color(egui::Color32::RED)
            .style(egui_plot::LineStyle::dashed_loose()));
    });
```

### 3.9 `ProcessRefreshKind` constructor chain

```rust
// CORRECT for sysinfo 0.40.x
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};

sys.refresh_processes_specifics(
    ProcessesToUpdate::All,
    true,  // remove_dead_processes
    ProcessRefreshKind::nothing().with_cpu().with_disk_usage(),
);
```

---

## 4. Architecture: 4 Must-Have Windows Requirements

### Req 1 — Decoupled Background Watchdog Thread (Critical)

`eframe` **stops calling `App::ui()`** when the window is minimized or the monitor turns off. The sleep timer MUST NOT live in the UI thread.

```
Main Thread (eframe/egui):
  └─ draws UI, reads Arc<Mutex<SharedState>>

Background Thread (std::thread::spawn):
  └─ 1-second loop
  └─ polls sysinfo CPU/disk/net/processes
  └─ polls Win32 GetLastInputInfo (idle time)
  └─ polls Win32 PowerGetEffectivePowerMode (power requests)
  └─ evaluates all 9 conditions
  └─ decrements sleep countdown
  └─ calls SetSuspendState when countdown hits 0
  └─ writes updated SharedState
  └─ calls ctx.request_repaint() to wake GUI
```

### Req 2 — Low Background RAM (`glow` + `K32EmptyWorkingSet` + 1 Hz repaint)

```rust
// In logic() or ui() — throttle repaints to 1/second
ctx.request_repaint_after(std::time::Duration::from_millis(1000));

// When minimized, flush GPU buffers from physical RAM
if ctx.input(|i| i.viewport().minimized == Some(true)) {
    unsafe {
        let proc = GetCurrentProcess();
        K32EmptyWorkingSet(proc);
    }
}
```

### Req 3 — Zero-Allocation sysinfo Polling

Create **one** `System`, **one** `Networks`, and **one** `Disks` instance on the background thread. Reuse them. Never call `refresh_all()`.

```rust
let mut sys  = sysinfo::System::new().expect("sysinfo");
let mut nets = sysinfo::Networks::new_with_refreshed_list().expect("nets");
let mut disks = sysinfo::Disks::new_with_refreshed_list_specifics(
    sysinfo::DiskRefreshKind::nothing().with_io_usage()
).expect("disks");

loop {
    std::thread::sleep(Duration::from_secs(1));
    sys.refresh_cpu_usage();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All, true,
        ProcessRefreshKind::nothing().with_cpu().with_disk_usage()
    );
    nets.refresh(false);
    disks.refresh_specifics(false, DiskRefreshKind::nothing().with_io_usage());
    // ... evaluate conditions, update SharedState
}
```

### Req 4 — `SeShutdownPrivilege` + `SetSuspendState`

```rust
unsafe fn sleep_now() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    if OpenProcessToken(
        GetCurrentProcess(),
        TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
        &mut token
    ) == 0 { return false; }

    let mut luid = LUID { LowPart: 0, HighPart: 0 };
    let name: Vec<u16> = "SeShutdownPrivilege\0".encode_utf16().collect();
    LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid);

    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
    CloseHandle(token);

    // bool args (not integer 0!)
    SetSuspendState(false, false, false)
}
```

---

## 5. SharedState Struct

```rust
#[derive(Default)]
struct SharedState {
    // Guard toggle
    guard_enabled: bool,

    // Countdown
    countdown_secs: u32,
    countdown_max: u32,

    // Live readings (updated by background thread every 1s)
    idle_secs: u32,
    agent_cpu_pct: f32,
    agent_top_process: String,
    pc_cpu_pct: f32,
    pc_disk_kbps: f32,
    pc_net_kbps: f32,
    download_kbps: f32,  // IDMan.exe specific

    // 120-point rolling history for graphs
    history_agent_cpu: VecDeque<f32>,   // max 120
    history_pc_cpu:    VecDeque<f32>,
    history_disk:      VecDeque<f32>,
    history_net:       VecDeque<f32>,

    // Current blocker description
    blocker_msg: String,

    // Config (also persisted to config.json)
    config: AppConfig,

    // Status
    last_wake_wall_secs: u64,  // for detecting sleep/wake (wall clock jump > 5s)
}

#[derive(Serialize, Deserialize, Clone)]
struct AppConfig {
    idle_threshold_secs:     u32,    // 10–1800, default 180
    agent_cpu_threshold:     f32,    // 0.5–50.0, default 2.0
    agent_keywords:          String, // comma-separated
    download_threshold_kbps: f32,   // 50–10000, default 100
    pc_cpu_threshold:        f32,    // 5–90, default 30
    pc_disk_threshold_kbps:  f32,   // 100–50000, default 5000
    pc_net_threshold_kbps:   f32,   // 10–10000, default 100
    spike_filter_secs:       u32,    // 1–60, default 15
    monitor_timeout_mins:    u32,    // 1–30, default 3
    respect_display_requests: bool,
    dark_mode:               bool,
    startup_enabled:         bool,
    // per-condition enable flags
    cond1_enabled: bool,  // idle time
    cond2_enabled: bool,  // agent cpu
    cond3_enabled: bool,  // download
    cond4_enabled: bool,  // pc cpu
    cond5_enabled: bool,  // pc disk
    cond6_enabled: bool,  // pc net
    cond7_enabled: bool,  // spike filter
    cond8_enabled: bool,  // monitor timeout
    cond9_enabled: bool,  // power requests
}
```

---

## 6. UI Layout

```
+------------------------------------------------------------------------------+
|  TOP BAR (Panel::top)                                                        |
|  [Guard: ON/OFF]  [████░░ Sleeping in 142s]  [Test Sleep] [15s Test]        |
|  [Apply Power Plan]  [Remove Old Task]  [Dark/Light]                         |
+---------------------------+--------------------------------------------------+
|  LEFT PANEL               |  RIGHT PANEL (CentralPanel)                      |
|  Condition 1: Idle        |  +-- Agent CPU % ---------------------------+    |
|  [x] 180s slider          |  |  ~~~graph~~~  - - threshold - -          |    |
|  [PASS: idle 245s]        |  +------------------------------------------+    |
|                           |  +-- Whole-PC CPU % ------------------------+    |
|  Condition 2: Agent CPU   |  |  ~~~graph~~~                             |    |
|  [x] 2.0% slider          |  +------------------------------------------+    |
|  [BLOCKING: node 4.8%]    |  +-- Disk KB/s ----------------------------+    |
|  Keywords: [textbox]      |  |  ~~~graph~~~                             |    |
|                           |  +------------------------------------------+    |
|  Condition 3: Download    |  +-- Network KB/s -------------------------+    |
|  ...                      |  |  ~~~graph~~~                             |    |
|                           |  +------------------------------------------+    |
|  Condition 4-9...         |                                                  |
|                           |  +-- BLOCKER INSPECTOR -------------------+     |
|                           |  | BLOCKED BY: Agent CPU (node.exe 4.8%) |     |
|                           |  +---------------------------------------+      |
+---------------------------+--------------------------------------------------+
|  [Run at Windows Startup: ON/OFF]                                            |
+------------------------------------------------------------------------------+
```

---

## 7. Build Script (`build.rs`)

```rust
use embed_manifest::manifest::ExecutionLevel;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(
            new_manifest("AgentPowerGuard.App")
                .requested_execution_level(ExecutionLevel::RequireAdministrator),
        )
        .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
```

---

## 8. Config Persistence

- Path: `%APPDATA%\AgentPowerGuard\config.json`
- Save: on every slider/checkbox change (debounced 500ms)
- Load: at startup; fall back to `AppConfig::default()` if missing/corrupt

---

## 9. Task Scheduler Integration

### Register startup task (PowerShell, hidden):

```rust
fn register_startup_task(exe_path: &str) {
    std::process::Command::new("powershell")
        .args(["-WindowStyle", "Hidden", "-Command",
            &format!(r#"Register-ScheduledTask -Force -TaskName "AgentPowerGuard" \
                -Action (New-ScheduledTaskAction -Execute "{exe_path}") \
                -Trigger (New-ScheduledTaskTrigger -AtLogOn) \
                -RunLevel Highest -Settings (New-ScheduledTaskSettingsSet -ExecutionTimeLimit 0)"#)
        ])
        .creation_flags(0x08000000)  // CREATE_NO_WINDOW
        .spawn().ok();
}

fn unregister_startup_task() {
    std::process::Command::new("powershell")
        .args(["-WindowStyle", "Hidden", "-Command",
            r#"Unregister-ScheduledTask -TaskName "AgentPowerGuard" -Confirm:$false 2>$null; \
               Remove-Item -Recurse -Force "C:\ProgramData\AgentPowerGuard" 2>$null"#
        ])
        .creation_flags(0x08000000)
        .spawn().ok();
}
```

---

## 10. `powercfg` Actions (Apply Optimal Power Plan button)

Runs `powercfg` with `CREATE_NO_WINDOW` flag:

- CPU Minimum State = 5%
- CPU Maximum State = 100%
- Monitor timeout = slider value (minutes)
- Standby timeout (AC) = 0 (disabled — app controls sleep)
- Apply active scheme

---

## 11. Power Requests Override (Condition 9)

```rust
fn override_noisy_power_locks() {
    let overrides = [
        ("PROCESS", "msedge.exe"),
        ("PROCESS", "IDMan.exe"),
        ("DRIVER",  "Legacy Kernel Caller"),
    ];
    for (kind, name) in &overrides {
        std::process::Command::new("powercfg")
            .args(["/requestsoverride", kind, name, "DISPLAY", "SYSTEM"])
            .creation_flags(0x08000000)
            .spawn().ok();
    }
}
```

---

## 12. Sleep/Wake Detection (Grace Period Reset)

After waking from sleep, the wall clock jumps forward. Detect in background thread:

```rust
let now = SystemTime::now()
    .duration_since(UNIX_EPOCH).unwrap().as_secs();
let last = state.last_wake_wall_secs;
if now.saturating_sub(last) > 5 && last != 0 {
    // Woke from sleep — reset countdown to full
    state.countdown_secs = state.config.idle_threshold_secs;
}
state.last_wake_wall_secs = now;
```

---

## 13. Condition Logic Summary

All 9 conditions must PASS (or be disabled) for the countdown to tick down. Any single BLOCKING condition resets the countdown to `idle_threshold_secs`.

| # | Condition | Blocking when... |
|---|-----------|-----------------|
| 1 | Human Input Idle | `GetLastInputInfo` idle < threshold |
| 2 | Agent CPU % | any matched process CPU >= threshold |
| 3 | IDMan.exe Download | IDMan.exe disk+net > threshold KB/s |
| 4 | Whole-PC CPU | `global_cpu_usage()` >= threshold |
| 5 | Whole-PC Disk | total disk read+write >= threshold KB/s |
| 6 | Whole-PC Network | total net recv+send >= threshold KB/s |
| 7 | Spike Filter | conditions 4–6 must be sustained for N seconds |
| 8 | Monitor Timeout | UI-only (applies powercfg, doesn't block sleep) |
| 9 | Power Requests | `powercfg /requests` has DISPLAY entry |

---

## 14. Implementation Checklist for Gemini

- [ ] Write `Cargo.toml` with exact versions from Section 2
- [ ] Write `build.rs` from Section 7
- [ ] Define `AppConfig` with `Serialize/Deserialize` and `Default`
- [ ] Define `SharedState` with `Arc<Mutex<SharedState>>`
- [ ] Implement `sleep_now()` with `SeShutdownPrivilege` (Section 4, using `bool` args)
- [ ] Spawn background watchdog thread (Section 4 Req 1)
  - [ ] `System::new().expect(...)` — not `System::new()` bare (Section 3.1)
  - [ ] `SetSuspendState(false, false, false)` — not integer 0 (Section 3.2)
  - [ ] Iterate `&disks` directly — no `.list()` (Section 3.3)
  - [ ] `.to_string_lossy()` on OsStr before `.contains()` (Section 3.5)
- [ ] Implement `eframe::App` with `logic()` + `ui()` (not `update()`) (Section 3.6)
  - [ ] `ctx.request_repaint_after(Duration::from_millis(1000))` in `logic()`
  - [ ] `K32EmptyWorkingSet` when minimized
- [ ] Build top bar panel (Section 6)
- [ ] Build left panel with 9 conditions, sliders, enable checkboxes, live badges
- [ ] Build right panel with 4 `egui_plot` graphs + blocker inspector
- [ ] Config load/save to `%APPDATA%\AgentPowerGuard\config.json`
- [ ] Task Scheduler register/unregister (Section 9)
- [ ] `Apply Optimal Power Plan` button (Section 10)
- [ ] Power requests override (Section 11)
- [ ] Sleep/wake wall-clock jump detection (Section 12)
- [ ] `cargo build --release --target x86_64-pc-windows-msvc` compiles with 0 errors

---

## 15. File Output Locations

```
C:\Github\agent-power-guard\
├── Cargo.toml
├── build.rs
├── src\
│   └── main.rs
└── (plan.md, progress.md — do not modify these)
```

> TIP: Run `cargo check --target x86_64-pc-windows-msvc` after writing all files. Fix any errors before marking done. The most likely errors are catalogued in Section 3 above.

