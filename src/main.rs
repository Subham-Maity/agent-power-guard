#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
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
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
};
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
const HISTORY_LEN: usize = 120;
const DASHBOARD_HTML: &str = include_str!("../assets/dashboard.html");

// ----------------------------------------------------------------------------
// Configuration & Persistence
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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

    // Advanced features: GPU, Mic/Cam, Grace, Energy, Web Server
    pub cond10_gpu_enabled: bool,
    pub gpu_threshold_pct: f32,         // 0.5..=90.0, default 5.0
    pub cond11_mic_cam_enabled: bool,
    pub grace_period_secs: u32,         // 5..=60, default 30
    pub record_energy_data: bool,       // default true
    pub active_watts: f32,              // default 150.0
    pub monitor_watts: f32,             // default 50.0
    pub sleep_watts: f32,               // default 3.0
    pub electricity_rate_kwh: f32,      // default 0.15
    pub web_server_port: u16,           // default 49281
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

            cond10_gpu_enabled: true,
            gpu_threshold_pct: 5.0,
            cond11_mic_cam_enabled: true,
            grace_period_secs: 30,
            record_energy_data: true,
            active_watts: 150.0,
            monitor_watts: 50.0,
            sleep_watts: 3.0,
            electricity_rate_kwh: 0.15,
            web_server_port: 49281,
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

fn get_energy_history_path() -> PathBuf {
    get_config_path().parent().unwrap_or(&PathBuf::from(".")).join("energy_history.json")
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
// Energy History & Data Models
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Guard,
    Energy,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnergyFilter {
    All,
    Today,
    Last7Days,
    ThisMonth,
    ThisYear,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnergyRecord {
    pub id: u64,
    pub timestamp: u64,
    pub event_type: String, // "sleep", "monitor_off"
    pub start_time: String,
    pub duration_secs: u64,
    pub active_watts: f32,
    pub idle_watts: f32,
    pub watts_saved_avg: f32,
    pub kwh_saved: f64,
    pub cost_saved: f64,
}

fn format_epoch_local(epoch: u64) -> String {
    let days = epoch / 86400;
    let secs_of_day = epoch % 86400;
    let hours = secs_of_day / 3600;
    let minutes = (secs_of_day % 3600) / 60;
    let seconds = secs_of_day % 60;

    let mut year = 1970;
    let mut d = days;
    loop {
        let leap = if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) { 1 } else { 0 };
        let days_in_year = 365 + leap;
        if d >= days_in_year {
            d -= days_in_year;
            year += 1;
        } else {
            break;
        }
    }
    let leap = if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) { 1 } else { 0 };
    let month_days = [31, 28 + leap, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1;
    for &md in &month_days {
        if d >= md {
            d -= md;
            month += 1;
        } else {
            break;
        }
    }
    let day = d + 1;
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", year, month, day, hours, minutes, seconds)
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
    pub live_gpu_pct: f32,
    pub live_download_kbps: f32,
    pub live_download_process_name: String,
    pub live_pc_cpu_pct: f32,
    pub live_pc_disk_kbps: f32,
    pub live_pc_net_kbps: f32,
    pub live_mic_active: bool,
    pub live_cam_active: bool,
    pub live_mic_cam_app: String,
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
    pub cond10_gpu: ConditionStatus,
    pub cond11_mic_cam: ConditionStatus,

    // Graphs history (120 seconds rolling)
    pub history_agent_cpu: VecDeque<f32>,
    pub history_gpu: VecDeque<f32>,
    pub history_pc_cpu: VecDeque<f32>,
    pub history_pc_disk: VecDeque<f32>,
    pub history_pc_net: VecDeque<f32>,

    // Blocker inspector banner
    pub blocker_banner_text: String,
    pub all_conditions_pass: bool,

    // Grace period & wall clock sleep detection
    pub last_wall_clock_secs: u64,
    pub pc_spike_sustained_counter: u32,
    pub grace_active: bool,
    pub postponed_until: Option<Instant>,

    // Agent Lease
    pub agent_lease_until: Option<Instant>,
    pub agent_lease_reason: String,

    // Network / WoL
    pub local_ip: String,
    pub mac_address: String,

    // Energy Tracking
    pub energy_history: Vec<EnergyRecord>,
    pub energy_filter: EnergyFilter,
    pub active_tab: ActiveTab,
    pub monitor_standby_start: Option<Instant>,

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
            live_gpu_pct: 0.0,
            live_download_kbps: 0.0,
            live_download_process_name: "None".to_string(),
            live_pc_cpu_pct: 0.0,
            live_pc_disk_kbps: 0.0,
            live_pc_net_kbps: 0.0,
            live_mic_active: false,
            live_cam_active: false,
            live_mic_cam_app: String::new(),
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
            cond10_gpu: ConditionStatus::default(),
            cond11_mic_cam: ConditionStatus::default(),

            history_agent_cpu: VecDeque::with_capacity(HISTORY_LEN),
            history_gpu: VecDeque::with_capacity(HISTORY_LEN),
            history_pc_cpu: VecDeque::with_capacity(HISTORY_LEN),
            history_pc_disk: VecDeque::with_capacity(HISTORY_LEN),
            history_pc_net: VecDeque::with_capacity(HISTORY_LEN),

            blocker_banner_text: "Initializing watchdog...".to_string(),
            all_conditions_pass: false,

            last_wall_clock_secs: 0,
            pc_spike_sustained_counter: 0,
            grace_active: false,
            postponed_until: None,

            agent_lease_until: None,
            agent_lease_reason: String::new(),

            local_ip: get_local_ip(),
            mac_address: "00:00:00:00:00:00".to_string(),

            energy_history: Vec::new(),
            energy_filter: EnergyFilter::All,
            active_tab: ActiveTab::Guard,
            monitor_standby_start: None,

            toast_message: None,
        };

        for _ in 0..HISTORY_LEN {
            s.history_agent_cpu.push_back(0.0);
            s.history_gpu.push_back(0.0);
            s.history_pc_cpu.push_back(0.0);
            s.history_pc_disk.push_back(0.0);
            s.history_pc_net.push_back(0.0);
        }

        s.load_energy_history();
        s
    }

    pub fn set_toast(&mut self, text: impl Into<String>) {
        self.toast_message = Some((text.into(), Instant::now()));
    }

    pub fn load_energy_history(&mut self) {
        let path = get_energy_history_path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(records) = serde_json::from_str::<Vec<EnergyRecord>>(&content) {
                self.energy_history = records;
            }
        }
    }

    pub fn save_energy_history(&self) {
        let path = get_energy_history_path();
        if let Ok(json) = serde_json::to_string_pretty(&self.energy_history) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn log_energy_event(&mut self, event_type: &str, duration_secs: u64) {
        if !self.config.record_energy_data || duration_secs < 5 {
            return;
        }
        let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let active_w = self.config.active_watts;
        let idle_w = if event_type == "monitor_off" {
            (self.config.active_watts - self.config.monitor_watts).max(0.0)
        } else {
            self.config.sleep_watts
        };
        let watts_saved = (active_w - idle_w).max(0.0);
        let kwh_saved = (watts_saved as f64 * duration_secs as f64) / (3600.0 * 1000.0);
        let cost_saved = kwh_saved * (self.config.electricity_rate_kwh as f64);

        let record = EnergyRecord {
            id: now_epoch,
            timestamp: now_epoch,
            event_type: event_type.to_string(),
            start_time: format_epoch_local(now_epoch),
            duration_secs,
            active_watts: active_w,
            idle_watts: idle_w,
            watts_saved_avg: watts_saved,
            kwh_saved,
            cost_saved,
        };

        self.energy_history.push(record);
        self.save_energy_history();
    }
}

// ----------------------------------------------------------------------------
// Local IP & MAC Discovery
// ----------------------------------------------------------------------------

fn get_local_ip() -> String {
    if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "127.0.0.1".to_string()
}

fn get_primary_mac(networks: &Networks) -> String {
    for (_name, data) in networks {
        let mac = data.mac_address();
        if mac.0 != [0, 0, 0, 0, 0, 0] {
            return format!(
                "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                mac.0[0], mac.0[1], mac.0[2], mac.0[3], mac.0[4], mac.0[5]
            );
        }
    }
    "00:00:00:00:00:00".to_string()
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
                std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            CloseHandle(token);

            SetSuspendState(false, false, false)
        }
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("systemctl")
            .arg("suspend")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("pmset")
            .args(["sleepnow"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Human Input Idle
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
                let tick_count = GetTickCount();
                if tick_count >= lii.dwTime {
                    (tick_count - lii.dwTime) / 1000
                } else {
                    0
                }
            } else {
                0
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("xprintidle").output() {
            if let Ok(s) = String::from_utf8(output.stdout) {
                if let Ok(ms) = s.trim().parse::<u64>() {
                    return (ms / 1000) as u32;
                }
            }
        }
        0
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("ioreg").args(["-c", "IOHIDSystem"]).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                if line.contains("\"HIDIdleTime\" = ") {
                    if let Some(pos) = line.find("= ") {
                        let num_str = line[pos + 2..].trim();
                        if let Ok(nanos) = num_str.parse::<u64>() {
                            return (nanos / 1_000_000_000) as u32;
                        }
                    }
                }
            }
        }
        0
    }
}

// ----------------------------------------------------------------------------
// Platform Implementation: Monitor Control & Power Plan
// ----------------------------------------------------------------------------

pub fn turn_off_monitors_now() {
    #[cfg(windows)]
    {
        let _ = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Add-Type -MemberDefinition '[DllImport(\"user32.dll\")] public static extern int SendMessage(int hWnd, int hMsg, int wParam, int lParam);' -Name 'Win32SendMsg' -Namespace Win32Functions -PassThru)::SendMessage(0xffff, 0x0112, 0xF170, 2) | Out-Null"
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("xset").args(["dpms", "force", "off"]).status();
    }

    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("pmset").args(["displaysleepnow"]).status();
    }
}

pub fn apply_monitor_timeout(mins: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("powercfg")
            .args(["/change", "monitor-timeout-ac", &mins.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        let _ = Command::new("powercfg")
            .args(["/change", "monitor-timeout-dc", &mins.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

pub fn apply_optimal_power_plan(monitor_timeout_mins: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("powercfg").args(["/change", "standby-timeout-ac", "0"]).creation_flags(CREATE_NO_WINDOW).status();
        let _ = Command::new("powercfg").args(["/change", "standby-timeout-dc", "0"]).creation_flags(CREATE_NO_WINDOW).status();
        apply_monitor_timeout(monitor_timeout_mins);
        let _ = Command::new("powercfg").args(["/setacvalueindex", "SCHEME_CURRENT", "SUB_PROCESSOR", "PROCTHROTTLEMIN", "5"]).creation_flags(CREATE_NO_WINDOW).status();
        let _ = Command::new("powercfg").args(["/setacvalueindex", "SCHEME_CURRENT", "SUB_PROCESSOR", "PROCTHROTTLEMAX", "100"]).creation_flags(CREATE_NO_WINDOW).status();
        let _ = Command::new("powercfg").args(["/setactive", "SCHEME_CURRENT"]).creation_flags(CREATE_NO_WINDOW).status();
    }
}

pub fn override_audio_browser_power_requests() {
    #[cfg(windows)]
    {
        let callers = [
            ("DRIVER", "High Definition Audio Device"),
            ("DRIVER", "Realtek High Definition Audio"),
            ("PROCESS", "msedge.exe"),
            ("PROCESS", "chrome.exe"),
            ("PROCESS", "brave.exe"),
            ("PROCESS", "firefox.exe"),
            ("PROCESS", "IDMan.exe"),
            ("DRIVER", "VB-Audio VoiceMeeter VAIO"),
        ];

        for (caller_type, name) in callers {
            let _ = Command::new("powercfg")
                .args(["/requestsoverride", caller_type, name, "EXECUTION", "SYSTEM", "AWAYMODE"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
    }
}

pub fn check_power_requests() -> (bool, bool) {
    #[cfg(windows)]
    {
        if let Ok(output) = Command::new("powercfg").arg("/requests").creation_flags(CREATE_NO_WINDOW).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut in_display = false;
            let mut in_system = false;
            let mut has_display_request = false;
            let mut has_system_request = false;

            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("DISPLAY:") {
                    in_display = true;
                    in_system = false;
                } else if trimmed.starts_with("SYSTEM:") {
                    in_display = false;
                    in_system = true;
                } else if trimmed.starts_with("AWAYMODE:") || trimmed.starts_with("EXECUTION:") || trimmed.starts_with("PERFBOOST:") {
                    in_display = false;
                    in_system = false;
                } else if !trimmed.is_empty() && trimmed != "None." {
                    if in_display {
                        has_display_request = true;
                    }
                    if in_system {
                        has_system_request = true;
                    }
                }
            }
            return (has_display_request, has_system_request);
        }
    }
    (false, false)
}

pub fn set_startup_task(enable: bool) {
    #[cfg(windows)]
    {
        if enable {
            if let Ok(exe_path) = std::env::current_exe() {
                let exe_str = exe_path.to_string_lossy().to_string();
                let _ = Command::new("schtasks")
                    .args([
                        "/create",
                        "/tn", "AgentPowerGuard",
                        "/tr", &format!("\"{}\"", exe_str),
                        "/sc", "onlogon",
                        "/rl", "highest",
                        "/f",
                    ])
                    .creation_flags(CREATE_NO_WINDOW)
                    .status();
            }
        } else {
            let _ = Command::new("schtasks")
                .args(["/delete", "/tn", "AgentPowerGuard", "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
    }
}

pub fn remove_old_v3_scheduled_task() {
    #[cfg(windows)]
    {
        let _ = Command::new("schtasks").args(["/delete", "/tn", "AgentPowerGuard", "/f"]).creation_flags(CREATE_NO_WINDOW).status();
        let _ = Command::new("schtasks").args(["/delete", "/tn", "AgentPowerGuard_v3", "/f"]).creation_flags(CREATE_NO_WINDOW).status();
        if let Ok(pdata) = std::env::var("ProgramData") {
            let path = PathBuf::from(pdata).join("AgentPowerGuard");
            if path.exists() {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }
}

pub fn flush_working_set() {
    #[cfg(windows)]
    {
        unsafe {
            K32EmptyWorkingSet(GetCurrentProcess());
        }
    }
}

// ----------------------------------------------------------------------------
// Feature 1: GPU Activity Monitoring (NVIDIA NVML & Linux sysfs)
// ----------------------------------------------------------------------------

#[cfg(windows)]
fn query_nvidia_gpu_utilization() -> Option<f32> {
    unsafe {
        let nvml_lib = LoadLibraryA(b"nvml.dll\0".as_ptr());
        if nvml_lib.is_null() {
            return None;
        }

        type NvmlInit = unsafe extern "C" fn() -> u32;
        type NvmlShutdown = unsafe extern "C" fn() -> u32;
        type NvmlDeviceGetHandleByIndex = unsafe extern "C" fn(u32, *mut usize) -> u32;
        #[repr(C)]
        struct NvmlUtilization {
            gpu: u32,
            memory: u32,
        }
        type NvmlDeviceGetUtilizationRates = unsafe extern "C" fn(usize, *mut NvmlUtilization) -> u32;

        let init_fn: Option<NvmlInit> = std::mem::transmute(GetProcAddress(nvml_lib, b"nvmlInit_v2\0".as_ptr()));
        let get_handle_fn: Option<NvmlDeviceGetHandleByIndex> = std::mem::transmute(GetProcAddress(nvml_lib, b"nvmlDeviceGetHandleByIndex_v2\0".as_ptr()));
        let get_rates_fn: Option<NvmlDeviceGetUtilizationRates> = std::mem::transmute(GetProcAddress(nvml_lib, b"nvmlDeviceGetUtilizationRates\0".as_ptr()));
        let shutdown_fn: Option<NvmlShutdown> = std::mem::transmute(GetProcAddress(nvml_lib, b"nvmlShutdown\0".as_ptr()));

        if let (Some(init), Some(get_handle), Some(get_rates), Some(shutdown)) = (init_fn, get_handle_fn, get_rates_fn, shutdown_fn) {
            if init() == 0 {
                let mut device: usize = 0;
                let mut max_util: f32 = 0.0;
                if get_handle(0, &mut device) == 0 {
                    let mut util = NvmlUtilization { gpu: 0, memory: 0 };
                    if get_rates(device, &mut util) == 0 {
                        max_util = util.gpu as f32;
                    }
                }
                shutdown();
                return Some(max_util);
            }
        }
        None
    }
}

pub fn get_gpu_utilization() -> f32 {
    #[cfg(windows)]
    {
        if let Some(u) = query_nvidia_gpu_utilization() {
            return u;
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/sys/class/drm/card0/device/gpu_busy_percent") {
            if let Ok(val) = content.trim().parse::<f32>() {
                return val;
            }
        }
    }

    0.0
}

// ----------------------------------------------------------------------------
// Feature 3: Microphone, Camera & Meeting Detection
// ----------------------------------------------------------------------------

#[cfg(windows)]
fn check_mic_camera_active_win() -> (bool, bool, String) {
    let check_path = |base_path: &str| -> Option<String> {
        let wide: Vec<u16> = base_path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut hkey: HKEY = std::ptr::null_mut();
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, wide.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
                return None;
            }
            let mut index = 0u32;
            let mut name_buf = [0u16; 512];
            loop {
                let mut name_len = name_buf.len() as u32;
                if RegEnumKeyExW(
                    hkey,
                    index,
                    name_buf.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) != 0 {
                    break;
                }
                index += 1;

                let mut sub_key: HKEY = std::ptr::null_mut();
                if RegOpenKeyExW(hkey, name_buf.as_ptr(), 0, KEY_READ, &mut sub_key) == 0 {
                    let val_stop_name: Vec<u16> = "LastUsedTimeStop\0".encode_utf16().collect();
                    let val_start_name: Vec<u16> = "LastUsedTimeStart\0".encode_utf16().collect();
                    let mut stop_time = 0u64;
                    let mut start_time = 0u64;
                    let mut size_stop = 8u32;
                    let mut size_start = 8u32;

                    let res_stop = RegQueryValueExW(
                        sub_key,
                        val_stop_name.as_ptr(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut stop_time as *mut u64 as *mut u8,
                        &mut size_stop,
                    );
                    let res_start = RegQueryValueExW(
                        sub_key,
                        val_start_name.as_ptr(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut start_time as *mut u64 as *mut u8,
                        &mut size_start,
                    );

                    RegCloseKey(sub_key);

                    if res_stop == 0 && res_start == 0 && stop_time == 0 && start_time > 0 {
                        let raw_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
                        let clean_name = raw_name
                            .split('#')
                            .last()
                            .unwrap_or(&raw_name)
                            .split('.')
                            .next()
                            .unwrap_or(&raw_name)
                            .to_string();
                        RegCloseKey(hkey);
                        return Some(clean_name);
                    }
                }
            }
            RegCloseKey(hkey);
        }
        None
    };

    let mic_app = check_path("Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone")
        .or_else(|| check_path("Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone\\NonPackaged"));
    let cam_app = check_path("Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\webcam")
        .or_else(|| check_path("Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\webcam\\NonPackaged"));

    let mic_active = mic_app.is_some();
    let cam_active = cam_app.is_some();
    let app_name = mic_app.or(cam_app).unwrap_or_default();
    (mic_active, cam_active, app_name)
}

pub fn check_mic_camera_active() -> (bool, bool, String) {
    #[cfg(windows)]
    {
        check_mic_camera_active_win()
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = Command::new("pactl").args(["list", "short", "source-outputs"]).output() {
            if !output.stdout.is_empty() {
                return (true, false, "PulseAudio Capture".to_string());
            }
        }
        (false, false, String::new())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        (false, false, String::new())
    }
}

// ----------------------------------------------------------------------------
// Feature 2 & 6: Embedded Web Server & Agent Heartbeat API
// ----------------------------------------------------------------------------

fn parse_query_param(url: &str, key: &str) -> Option<String> {
    if let Some(pos) = url.find('?') {
        let query = &url[pos + 1..];
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                if k == key {
                    let decoded = v.replace("%20", " ").replace('+', " ");
                    return Some(decoded);
                }
            }
        }
    }
    None
}

fn handle_http_client(mut stream: TcpStream, state: Arc<Mutex<SharedState>>) {
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req_str = String::from_utf8_lossy(&buf[..n]);
    let first_line = req_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let path = parts[1];

    if path == "/" || path == "/index.html" {
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            DASHBOARD_HTML.len(),
            DASHBOARD_HTML
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path == "/api/status" {
        let json = {
            let s = state.lock().unwrap();
            let total_kwh: f64 = s.energy_history.iter().map(|e| e.kwh_saved).sum();
            let total_cost: f64 = s.energy_history.iter().map(|e| e.cost_saved).sum();
            let total_duration: u64 = s.energy_history.iter().map(|e| e.duration_secs).sum();

            let lease_active = s.agent_lease_until.map(|u| Instant::now() < u).unwrap_or(false);
            let lease_remaining = s.agent_lease_until
                .and_then(|u| u.checked_duration_since(Instant::now()))
                .map(|d| d.as_secs())
                .unwrap_or(0);

            serde_json::json!({
                "guard_enabled": s.config.guard_enabled,
                "all_conditions_pass": s.all_conditions_pass,
                "countdown_secs": s.countdown_secs,
                "countdown_max": s.countdown_max,
                "blocker_banner_text": s.blocker_banner_text,
                "live_idle_secs": s.live_idle_secs,
                "live_agent_cpu": s.live_agent_cpu_pct,
                "live_agent_proc": s.live_agent_process_name,
                "live_gpu": s.live_gpu_pct,
                "live_download": s.live_download_kbps,
                "live_pc_cpu": s.live_pc_cpu_pct,
                "live_pc_disk": s.live_pc_disk_kbps,
                "live_pc_net": s.live_pc_net_kbps,
                "live_mic_cam_active": s.live_mic_active || s.live_cam_active,
                "live_mic_cam_app": s.live_mic_cam_app,
                "power_request_active": s.live_power_display_active,
                "spike_counter": s.pc_spike_sustained_counter,
                "spike_limit": s.config.spike_filter_secs,
                "cond1_pass": s.cond1_idle.is_passing,
                "cond2_pass": s.cond2_agent_cpu.is_passing,
                "cond3_pass": s.cond3_download.is_passing,
                "cond4_pass": s.cond4_pc_cpu.is_passing,
                "cond5_pass": s.cond5_pc_disk.is_passing,
                "cond6_pass": s.cond6_pc_net.is_passing,
                "cond10_pass": s.cond10_gpu.is_passing,
                "cond11_pass": s.cond11_mic_cam.is_passing,
                "agent_lease_active": lease_active,
                "agent_lease_reason": s.agent_lease_reason,
                "agent_lease_remaining_secs": lease_remaining,
                "local_ip": s.local_ip,
                "mac_address": s.mac_address,
                "energy": {
                    "kwh_saved": total_kwh,
                    "cost_saved": total_cost,
                    "duration_secs": total_duration,
                }
            }).to_string()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            json.len(),
            json
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/sleep") {
        let mut s = state.lock().unwrap();
        s.set_toast("Remote command: Sleep Now executed.");
        let _ = sleep_now();
        let body = r#"{"status":"ok","message":"System sleep command issued"}"#;
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/postpone") {
        let mins = parse_query_param(path, "mins").and_then(|m| m.parse::<u64>().ok()).unwrap_or(15);
        let mut s = state.lock().unwrap();
        s.postponed_until = Some(Instant::now() + Duration::from_secs(mins * 60));
        s.set_toast(&format!("Sleep postponed by {} minutes via Remote/API.", mins));
        let body = format!(r#"{{"status":"ok","message":"Postponed for {} minutes","mins":{}}}"#, mins, mins);
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/toggle_guard") {
        let mut s = state.lock().unwrap();
        s.config.guard_enabled = !s.config.guard_enabled;
        s.config_dirty = true;
        let body = format!(r#"{{"status":"ok","guard_enabled":{}}}"#, s.config.guard_enabled);
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/lease") {
        let mins = parse_query_param(path, "mins").and_then(|m| m.parse::<u64>().ok()).unwrap_or(30);
        let reason = parse_query_param(path, "reason").unwrap_or_else(|| "Remote Lease".to_string());
        let mut s = state.lock().unwrap();
        s.agent_lease_until = Some(Instant::now() + Duration::from_secs(mins * 60));
        s.agent_lease_reason = reason.clone();
        s.set_toast(&format!("Agent Lease granted for {}m: {}", mins, reason));
        let body = format!(r#"{{"status":"ok","lease_mins":{},"reason":"{}"}}"#, mins, reason);
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/release") {
        let mut s = state.lock().unwrap();
        s.agent_lease_until = None;
        s.agent_lease_reason.clear();
        s.set_toast("Agent Lease released via Remote/API.");
        let body = r#"{"status":"ok","message":"Agent lease cleared"}"#;
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/energy/clear") {
        let mut s = state.lock().unwrap();
        s.energy_history.clear();
        s.save_energy_history();
        s.set_toast("Energy history cleared via Remote/API.");
        let body = r#"{"status":"ok","message":"Energy records deleted"}"#;
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let body = r#"{"error":"not_found"}"#;
    let resp = format!("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
    let _ = stream.write_all(resp.as_bytes());
}

fn start_web_server(state: Arc<Mutex<SharedState>>, port: u16) {
    std::thread::spawn(move || {
        let addr = format!("0.0.0.0:{}", port);
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(_) => return,
        };

        for stream in listener.incoming().flatten() {
            let state_clone = Arc::clone(&state);
            std::thread::spawn(move || {
                handle_http_client(stream, state_clone);
            });
        }
    });
}

fn send_cli_request(path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect("127.0.0.1:49281")
        .map_err(|e| format!("Could not connect to running Agent Power Guard instance on 127.0.0.1:49281 (Is the app running?): {}", e))?;
    let req = format!("GET {} HTTP/1.1\r\nHost: 127.0.0.1:49281\r\nConnection: close\r\n\r\n", path);
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp).map_err(|e| e.to_string())?;
    if let Some(idx) = resp.find("\r\n\r\n") {
        Ok(resp[idx + 4..].to_string())
    } else {
        Ok(resp)
    }
}

fn handle_cli_command(args: &[String]) {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(0xFFFFFFFF);
    }
    let cmd = args[1].to_lowercase();
    match cmd.as_str() {
        "lease" => {
            let mins = args.get(2).and_then(|m| m.parse::<u32>().ok()).unwrap_or(30);
            let reason = args.get(3).cloned().unwrap_or_else(|| "CLI Lease".to_string());
            let encoded_reason = reason.replace(' ', "%20");
            match send_cli_request(&format!("/api/lease?mins={}&reason={}", mins, encoded_reason)) {
                Ok(resp) => println!("Success: {}", resp),
                Err(err) => eprintln!("Error: {}", err),
            }
        }
        "release" => {
            match send_cli_request("/api/release") {
                Ok(resp) => println!("Success: {}", resp),
                Err(err) => eprintln!("Error: {}", err),
            }
        }
        "status" => {
            match send_cli_request("/api/status") {
                Ok(resp) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        println!("==================================================");
                        println!("           AGENT POWER GUARD - STATUS             ");
                        println!("==================================================");
                        println!("Guard Enabled    : {}", v["guard_enabled"]);
                        println!("All Conditions   : {}", if v["all_conditions_pass"] == true { "PASSING (Arming Sleep)" } else { "BLOCKED" });
                        println!("Countdown        : {}s / {}s", v["countdown_secs"], v["countdown_max"]);
                        println!("Banner           : {}", v["blocker_banner_text"]);
                        println!("Human Idle       : {}s", v["live_idle_secs"]);
                        println!("Agent CPU        : {:.1}% ({})", v["live_agent_cpu"], v["live_agent_proc"]);
                        println!("GPU Compute      : {:.1}%", v["live_gpu"]);
                        println!("Whole-PC CPU     : {:.1}%", v["live_pc_cpu"]);
                        println!("Whole-PC Disk    : {:.0} KB/s", v["live_pc_disk"]);
                        println!("Whole-PC Network : {:.0} KB/s", v["live_pc_net"]);
                        println!("Mic/Cam In Use   : {}", v["live_mic_cam_active"]);
                        println!("Agent Lease      : {} ({})", v["agent_lease_active"], v["agent_lease_reason"]);
                        println!("Local Web URL    : http://{}:49281", v["local_ip"]);
                        println!("==================================================");
                    } else {
                        println!("{}", resp);
                    }
                }
                Err(err) => eprintln!("Error: {}", err),
            }
        }
        "sleep" => {
            match send_cli_request("/api/sleep") {
                Ok(resp) => println!("Success: {}", resp),
                Err(err) => eprintln!("Error: {}", err),
            }
        }
        "postpone" => {
            let mins = args.get(2).and_then(|m| m.parse::<u32>().ok()).unwrap_or(15);
            match send_cli_request(&format!("/api/postpone?mins={}", mins)) {
                Ok(resp) => println!("Success: {}", resp),
                Err(err) => eprintln!("Error: {}", err),
            }
        }
        "help" | "--help" | "-h" => {
            println!("Agent Power Guard CLI");
            println!();
            println!("Usage:");
            println!("  agent-power-guard lease [mins] [reason]  - Block sleep with an active agent lease");
            println!("  agent-power-guard release                - Release active agent lease");
            println!("  agent-power-guard status                 - Query watchdog status");
            println!("  agent-power-guard postpone [mins]        - Postpone sleep by N minutes");
            println!("  agent-power-guard sleep                  - Trigger immediate system sleep");
            println!("  agent-power-guard help                   - Show this help message");
        }
        _ => {
            eprintln!("Unknown command: '{}'. Run 'agent-power-guard help' for options.", cmd);
        }
    }
}

// ----------------------------------------------------------------------------
// Background Watchdog Engine (1 Hz Polling)
// ----------------------------------------------------------------------------

pub fn start_watchdog_thread(state: Arc<Mutex<SharedState>>, egui_ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut sys = System::new().expect("sysinfo System::new");
        let mut nets = Networks::new_with_refreshed_list().expect("Networks::new");
        let mut disks = Disks::new_with_refreshed_list_specifics(
            DiskRefreshKind::nothing().with_io_usage(),
        ).expect("Disks::new");

        // Warm up CPU usage
        sys.refresh_cpu_usage();
        std::thread::sleep(Duration::from_millis(500));

        // Populate MAC address once
        {
            let mut s = state.lock().unwrap();
            s.mac_address = get_primary_mac(&nets);
        }

        let mut cached_display_request = false;
        let mut last_requests_check = Instant::now() - Duration::from_secs(10);
        let mut consecutive_spikes: u32 = 0;

        loop {
            let loop_start = Instant::now();

            // 1. Check Wall-Clock Wakeup
            let now_wall = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            {
                let mut s = state.lock().unwrap();
                if s.last_wall_clock_secs > 0 && now_wall > s.last_wall_clock_secs + 10 {
                    s.countdown_secs = s.countdown_max;
                    s.set_toast("System woke from sleep! Watchdog countdown reset.");
                    consecutive_spikes = 0;
                    s.pc_spike_sustained_counter = 0;
                }
                s.last_wall_clock_secs = now_wall;
            }

            // 2. Sample Metrics
            let idle_secs = get_idle_seconds();

            sys.refresh_cpu_usage();
            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing().with_cpu().with_disk_usage(),
            );

            disks.refresh_specifics(false, DiskRefreshKind::nothing().with_io_usage());
            nets.refresh(false);

            // Whole-PC CPU %
            let whole_pc_cpu = sys.global_cpu_usage();

            // Whole-PC Disk I/O
            let mut total_disk_r = 0u64;
            let mut total_disk_w = 0u64;
            for disk in &disks {
                let u = disk.usage();
                total_disk_r = total_disk_r.saturating_add(u.read_bytes);
                total_disk_w = total_disk_w.saturating_add(u.written_bytes);
            }
            let whole_pc_disk_kbps = (total_disk_r + total_disk_w) as f32 / 1024.0;

            // Whole-PC Network I/O
            let mut total_net_rx = 0u64;
            let mut total_net_tx = 0u64;
            for (_name, net) in &nets {
                total_net_rx = total_net_rx.saturating_add(net.received());
                total_net_tx = total_net_tx.saturating_add(net.transmitted());
            }
            let whole_pc_net_kbps = (total_net_rx + total_net_tx) as f32 / 1024.0;

            // GPU Utilization
            let gpu_util = get_gpu_utilization();

            // Microphone & Camera Active
            let (mic_active, cam_active, mic_cam_app) = check_mic_camera_active();

            // Windows Power Requests (Sample every 3s)
            if last_requests_check.elapsed() >= Duration::from_secs(3) {
                let (has_disp, _has_sys) = check_power_requests();
                cached_display_request = has_disp;
                last_requests_check = Instant::now();
            }

            let mut trigger_sleep = false;

            {
                let mut s = state.lock().unwrap();

                // Agent Keywords matching
                let agent_keywords: Vec<String> = s
                    .config
                    .agent_keywords
                    .split(',')
                    .map(|w| w.trim().to_lowercase())
                    .filter(|w| !w.is_empty())
                    .collect();

                let download_keywords: Vec<String> = s
                    .config
                    .download_keywords
                    .split(',')
                    .map(|w| w.trim().to_lowercase())
                    .filter(|w| !w.is_empty())
                    .collect();

                let mut max_agent_cpu = 0.0f32;
                let mut max_agent_proc = "None".to_string();
                let mut max_dl_kbps = 0.0f32;
                let mut max_dl_proc = "None".to_string();

                for (_pid, proc) in sys.processes() {
                    let proc_name = proc.name().to_string_lossy().to_lowercase();

                    for kw in &agent_keywords {
                        if proc_name.contains(kw) {
                            let cpu = proc.cpu_usage();
                            if cpu > max_agent_cpu {
                                max_agent_cpu = cpu;
                                max_agent_proc = proc.name().to_string_lossy().to_string();
                            }
                            break;
                        }
                    }

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
                s.live_gpu_pct = gpu_util;
                s.live_download_kbps = max_dl_kbps;
                s.live_download_process_name = max_dl_proc.clone();
                s.live_pc_cpu_pct = whole_pc_cpu;
                s.live_pc_disk_kbps = whole_pc_disk_kbps;
                s.live_pc_net_kbps = whole_pc_net_kbps;
                s.live_mic_active = mic_active;
                s.live_cam_active = cam_active;
                s.live_mic_cam_app = mic_cam_app.clone();
                s.live_power_display_active = cached_display_request;

                // Update 120s rolling history
                if s.history_agent_cpu.len() >= HISTORY_LEN {
                    s.history_agent_cpu.pop_front();
                }
                s.history_agent_cpu.push_back(max_agent_cpu);

                if s.history_gpu.len() >= HISTORY_LEN {
                    s.history_gpu.pop_front();
                }
                s.history_gpu.push_back(gpu_util);

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
                    blockers.push(format!("Active Download ({} at {:.0} KB/s)", max_dl_proc, max_dl_kbps));
                }

                // Condition 4, 5, 6: Whole-PC Metrics
                let cond4_pass = whole_pc_cpu < cfg.pc_cpu_threshold;
                let cond5_pass = whole_pc_disk_kbps < cfg.pc_disk_threshold_kbps;
                let cond6_pass = whole_pc_net_kbps < cfg.pc_net_threshold_kbps;

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

                s.cond5_pc_disk = ConditionStatus {
                    enabled: cfg.cond5_pc_disk_enabled,
                    is_passing: cond5_pass,
                    value_text: format!("{:.0} KB/s", whole_pc_disk_kbps),
                    threshold_text: format!("< {:.0} KB/s", cfg.pc_disk_threshold_kbps),
                    blocker_detail: if !cond5_pass && cfg.cond5_pc_disk_enabled {
                        Some(format!("Whole-PC Disk I/O ({:.0} KB/s >= {:.0} KB/s)", whole_pc_disk_kbps, cfg.pc_disk_threshold_kbps))
                    } else {
                        None
                    },
                };

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

                // Condition 7: Sustained Spike Filter
                let any_pc_metric_tripped = (cfg.cond4_pc_cpu_enabled && !cond4_pass)
                    || (cfg.cond5_pc_disk_enabled && !cond5_pass)
                    || (cfg.cond6_pc_net_enabled && !cond6_pass);

                if any_pc_metric_tripped {
                    consecutive_spikes = consecutive_spikes.saturating_add(1);
                } else {
                    consecutive_spikes = 0;
                }
                s.pc_spike_sustained_counter = consecutive_spikes;

                let spike_sustained = consecutive_spikes >= cfg.spike_filter_secs;
                s.cond7_spike_filter = ConditionStatus {
                    enabled: cfg.cond7_spike_filter_enabled,
                    is_passing: !any_pc_metric_tripped || !spike_sustained,
                    value_text: format!("{}s / {}s", consecutive_spikes, cfg.spike_filter_secs),
                    threshold_text: format!("Debounce {}s", cfg.spike_filter_secs),
                    blocker_detail: if any_pc_metric_tripped && spike_sustained && cfg.cond7_spike_filter_enabled {
                        Some(format!("Sustained PC Spike (Activity exceeded {}s debounce filter)", cfg.spike_filter_secs))
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

                // Condition 8: Monitor Turn-Off Timeout
                s.cond8_monitor_timeout = ConditionStatus {
                    enabled: cfg.cond8_monitor_timeout_enabled,
                    is_passing: true,
                    value_text: format!("{}m", cfg.monitor_timeout_mins),
                    threshold_text: "Standby sync".to_string(),
                    blocker_detail: None,
                };

                // Track Monitor Off time for Energy Savings
                let monitor_timeout_secs = cfg.monitor_timeout_mins * 60;
                if idle_secs >= monitor_timeout_secs {
                    if s.monitor_standby_start.is_none() {
                        s.monitor_standby_start = Some(Instant::now());
                    }
                } else if let Some(start) = s.monitor_standby_start.take() {
                    let dur = start.elapsed().as_secs();
                    if dur > 10 {
                        s.log_energy_event("monitor_off", dur);
                    }
                }

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

                // Condition 10: Watched GPU % (AI Models & Compute)
                let cond10_pass = !cfg.cond10_gpu_enabled || gpu_util < cfg.gpu_threshold_pct;
                s.cond10_gpu = ConditionStatus {
                    enabled: cfg.cond10_gpu_enabled,
                    is_passing: cond10_pass,
                    value_text: format!("{:.1}%", gpu_util),
                    threshold_text: format!("< {:.1}%", cfg.gpu_threshold_pct),
                    blocker_detail: if !cond10_pass && cfg.cond10_gpu_enabled {
                        Some(format!("Watched GPU ({:.1}%)", gpu_util))
                    } else {
                        None
                    },
                };
                if cfg.cond10_gpu_enabled && !cond10_pass {
                    blockers.push(format!("Watched GPU ({:.1}%)", gpu_util));
                }

                // Condition 11: Microphone & Camera Active (Meetings & Calls)
                let cond11_pass = !cfg.cond11_mic_cam_enabled || (!mic_active && !cam_active);
                s.cond11_mic_cam = ConditionStatus {
                    enabled: cfg.cond11_mic_cam_enabled,
                    is_passing: cond11_pass,
                    value_text: if mic_active || cam_active {
                        format!("In Use ({})", if mic_cam_app.is_empty() { "Call/Meeting" } else { &mic_cam_app })
                    } else {
                        "Quiet".to_string()
                    },
                    threshold_text: "Require Idle".to_string(),
                    blocker_detail: if !cond11_pass && cfg.cond11_mic_cam_enabled {
                        Some(format!("Microphone/Camera in use ({})", mic_cam_app))
                    } else {
                        None
                    },
                };
                if cfg.cond11_mic_cam_enabled && !cond11_pass {
                    blockers.push(format!("Mic/Cam In Use ({})", if mic_cam_app.is_empty() { "Meeting" } else { &mic_cam_app }));
                }

                // Check Agent Lease
                if let Some(lease_until) = s.agent_lease_until {
                    if Instant::now() < lease_until {
                        let rem = lease_until.duration_since(Instant::now()).as_secs();
                        let reason = if s.agent_lease_reason.is_empty() { "Agent Task".to_string() } else { s.agent_lease_reason.clone() };
                        blockers.push(format!("Agent Lease: \"{}\" ({}s remaining)", reason, rem));
                    } else {
                        s.agent_lease_until = None;
                        s.agent_lease_reason.clear();
                    }
                }

                // Check Postpone
                if let Some(postponed_until) = s.postponed_until {
                    if Instant::now() < postponed_until {
                        let rem = postponed_until.duration_since(Instant::now()).as_secs();
                        blockers.push(format!("Postponed ({}s remaining)", rem));
                    } else {
                        s.postponed_until = None;
                    }
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

                // Grace Period Alert
                s.grace_active = s.all_conditions_pass && s.countdown_secs <= cfg.grace_period_secs && s.countdown_secs > 0;

                // Auto-save configuration if dirty
                if s.config_dirty {
                    s.config_dirty = false;
                    s.config.save();
                }
            } // unlock state

            // Execute sleep outside of mutex lock
            if trigger_sleep {
                let ok = sleep_now();
                let mut s = state.lock().unwrap();
                if ok {
                    let idle_dur = s.config.idle_threshold_secs as u64;
                    s.log_energy_event("sleep", idle_dur);
                } else {
                    s.set_toast("Sleep trigger returned false (ensure Administrator elevation).");
                }
            }

            egui_ctx.request_repaint();

            let elapsed = loop_start.elapsed();
            if elapsed < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_secs(1) - elapsed);
            }
        }
    });
}

// ----------------------------------------------------------------------------
// GUI Application Implementation
// ----------------------------------------------------------------------------

fn format_time_compact(secs: u32) -> String {
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 {
        format!("{}m {:02}s", m, s)
    } else {
        format!("{}s", s)
    }
}

fn load_tray_icon() -> tray_icon::Icon {
    let png32_bytes = include_bytes!("../assets/icon_32.png");
    if let Ok(img) = image::load_from_memory(png32_bytes) {
        let rgba = img.into_rgba8();
        let (w, h) = rgba.dimensions();
        if let Ok(icon) = tray_icon::Icon::from_rgba(rgba.into_raw(), w, h) {
            return icon;
        }
    }
    let ico_bytes = include_bytes!("../assets/app.ico");
    if let Ok(img) = image::load_from_memory(ico_bytes) {
        let rgba = img.into_rgba8();
        let (w, h) = rgba.dimensions();
        if let Ok(icon) = tray_icon::Icon::from_rgba(rgba.into_raw(), w, h) {
            return icon;
        }
    }
    let fallback = vec![16u8, 185u8, 129u8, 255u8].repeat(32 * 32);
    tray_icon::Icon::from_rgba(fallback, 32, 32).unwrap()
}

pub struct AgentPowerGuardApp {
    state: Arc<Mutex<SharedState>>,
    was_minimized: bool,
    tray_icon: Option<tray_icon::TrayIcon>,
    tray_show_id: tray_icon::menu::MenuId,
    tray_toggle_id: tray_icon::menu::MenuId,
    tray_sleep_id: tray_icon::menu::MenuId,
    tray_exit_id: tray_icon::menu::MenuId,
}

impl AgentPowerGuardApp {
    pub fn new(cc: &eframe::CreationContext<'_>, state: Arc<Mutex<SharedState>>) -> Self {
        let dark_mode = {
            let s = state.lock().unwrap();
            s.config.dark_mode
        };
        cc.egui_ctx.set_visuals(if dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });

        // Initialize System Tray Icon
        let (tray_icon, show_id, toggle_id, sleep_id, exit_id) = {
            let icon = load_tray_icon();
            let menu = tray_icon::menu::Menu::new();
            let show_item = tray_icon::menu::MenuItem::new("Show Agent Power Guard", true, None);
            let toggle_item = tray_icon::menu::MenuItem::new("Toggle Guard ON/OFF", true, None);
            let sleep_item = tray_icon::menu::MenuItem::new("⚡ Test Sleep Now", true, None);
            let exit_item = tray_icon::menu::MenuItem::new("Exit Agent Power Guard", true, None);

            let _ = menu.append_items(&[
                &show_item,
                &toggle_item,
                &sleep_item,
                &exit_item,
            ]);

            let tray = tray_icon::TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("Agent Power Guard")
                .with_icon(icon)
                .build();

            (
                tray.ok(),
                show_item.id().clone(),
                toggle_item.id().clone(),
                sleep_item.id().clone(),
                exit_item.id().clone(),
            )
        };

        // Launch embedded Web Server & Remote API
        start_web_server(Arc::clone(&state), 49281);

        // Launch background watchdog thread
        start_watchdog_thread(Arc::clone(&state), cc.egui_ctx.clone());

        Self {
            state,
            was_minimized: false,
            tray_icon,
            tray_show_id: show_id,
            tray_toggle_id: toggle_id,
            tray_sleep_id: sleep_id,
            tray_exit_id: exit_id,
        }
    }
}

impl eframe::App for AgentPowerGuardApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Intercept Window Close [X]: Hide to System Tray instead of quitting!
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.tray_icon.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                flush_working_set();
                let mut s = self.state.lock().unwrap();
                s.set_toast("Minimized to system tray. Watchdog still active in background.");
            }
        }

        // Handle Tray Icon Left-click or Double-click
        while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
            match event {
                tray_icon::TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                }
                | tray_icon::TrayIconEvent::DoubleClick {
                    button: tray_icon::MouseButton::Left,
                    ..
                } => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                _ => {}
            }
        }

        // Handle Tray Context Menu clicks
        while let Ok(menu_event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if menu_event.id == self.tray_show_id {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else if menu_event.id == self.tray_toggle_id {
                let mut s = self.state.lock().unwrap();
                s.config.guard_enabled = !s.config.guard_enabled;
                s.config_dirty = true;
                if s.config.guard_enabled {
                    s.set_toast("Guard Enabled via Tray");
                } else {
                    s.set_toast("Guard Disabled via Tray");
                }
            } else if menu_event.id == self.tray_sleep_id {
                sleep_now();
            } else if menu_event.id == self.tray_exit_id {
                std::process::exit(0);
            }
        }

        // Update Tray Tooltip dynamically
        if let Some(tray) = &self.tray_icon {
            let s = self.state.lock().unwrap();
            let tooltip = if !s.config.guard_enabled {
                "Agent Power Guard [PAUSED]".to_string()
            } else if s.all_conditions_pass {
                format!(
                    "Agent Power Guard - Sleeping in {} / {}",
                    format_time_compact(s.countdown_secs),
                    format_time_compact(s.countdown_max)
                )
            } else {
                format!("Agent Power Guard - {}", s.blocker_banner_text)
            };
            let _ = tray.set_tooltip(Some(&tooltip));
        }

        // Memory trim when minimized or hidden
        let is_minimized = ctx.input(|i| i.viewport().minimized == Some(true));
        if is_minimized && !self.was_minimized {
            flush_working_set();
        }
        self.was_minimized = is_minimized;

        ctx.request_repaint_after(Duration::from_millis(1000));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut s = self.state.lock().unwrap();

        if ui.visuals().dark_mode != s.config.dark_mode {
            ui.ctx().set_visuals(if s.config.dark_mode {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
        }

        // ====================================================================
        // TOP CONTROL BAR & NAVIGATION
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
                    egui::Color32::from_rgb(34, 139, 34)
                } else {
                    egui::Color32::from_rgb(105, 105, 105)
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

                // Quick Action Buttons
                if ui.button("⚡ Sleep").clicked() {
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
                    s.set_toast("Optimal Power Plan applied.");
                }

                if ui.button("🧹 Clean v3").clicked() {
                    remove_old_v3_scheduled_task();
                    s.set_toast("Cleaned legacy tasks.");
                }

                if ui.button("📥 Tray").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    flush_working_set();
                    s.set_toast("Minimized to system tray. Active in background.");
                }

                let theme_icon = if s.config.dark_mode { "☀ Light" } else { "🌙 Dark" };
                if ui.button(theme_icon).clicked() {
                    s.config.dark_mode = !s.config.dark_mode;
                    s.config_dirty = true;
                }
            });

            ui.add_space(4.0);

            // Tab Navigation Bar
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.active_tab, ActiveTab::Guard, egui::RichText::new("🛡️ Watchdog & Presets").strong());
                ui.selectable_value(&mut s.active_tab, ActiveTab::Energy, egui::RichText::new("💰 Energy & Cost Savings").strong());
                ui.selectable_value(&mut s.active_tab, ActiveTab::Remote, egui::RichText::new("🌐 Web & Mobile Remote").strong());
            });

            // Feature 4: Grace Period Imminent Sleep Alert Banner
            if s.grace_active {
                ui.add_space(4.0);
                let grace_frame = egui::Frame::new()
                    .fill(egui::Color32::from_rgb(180, 83, 9))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(12, 6));
                grace_frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("⚠️ SLEEP IMMINENT: PC sleeping in {} seconds!", s.countdown_secs)).strong().color(egui::Color32::WHITE));
                        if ui.button(egui::RichText::new("⏳ Postpone (+15m)").strong().color(egui::Color32::BLACK)).clicked() {
                            s.postponed_until = Some(Instant::now() + Duration::from_secs(15 * 60));
                            s.set_toast("Sleep postponed for 15 minutes.");
                        }
                        if ui.button(egui::RichText::new("⚡ Sleep Immediately").strong().color(egui::Color32::WHITE)).clicked() {
                            let _ = sleep_now();
                        }
                    });
                });
            }

            // Feature 2: Active Agent Lease Banner
            if let Some(lease_until) = s.agent_lease_until {
                if Instant::now() < lease_until {
                    let rem = lease_until.duration_since(Instant::now()).as_secs();
                    ui.add_space(4.0);
                    let lease_frame = egui::Frame::new()
                        .fill(egui::Color32::from_rgb(30, 64, 175))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(12, 4));
                    lease_frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("🔒 ACTIVE AGENT LEASE: \"{}\" ({}s remaining)", s.agent_lease_reason, rem)).color(egui::Color32::WHITE).strong());
                            if ui.button("Release Lease").clicked() {
                                s.agent_lease_until = None;
                                s.agent_lease_reason.clear();
                                s.set_toast("Agent Lease released.");
                            }
                        });
                    });
                }
            }

            // Toast notification banner
            if let Some((msg, created)) = &s.toast_message.clone() {
                if created.elapsed() < Duration::from_secs(5) {
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(format!("ℹ {}", msg)).color(egui::Color32::LIGHT_BLUE));
                }
            }
            ui.add_space(4.0);
        });

        // ====================================================================
        // TAB 1: WATCHDOG & PRESETS VIEW
        // ====================================================================
        if s.active_tab == ActiveTab::Guard {
            // LEFT PANEL: Conditions & Presets
            egui::Panel::left("conditions_panel")
                .default_size(440.0)
                .min_size(380.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add_space(6.0);
                        ui.heading("Conditions & Thresholds");
                        ui.separator();

                        // Quick Presets
                        ui.group(|ui| {
                            ui.label(egui::RichText::new("⏱ Quick Presets (Staged Power Strategy)").strong());
                            ui.label(egui::RichText::new("Turns monitor off first, then puts PC to deep sleep.").weak().size(11.0));
                            ui.add_space(4.0);

                            ui.horizontal_wrapped(|ui| {
                                let presets: [(&str, u32, u32, &str); 10] = [
                                    ("15s Test", 15, 1, "15s Test Mode"),
                                    ("3m ★", 180, 2, "3m Default"),
                                    ("5m", 300, 3, "5m Preset"),
                                    ("10m", 600, 5, "10m Preset"),
                                    ("15m", 900, 10, "15m Preset"),
                                    ("30m", 1800, 15, "30m Preset"),
                                    ("45m", 2700, 20, "45m Preset"),
                                    ("1h", 3600, 30, "1 Hour Preset"),
                                    ("2h", 7200, 45, "2 Hours Preset"),
                                    ("3h", 10800, 60, "3 Hours Preset"),
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
                                        s.set_toast(format!("Applied {}.", desc));
                                    }
                                }
                            });
                        });

                        ui.add_space(4.0);

                        // Condition 1: Human Input Idle
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond1_idle_enabled, "1. Human Input Idle").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond1_idle.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            ui.label(egui::RichText::new(format!("Mouse & Keyboard Idle: {} (>= {}s)", s.cond1_idle.value_text, s.config.idle_threshold_secs)).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.idle_threshold_secs, 10..=1800).suffix("s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        // Condition 2: Watched Agent CPU %
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond2_agent_cpu_enabled, "2. Watched Agent CPU %").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond2_agent_cpu.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            ui.label(egui::RichText::new(format!("Top Agent: {}", s.cond2_agent_cpu.value_text)).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.agent_cpu_threshold, 0.5..=50.0).suffix("%")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.collapsing("Edit Process Keywords", |ui| {
                                ui.label(egui::RichText::new("Comma-separated matching process names:").size(10.0));
                                if ui.text_edit_multiline(&mut s.config.agent_keywords).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        // Condition 10: Watched GPU % (AI Models & Compute)
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond10_gpu_enabled, "3. Watched GPU % (AI Models)").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond10_gpu.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            ui.label(egui::RichText::new(format!("Live GPU Compute: {:.1}% (Ollama, PyTorch, CUDA, Metal)", s.live_gpu_pct)).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.gpu_threshold_pct, 0.5..=90.0).suffix("%")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        // Condition 3: Active Download / IDM
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond3_download_enabled, "4. Active Download / IDM").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond3_download.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            ui.label(egui::RichText::new(format!("Top Downloader: {}", s.cond3_download.value_text)).weak().size(11.0));
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

                        // Condition 11: Microphone & Camera Active (Calls & Meetings)
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond11_mic_cam_enabled, "5. Mic & Cam (Meetings & Calls)").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond11_mic_cam.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            ui.label(egui::RichText::new(format!("Status: {}", s.cond11_mic_cam.value_text)).weak().size(11.0));
                            ui.label(egui::RichText::new("Prevents sleep during Zoom, Teams, Google Meet, or Discord calls even when mouse is still.").weak().size(10.0));
                        });

                        // Condition 4: Whole-PC CPU %
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond4_pc_cpu_enabled, "6. Whole-PC CPU %").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond4_pc_cpu.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            ui.label(egui::RichText::new(format!("Live Global CPU: {:.1}%", s.live_pc_cpu_pct)).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.pc_cpu_threshold, 5.0..=90.0).suffix("%")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        // Condition 5: Whole-PC Disk I/O
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond5_pc_disk_enabled, "7. Whole-PC Disk I/O").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond5_pc_disk.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            ui.label(egui::RichText::new(format!("Live Disk I/O: {:.0} KB/s", s.live_pc_disk_kbps)).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.pc_disk_threshold_kbps, 100.0..=50000.0).suffix(" KB/s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        // Condition 6: Whole-PC Network
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond6_pc_net_enabled, "8. Whole-PC Network").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond6_pc_net.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            ui.label(egui::RichText::new(format!("Live Network I/O: {:.0} KB/s", s.live_pc_net_kbps)).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.pc_net_threshold_kbps, 10.0..=10000.0).suffix(" KB/s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        // Condition 7: Sustained Spike Filter
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond7_spike_filter_enabled, "9. Sustained Spike Filter").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond7_spike_filter.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            ui.label(egui::RichText::new("Filter brief PC CPU / Disk / Net blips (e.g. Edge tab or wallpaper):").weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Sustain Duration:");
                                if ui.add(egui::Slider::new(&mut s.config.spike_filter_secs, 1..=60).suffix("s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.label(egui::RichText::new(format!("Consecutive Spike Counter: {}s / {}s", s.pc_spike_sustained_counter, s.config.spike_filter_secs)).weak().size(11.0));
                        });

                        // Condition 8: Monitor Timeout
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("10. Monitor Timeout").strong());
                                if ui.button("Apply to Windows").clicked() {
                                    apply_monitor_timeout(s.config.monitor_timeout_mins);
                                    let mins = s.config.monitor_timeout_mins;
                                    s.set_toast(format!("Applied {}m monitor timeout to Windows Power Scheme.", mins));
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Turn Off Display After:");
                                if ui.add(egui::Slider::new(&mut s.config.monitor_timeout_mins, 1..=30).suffix(" min")).changed() {
                                    s.config_dirty = true;
                                    apply_monitor_timeout(s.config.monitor_timeout_mins);
                                }
                            });
                        });

                        // Condition 9: Power Requests
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond9_power_requests_enabled, "11. Power Requests").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let tag = if s.cond9_power_requests.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                    ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                });
                            });
                            if ui.checkbox(&mut s.config.respect_display_requests, "Respect DISPLAY Requests (Video Playback)").changed() {
                                s.config_dirty = true;
                            }
                            ui.label(egui::RichText::new(format!("Display Lock State: {}", if s.live_power_display_active { "Active (Video/Browser)" } else { "None" })).weak().size(11.0));
                            if ui.button("Override Audio/Browser Locks").clicked() {
                                override_audio_browser_power_requests();
                                s.set_toast("Powercfg overrides registered for Audio and Browsers.");
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
                                    s.set_toast("Scheduled Task registered to launch on logon.");
                                } else {
                                    s.set_toast("Scheduled Task removed.");
                                }
                            }
                        });

                        ui.add_space(8.0);
                    });
                });

            // CENTRAL PANEL: Real-Time Graphs & Blocker Inspector
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.heading("Real-Time Graphs (120s Rolling History)");
                    ui.label(egui::RichText::new("— 1 Hz live sampling").color(egui::Color32::GRAY));
                });
                ui.separator();

                let available_h = ui.available_height() - 75.0;
                let chart_h = (available_h / 5.0).clamp(55.0, 110.0);

                // Chart 1: Agent CPU %
                ui.label(egui::RichText::new(format!("1. Agent CPU % (Live: {:.1}%)", s.live_agent_cpu_pct)).strong().size(11.0));
                let agent_pts: PlotPoints = s.history_agent_cpu.iter().enumerate().map(|(i, &v)| [i as f64, v as f64]).collect::<Vec<[f64; 2]>>().into();
                Plot::new("chart_agent_cpu")
                    .height(chart_h)
                    .allow_drag(false).allow_zoom(false).allow_scroll(false)
                    .include_y(0.0).include_y(s.config.agent_cpu_threshold * 1.5)
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new("Agent CPU", agent_pts).color(egui::Color32::from_rgb(0, 191, 255)));
                        plot_ui.hline(HLine::new("Threshold", s.config.agent_cpu_threshold as f64).color(egui::Color32::RED).style(LineStyle::dashed_loose()));
                    });

                // Chart 2: GPU %
                ui.label(egui::RichText::new(format!("2. GPU % (Live: {:.1}%)", s.live_gpu_pct)).strong().size(11.0));
                let gpu_pts: PlotPoints = s.history_gpu.iter().enumerate().map(|(i, &v)| [i as f64, v as f64]).collect::<Vec<[f64; 2]>>().into();
                Plot::new("chart_gpu")
                    .height(chart_h)
                    .allow_drag(false).allow_zoom(false).allow_scroll(false)
                    .include_y(0.0).include_y(s.config.gpu_threshold_pct * 1.5)
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new("GPU Compute", gpu_pts).color(egui::Color32::from_rgb(16, 185, 129)));
                        plot_ui.hline(HLine::new("Threshold", s.config.gpu_threshold_pct as f64).color(egui::Color32::RED).style(LineStyle::dashed_loose()));
                    });

                // Chart 3: Whole-PC CPU %
                ui.label(egui::RichText::new(format!("3. Whole-PC CPU % (Live: {:.1}%)", s.live_pc_cpu_pct)).strong().size(11.0));
                let pc_cpu_pts: PlotPoints = s.history_pc_cpu.iter().enumerate().map(|(i, &v)| [i as f64, v as f64]).collect::<Vec<[f64; 2]>>().into();
                Plot::new("chart_pc_cpu")
                    .height(chart_h)
                    .allow_drag(false).allow_zoom(false).allow_scroll(false)
                    .include_y(0.0).include_y(s.config.pc_cpu_threshold * 1.3)
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new("PC CPU", pc_cpu_pts).color(egui::Color32::from_rgb(147, 112, 219)));
                        plot_ui.hline(HLine::new("Threshold", s.config.pc_cpu_threshold as f64).color(egui::Color32::RED).style(LineStyle::dashed_loose()));
                    });

                // Chart 4: Disk I/O
                ui.label(egui::RichText::new(format!("4. Total Disk I/O (Live: {:.0} KB/s)", s.live_pc_disk_kbps)).strong().size(11.0));
                let disk_pts: PlotPoints = s.history_pc_disk.iter().enumerate().map(|(i, &v)| [i as f64, v as f64]).collect::<Vec<[f64; 2]>>().into();
                Plot::new("chart_disk")
                    .height(chart_h)
                    .allow_drag(false).allow_zoom(false).allow_scroll(false)
                    .include_y(0.0).include_y(s.config.pc_disk_threshold_kbps * 1.3)
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new("Disk KB/s", disk_pts).color(egui::Color32::from_rgb(255, 165, 0)));
                        plot_ui.hline(HLine::new("Threshold", s.config.pc_disk_threshold_kbps as f64).color(egui::Color32::RED).style(LineStyle::dashed_loose()));
                    });

                // Chart 5: Network I/O
                ui.label(egui::RichText::new(format!("5. Total Network I/O (Live: {:.0} KB/s)", s.live_pc_net_kbps)).strong().size(11.0));
                let net_pts: PlotPoints = s.history_pc_net.iter().enumerate().map(|(i, &v)| [i as f64, v as f64]).collect::<Vec<[f64; 2]>>().into();
                Plot::new("chart_net")
                    .height(chart_h)
                    .allow_drag(false).allow_zoom(false).allow_scroll(false)
                    .include_y(0.0).include_y(s.config.pc_net_threshold_kbps * 1.3)
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new("Net KB/s", net_pts).color(egui::Color32::from_rgb(50, 205, 50)));
                        plot_ui.hline(HLine::new("Threshold", s.config.pc_net_threshold_kbps as f64).color(egui::Color32::RED).style(LineStyle::dashed_loose()));
                    });

                ui.add_space(6.0);

                // Blocker Banner at Bottom
                let banner_fill = if !s.config.guard_enabled {
                    egui::Color32::from_rgb(60, 60, 60)
                } else if s.all_conditions_pass {
                    egui::Color32::from_rgb(20, 100, 20)
                } else {
                    egui::Color32::from_rgb(139, 26, 26)
                };

                let banner_frame = egui::Frame::new()
                    .fill(banner_fill)
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(14, 10));

                banner_frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&s.blocker_banner_text).color(egui::Color32::WHITE).strong());
                    });
                });
            });
        }

        // ====================================================================
        // TAB 2: ENERGY & COST SAVINGS DASHBOARD VIEW
        // ====================================================================
        else if s.active_tab == ActiveTab::Energy {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.add_space(4.0);
                ui.heading("💰 Energy & Cost Savings Dashboard");
                ui.label(egui::RichText::new("Track electricity and money saved when Agent Power Guard puts your monitor and workstation into deep standby.").weak());
                ui.separator();

                // 1. Metric Summary Cards
                let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                let filtered_records: Vec<EnergyRecord> = s.energy_history.iter().filter(|r| {
                    match s.energy_filter {
                        EnergyFilter::All => true,
                        EnergyFilter::Today => now_epoch.saturating_sub(r.timestamp) <= 86400,
                        EnergyFilter::Last7Days => now_epoch.saturating_sub(r.timestamp) <= 7 * 86400,
                        EnergyFilter::ThisMonth => now_epoch.saturating_sub(r.timestamp) <= 30 * 86400,
                        EnergyFilter::ThisYear => now_epoch.saturating_sub(r.timestamp) <= 365 * 86400,
                    }
                }).cloned().collect();

                let total_kwh: f64 = filtered_records.iter().map(|r| r.kwh_saved).sum();
                let total_cost: f64 = filtered_records.iter().map(|r| r.cost_saved).sum();
                let total_secs: u64 = filtered_records.iter().map(|r| r.duration_secs).sum();
                let total_co2_kg = total_kwh * 0.4;

                ui.horizontal(|ui| {
                    let card_w = (ui.available_width() - 36.0) / 4.0;

                    let stat_card = |ui: &mut egui::Ui, title: &str, val: String, color: egui::Color32| {
                        egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                            ui.set_width(card_w);
                            ui.label(egui::RichText::new(title).size(11.0).weak());
                            ui.label(egui::RichText::new(val).size(22.0).strong().color(color));
                        });
                    };

                    stat_card(ui, "Money Saved", format!("${:.2}", total_cost), egui::Color32::from_rgb(16, 185, 129));
                    stat_card(ui, "Electricity Saved", format!("{:.1} kWh", total_kwh), egui::Color32::from_rgb(59, 130, 246));
                    stat_card(ui, "Standby Hours", format!("{:.1} hrs", total_secs as f64 / 3600.0), egui::Color32::from_rgb(245, 158, 11));
                    stat_card(ui, "CO₂ Avoided", format!("{:.1} kg", total_co2_kg), egui::Color32::from_rgb(168, 85, 247));
                });

                ui.add_space(8.0);

                // 2. Storage Location & Manual Reset Info
                let storage_path = get_energy_history_path();
                let storage_str = storage_path.to_string_lossy().to_string();

                egui::Frame::new().fill(egui::Color32::from_rgb(15, 23, 42)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("📁 Data Storage File:").strong());
                        ui.label(egui::RichText::new(&storage_str).monospace().color(egui::Color32::LIGHT_BLUE));

                        if ui.button("📂 Open Folder").clicked() {
                            if let Some(parent) = storage_path.parent() {
                                #[cfg(windows)]
                                let _ = Command::new("explorer").arg(parent).status();
                                #[cfg(target_os = "linux")]
                                let _ = Command::new("xdg-open").arg(parent).status();
                                #[cfg(target_os = "macos")]
                                let _ = Command::new("open").arg(parent).status();
                            }
                        }

                        if ui.button("📋 Copy Path").clicked() {
                            ui.ctx().copy_text(storage_str.clone());
                            s.set_toast("Path copied to clipboard.");
                        }
                    });
                    ui.label(egui::RichText::new("💡 To manually reset, edit, or back up data: You can directly open or delete this JSON file anytime. The app will automatically recreate an empty file on the next sleep event.").weak().size(11.0));
                });

                ui.add_space(8.0);

                // 3. Wattage & Cost Settings + Filters & Deletion
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut s.config.record_energy_data, "Record Energy Data").changed() {
                        s.config_dirty = true;
                    }
                    ui.separator();
                    ui.label("Active Power:");
                    if ui.add(egui::DragValue::new(&mut s.config.active_watts).suffix("W").range(20.0..=1000.0)).changed() {
                        s.config_dirty = true;
                    }
                    ui.label("Monitor Power:");
                    if ui.add(egui::DragValue::new(&mut s.config.monitor_watts).suffix("W").range(5.0..=300.0)).changed() {
                        s.config_dirty = true;
                    }
                    ui.label("Electricity Rate:");
                    if ui.add(egui::DragValue::new(&mut s.config.electricity_rate_kwh).prefix("$").suffix("/kWh").range(0.01..=2.0).speed(0.01)).changed() {
                        s.config_dirty = true;
                    }
                });

                ui.add_space(6.0);

                // Filter Buttons & Clear
                ui.horizontal(|ui| {
                    ui.label("Filter Period:");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::All, "All Time");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::Today, "Today");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::Last7Days, "Last 7 Days");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::ThisMonth, "This Month");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::ThisYear, "This Year");

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("⚠️ Delete All Data").color(egui::Color32::RED)).clicked() {
                            s.energy_history.clear();
                            s.save_energy_history();
                            s.set_toast("All energy history deleted.");
                        }

                        if s.energy_filter != EnergyFilter::All {
                            if ui.button("🗑️ Delete Selected Period").clicked() {
                                let filter = s.energy_filter;
                                s.energy_history.retain(|r| {
                                    let matches = match filter {
                                        EnergyFilter::All => true,
                                        EnergyFilter::Today => now_epoch.saturating_sub(r.timestamp) <= 86400,
                                        EnergyFilter::Last7Days => now_epoch.saturating_sub(r.timestamp) <= 7 * 86400,
                                        EnergyFilter::ThisMonth => now_epoch.saturating_sub(r.timestamp) <= 30 * 86400,
                                        EnergyFilter::ThisYear => now_epoch.saturating_sub(r.timestamp) <= 365 * 86400,
                                    };
                                    !matches
                                });
                                s.save_energy_history();
                                s.set_toast("Selected period data deleted.");
                            }
                        }
                    });
                });

                ui.separator();

                // 4. History Table
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if filtered_records.is_empty() {
                        ui.add_space(20.0);
                        ui.label(egui::RichText::new("No energy events recorded for this period yet. Leave the guard running to automatically accumulate power savings!").weak());
                    } else {
                        egui::Grid::new("energy_table")
                            .striped(true)
                            .min_col_width(100.0)
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new("Timestamp").strong());
                                ui.label(egui::RichText::new("Event Type").strong());
                                ui.label(egui::RichText::new("Duration").strong());
                                ui.label(egui::RichText::new("Watts Saved").strong());
                                ui.label(egui::RichText::new("kWh Saved").strong());
                                ui.label(egui::RichText::new("Money Saved").strong());
                                ui.end_row();

                                for record in filtered_records.iter().rev() {
                                    ui.label(&record.start_time);
                                    let tag = if record.event_type == "sleep" { "⚡ Deep Sleep" } else { "🖥 Monitor Standby" };
                                    ui.label(tag);
                                    ui.label(format_time_compact(record.duration_secs as u32));
                                    ui.label(format!("{:.0} W", record.watts_saved_avg));
                                    ui.label(format!("{:.3} kWh", record.kwh_saved));
                                    ui.label(format!("${:.3}", record.cost_saved));
                                    ui.end_row();
                                }
                            });
                    }
                });
            });
        }

        // ====================================================================
        // TAB 3: WEB & MOBILE REMOTE VIEW
        // ====================================================================
        else if s.active_tab == ActiveTab::Remote {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.add_space(4.0);
                ui.heading("🌐 Web & Mobile Remote Dashboard");
                ui.label(egui::RichText::new("Control Agent Power Guard from your phone, tablet, or another browser on your local Wi-Fi.").weak());
                ui.separator();

                let local_url = format!("http://{}:{}", s.local_ip, s.config.web_server_port);

                egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(10.0).inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("📱 Remote Mobile URL:").strong().size(14.0));
                        ui.label(egui::RichText::new(&local_url).strong().size(16.0).color(egui::Color32::from_rgb(52, 211, 153)));

                        if ui.button("🌐 Open in Browser").clicked() {
                            #[cfg(windows)]
                            let _ = Command::new("cmd").args(["/c", "start", &local_url]).creation_flags(CREATE_NO_WINDOW).status();
                            #[cfg(target_os = "linux")]
                            let _ = Command::new("xdg-open").arg(&local_url).status();
                            #[cfg(target_os = "macos")]
                            let _ = Command::new("open").arg(&local_url).status();
                        }

                        if ui.button("📋 Copy Link").clicked() {
                            ui.ctx().copy_text(local_url.clone());
                            s.set_toast("Mobile remote link copied to clipboard.");
                        }
                    });

                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Open this URL on your phone while connected to the same Wi-Fi. You can view live condition graphs, monitor countdown, put the PC to sleep immediately, postpone sleep by 15 minutes, toggle the guard, or grant/release agent leases from your phone!").weak());
                });

                ui.add_space(12.0);

                // Wake-on-LAN Remote Turn-On Card
                egui::Frame::new().fill(egui::Color32::from_rgb(15, 23, 42)).corner_radius(10.0).inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
                    ui.heading("⚡ Turning PC Back On (Wake-on-LAN)");
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("When your PC enters sleep, the CPU is suspended and the web server pauses. To turn it back ON from your phone, you can send a standard Wake-on-LAN (WoL) magic packet using any free app (e.g. 'Wake On Lan' on iOS / Android or your router's admin portal).").weak());
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Target PC MAC Address:").strong());
                        ui.label(egui::RichText::new(&s.mac_address).monospace().color(egui::Color32::from_rgb(96, 165, 250)));
                        if ui.button("📋 Copy MAC").clicked() {
                            ui.ctx().copy_text(s.mac_address.clone());
                            s.set_toast("MAC address copied.");
                        }

                        ui.separator();
                        ui.label(egui::RichText::new("Port:").strong());
                        ui.label(egui::RichText::new("9 (Standard WoL UDP)").monospace());
                    });
                });

                ui.add_space(12.0);

                // HTTP API Endpoints Reference
                egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(10.0).inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
                    ui.heading("🔌 Lightweight Agent Heartbeat API Reference");
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Any AI agent, CLI script, or IDE hook can invoke these endpoints on localhost to control sleep:").weak());
                    ui.add_space(6.0);

                    egui::Grid::new("api_routes_grid").striped(true).min_col_width(120.0).show(ui, |ui| {
                        ui.label(egui::RichText::new("Endpoint").strong());
                        ui.label(egui::RichText::new("Method").strong());
                        ui.label(egui::RichText::new("Description").strong());
                        ui.label(egui::RichText::new("CLI Example").strong());
                        ui.end_row();

                        ui.label("POST /api/lease?mins=30&reason=Build");
                        ui.label("GET / POST");
                        ui.label("Block sleep for N minutes with a named lease");
                        ui.label("agent-power-guard lease 30 \"Build\"");
                        ui.end_row();

                        ui.label("POST /api/release");
                        ui.label("GET / POST");
                        ui.label("Release the active agent lease");
                        ui.label("agent-power-guard release");
                        ui.end_row();

                        ui.label("GET /api/status");
                        ui.label("GET");
                        ui.label("Get live JSON metrics, conditions, countdown");
                        ui.label("agent-power-guard status");
                        ui.end_row();

                        ui.label("POST /api/postpone?mins=15");
                        ui.label("GET / POST");
                        ui.label("Postpone sleep by 15 minutes");
                        ui.label("agent-power-guard postpone 15");
                        ui.end_row();

                        ui.label("POST /api/sleep");
                        ui.label("GET / POST");
                        ui.label("Put the PC to sleep immediately");
                        ui.label("agent-power-guard sleep");
                        ui.end_row();
                    });
                });
            });
        }
    }
}

// ----------------------------------------------------------------------------
// Application Entry Point
// ----------------------------------------------------------------------------

fn main() -> eframe::Result<()> {
    // 1. Check for CLI Subcommands
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        handle_cli_command(&args);
        return Ok(());
    }

    // 2. Load Configuration & Initialize State
    let config = AppConfig::load();
    let state = Arc::new(Mutex::new(SharedState::new(config)));

    // 3. Configure GUI Viewport
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Agent Power Guard")
            .with_inner_size([1040.0, 740.0])
            .with_min_inner_size([780.0, 520.0])
            .with_active(true),
        ..Default::default()
    };

    eframe::run_native(
        "Agent Power Guard",
        native_options,
        Box::new(move |cc| Ok(Box::new(AgentPowerGuardApp::new(cc, state)))),
    )
}
