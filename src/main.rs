#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use eframe::egui;
use egui_plot::{HLine, Line, LineStyle, Plot, PlotPoints};
use serde::{Deserialize, Serialize};
use sysinfo::{
    DiskRefreshKind, Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System,
};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LUID};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES,
    SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
#[cfg(windows)]
use windows_sys::Win32::System::Power::SetSuspendState;
#[cfg(windows)]
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
#[cfg(windows)]
use windows_sys::Win32::System::SystemInformation::GetTickCount;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
#[cfg(windows)]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
const HISTORY_LEN: usize = 120;

// ----------------------------------------------------------------------------
// Configuration & Persistence
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub guard_enabled: bool,

    // Thresholds
    pub idle_threshold_secs: u32,       // 10..=1800, default 180
    pub agent_cpu_threshold: f32,       // 0.5..=50.0, default 2.0
    pub agent_keywords: String,         // comma-separated
    pub download_threshold_kbps: f32,   // 50..=10000, default 100
    pub download_keywords: String,      // comma-separated (e.g. "idman, aria2, qbittorrent")
    pub pc_cpu_threshold: f32,          // 5..=90, default 30
    pub pc_disk_threshold_kbps: f32,    // 100..=50000, default 5000
    pub pc_net_threshold_kbps: f32,     // 10..=10000, default 100
    pub spike_filter_secs: u32,         // 1..=60, default 15
    pub monitor_timeout_mins: u32,      // 1..=30, default 3
    pub respect_display_requests: bool, // default true
    pub dark_mode: bool,                // default true
    pub run_at_startup: bool,           // default false

    // Per-condition toggles
    pub cond1_idle_enabled: bool,
    pub cond2_agent_cpu_enabled: bool,
    pub cond3_download_enabled: bool,
    pub cond4_pc_cpu_enabled: bool,
    pub cond5_pc_disk_enabled: bool,
    pub cond6_pc_net_enabled: bool,
    pub cond7_spike_filter_enabled: bool,
    pub cond8_monitor_timeout_enabled: bool,
    pub cond9_power_requests_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            guard_enabled: true,
            idle_threshold_secs: 180,
            agent_cpu_threshold: 2.0,
            agent_keywords: "antigravity, language_server, cursor, claude, gemini, code, node, npm, pnpm, bun, tsc, esbuild, vite, next, nest, git, cargo, rustc, python, docker, wsl".to_string(),
            download_threshold_kbps: 100.0,
            download_keywords: "idman, aria2, qbittorrent, fdm".to_string(),
            pc_cpu_threshold: 30.0,
            pc_disk_threshold_kbps: 5000.0,
            pc_net_threshold_kbps: 100.0,
            spike_filter_secs: 15,
            monitor_timeout_mins: 3,
            respect_display_requests: true,
            dark_mode: true,
            run_at_startup: false,

            cond1_idle_enabled: true,
            cond2_agent_cpu_enabled: true,
            cond3_download_enabled: true,
            cond4_pc_cpu_enabled: true,
            cond5_pc_disk_enabled: true,
            cond6_pc_net_enabled: true,
            cond7_spike_filter_enabled: true,
            cond8_monitor_timeout_enabled: true,
            cond9_power_requests_enabled: true,
        }
    }
}

fn get_config_path() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let dir = PathBuf::from(appdata).join("AgentPowerGuard");
            let _ = std::fs::create_dir_all(&dir);
            return dir.join("config.json");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let dir = PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("AgentPowerGuard");
            let _ = std::fs::create_dir_all(&dir);
            return dir.join("config.json");
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let dir = PathBuf::from(xdg).join("agent-power-guard");
            let _ = std::fs::create_dir_all(&dir);
            return dir.join("config.json");
        } else if let Ok(home) = std::env::var("HOME") {
            let dir = PathBuf::from(home).join(".config").join("agent-power-guard");
            let _ = std::fs::create_dir_all(&dir);
            return dir.join("config.json");
        }
    }

    PathBuf::from("config.json")
}

impl AppConfig {
    pub fn load() -> Self {
        let path = get_config_path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<AppConfig>(&content) {
                return cfg;
            }
        }
        let default_cfg = Self::default();
        let _ = default_cfg.save();
        default_cfg
    }

    pub fn save(&self) -> bool {
        let path = get_config_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            return std::fs::write(path, json).is_ok();
        }
        false
    }
}

// ----------------------------------------------------------------------------
// Shared State & Condition Evaluation
// ----------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct ConditionStatus {
    pub enabled: bool,
    pub is_passing: bool,
    pub value_text: String,
    pub threshold_text: String,
    pub blocker_detail: Option<String>,
}

pub struct SharedState {
    pub config: AppConfig,
    pub config_dirty: bool,

    // Sleep countdown
    pub countdown_secs: u32,
    pub countdown_max: u32,

    // Real-time metrics
    pub live_idle_secs: u32,
    pub live_agent_cpu_pct: f32,
    pub live_agent_process_name: String,
    pub live_download_kbps: f32,
    pub live_download_process_name: String,
    pub live_pc_cpu_pct: f32,
    pub live_pc_disk_kbps: f32,
    pub live_pc_net_kbps: f32,
    pub live_power_display_active: bool,

    // Conditions
    pub cond1_idle: ConditionStatus,
    pub cond2_agent_cpu: ConditionStatus,
    pub cond3_download: ConditionStatus,
    pub cond4_pc_cpu: ConditionStatus,
    pub cond5_pc_disk: ConditionStatus,
    pub cond6_pc_net: ConditionStatus,
    pub cond7_spike_filter: ConditionStatus,
    pub cond8_monitor_timeout: ConditionStatus,
    pub cond9_power_requests: ConditionStatus,

    // Graphs history (120 seconds rolling)
    pub history_agent_cpu: VecDeque<f32>,
    pub history_pc_cpu: VecDeque<f32>,
    pub history_pc_disk: VecDeque<f32>,
    pub history_pc_net: VecDeque<f32>,

    // Blocker inspector banner
    pub blocker_banner_text: String,
    pub all_conditions_pass: bool,

    // Grace period & wall clock sleep detection
    pub last_wall_clock_secs: u64,
    pub pc_spike_sustained_counter: u32,

    // UI feedback toast/notification
    pub toast_message: Option<(String, Instant)>,
}

impl SharedState {
    pub fn new(config: AppConfig) -> Self {
        let max_cd = config.idle_threshold_secs;
        let mut s = Self {
            config,
            config_dirty: false,
            countdown_secs: max_cd,
            countdown_max: max_cd,

            live_idle_secs: 0,
            live_agent_cpu_pct: 0.0,
            live_agent_process_name: "None".to_string(),
            live_download_kbps: 0.0,
            live_download_process_name: "None".to_string(),
            live_pc_cpu_pct: 0.0,
            live_pc_disk_kbps: 0.0,
            live_pc_net_kbps: 0.0,
            live_power_display_active: false,

            cond1_idle: ConditionStatus::default(),
            cond2_agent_cpu: ConditionStatus::default(),
            cond3_download: ConditionStatus::default(),
            cond4_pc_cpu: ConditionStatus::default(),
            cond5_pc_disk: ConditionStatus::default(),
            cond6_pc_net: ConditionStatus::default(),
            cond7_spike_filter: ConditionStatus::default(),
            cond8_monitor_timeout: ConditionStatus::default(),
            cond9_power_requests: ConditionStatus::default(),

            history_agent_cpu: VecDeque::with_capacity(HISTORY_LEN),
            history_pc_cpu: VecDeque::with_capacity(HISTORY_LEN),
            history_pc_disk: VecDeque::with_capacity(HISTORY_LEN),
            history_pc_net: VecDeque::with_capacity(HISTORY_LEN),

            blocker_banner_text: "Initializing watchdog...".to_string(),
            all_conditions_pass: false,

            last_wall_clock_secs: 0,
            pc_spike_sustained_counter: 0,
            toast_message: None,
        };

        for _ in 0..HISTORY_LEN {
            s.history_agent_cpu.push_back(0.0);
            s.history_pc_cpu.push_back(0.0);
            s.history_pc_disk.push_back(0.0);
            s.history_pc_net.push_back(0.0);
        }

        s
    }

    pub fn set_toast(&mut self, text: impl Into<String>) {
        self.toast_message = Some((text.into(), Instant::now()));
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Sleep
// ----------------------------------------------------------------------------

pub fn sleep_now() -> bool {
    #[cfg(windows)]
    {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            ) == 0
            {
                return false;
            }

            let mut luid = LUID { LowPart: 0, HighPart: 0 };
            let priv_name: Vec<u16> = "SeShutdownPrivilege\0".encode_utf16().collect();
            if LookupPrivilegeValueW(std::ptr::null(), priv_name.as_ptr(), &mut luid) == 0 {
                CloseHandle(token);
                return false;
            }

            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            AdjustTokenPrivileges(
                token,
                0,
                &tp,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            CloseHandle(token);

            SetSuspendState(false, false, false)
        }
    }

    #[cfg(target_os = "linux")]
    {
        if Command::new("systemctl").arg("suspend").status().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
        if Command::new("loginctl").arg("suspend").status().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
        Command::new("dbus-send")
            .args(["--system", "--print-reply", "--dest=org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager.Suspend", "boolean:true"])
            .status().map(|s| s.success()).unwrap_or(false)
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("pmset")
            .arg("sleepnow")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Idle Input Seconds
// ----------------------------------------------------------------------------

pub fn get_idle_seconds() -> u32 {
    #[cfg(windows)]
    {
        unsafe {
            let mut lii = LASTINPUTINFO {
                cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                dwTime: 0,
            };
            if GetLastInputInfo(&mut lii) != 0 {
                let ticks = GetTickCount();
                let idle_ms = ticks.wrapping_sub(lii.dwTime);
                idle_ms / 1000
            } else {
                0
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("ioreg").args(["-c", "IOHIDSystem"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                if line.contains("\"HIDIdleTime\"") {
                    if let Some(val_str) = line.split('=').nth(1) {
                        if let Ok(nanos) = val_str.trim().parse::<u64>() {
                            return (nanos / 1_000_000_000) as u32;
                        }
                    }
                }
            }
        }
        0
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("xprintidle").output() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Ok(ms) = text.trim().parse::<u32>() {
                return ms / 1000;
            }
        }
        0
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        0
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Memory Flush
// ----------------------------------------------------------------------------

pub fn flush_working_set() {
    #[cfg(windows)]
    {
        unsafe {
            let proc = GetCurrentProcess();
            K32EmptyWorkingSet(proc);
        }
    }

    #[cfg(not(windows))]
    {
        // Linux and macOS manage inactive app memory pages natively
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Power Plan
// ----------------------------------------------------------------------------

pub fn apply_optimal_power_plan(monitor_timeout_mins: u32) {
    #[cfg(windows)]
    {
        let mon_arg = monitor_timeout_mins.to_string();
        let commands = [
            vec!["/change", "monitor-timeout-ac", &mon_arg],
            vec!["/setacvalueindex", "SCHEME_CURRENT", "54533251-82be-4824-96c1-47b60b740d00", "893dee8e-2bef-41e0-89c6-b55d0929964c", "5"],
            vec!["/setacvalueindex", "SCHEME_CURRENT", "54533251-82be-4824-96c1-47b60b740d00", "bc5038f7-23e0-4960-96da-33abaf5935ec", "100"],
            vec!["/setacvalueindex", "SCHEME_CURRENT", "501a4d13-42af-4429-9e86-e28c4331ba0f", "ee130207-d030-4e61-ab30-e30f57ed0c4e", "1"],
            vec!["/setacvalueindex", "SCHEME_CURRENT", "238c9fa8-0aad-41ed-b199-9e4920d77830", "bd3b718a-0680-4d9d-8ab2-e1d2b4f0800d", "0"],
            vec!["/change", "standby-timeout-ac", "0"],
            vec!["/setactive", "SCHEME_CURRENT"],
        ];

        for args in &commands {
            let mut cmd = Command::new("powercfg");
            cmd.args(args);
            cmd.creation_flags(CREATE_NO_WINDOW);
            let _ = cmd.spawn();
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = monitor_timeout_mins;
        let _ = Command::new("powerprofilesctl").args(["set", "balanced"]).spawn();
    }

    #[cfg(target_os = "macos")]
    {
        let s = monitor_timeout_mins.to_string();
        let _ = Command::new("pmset").args(["-a", "displaysleep", &s]).spawn();
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Monitor Timeout
// ----------------------------------------------------------------------------

pub fn apply_monitor_timeout(mins: u32) {
    #[cfg(windows)]
    {
        let s = mins.to_string();
        let mut cmd = Command::new("powercfg");
        cmd.args(["/change", "monitor-timeout-ac", &s]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.spawn();
    }

    #[cfg(target_os = "linux")]
    {
        let secs = (mins * 60).to_string();
        let _ = Command::new("xset").args(["dpms", &secs, &secs, &secs]).spawn();
    }

    #[cfg(target_os = "macos")]
    {
        let s = mins.to_string();
        let _ = Command::new("pmset").args(["-a", "displaysleep", &s]).spawn();
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Driver Audio Overrides
// ----------------------------------------------------------------------------

pub fn override_noisy_audio_drivers() {
    #[cfg(windows)]
    {
        let overrides = [
            ("DRIVER", "VB-Audio Virtual Cable"),
            ("DRIVER", "FxSound Audio Enhancer"),
            ("DRIVER", "USB Audio"),
            ("DRIVER", "Legacy Kernel Caller"),
            ("PROCESS", "msedge.exe"),
            ("PROCESS", "IDMan.exe"),
        ];

        for (cat, name) in &overrides {
            let mut cmd = Command::new("powercfg");
            cmd.args(["/requestsoverride", cat, name, "DISPLAY", "SYSTEM", "AWAYMODE"]);
            cmd.creation_flags(CREATE_NO_WINDOW);
            let _ = cmd.spawn();
        }
    }

    #[cfg(not(windows))]
    {
        // Linux / macOS audio subsystems do not hold indefinite powercfg sleep locks
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Legacy Cleanup
// ----------------------------------------------------------------------------

pub fn remove_old_v3_scheduled_task() {
    #[cfg(windows)]
    {
        let cmd = r#"Unregister-ScheduledTask -TaskName "AgentPowerGuard" -Confirm:$false 2>$null; Remove-Item -Recurse -Force "C:\ProgramData\AgentPowerGuard" 2>$null"#;
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", cmd]);
        c.creation_flags(CREATE_NO_WINDOW);
        let _ = c.spawn();
    }

    #[cfg(not(windows))]
    {
        // No legacy Windows scheduled task on Unix systems
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Autostart
// ----------------------------------------------------------------------------

pub fn set_startup_task(enable: bool) {
    #[cfg(windows)]
    {
        if enable {
            if let Ok(exe) = std::env::current_exe() {
                let path = exe.to_string_lossy().to_string();
                let cmd = format!(
                    r#"Register-ScheduledTask -Force -TaskName "AgentPowerGuard" -Action (New-ScheduledTaskAction -Execute "{path}") -Trigger (New-ScheduledTaskTrigger -AtLogOn) -RunLevel Highest -Settings (New-ScheduledTaskSettingsSet -ExecutionTimeLimit 0 -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries)"#
                );
                let mut c = Command::new("powershell");
                c.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &cmd]);
                c.creation_flags(CREATE_NO_WINDOW);
                let _ = c.spawn();
            }
        } else {
            let cmd = r#"Unregister-ScheduledTask -TaskName "AgentPowerGuard" -Confirm:$false 2>$null"#;
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", cmd]);
            c.creation_flags(CREATE_NO_WINDOW);
            let _ = c.spawn();
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let autostart_dir = PathBuf::from(home).join(".config").join("autostart");
            let desktop_file = autostart_dir.join("agent-power-guard.desktop");
            if enable {
                let _ = std::fs::create_dir_all(&autostart_dir);
                if let Ok(exe) = std::env::current_exe() {
                    let content = format!(
                        "[Desktop Entry]\nType=Application\nName=Agent Power Guard\nExec={}\nHidden=false\nNoDisplay=false\nX-GNOME-Autostart-enabled=true\n",
                        exe.display()
                    );
                    let _ = std::fs::write(desktop_file, content);
                }
            } else {
                let _ = std::fs::remove_file(desktop_file);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let launch_agents = PathBuf::from(home).join("Library").join("LaunchAgents");
            let plist_path = launch_agents.join("com.agentpowerguard.app.plist");
            if enable {
                let _ = std::fs::create_dir_all(&launch_agents);
                if let Ok(exe) = std::env::current_exe() {
                    let content = format!(
                        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.agentpowerguard.app</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>"#,
                        exe.display()
                    );
                    let _ = std::fs::write(plist_path, content);
                }
            } else {
                let _ = std::fs::remove_file(plist_path);
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Display Power Requests
// ----------------------------------------------------------------------------

pub fn check_power_requests_display() -> bool {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("powercfg");
        cmd.arg("/requests");
        cmd.creation_flags(CREATE_NO_WINDOW);
        if let Ok(out) = cmd.output() {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut in_display = false;
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.eq_ignore_ascii_case("DISPLAY:") {
                    in_display = true;
                    continue;
                }
                if in_display {
                    if trimmed.ends_with(':') {
                        break;
                    }
                    if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("None.") {
                        return true;
                    }
                }
            }
        }
        false
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new("pmset").args(["-g", "assertions"]).output() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if line.contains("PreventUserIdleDisplaySleep") && line.contains("1") {
                    return true;
                }
            }
        }
        false
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(out) = Command::new("systemd-inhibit").args(["--list", "--mode=block"]).output() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if line.contains("idle") || line.contains("sleep") {
                    return true;
                }
            }
        }
        false
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

// ----------------------------------------------------------------------------
// Background Watchdog Thread
// ----------------------------------------------------------------------------

fn start_watchdog_thread(state: Arc<Mutex<SharedState>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut sys = System::new().expect("sysinfo System::new");
        let mut nets = Networks::new_with_refreshed_list().expect("Networks::new");
        let mut disks = Disks::new_with_refreshed_list_specifics(
            DiskRefreshKind::nothing().with_io_usage(),
        ).expect("Disks::new");

        // Warm up CPU usage
        sys.refresh_cpu_usage();
        std::thread::sleep(Duration::from_millis(500));

        let mut check_requests_counter = 0u32;
        let mut cached_display_request = false;

        loop {
            let loop_start = Instant::now();

            // 1. Refresh System metrics (Zero-allocation refresh)
            sys.refresh_cpu_usage();
            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing().with_cpu().with_disk_usage(),
            );
            nets.refresh(false);
            disks.refresh_specifics(false, DiskRefreshKind::nothing().with_io_usage());

            let num_cpus = sys.cpus().len().max(1) as f32;
            let whole_pc_cpu = sys.global_cpu_usage();

            // Whole-PC Network KB/s
            let mut total_net_rx = 0u64;
            let mut total_net_tx = 0u64;
            for (_name, net) in &nets {
                total_net_rx = total_net_rx.saturating_add(net.received());
                total_net_tx = total_net_tx.saturating_add(net.transmitted());
            }
            let whole_pc_net_kbps = (total_net_rx + total_net_tx) as f32 / 1024.0;

            // Whole-PC Disk KB/s
            let mut total_disk_r = 0u64;
            let mut total_disk_w = 0u64;
            for disk in &disks {
                let u = disk.usage();
                total_disk_r = total_disk_r.saturating_add(u.read_bytes);
                total_disk_w = total_disk_w.saturating_add(u.written_bytes);
            }
            let whole_pc_disk_kbps = (total_disk_r + total_disk_w) as f32 / 1024.0;

            // Check Win32 power requests every 3 seconds to avoid process spawn overhead
            check_requests_counter = check_requests_counter.saturating_add(1);
            if check_requests_counter >= 3 {
                check_requests_counter = 0;
                cached_display_request = check_power_requests_display();
            }

            // Human input idle
            let idle_secs = get_idle_seconds();

            // 2. Lock state and evaluate
            let mut trigger_sleep = false;
            {
                let mut s = state.lock().unwrap();

                // Wall clock jump detection (> 5s jump indicates sleep/wake)
                let wall_now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                if s.last_wall_clock_secs > 0 {
                    let jump = wall_now.saturating_sub(s.last_wall_clock_secs);
                    if jump > 5 {
                        // Woke up from sleep!
                        s.countdown_secs = s.config.idle_threshold_secs;
                        s.pc_spike_sustained_counter = 0;
                        s.set_toast(format!("Woke from sleep (+{}s jump)! Countdown reset.", jump));
                    }
                }
                s.last_wall_clock_secs = wall_now;

                // Parse keyword filters
                let agent_keywords: Vec<String> = s
                    .config
                    .agent_keywords
                    .split(',')
                    .map(|k| k.trim().to_lowercase())
                    .filter(|k| !k.is_empty())
                    .collect();

                let download_keywords: Vec<String> = s
                    .config
                    .download_keywords
                    .split(',')
                    .map(|k| k.trim().to_lowercase())
                    .filter(|k| !k.is_empty())
                    .collect();

                // Inspect processes
                let mut max_agent_cpu = 0.0f32;
                let mut max_agent_proc = "None".to_string();
                let mut max_dl_kbps = 0.0f32;
                let mut max_dl_proc = "None".to_string();

                for (_pid, proc) in sys.processes() {
                    let proc_name = proc.name().to_string_lossy().to_lowercase();
                    let norm_cpu = proc.cpu_usage() / num_cpus;

                    // Match agent processes
                    for kw in &agent_keywords {
                        if proc_name.contains(kw) {
                            if norm_cpu > max_agent_cpu {
                                max_agent_cpu = norm_cpu;
                                max_agent_proc = proc.name().to_string_lossy().to_string();
                            }
                            break;
                        }
                    }

                    // Match download processes (IDMan.exe, etc.)
                    for kw in &download_keywords {
                        if proc_name.contains(kw) {
                            let du = proc.disk_usage();
                            let dl_kbps = (du.read_bytes + du.written_bytes) as f32 / 1024.0;
                            if dl_kbps > max_dl_kbps {
                                max_dl_kbps = dl_kbps;
                                max_dl_proc = proc.name().to_string_lossy().to_string();
                            }
                            break;
                        }
                    }
                }

                // Update live metrics
                s.live_idle_secs = idle_secs;
                s.live_agent_cpu_pct = max_agent_cpu;
                s.live_agent_process_name = max_agent_proc.clone();
                s.live_download_kbps = max_dl_kbps;
                s.live_download_process_name = max_dl_proc.clone();
                s.live_pc_cpu_pct = whole_pc_cpu;
                s.live_pc_disk_kbps = whole_pc_disk_kbps;
                s.live_pc_net_kbps = whole_pc_net_kbps;
                s.live_power_display_active = cached_display_request;

                // Update 120s rolling history
                if s.history_agent_cpu.len() >= HISTORY_LEN {
                    s.history_agent_cpu.pop_front();
                }
                s.history_agent_cpu.push_back(max_agent_cpu);

                if s.history_pc_cpu.len() >= HISTORY_LEN {
                    s.history_pc_cpu.pop_front();
                }
                s.history_pc_cpu.push_back(whole_pc_cpu);

                if s.history_pc_disk.len() >= HISTORY_LEN {
                    s.history_pc_disk.pop_front();
                }
                s.history_pc_disk.push_back(whole_pc_disk_kbps);

                if s.history_pc_net.len() >= HISTORY_LEN {
                    s.history_pc_net.pop_front();
                }
                s.history_pc_net.push_back(whole_pc_net_kbps);

                // 3. Condition Evaluation
                let cfg = s.config.clone();
                let mut blockers = Vec::new();

                // Condition 1: Human Input Idle
                let cond1_pass = idle_secs >= cfg.idle_threshold_secs;
                s.cond1_idle = ConditionStatus {
                    enabled: cfg.cond1_idle_enabled,
                    is_passing: cond1_pass,
                    value_text: format!("{}s", idle_secs),
                    threshold_text: format!(">= {}s", cfg.idle_threshold_secs),
                    blocker_detail: if !cond1_pass && cfg.cond1_idle_enabled {
                        Some(format!("Human Activity (User active, idle only {}s < {}s)", idle_secs, cfg.idle_threshold_secs))
                    } else {
                        None
                    },
                };
                if cfg.cond1_idle_enabled && !cond1_pass {
                    blockers.push(format!("Human Idle ({}s < {}s)", idle_secs, cfg.idle_threshold_secs));
                }

                // Condition 2: Watched Agent CPU %
                let cond2_pass = max_agent_cpu < cfg.agent_cpu_threshold;
                s.cond2_agent_cpu = ConditionStatus {
                    enabled: cfg.cond2_agent_cpu_enabled,
                    is_passing: cond2_pass,
                    value_text: format!("{:.1}% ({})", max_agent_cpu, max_agent_proc),
                    threshold_text: format!("< {:.1}%", cfg.agent_cpu_threshold),
                    blocker_detail: if !cond2_pass && cfg.cond2_agent_cpu_enabled {
                        Some(format!("Agent CPU ({} at {:.1}% >= {:.1}%)", max_agent_proc, max_agent_cpu, cfg.agent_cpu_threshold))
                    } else {
                        None
                    },
                };
                if cfg.cond2_agent_cpu_enabled && !cond2_pass {
                    blockers.push(format!("Agent CPU ({} at {:.1}% >= {:.1}%)", max_agent_proc, max_agent_cpu, cfg.agent_cpu_threshold));
                }

                // Condition 3: Active Download / IDM
                let cond3_pass = max_dl_kbps < cfg.download_threshold_kbps;
                s.cond3_download = ConditionStatus {
                    enabled: cfg.cond3_download_enabled,
                    is_passing: cond3_pass,
                    value_text: format!("{:.0} KB/s ({})", max_dl_kbps, max_dl_proc),
                    threshold_text: format!("< {:.0} KB/s", cfg.download_threshold_kbps),
                    blocker_detail: if !cond3_pass && cfg.cond3_download_enabled {
                        Some(format!("Active Download ({} at {:.0} KB/s >= {:.0} KB/s)", max_dl_proc, max_dl_kbps, cfg.download_threshold_kbps))
                    } else {
                        None
                    },
                };
                if cfg.cond3_download_enabled && !cond3_pass {
                    blockers.push(format!("Download ({} at {:.0} KB/s)", max_dl_proc, max_dl_kbps));
                }

                // Condition 4, 5, 6: Whole-PC Metrics
                let cond4_pass = whole_pc_cpu < cfg.pc_cpu_threshold;
                s.cond4_pc_cpu = ConditionStatus {
                    enabled: cfg.cond4_pc_cpu_enabled,
                    is_passing: cond4_pass,
                    value_text: format!("{:.1}%", whole_pc_cpu),
                    threshold_text: format!("< {:.1}%", cfg.pc_cpu_threshold),
                    blocker_detail: if !cond4_pass && cfg.cond4_pc_cpu_enabled {
                        Some(format!("Whole-PC CPU ({:.1}% >= {:.1}%)", whole_pc_cpu, cfg.pc_cpu_threshold))
                    } else {
                        None
                    },
                };

                let cond5_pass = whole_pc_disk_kbps < cfg.pc_disk_threshold_kbps;
                s.cond5_pc_disk = ConditionStatus {
                    enabled: cfg.cond5_pc_disk_enabled,
                    is_passing: cond5_pass,
                    value_text: format!("{:.0} KB/s", whole_pc_disk_kbps),
                    threshold_text: format!("< {:.0} KB/s", cfg.pc_disk_threshold_kbps),
                    blocker_detail: if !cond5_pass && cfg.cond5_pc_disk_enabled {
                        Some(format!("Whole-PC Disk ({:.0} KB/s >= {:.0} KB/s)", whole_pc_disk_kbps, cfg.pc_disk_threshold_kbps))
                    } else {
                        None
                    },
                };

                let cond6_pass = whole_pc_net_kbps < cfg.pc_net_threshold_kbps;
                s.cond6_pc_net = ConditionStatus {
                    enabled: cfg.cond6_pc_net_enabled,
                    is_passing: cond6_pass,
                    value_text: format!("{:.0} KB/s", whole_pc_net_kbps),
                    threshold_text: format!("< {:.0} KB/s", cfg.pc_net_threshold_kbps),
                    blocker_detail: if !cond6_pass && cfg.cond6_pc_net_enabled {
                        Some(format!("Whole-PC Network ({:.0} KB/s >= {:.0} KB/s)", whole_pc_net_kbps, cfg.pc_net_threshold_kbps))
                    } else {
                        None
                    },
                };

                // Condition 7: Sustained Spike Filter for Whole-PC Metrics
                let any_pc_metric_tripped = (cfg.cond4_pc_cpu_enabled && !cond4_pass)
                    || (cfg.cond5_pc_disk_enabled && !cond5_pass)
                    || (cfg.cond6_pc_net_enabled && !cond6_pass);

                if any_pc_metric_tripped {
                    s.pc_spike_sustained_counter = s.pc_spike_sustained_counter.saturating_add(1);
                } else {
                    s.pc_spike_sustained_counter = 0;
                }

                let spike_sustained = s.pc_spike_sustained_counter >= cfg.spike_filter_secs;
                s.cond7_spike_filter = ConditionStatus {
                    enabled: cfg.cond7_spike_filter_enabled,
                    is_passing: !spike_sustained,
                    value_text: format!("{}s / {}s", s.pc_spike_sustained_counter, cfg.spike_filter_secs),
                    threshold_text: format!("Filter: {}s", cfg.spike_filter_secs),
                    blocker_detail: if spike_sustained && cfg.cond7_spike_filter_enabled {
                        Some(format!("Sustained Whole-PC Spike (Active for {}s >= {}s)", s.pc_spike_sustained_counter, cfg.spike_filter_secs))
                    } else {
                        None
                    },
                };

                if cfg.cond7_spike_filter_enabled {
                    if any_pc_metric_tripped && spike_sustained {
                        if cfg.cond4_pc_cpu_enabled && !cond4_pass {
                            blockers.push(format!("Whole-PC CPU ({:.1}%)", whole_pc_cpu));
                        }
                        if cfg.cond5_pc_disk_enabled && !cond5_pass {
                            blockers.push(format!("Whole-PC Disk ({:.0} KB/s)", whole_pc_disk_kbps));
                        }
                        if cfg.cond6_pc_net_enabled && !cond6_pass {
                            blockers.push(format!("Whole-PC Network ({:.0} KB/s)", whole_pc_net_kbps));
                        }
                    }
                } else if any_pc_metric_tripped {
                    if cfg.cond4_pc_cpu_enabled && !cond4_pass {
                        blockers.push(format!("Whole-PC CPU ({:.1}%)", whole_pc_cpu));
                    }
                    if cfg.cond5_pc_disk_enabled && !cond5_pass {
                        blockers.push(format!("Whole-PC Disk ({:.0} KB/s)", whole_pc_disk_kbps));
                    }
                    if cfg.cond6_pc_net_enabled && !cond6_pass {
                        blockers.push(format!("Whole-PC Network ({:.0} KB/s)", whole_pc_net_kbps));
                    }
                }

                // Condition 8: Monitor Turn-Off Timeout (Informational / Action)
                s.cond8_monitor_timeout = ConditionStatus {
                    enabled: cfg.cond8_monitor_timeout_enabled,
                    is_passing: true,
                    value_text: format!("{}m", cfg.monitor_timeout_mins),
                    threshold_text: "Standby sync".to_string(),
                    blocker_detail: None,
                };

                // Condition 9: Windows Power Requests (DISPLAY lock)
                let cond9_pass = !cached_display_request;
                s.cond9_power_requests = ConditionStatus {
                    enabled: cfg.cond9_power_requests_enabled && cfg.respect_display_requests,
                    is_passing: cond9_pass,
                    value_text: if cached_display_request { "Active (Video)".to_string() } else { "None".to_string() },
                    threshold_text: "Respect DISPLAY".to_string(),
                    blocker_detail: if !cond9_pass && cfg.cond9_power_requests_enabled && cfg.respect_display_requests {
                        Some("Windows DISPLAY Request (Active video playback or media lock)".to_string())
                    } else {
                        None
                    },
                };
                if cfg.cond9_power_requests_enabled && cfg.respect_display_requests && cached_display_request {
                    blockers.push("DISPLAY Request (Video playback active)".to_string());
                }

                // Overall Blocker Evaluation
                s.countdown_max = cfg.idle_threshold_secs;

                if !cfg.guard_enabled {
                    s.all_conditions_pass = false;
                    s.countdown_secs = cfg.idle_threshold_secs;
                    s.blocker_banner_text = "PAUSED: Master Guard toggle is OFF. Automatic sleep disabled.".to_string();
                } else if blockers.is_empty() {
                    s.all_conditions_pass = true;
                    if s.countdown_secs > 0 {
                        s.countdown_secs -= 1;
                    }
                    s.blocker_banner_text = format!(
                        "ALL CONDITIONS PASS -> PC Sleeping in {}s / {}s",
                        s.countdown_secs, s.countdown_max
                    );

                    if s.countdown_secs == 0 {
                        trigger_sleep = true;
                        // Reset countdown for when we wake up
                        s.countdown_secs = s.countdown_max;
                    }
                } else {
                    s.all_conditions_pass = false;
                    s.countdown_secs = cfg.idle_threshold_secs;
                    let first = &blockers[0];
                    let extra = if blockers.len() > 1 {
                        format!(" (+{} other conditions)", blockers.len() - 1)
                    } else {
                        String::new()
                    };
                    s.blocker_banner_text = format!("BLOCKED BY: {}{}", first, extra);
                }

                // Auto-save configuration if dirty
                if s.config_dirty {
                    s.config_dirty = false;
                    s.config.save();
                }
            } // unlock state

            // Execute sleep outside of mutex lock
            if trigger_sleep {
                let ok = sleep_now();
                if !ok {
                    let mut s = state.lock().unwrap();
                    s.set_toast("Sleep trigger returned false (ensure Administrator elevation).");
                }
            }

            // Wake up GUI thread for 1 Hz redraw
            ctx.request_repaint();

            // Precise 1-second loop cadence
            let elapsed = loop_start.elapsed();
            if elapsed < Duration::from_millis(1000) {
                std::thread::sleep(Duration::from_millis(1000) - elapsed);
            }
        }
    });
}

// ----------------------------------------------------------------------------
// Eframe GUI Application
// ----------------------------------------------------------------------------

fn format_time_compact(secs: u32) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let mins = secs / 60;
        let rem = secs % 60;
        if rem == 0 {
            format!("{}m", mins)
        } else {
            format!("{}m {}s", mins, rem)
        }
    } else {
        let hrs = secs / 3600;
        let mins = (secs % 3600) / 60;
        if mins == 0 {
            format!("{}h", hrs)
        } else {
            format!("{}h {}m", hrs, mins)
        }
    }
}

pub struct AgentPowerGuardApp {
    state: Arc<Mutex<SharedState>>,
    was_minimized: bool,
}

impl AgentPowerGuardApp {
    pub fn new(cc: &eframe::CreationContext<'_>, state: Arc<Mutex<SharedState>>) -> Self {
        // Set up initial visuals
        let dark_mode = {
            let s = state.lock().unwrap();
            s.config.dark_mode
        };
        cc.egui_ctx.set_visuals(if dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });

        // Launch background watchdog thread
        start_watchdog_thread(Arc::clone(&state), cc.egui_ctx.clone());

        Self {
            state,
            was_minimized: false,
        }
    }
}

impl eframe::App for AgentPowerGuardApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Architectural requirement 2: Working Set flush when minimized
        let is_minimized = ctx.input(|i| i.viewport().minimized == Some(true));
        if is_minimized && !self.was_minimized {
            flush_working_set();
        }
        self.was_minimized = is_minimized;

        // 1 Hz repaint cadence when idle
        ctx.request_repaint_after(Duration::from_millis(1000));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut s = self.state.lock().unwrap();

        // Check if visuals need updating
        if ui.visuals().dark_mode != s.config.dark_mode {
            ui.ctx().set_visuals(if s.config.dark_mode {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
        }

        // ====================================================================
        // TOP CONTROL BAR
        // ====================================================================
        egui::Panel::top("top_bar").resizable(false).show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // Master Guard Toggle
                let guard_on = s.config.guard_enabled;
                let toggle_text = if guard_on {
                    egui::RichText::new("GUARD: [ ON ]").color(egui::Color32::WHITE).strong()
                } else {
                    egui::RichText::new("GUARD: [ OFF ]").color(egui::Color32::LIGHT_GRAY).strong()
                };
                let toggle_btn = egui::Button::new(toggle_text).fill(if guard_on {
                    egui::Color32::from_rgb(34, 139, 34) // Forest Green
                } else {
                    egui::Color32::from_rgb(105, 105, 105) // Dim Gray
                });

                if ui.add(toggle_btn).clicked() {
                    s.config.guard_enabled = !s.config.guard_enabled;
                    s.config_dirty = true;
                    if s.config.guard_enabled {
                        s.set_toast("Guard Enabled");
                    } else {
                        s.set_toast("Guard Disabled");
                    }
                }

                ui.separator();

                // Live Countdown Progress Bar
                let fraction = if s.countdown_max > 0 {
                    (s.countdown_secs as f32 / s.countdown_max as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };

                let progress_label = if !s.config.guard_enabled {
                    "PAUSED".to_string()
                } else if s.all_conditions_pass {
                    format!("Time until sleep: {} / {}", format_time_compact(s.countdown_secs), format_time_compact(s.countdown_max))
                } else {
                    format!("RESET / BLOCKED ({})", format_time_compact(s.countdown_max))
                };

                let pbar = egui::ProgressBar::new(fraction)
                    .text(egui::RichText::new(progress_label).strong())
                    .animate(s.all_conditions_pass);

                ui.add_sized([ui.available_width() - 530.0, 24.0], pbar);

                ui.separator();

                // Quick Action & Test Buttons
                if ui.button("⚡ Test Sleep").clicked() {
                    s.set_toast("Executing sleep now via Win32 SetSuspendState...");
                    let ok = sleep_now();
                    if !ok {
                        s.set_toast("Sleep call failed. Ensure app is elevated as Administrator.");
                    }
                }

                if ui.button("⏱ 15s Test").clicked() {
                    s.config.idle_threshold_secs = 15;
                    s.countdown_secs = 15;
                    s.countdown_max = 15;
                    s.config_dirty = true;
                    s.set_toast("15-second quick test mode activated.");
                }

                if ui.button("⚙ Power Plan").clicked() {
                    apply_optimal_power_plan(s.config.monitor_timeout_mins);
                    s.set_toast("Optimal Windows Power Plan applied (standby=0, monitor=timeout, CPU 5-100%).");
                }

                if ui.button("🧹 Clean v3").clicked() {
                    remove_old_v3_scheduled_task();
                    s.set_toast("Unregistered legacy AgentPowerGuard task and removed ProgramData.");
                }

                // Theme Toggle
                let theme_icon = if s.config.dark_mode { "☀ Light" } else { "🌙 Dark" };
                if ui.button(theme_icon).clicked() {
                    s.config.dark_mode = !s.config.dark_mode;
                    s.config_dirty = true;
                }
            });

            // Toast notification banner if any
            if let Some((msg, created)) = &s.toast_message.clone() {
                if created.elapsed() < Duration::from_secs(5) {
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(format!("ℹ {}", msg)).color(egui::Color32::LIGHT_BLUE));
                }
            }
            ui.add_space(4.0);
        });

        // ====================================================================
        // LEFT PANEL: Interactive IF/ELSE Condition Sliders & Checkboxes
        // ====================================================================
        egui::Panel::left("conditions_panel")
            .default_size(440.0)
            .min_size(380.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(6.0);
                    ui.heading("Conditions & Thresholds");
                    ui.separator();

                    // --- Quick Presets & Staged Timers ---
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("⏱ Quick Presets (Staged Power Strategy)").strong());
                        ui.label(egui::RichText::new("Turns monitor off first (saves 50-100W, prevents burn-in while agents work), then puts PC to sleep.").weak().size(11.0));
                        ui.add_space(4.0);

                        ui.horizontal_wrapped(|ui| {
                            let presets: [(&str, u32, u32, &str); 10] = [
                                ("15s Test", 15, 1, "15s Test Mode (Monitor: 1m, Sleep: 15s)"),
                                ("3m ★", 180, 2, "3m Default (Monitor: 2m, Sleep: 3m)"),
                                ("5m", 300, 3, "5m Preset (Monitor: 3m, Sleep: 5m)"),
                                ("10m", 600, 5, "10m Preset (Monitor: 5m, Sleep: 10m)"),
                                ("15m", 900, 10, "15m Preset (Monitor: 10m, Sleep: 15m)"),
                                ("30m", 1800, 15, "30m Preset (Monitor: 15m, Sleep: 30m)"),
                                ("45m", 2700, 20, "45m Preset (Monitor: 20m, Sleep: 45m)"),
                                ("1h", 3600, 30, "1 Hour Preset (Monitor: 30m, Sleep: 1h)"),
                                ("2h", 7200, 45, "2 Hours Preset (Monitor: 45m, Sleep: 2h)"),
                                ("3h", 10800, 60, "3 Hours Preset (Monitor: 60m, Sleep: 3h)"),
                            ];

                            for (label, sleep_s, mon_m, desc) in presets {
                                let is_active = s.config.idle_threshold_secs == sleep_s;
                                let btn = if is_active {
                                    egui::Button::new(egui::RichText::new(label).strong().color(egui::Color32::WHITE))
                                        .fill(egui::Color32::from_rgb(34, 139, 34))
                                } else {
                                    egui::Button::new(label)
                                };

                                if ui.add(btn).clicked() {
                                    s.config.idle_threshold_secs = sleep_s;
                                    s.config.monitor_timeout_mins = mon_m;
                                    s.countdown_secs = sleep_s;
                                    s.countdown_max = sleep_s;
                                    s.config_dirty = true;
                                    apply_monitor_timeout(mon_m);
                                    s.set_toast(format!("Applied {}. Windows monitor timeout synced to {}m.", desc, mon_m));
                                }
                            }
                        });
                    });

                    ui.add_space(4.0);

                    // --- Condition 1: Human Input Idle ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.cond1_idle_enabled, "1. Human Input Idle").changed() {
                                s.config_dirty = true;
                            }
                            draw_status_badge(ui, &s.cond1_idle);
                        });
                        let current_idle_str = format_time_compact(s.live_idle_secs);
                        let thresh_str = format_time_compact(s.config.idle_threshold_secs);
                        ui.label(format!("Mouse & Keyboard Inactivity: {} (threshold: >= {})", current_idle_str, thresh_str));
                        ui.horizontal(|ui| {
                            ui.label("Idle Timer:");
                            if ui.add(egui::Slider::new(&mut s.config.idle_threshold_secs, 10..=10800).custom_formatter(|n, _| format_time_compact(n as u32))).changed() {
                                s.config_dirty = true;
                                s.countdown_max = s.config.idle_threshold_secs;
                            }
                        });
                    });

                    ui.add_space(4.0);

                    // --- Condition 2: Watched Agent CPU % ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.cond2_agent_cpu_enabled, "2. Watched Agent CPU %").changed() {
                                s.config_dirty = true;
                            }
                            draw_status_badge(ui, &s.cond2_agent_cpu);
                        });
                        ui.label(format!("Top Agent: {}", s.cond2_agent_cpu.value_text));
                        ui.horizontal(|ui| {
                            ui.label("Threshold:");
                            if ui.add(egui::Slider::new(&mut s.config.agent_cpu_threshold, 0.5..=50.0).suffix("%")).changed() {
                                s.config_dirty = true;
                            }
                        });
                        ui.collapsing("Edit Process Keywords", |ui| {
                            ui.label("Comma-separated matching process names:");
                            if ui.text_edit_multiline(&mut s.config.agent_keywords).changed() {
                                s.config_dirty = true;
                            }
                        });
                    });

                    ui.add_space(4.0);

                    // --- Condition 3: Active Download / IDM ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.cond3_download_enabled, "3. Active Download / IDM").changed() {
                                s.config_dirty = true;
                            }
                            draw_status_badge(ui, &s.cond3_download);
                        });
                        ui.label(format!("Top Downloader: {}", s.cond3_download.value_text));
                        ui.horizontal(|ui| {
                            ui.label("Threshold:");
                            if ui.add(egui::Slider::new(&mut s.config.download_threshold_kbps, 50.0..=10000.0).suffix(" KB/s")).changed() {
                                s.config_dirty = true;
                            }
                        });
                        ui.collapsing("Edit Download Keywords", |ui| {
                            if ui.text_edit_singleline(&mut s.config.download_keywords).changed() {
                                s.config_dirty = true;
                            }
                        });
                    });

                    ui.add_space(4.0);

                    // --- Condition 4: Whole-PC Fallback CPU % ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.cond4_pc_cpu_enabled, "4. Whole-PC CPU %").changed() {
                                s.config_dirty = true;
                            }
                            draw_status_badge(ui, &s.cond4_pc_cpu);
                        });
                        ui.label(format!("Live Global CPU: {}", s.cond4_pc_cpu.value_text));
                        ui.horizontal(|ui| {
                            ui.label("Threshold:");
                            if ui.add(egui::Slider::new(&mut s.config.pc_cpu_threshold, 5.0..=90.0).suffix("%")).changed() {
                                s.config_dirty = true;
                            }
                        });
                    });

                    ui.add_space(4.0);

                    // --- Condition 5: Whole-PC Disk Read+Write ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.cond5_pc_disk_enabled, "5. Whole-PC Disk I/O").changed() {
                                s.config_dirty = true;
                            }
                            draw_status_badge(ui, &s.cond5_pc_disk);
                        });
                        ui.label(format!("Live Disk I/O: {}", s.cond5_pc_disk.value_text));
                        ui.horizontal(|ui| {
                            ui.label("Threshold:");
                            if ui.add(egui::Slider::new(&mut s.config.pc_disk_threshold_kbps, 100.0..=50000.0).suffix(" KB/s")).changed() {
                                s.config_dirty = true;
                            }
                        });
                    });

                    ui.add_space(4.0);

                    // --- Condition 6: Whole-PC Network Send+Recv ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.cond6_pc_net_enabled, "6. Whole-PC Network").changed() {
                                s.config_dirty = true;
                            }
                            draw_status_badge(ui, &s.cond6_pc_net);
                        });
                        ui.label(format!("Live Network I/O: {}", s.cond6_pc_net.value_text));
                        ui.horizontal(|ui| {
                            ui.label("Threshold:");
                            if ui.add(egui::Slider::new(&mut s.config.pc_net_threshold_kbps, 10.0..=10000.0).suffix(" KB/s")).changed() {
                                s.config_dirty = true;
                            }
                        });
                    });

                    ui.add_space(4.0);

                    // --- Condition 7: Sustained Spike Filter ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.cond7_spike_filter_enabled, "7. Sustained Spike Filter").changed() {
                                s.config_dirty = true;
                            }
                            draw_status_badge(ui, &s.cond7_spike_filter);
                        });
                        ui.label("Filter brief PC CPU / Disk / Net blips (e.g. Edge tab or wallpaper):");
                        ui.horizontal(|ui| {
                            ui.label("Sustain Duration:");
                            if ui.add(egui::Slider::new(&mut s.config.spike_filter_secs, 1..=60).suffix("s")).changed() {
                                s.config_dirty = true;
                            }
                        });
                        ui.label(format!("Consecutive Spike Counter: {}", s.cond7_spike_filter.value_text));
                    });

                    ui.add_space(4.0);

                    // --- Condition 8: Monitor Turn-Off Timeout ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("8. Monitor Timeout").strong());
                            if ui.button("Apply to Windows").clicked() {
                                let mins = s.config.monitor_timeout_mins;
                                apply_monitor_timeout(mins);
                                s.set_toast(format!("Applied {} min monitor timeout to Windows AC profile.", mins));
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Turn Off Display After:");
                            if ui.add(egui::Slider::new(&mut s.config.monitor_timeout_mins, 1..=180).suffix(" min")).changed() {
                                s.config_dirty = true;
                            }
                        });
                    });

                    ui.add_space(4.0);

                    // --- Condition 9: Windows Power Requests & Audio Override ---
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.cond9_power_requests_enabled, "9. Power Requests").changed() {
                                s.config_dirty = true;
                            }
                            draw_status_badge(ui, &s.cond9_power_requests);
                        });
                        if ui.checkbox(&mut s.config.respect_display_requests, "Respect DISPLAY Requests (Video Playback)").changed() {
                            s.config_dirty = true;
                        }
                        ui.label(format!("Display Lock State: {}", s.cond9_power_requests.value_text));

                        if ui.button("🔇 Override Audio/Browser Locks").clicked() {
                            override_noisy_audio_drivers();
                            s.set_toast("Powercfg overrides registered for VB-Audio, FxSound, USB Audio, msedge, IDMan.");
                        }
                    });

                    ui.add_space(6.0);
                    ui.separator();

                    // Windows Startup Task
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut s.config.run_at_startup, "Run at Windows Startup (Task Scheduler)").changed() {
                            s.config_dirty = true;
                            set_startup_task(s.config.run_at_startup);
                            if s.config.run_at_startup {
                                s.set_toast("Scheduled Task registered to launch with highest privileges on logon.");
                            } else {
                                s.set_toast("Scheduled Task removed.");
                            }
                        }
                    });

                    ui.add_space(8.0);
                });
            });

        // ====================================================================
        // CENTRAL PANEL: Real-Time Scrolling Graphs & Blocker Inspector
        // ====================================================================
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.heading("Real-Time Graphs (120s Rolling History)");
                ui.label(egui::RichText::new("— 1 Hz live sampling").color(egui::Color32::GRAY));
            });
            ui.separator();

            let available_h = ui.available_height() - 75.0; // reserve room for banner
            let chart_h = (available_h / 4.0).clamp(70.0, 140.0);

            // Chart 1: Agent CPU %
            ui.label(egui::RichText::new(format!("1. Agent CPU % (Live: {:.1}%)", s.live_agent_cpu_pct)).strong());
            let agent_pts: PlotPoints = s.history_agent_cpu
                .iter()
                .enumerate()
                .map(|(i, &v)| [i as f64, v as f64])
                .collect::<Vec<[f64; 2]>>()
                .into();

            Plot::new("chart_agent_cpu")
                .height(chart_h)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .include_y(0.0)
                .include_y(s.config.agent_cpu_threshold * 1.5)
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new("Agent CPU", agent_pts).color(egui::Color32::from_rgb(0, 191, 255)));
                    plot_ui.hline(
                        HLine::new("Threshold", s.config.agent_cpu_threshold as f64)
                            .color(egui::Color32::RED)
                            .style(LineStyle::dashed_loose()),
                    );
                });

            ui.add_space(2.0);

            // Chart 2: Whole-PC CPU %
            ui.label(egui::RichText::new(format!("2. Whole-PC CPU % (Live: {:.1}%)", s.live_pc_cpu_pct)).strong());
            let pc_cpu_pts: PlotPoints = s.history_pc_cpu
                .iter()
                .enumerate()
                .map(|(i, &v)| [i as f64, v as f64])
                .collect::<Vec<[f64; 2]>>()
                .into();

            Plot::new("chart_pc_cpu")
                .height(chart_h)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .include_y(0.0)
                .include_y(s.config.pc_cpu_threshold * 1.3)
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new("PC CPU", pc_cpu_pts).color(egui::Color32::from_rgb(147, 112, 219)));
                    plot_ui.hline(
                        HLine::new("Threshold", s.config.pc_cpu_threshold as f64)
                            .color(egui::Color32::RED)
                            .style(LineStyle::dashed_loose()),
                    );
                });

            ui.add_space(2.0);

            // Chart 3: Disk I/O KB/s
            ui.label(egui::RichText::new(format!("3. Total Disk I/O (Live: {:.0} KB/s)", s.live_pc_disk_kbps)).strong());
            let disk_pts: PlotPoints = s.history_pc_disk
                .iter()
                .enumerate()
                .map(|(i, &v)| [i as f64, v as f64])
                .collect::<Vec<[f64; 2]>>()
                .into();

            Plot::new("chart_disk")
                .height(chart_h)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .include_y(0.0)
                .include_y(s.config.pc_disk_threshold_kbps * 1.3)
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new("Disk KB/s", disk_pts).color(egui::Color32::from_rgb(255, 165, 0)));
                    plot_ui.hline(
                        HLine::new("Threshold", s.config.pc_disk_threshold_kbps as f64)
                            .color(egui::Color32::RED)
                            .style(LineStyle::dashed_loose()),
                    );
                });

            ui.add_space(2.0);

            // Chart 4: Network I/O KB/s
            ui.label(egui::RichText::new(format!("4. Total Network I/O (Live: {:.0} KB/s)", s.live_pc_net_kbps)).strong());
            let net_pts: PlotPoints = s.history_pc_net
                .iter()
                .enumerate()
                .map(|(i, &v)| [i as f64, v as f64])
                .collect::<Vec<[f64; 2]>>()
                .into();

            Plot::new("chart_net")
                .height(chart_h)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .include_y(0.0)
                .include_y(s.config.pc_net_threshold_kbps * 1.5)
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new("Network KB/s", net_pts).color(egui::Color32::from_rgb(50, 205, 50)));
                    plot_ui.hline(
                        HLine::new("Threshold", s.config.pc_net_threshold_kbps as f64)
                            .color(egui::Color32::RED)
                            .style(LineStyle::dashed_loose()),
                    );
                });

            ui.add_space(6.0);

            // ================================================================
            // LIVE BLOCKER INSPECTOR BANNER
            // ================================================================
            let (bg_color, text_color) = if !s.config.guard_enabled {
                (egui::Color32::from_rgb(50, 50, 50), egui::Color32::LIGHT_GRAY)
            } else if s.all_conditions_pass {
                (egui::Color32::from_rgb(20, 85, 40), egui::Color32::WHITE)
            } else {
                (egui::Color32::from_rgb(130, 25, 25), egui::Color32::WHITE)
            };

            egui::Frame::new()
                .fill(bg_color)
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        egui::RichText::new(&s.blocker_banner_text)
                            .color(text_color)
                            .strong()
                            .size(14.0),
                    );
                });
        });
    }
}

// ----------------------------------------------------------------------------
// Helper: Draw Condition Status Badge
// ----------------------------------------------------------------------------

fn draw_status_badge(ui: &mut egui::Ui, status: &ConditionStatus) {
    let (label, bg, text_c) = if !status.enabled {
        ("BYPASSED", egui::Color32::from_rgb(60, 60, 60), egui::Color32::LIGHT_GRAY)
    } else if status.is_passing {
        ("PASS (Quiet)", egui::Color32::from_rgb(26, 95, 50), egui::Color32::WHITE)
    } else {
        ("BLOCKING SLEEP", egui::Color32::from_rgb(160, 30, 30), egui::Color32::WHITE)
    };

    egui::Frame::new()
        .fill(bg)
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).color(text_c).strong().size(11.0));
        });
}

// ----------------------------------------------------------------------------
// Main Entry Point
// ----------------------------------------------------------------------------

fn main() -> eframe::Result {
    let config = AppConfig::load();
    let shared_state = Arc::new(Mutex::new(SharedState::new(config)));

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Agent Power Guard")
            .with_min_inner_size([960.0, 680.0])
            .with_inner_size([1240.0, 800.0]),
        ..Default::default()
    };

    let app_state = Arc::clone(&shared_state);
    eframe::run_native(
        "Agent Power Guard",
        native_options,
        Box::new(move |cc| Ok(Box::new(AgentPowerGuardApp::new(cc, app_state)))),
    )
}
