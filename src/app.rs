#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![recursion_limit = "256"]

// Agent Power Guard - Native Entry Point

pub mod diag;
pub mod evaluator;
pub mod intent;
pub mod probes;
pub mod qr;
pub mod signal;
pub mod watt_calc;

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::diag::{
    build_github_issue_url, open_browser_url, DiagSeverity, DiagnosticPhase, DiagnosticSession,
};
#[cfg(test)]
use crate::evaluator::Decision;
use crate::evaluator::{evaluate, ConditionStatus, EvalConfig, EvaluationState, Metrics};
use crate::intent::{
    get_info_guide_sections, BlockerInfo, MediaPromptAction, MicCamSnapshot, OperationalMode,
    PowerRequestOwner, UserIntent,
};
use crate::signal::SuggestedThresholds;
use crate::watt_calc::{
    build_ai_watt_prompt, compute_power_projection, dedupe_clean_list, format_ram_sticks_summary,
    normalize_currency_input, parse_ai_watt_response, DetectedSystemSpecs, WattCalculatorState,
    MAJOR_CURRENCIES,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use eframe::egui;
use egui_plot::{HLine, Line, LineStyle, Plot, PlotPoints};
use serde::{Deserialize, Serialize};
use sysinfo::{DiskRefreshKind, Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LUID};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
#[cfg(windows)]
use windows_sys::Win32::System::Power::SetSuspendState;
#[cfg(windows)]
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
#[cfg(windows)]
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ,
};
#[cfg(windows)]
use windows_sys::Win32::System::SystemInformation::GetTickCount;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
#[cfg(windows)]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SC_MONITORPOWER, SMTO_ABORTIFHUNG, WM_SYSCOMMAND,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
const HISTORY_LEN: usize = 120;
const DASHBOARD_HTML: &str = include_str!("../assets/dashboard.html");

// ----------------------------------------------------------------------------
// Configuration & Persistence
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerMode {
    Balanced,
    Work,
    Night,
    Away,
    Custom,
}

impl PowerMode {
    pub fn name(&self) -> &'static str {
        match self {
            PowerMode::Balanced => "Balanced",
            PowerMode::Work => "Work",
            PowerMode::Night => "Night",
            PowerMode::Away => "Away",
            PowerMode::Custom => "Custom",
        }
    }

    pub fn apply_to_config(&self, cfg: &mut AppConfig) {
        match self {
            PowerMode::Balanced => {
                cfg.idle_threshold_secs = 180;
                cfg.monitor_timeout_mins = 2;
                cfg.agent_cpu_threshold = 2.0;
                cfg.active_mode = PowerMode::Balanced;
            }
            PowerMode::Work => {
                cfg.idle_threshold_secs = 600;
                cfg.monitor_timeout_mins = 5;
                cfg.agent_cpu_threshold = 1.0;
                cfg.active_mode = PowerMode::Work;
            }
            PowerMode::Night => {
                cfg.idle_threshold_secs = 60;
                cfg.monitor_timeout_mins = 1;
                cfg.agent_cpu_threshold = 1.0;
                cfg.active_mode = PowerMode::Night;
            }
            PowerMode::Away => {
                cfg.idle_threshold_secs = 30;
                cfg.monitor_timeout_mins = 1;
                cfg.agent_cpu_threshold = 1.0;
                cfg.active_mode = PowerMode::Away;
            }
            PowerMode::Custom => {
                cfg.active_mode = PowerMode::Custom;
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeTimer {
    pub id: String,
    pub time_str: String,
    pub label: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerMachine {
    pub id: String,
    pub name: String,
    pub url: String,
    pub token: String,
    pub permission: String, // "control" or "view"
    pub mac: Option<String>,
}

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
    pub gpu_threshold_pct: f32, // 0.5..=90.0, default 5.0
    pub cond11_mic_cam_enabled: bool,
    pub grace_period_secs: u32,    // 5..=60, default 30
    pub record_energy_data: bool,  // default true
    pub active_watts: f32,         // default 150.0
    pub monitor_watts: f32,        // default 50.0
    pub sleep_watts: f32,          // default 3.0
    pub electricity_rate_kwh: f32, // default 0.15
    pub web_server_port: u16,      // default 49281
    pub allow_lan_access: bool,    // default false (loopback only)
    pub ignored_processes: String, // comma-separated ignore list

    // Phase 2.1: Block History
    pub block_history_enabled: bool,
    pub block_history_retention_days: u32,

    // Phase 2.2: Notifications
    pub notify_ntfy_url: String,
    pub notify_ntfy_token: String,
    pub notify_telegram_token: String,
    pub notify_telegram_chat_id: String,
    pub notify_webhook_url: String,
    pub notify_on_sleep_imminent: bool,
    pub notify_on_blocked_long: bool,
    pub notify_blocked_long_hours: u32,
    pub notify_on_lease_ended: bool,
    pub notify_on_pc_woke: bool,
    pub notify_on_guard_paused_long: bool,
    pub notify_guard_paused_hours: u32,

    // Phase 2.3: Opt-in Update Check
    pub check_for_updates: bool,
    pub last_update_check_epoch_secs: u64,

    // Phase 3.1: Modes & Schedules
    pub active_mode: PowerMode,
    pub quiet_hours_enabled: bool,
    pub quiet_hours_start: String,
    pub quiet_hours_end: String,
    pub quiet_hours_mode: PowerMode,
    pub stay_awake_until_epoch_secs: Option<u64>,
    pub sleep_after_lease: bool,

    // Phase 3.2: Wake Timers
    pub wake_timers: Vec<WakeTimer>,

    // Phase 3.3: Laptop Awareness & Battery Profile
    pub battery_profile_enabled: bool,
    pub battery_idle_threshold_secs: u32,
    pub battery_agent_cpu_threshold: f32,
    pub low_battery_auto_sleep_enabled: bool,
    pub low_battery_percent_threshold: u8,

    // Phase 3.4: Accurate Energy, Carbon Grid & Currency
    pub carbon_region: String,
    pub custom_carbon_g_kwh: f32,
    pub currency_symbol: String,

    // Phase 5.3: Multi-Machine Dashboard Peers
    pub peers: Vec<PeerMachine>,

    // Intent-Aware Sleep Engine & Adaptive Controls
    pub operational_mode_name: String, // "smart_guard" | "guard_off" | "best_performance"
    pub gpu_baseline_margin_pct: f32,
    pub filter_busy_count: u32,
    pub filter_quiet_count: u32,
    pub disk_noise_processes: String,
    pub nic_allowlist_override: String,
    pub media_prompt_after_secs: u32,
    pub media_pause_grace_secs: u32,
    pub media_max_lease_hours: u32,
    pub mic_meeting_apps: String,
    pub mic_creative_apps: String,
    pub lease_idle_mins: u32,
    pub lease_auto_expire_enabled: bool,
    pub lease_auto_expire_mins: u32,
    pub ignored_power_owners: String,
    pub protect_monitor_audio: bool,
    pub unattended_wake_secs: u32,
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
            pc_disk_threshold_kbps: 10000.0,
            pc_net_threshold_kbps: 200.0,
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
            allow_lan_access: false,
            ignored_processes: "system, registry, smss, csrss, wininit, services, lsass, svchost, fontdrvhost, dwm".to_string(),

            block_history_enabled: true,
            block_history_retention_days: 14,

            notify_ntfy_url: String::new(),
            notify_ntfy_token: String::new(),
            notify_telegram_token: String::new(),
            notify_telegram_chat_id: String::new(),
            notify_webhook_url: String::new(),
            notify_on_sleep_imminent: true,
            notify_on_blocked_long: true,
            notify_blocked_long_hours: 2,
            notify_on_lease_ended: true,
            notify_on_pc_woke: false,
            notify_on_guard_paused_long: false,
            notify_guard_paused_hours: 2,

            check_for_updates: false,
            last_update_check_epoch_secs: 0,

            active_mode: PowerMode::Balanced,
            quiet_hours_enabled: false,
            quiet_hours_start: "23:00".to_string(),
            quiet_hours_end: "07:00".to_string(),
            quiet_hours_mode: PowerMode::Night,
            stay_awake_until_epoch_secs: None,
            sleep_after_lease: false,

            wake_timers: Vec::new(),

            battery_profile_enabled: true,
            battery_idle_threshold_secs: 60,
            battery_agent_cpu_threshold: 1.0,
            low_battery_auto_sleep_enabled: true,
            low_battery_percent_threshold: 15,

            carbon_region: "Global Average".to_string(),
            custom_carbon_g_kwh: 436.0,
            currency_symbol: "$".to_string(),

            peers: Vec::new(),

            operational_mode_name: "smart_guard".to_string(),
            gpu_baseline_margin_pct: 12.0,
            filter_busy_count: 8,
            filter_quiet_count: 2,
            disk_noise_processes: "msmpeng, mpdefendercoreservice, searchindexer, searchprotocolhost, searchfilterhost, tiworker, trustedinstaller, wuauclt, usoclient, svchost, system, registry, smss, csrss, lsass, vssvc, dwm, fontdrvhost, onedrive, dropbox, googledrivefs".to_string(),
            nic_allowlist_override: String::new(),
            media_prompt_after_secs: 300,
            media_pause_grace_secs: 60,
            media_max_lease_hours: 8,
            mic_meeting_apps: "zoom, teams, ms-teams, webex, discord, slack, meet, whatsapp, skype, facetime".to_string(),
            mic_creative_apps: "premiere, resolve, davinci, audacity, ableton, fl64, reaper, cubase, obs, vegas, audition, protools".to_string(),
            lease_idle_mins: 15,
            lease_auto_expire_enabled: false,
            lease_auto_expire_mins: 30,
            ignored_power_owners: String::new(),
            protect_monitor_audio: true,
            unattended_wake_secs: 120,
        }
    }
}

impl AppConfig {
    pub fn resolved_operational_mode(&self) -> OperationalMode {
        let parsed = OperationalMode::from_api_str(&self.operational_mode_name)
            .unwrap_or(OperationalMode::SmartGuard);
        if !self.guard_enabled && parsed == OperationalMode::SmartGuard {
            OperationalMode::GuardOff
        } else {
            parsed
        }
    }

    pub fn set_operational_mode(&mut self, mode: OperationalMode) {
        self.operational_mode_name = mode.api_key().to_string();
        self.guard_enabled = mode != OperationalMode::GuardOff;
    }

    pub fn to_eval_config(&self, in_quiet_hours: bool) -> EvalConfig {
        let qm_id = match self.quiet_hours_mode {
            PowerMode::Night => 1,
            PowerMode::Away => 2,
            _ => 0,
        };
        EvalConfig {
            operational_mode: self.resolved_operational_mode(),
            guard_enabled: self.guard_enabled,
            idle_threshold_secs: self.idle_threshold_secs,
            agent_cpu_threshold: self.agent_cpu_threshold,
            download_threshold_kbps: self.download_threshold_kbps,
            pc_cpu_threshold: self.pc_cpu_threshold,
            pc_disk_threshold_kbps: self.pc_disk_threshold_kbps,
            pc_net_threshold_kbps: self.pc_net_threshold_kbps,
            spike_filter_secs: self.spike_filter_secs,
            monitor_timeout_mins: self.monitor_timeout_mins,
            respect_display_requests: self.respect_display_requests,
            gpu_threshold_pct: self.gpu_threshold_pct,
            gpu_baseline_margin_pct: self.gpu_baseline_margin_pct,
            grace_period_secs: self.grace_period_secs,

            filter_busy_count: self.filter_busy_count,
            filter_quiet_count: self.filter_quiet_count,
            fast_filter_window_secs: 10,
            fast_filter_busy_count: 4,
            fast_filter_quiet_count: 1,

            cond1_idle_enabled: self.cond1_idle_enabled,
            cond2_agent_cpu_enabled: self.cond2_agent_cpu_enabled,
            cond3_download_enabled: self.cond3_download_enabled,
            cond4_pc_cpu_enabled: self.cond4_pc_cpu_enabled,
            cond5_pc_disk_enabled: self.cond5_pc_disk_enabled,
            cond6_pc_net_enabled: self.cond6_pc_net_enabled,
            cond7_spike_filter_enabled: self.cond7_spike_filter_enabled,
            cond8_monitor_timeout_enabled: self.cond8_monitor_timeout_enabled,
            cond9_power_requests_enabled: self.cond9_power_requests_enabled,
            cond10_gpu_enabled: self.cond10_gpu_enabled,
            cond11_mic_cam_enabled: self.cond11_mic_cam_enabled,

            in_quiet_hours,
            quiet_hours_mode_id: qm_id,
            sleep_after_lease: self.sleep_after_lease,
            battery_profile_enabled: self.battery_profile_enabled,
            battery_idle_threshold_secs: self.battery_idle_threshold_secs,
            battery_agent_cpu_threshold: self.battery_agent_cpu_threshold,
            low_battery_auto_sleep_enabled: self.low_battery_auto_sleep_enabled,
            low_battery_percent_threshold: self.low_battery_percent_threshold,

            media_prompt_after_secs: self.media_prompt_after_secs,
            media_pause_grace_secs: self.media_pause_grace_secs,
            media_max_lease_hours: self.media_max_lease_hours,

            mic_meeting_apps: self.mic_meeting_apps.clone(),
            mic_creative_apps: self.mic_creative_apps.clone(),

            lease_idle_mins: self.lease_idle_mins,
            lease_auto_expire_enabled: self.lease_auto_expire_enabled,
            lease_auto_expire_mins: self.lease_auto_expire_mins,

            ignored_power_owners: self.ignored_power_owners.clone(),
            protect_monitor_audio: self.protect_monitor_audio,
            unattended_wake_secs: self.unattended_wake_secs,
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
            let dir = PathBuf::from(home)
                .join(".config")
                .join("agent-power-guard");
            let _ = std::fs::create_dir_all(&dir);
            return dir.join("config.json");
        }
    }

    PathBuf::from("config.json")
}

fn get_energy_history_path() -> PathBuf {
    get_config_path()
        .parent()
        .unwrap_or(&PathBuf::from("."))
        .join("energy_history.json")
}

fn get_block_history_path() -> PathBuf {
    get_config_path()
        .parent()
        .unwrap_or(&PathBuf::from("."))
        .join("block_history.jsonl")
}

fn get_api_token_path() -> PathBuf {
    get_config_path()
        .parent()
        .unwrap_or(&PathBuf::from("."))
        .join("api_token.txt")
}

#[cfg(unix)]
fn set_user_only_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_user_only_permissions(_path: &std::path::Path) {}

fn fallback_random_token() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut s = String::new();
    for i in 0..4 {
        let mut hasher = DefaultHasher::new();
        SystemTime::now().hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        i.hash(&mut hasher);
        (&s as *const _ as usize).hash(&mut hasher);
        use std::fmt::Write;
        let _ = write!(s, "{:016x}", hasher.finish());
    }
    s.truncate(64);
    s
}

pub fn generate_crypto_token() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        let mut buf = [0u8; 32];
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status == 0 {
            let mut s = String::with_capacity(64);
            for byte in buf {
                use std::fmt::Write;
                let _ = write!(s, "{:02x}", byte);
            }
            return s;
        }
    }

    #[cfg(not(windows))]
    {
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 32];
            if f.read_exact(&mut buf).is_ok() {
                let mut s = String::with_capacity(64);
                for byte in buf {
                    use std::fmt::Write;
                    let _ = write!(s, "{:02x}", byte);
                }
                return s;
            }
        }
    }

    fallback_random_token()
}

pub fn get_or_create_api_token() -> String {
    let path = get_api_token_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        let trimmed = content.trim();
        if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return trimmed.to_string();
        }
    }

    let token = generate_crypto_token();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, &token).is_ok() {
        set_user_only_permissions(&path);
    }
    token
}

pub fn regenerate_api_token() -> String {
    let path = get_api_token_path();
    let token = generate_crypto_token();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, &token).is_ok() {
        set_user_only_permissions(&path);
    }
    token
}

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

use std::collections::HashMap;
use std::net::IpAddr;

pub struct RateLimiter {
    failures: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            failures: Mutex::new(HashMap::new()),
        }
    }

    pub fn check_allowed(&self, ip: IpAddr) -> bool {
        let mut map = self.failures.lock().unwrap();
        let now = Instant::now();
        if let Some(timestamps) = map.get_mut(&ip) {
            timestamps.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
            timestamps.len() < 10
        } else {
            true
        }
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let mut map = self.failures.lock().unwrap();
        let now = Instant::now();
        let timestamps = map.entry(ip).or_default();
        timestamps.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        timestamps.push(now);
    }

    pub fn record_success(&self, ip: IpAddr) {
        let mut map = self.failures.lock().unwrap();
        map.remove(&ip);
    }
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
    History,
    Energy,
    Diagnostic,
    Notifications,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockRecord {
    pub start_epoch_secs: u64,
    pub end_epoch_secs: u64,
    pub duration_secs: u64,
    pub primary_blocker: String,
    pub all_blockers: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TopBlockerStat {
    pub name: String,
    pub total_secs: u64,
    pub count: usize,
}

pub fn calculate_top_blockers(history: &[BlockRecord]) -> Vec<TopBlockerStat> {
    let mut map: std::collections::HashMap<String, (u64, usize)> = std::collections::HashMap::new();
    for rec in history {
        let entry = map.entry(rec.primary_blocker.clone()).or_insert((0, 0));
        entry.0 += rec.duration_secs;
        entry.1 += 1;
    }
    let mut list: Vec<TopBlockerStat> = map
        .into_iter()
        .map(|(name, (total_secs, count))| TopBlockerStat {
            name,
            total_secs,
            count,
        })
        .collect();
    list.sort_by(|a, b| b.total_secs.cmp(&a.total_secs));
    list
}

#[derive(Debug, Clone)]
pub struct LastNightSummary {
    pub total_blocked_secs: u64,
    pub top_blocker: Option<String>,
    pub event_count: usize,
}

pub fn calculate_last_night_summary(history: &[BlockRecord], now_epoch: u64) -> LastNightSummary {
    let window_start = now_epoch.saturating_sub(12 * 3600);
    let overnight_records: Vec<&BlockRecord> = history
        .iter()
        .filter(|r| r.end_epoch_secs >= window_start)
        .collect();

    let total_secs: u64 = overnight_records.iter().map(|r| r.duration_secs).sum();
    let mut map: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for r in &overnight_records {
        *map.entry(r.primary_blocker.clone()).or_insert(0) += r.duration_secs;
    }
    let top_blocker = map
        .into_iter()
        .max_by_key(|(_, secs)| *secs)
        .map(|(name, _)| name);

    LastNightSummary {
        total_blocked_secs: total_secs,
        top_blocker,
        event_count: overnight_records.len(),
    }
}

pub fn export_block_history_csv(history: &[BlockRecord]) -> String {
    let mut csv =
        String::from("Start_Epoch,End_Epoch,Duration_Secs,Primary_Blocker,All_Blockers\n");
    for r in history {
        csv.push_str(&format!(
            "{},{},{},\"{}\",\"{}\"\n",
            r.start_epoch_secs,
            r.end_epoch_secs,
            r.duration_secs,
            r.primary_blocker.replace('"', "\"\""),
            r.all_blockers.join("; ").replace('"', "\"\""),
        ));
    }
    csv
}

pub fn export_block_history_json(history: &[BlockRecord]) -> String {
    serde_json::to_string_pretty(history).unwrap_or_else(|_| "[]".to_string())
}

pub fn format_human_duration(secs: u64) -> String {
    if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{}h {}m", h, m)
    } else if secs >= 60 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{}m {}s", m, s)
    } else {
        format!("{}s", secs)
    }
}

pub fn is_newer_version(remote: &str, current: &str) -> bool {
    let parse_parts = |s: &str| -> Vec<u32> {
        s.trim()
            .trim_start_matches('v')
            .split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .filter_map(|p| p.parse::<u32>().ok())
            .collect()
    };
    let r = parse_parts(remote);
    let c = parse_parts(current);
    for i in 0..r.len().max(c.len()) {
        let rv = r.get(i).copied().unwrap_or(0);
        let cv = c.get(i).copied().unwrap_or(0);
        if rv > cv {
            return true;
        } else if rv < cv {
            return false;
        }
    }
    false
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NotificationEvent {
    SleepImminent { countdown_secs: u32 },
    BlockedLong { hours: u32, blocker: String },
    LeaseEnded { reason: String },
    ZombieLease { reason: String, idle_mins: u64 },
    MediaPrompt { app: String, title: String },
    PcWoke,
    GuardPausedLong { hours: u32 },
    BestPerfLockedLong { hours: u32 },
    TestMessage,
}

impl NotificationEvent {
    pub fn title_and_message(&self) -> (String, String) {
        match self {
            NotificationEvent::SleepImminent { countdown_secs } => (
                "Agent Power Guard: Sleep Imminent".to_string(),
                format!(
                    "All conditions pass. PC will sleep in {} seconds.",
                    countdown_secs
                ),
            ),
            NotificationEvent::BlockedLong { hours, blocker } => (
                "Agent Power Guard: Machine Blocked".to_string(),
                format!(
                    "PC has been kept awake for > {} hours by: {}",
                    hours, blocker
                ),
            ),
            NotificationEvent::LeaseEnded { reason } => (
                "Agent Power Guard: Lease Concluded".to_string(),
                format!("Agent lease '{}' has concluded.", reason),
            ),
            NotificationEvent::ZombieLease { reason, idle_mins } => (
                "Agent Power Guard: Lease Looks Hung".to_string(),
                format!(
                    "PID-bound lease '{}' has used ~0% CPU & disk for {} minutes.",
                    reason, idle_mins
                ),
            ),
            NotificationEvent::MediaPrompt { app, title } => (
                format!("Agent Power Guard: Media in {}", app),
                if title.is_empty() {
                    format!(
                        "Playback detected in {} while idle. Still watching/listening?",
                        app
                    )
                } else {
                    format!(
                        "Playing '{}' in {} while idle. Keep awake or sleep via dashboard.",
                        title, app
                    )
                },
            ),
            NotificationEvent::PcWoke => (
                "Agent Power Guard: System Resumed".to_string(),
                "PC has resumed from sleep and monitoring is active.".to_string(),
            ),
            NotificationEvent::GuardPausedLong { hours } => (
                "Agent Power Guard: Guard Paused".to_string(),
                format!("Watchdog has been in Guard Off mode for > {} hours.", hours),
            ),
            NotificationEvent::BestPerfLockedLong { hours } => (
                "Agent Power Guard: Best Performance Lock Active".to_string(),
                format!(
                    "Best Performance (locked awake) has remained active for > {} hours.",
                    hours
                ),
            ),
            NotificationEvent::TestMessage => (
                "Agent Power Guard: Test Alert".to_string(),
                "This is a test notification from Agent Power Guard.".to_string(),
            ),
        }
    }

    pub fn event_name(&self) -> &'static str {
        match self {
            NotificationEvent::SleepImminent { .. } => "sleep_imminent",
            NotificationEvent::BlockedLong { .. } => "blocked_long",
            NotificationEvent::LeaseEnded { .. } => "lease_ended",
            NotificationEvent::ZombieLease { .. } => "zombie_lease",
            NotificationEvent::MediaPrompt { .. } => "media_prompt",
            NotificationEvent::PcWoke => "pc_woke",
            NotificationEvent::GuardPausedLong { .. } => "guard_paused_long",
            NotificationEvent::BestPerfLockedLong { .. } => "best_perf_locked_long",
            NotificationEvent::TestMessage => "test",
        }
    }
}

pub fn send_notification(config: &AppConfig, event: &NotificationEvent) {
    let ntfy_url = config.notify_ntfy_url.trim().to_string();
    let ntfy_token = config.notify_ntfy_token.trim().to_string();
    let tg_token = config.notify_telegram_token.trim().to_string();
    let tg_chat = config.notify_telegram_chat_id.trim().to_string();
    let webhook_url = config.notify_webhook_url.trim().to_string();

    if ntfy_url.is_empty() && (tg_token.is_empty() || tg_chat.is_empty()) && webhook_url.is_empty()
    {
        return;
    }

    let (title, message) = event.title_and_message();
    let event_name = event.event_name().to_string();
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    std::thread::spawn(move || {
        // 1. ntfy
        if !ntfy_url.is_empty() {
            let mut cmd = Command::new("curl");
            #[cfg(windows)]
            cmd.creation_flags(CREATE_NO_WINDOW);
            cmd.args([
                "-s",
                "--max-time",
                "10",
                "--connect-timeout",
                "5",
                "-X",
                "POST",
            ]);
            cmd.args(["-H", &format!("Title: {}", title)]);
            if !ntfy_token.is_empty() {
                cmd.args(["-H", &format!("Authorization: Bearer {}", ntfy_token)]);
            }
            cmd.args(["-d", &message, &ntfy_url]);
            let _ = cmd.status();
        }

        // 2. Telegram
        if !tg_token.is_empty() && !tg_chat.is_empty() {
            let url = format!("https://api.telegram.org/bot{}/sendMessage", tg_token);
            let mut cmd = Command::new("curl");
            #[cfg(windows)]
            cmd.creation_flags(CREATE_NO_WINDOW);
            cmd.args([
                "-s",
                "--max-time",
                "10",
                "--connect-timeout",
                "5",
                "-X",
                "POST",
                &url,
            ]);
            cmd.args(["-d", &format!("chat_id={}", tg_chat)]);
            cmd.args([
                "--data-urlencode",
                &format!("text={}\n\n{}", title, message),
            ]);
            let _ = cmd.status();
        }

        // 3. Webhook
        if !webhook_url.is_empty() {
            let payload = serde_json::json!({
                "event": event_name,
                "title": title,
                "message": message,
                "timestamp": now_epoch,
            });
            let mut cmd = Command::new("curl");
            #[cfg(windows)]
            cmd.creation_flags(CREATE_NO_WINDOW);
            cmd.args([
                "-s",
                "--max-time",
                "10",
                "--connect-timeout",
                "5",
                "-X",
                "POST",
                &webhook_url,
            ]);
            cmd.args(["-H", "Content-Type: application/json"]);
            cmd.args(["-d", &payload.to_string()]);
            let _ = cmd.status();
        }
    });
}

pub fn check_and_rotate_block_history() {
    let path = get_block_history_path();
    if let Ok(metadata) = std::fs::metadata(&path) {
        if metadata.len() > 5 * 1024 * 1024 {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let lines: Vec<&str> = content.lines().collect();
                if lines.len() > 1000 {
                    let keep = &lines[lines.len() - 1000..];
                    let new_content = keep.join("\n") + "\n";
                    let _ = std::fs::write(&path, new_content);
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Phase 3: Battery Status, Carbon Grid & Scheduling
// ----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    pub has_battery: bool,
    pub on_ac_power: bool,
    pub percent: Option<u8>,
    pub is_charging: bool,
}

pub fn get_battery_status() -> BatteryStatus {
    #[cfg(windows)]
    {
        unsafe {
            let mut status: windows_sys::Win32::System::Power::SYSTEM_POWER_STATUS =
                std::mem::zeroed();
            if windows_sys::Win32::System::Power::GetSystemPowerStatus(&mut status) != 0 {
                let has_battery = (status.BatteryFlag & 128) == 0 && status.BatteryFlag != 255;
                let on_ac = status.ACLineStatus == 1;
                let pct = if status.BatteryLifePercent <= 100 {
                    Some(status.BatteryLifePercent)
                } else {
                    None
                };
                let is_charging = (status.BatteryFlag & 8) != 0;
                return BatteryStatus {
                    has_battery,
                    on_ac_power: on_ac,
                    percent: pct,
                    is_charging,
                };
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(type_str) = std::fs::read_to_string(path.join("type")) {
                    if type_str.trim() == "Battery" {
                        let capacity = std::fs::read_to_string(path.join("capacity"))
                            .ok()
                            .and_then(|s| s.trim().parse::<u8>().ok());
                        let status_str =
                            std::fs::read_to_string(path.join("status")).unwrap_or_default();
                        let is_charging = status_str.trim().eq_ignore_ascii_case("Charging");
                        let on_ac = !status_str.trim().eq_ignore_ascii_case("Discharging");
                        return BatteryStatus {
                            has_battery: true,
                            on_ac_power: on_ac,
                            percent: capacity,
                            is_charging,
                        };
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("pmset").arg("-g").arg("batt").output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let has_battery = text.contains("InternalBattery");
            let on_ac = text.contains("AC Power") || !text.contains("Battery Power");
            let percent = text
                .split('%')
                .next()
                .and_then(|s| s.split_whitespace().last())
                .and_then(|p| p.parse::<u8>().ok());
            return BatteryStatus {
                has_battery,
                on_ac_power: on_ac,
                percent,
                is_charging: text.contains("charging"),
            };
        }
    }
    BatteryStatus {
        has_battery: false,
        on_ac_power: true,
        percent: None,
        is_charging: false,
    }
}

#[derive(Clone, Debug)]
pub struct CarbonRegion {
    pub name: &'static str,
    pub g_per_kwh: f32,
    pub source: &'static str,
}

pub fn get_carbon_regions() -> &'static [CarbonRegion] {
    &[
        CarbonRegion {
            name: "Global Average",
            g_per_kwh: 436.0,
            source: "IEA 2024",
        },
        CarbonRegion {
            name: "United States (Avg)",
            g_per_kwh: 368.0,
            source: "EPA eGRID 2023",
        },
        CarbonRegion {
            name: "US - California (CAMX)",
            g_per_kwh: 205.0,
            source: "EPA eGRID 2023",
        },
        CarbonRegion {
            name: "US - Texas (ERCO)",
            g_per_kwh: 380.0,
            source: "EPA eGRID 2023",
        },
        CarbonRegion {
            name: "European Union (Avg)",
            g_per_kwh: 230.0,
            source: "EEA 2023",
        },
        CarbonRegion {
            name: "United Kingdom",
            g_per_kwh: 148.0,
            source: "National Grid ESO 2023",
        },
        CarbonRegion {
            name: "Germany",
            g_per_kwh: 380.0,
            source: "UBA 2023",
        },
        CarbonRegion {
            name: "France",
            g_per_kwh: 52.0,
            source: "RTE 2023",
        },
        CarbonRegion {
            name: "India",
            g_per_kwh: 713.0,
            source: "CEA 2023",
        },
        CarbonRegion {
            name: "Australia",
            g_per_kwh: 510.0,
            source: "DCCEEW 2023",
        },
        CarbonRegion {
            name: "Custom",
            g_per_kwh: 400.0,
            source: "User Configured",
        },
    ]
}

pub fn get_carbon_factor_kg(config: &AppConfig) -> f64 {
    if config.carbon_region == "Custom" {
        (config.custom_carbon_g_kwh.max(0.0) as f64) / 1000.0
    } else {
        let regions = get_carbon_regions();
        let g = regions
            .iter()
            .find(|r| r.name == config.carbon_region)
            .map(|r| r.g_per_kwh)
            .unwrap_or(436.0);
        (g as f64) / 1000.0
    }
}

pub fn get_local_hour_minute() -> (u32, u32) {
    #[cfg(windows)]
    {
        unsafe {
            let mut st = std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>();
            windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut st);
            (st.wHour as u32, st.wMinute as u32)
        }
    }
    #[cfg(not(windows))]
    {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let secs = epoch % 86400;
        ((secs / 3600) as u32, ((secs % 3600) / 60) as u32)
    }
}

pub fn is_in_quiet_hours(cur_h: u32, cur_m: u32, start_hhmm: &str, end_hhmm: &str) -> bool {
    let cur_mins = cur_h * 60 + cur_m;

    let parse_hhmm = |s: &str| -> Option<u32> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 2 {
            let h = parts[0].trim().parse::<u32>().ok()?;
            let m = parts[1].trim().parse::<u32>().ok()?;
            Some(h * 60 + m)
        } else {
            None
        }
    };

    let start = parse_hhmm(start_hhmm).unwrap_or(23 * 60);
    let end = parse_hhmm(end_hhmm).unwrap_or(7 * 60);

    if start <= end {
        cur_mins >= start && cur_mins < end
    } else {
        cur_mins >= start || cur_mins < end
    }
}

pub fn schedule_os_wake(timer: &WakeTimer) {
    if !timer.enabled {
        return;
    }
    #[cfg(windows)]
    {
        let parts: Vec<&str> = timer.time_str.split(':').collect();
        if parts.len() == 2 {
            let task_name = format!("AgentPowerGuard_Wake_{}", timer.id);
            let time_formatted = format!("{}:00", timer.time_str);
            let ps_cmd = format!(
                "try {{ $action = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit'; \
                 $trigger = New-ScheduledTaskTrigger -Daily -At '{}'; \
                 $settings = New-ScheduledTaskSettingsSet -WakeToRun; \
                 Register-ScheduledTask -TaskName '{}' -Action $action -Trigger $trigger -Settings $settings -Force }} catch {{}}",
                time_formatted, task_name
            );
            let _ = Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("rtcwake").args(["-m", "no", "-l"]).status();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("pmset")
            .args(["schedule", "wake", &timer.time_str])
            .status();
    }
}

pub fn remove_os_wake(timer_id: &str) {
    let _ = timer_id;
    #[cfg(windows)]
    {
        let task_name = format!("AgentPowerGuard_Wake_{}", timer_id);
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", &task_name, "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
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
        let leap = if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
            1
        } else {
            0
        };
        let days_in_year = 365 + leap;
        if d >= days_in_year {
            d -= days_in_year;
            year += 1;
        } else {
            break;
        }
    }
    let leap = if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
        1
    } else {
        0
    };
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
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hours, minutes, seconds
    )
}

// ----------------------------------------------------------------------------
// Shared State & Condition Evaluation
// ----------------------------------------------------------------------------

// ConditionStatus, Metrics, EvaluationState, Decision, and evaluate() live in src/evaluator.rs.

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessCpuInfo {
    pub name: String,
    pub cpu_pct: f32,
    pub is_agent: bool,
    pub is_ignored: bool,
}

#[derive(Debug, Clone)]
pub struct ReleaseAssetInfo {
    pub name: String,
    pub download_url: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ReleaseEntry {
    pub tag: String,
    pub name: String,
    pub published_at: String,
    pub body: String,
    pub prerelease: bool,
    pub assets: Vec<ReleaseAssetInfo>,
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
    pub agent_lease_pid: Option<u32>,

    // Security & Auth
    pub api_token: String,

    // Network / WoL
    pub local_ip: String,
    pub mac_address: String,

    // Energy Tracking
    pub energy_history: Vec<EnergyRecord>,
    pub energy_filter: EnergyFilter,
    pub active_tab: ActiveTab,
    pub monitor_standby_start: Option<Instant>,

    // Phase 2.1: Block History
    pub block_history: Vec<BlockRecord>,
    pub current_block_start_epoch_secs: Option<u64>,
    pub current_primary_blocker: Option<String>,
    pub current_all_blockers: Vec<String>,

    // Phase 2.2: Notifications State & Cooldowns
    pub last_grace_active: bool,
    pub last_sleep_imminent_notif: u64,
    pub last_blocked_long_notif: u64,
    pub last_lease_ended_notif: u64,
    pub last_pc_woke_notif: u64,
    pub last_guard_paused_notif: u64,
    pub guard_paused_since: u64,

    // Phase 2.3: Update Check & In-Panel Auto-Updater
    pub update_available: Option<String>,
    pub update_latest_tag: Option<String>,
    pub update_check_in_progress: bool,
    pub update_check_status_msg: Option<String>,
    pub available_releases: Vec<ReleaseEntry>,
    pub update_download_in_progress: bool,
    pub update_downloaded_path: Option<PathBuf>,
    pub update_downloaded_version: Option<String>,
    pub update_install_prompt_open: bool,

    // Hardware Watt Calculator, AI Spec Prompt & Custom Currency
    pub detected_specs: DetectedSystemSpecs,
    pub watt_calc: WattCalculatorState,
    pub show_watt_calc_modal: bool,
    pub custom_currency_input: String,

    // Built-in Uninstaller UI state
    pub show_uninstall_modal: bool,
    pub uninstall_wipe_data: bool,
    pub uninstall_status_msg: Option<String>,

    // Phase 3: Battery & Power Status
    pub live_battery_status: BatteryStatus,

    // Phase 4: Active & Top Processes
    pub top_processes: Vec<ProcessCpuInfo>,

    // Intent-Aware Sleep Engine State
    pub eval_state: EvaluationState,
    pub operational_mode: OperationalMode,
    pub intent: UserIntent,
    pub structured_blockers: Vec<BlockerInfo>,
    pub show_info_guide: bool,
    pub media_prompt_active: bool,
    pub media_prompt_app: String,
    pub media_prompt_title: String,
    pub media_lock_active: bool,
    pub media_lock_app: String,
    pub active_power_owners: Vec<PowerRequestOwner>,
    pub suppressed_browser_request: Option<String>,
    pub gpu_effective_threshold_pct: f32,
    pub gpu_idle_floor_pct: f32,
    pub gpu_power_mw: Option<u32>,
    pub mic_capture_signal_label: String,
    pub mic_capture_peak: f32,
    pub raw_pc_disk_kbps: f32,
    pub raw_pc_net_kbps: f32,
    pub top_disk_proc: String,
    pub top_disk_kbps: f32,
    pub top_io_debug: Vec<(String, f32)>,
    pub stage1_display_off_allowed: bool,
    pub defer_display_off_for_monitor_audio: bool,
    pub monitor_off_triggered_this_idle: bool,
    pub suggested_thresholds: Option<SuggestedThresholds>,
    pub best_perf_locked_since: u64,
    pub last_media_prompt_notif: u64,

    // UI feedback toast/notification
    pub toast_message: Option<(String, Instant)>,

    // 120s Automated Diagnostic & Issue Reporter State
    pub diag_session: DiagnosticSession,
}

impl SharedState {
    pub fn new(config: AppConfig) -> Self {
        let max_cd = config.idle_threshold_secs;
        let op_mode = config.resolved_operational_mode();
        let init_mon_w = config.monitor_watts;
        let init_sleep_w = config.sleep_watts;
        let init_currency = config.currency_symbol.clone();
        let mut eval_state = EvaluationState::default();
        eval_state.countdown_secs = max_cd;
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

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
            agent_lease_pid: None,
            api_token: get_or_create_api_token(),

            local_ip: get_local_ip(),
            mac_address: "00:00:00:00:00:00".to_string(),

            energy_history: Vec::new(),
            energy_filter: EnergyFilter::All,
            active_tab: ActiveTab::Guard,
            monitor_standby_start: None,

            block_history: Vec::new(),
            current_block_start_epoch_secs: None,
            current_primary_blocker: None,
            current_all_blockers: Vec::new(),

            last_grace_active: false,
            last_sleep_imminent_notif: 0,
            last_blocked_long_notif: 0,
            last_lease_ended_notif: 0,
            last_pc_woke_notif: 0,
            last_guard_paused_notif: 0,
            guard_paused_since: now_s,

            update_available: None,
            update_latest_tag: None,
            update_check_in_progress: false,
            update_check_status_msg: None,
            available_releases: Vec::new(),
            update_download_in_progress: false,
            update_downloaded_path: None,
            update_downloaded_version: None,
            update_install_prompt_open: false,

            detected_specs: DetectedSystemSpecs::default(),
            watt_calc: WattCalculatorState {
                monitor_each_w: init_mon_w,
                sleep_watts: init_sleep_w,
                ..Default::default()
            },
            show_watt_calc_modal: false,
            custom_currency_input: init_currency,

            show_uninstall_modal: false,
            uninstall_wipe_data: false,
            uninstall_status_msg: None,

            live_battery_status: get_battery_status(),
            top_processes: Vec::new(),

            eval_state,
            operational_mode: op_mode,
            intent: UserIntent::default(),
            structured_blockers: Vec::new(),
            show_info_guide: false,
            media_prompt_active: false,
            media_prompt_app: String::new(),
            media_prompt_title: String::new(),
            media_lock_active: false,
            media_lock_app: String::new(),
            active_power_owners: Vec::new(),
            suppressed_browser_request: None,
            gpu_effective_threshold_pct: 5.0,
            gpu_idle_floor_pct: 0.0,
            gpu_power_mw: None,
            mic_capture_signal_label: "Dead/Muted".to_string(),
            mic_capture_peak: 0.0,
            raw_pc_disk_kbps: 0.0,
            raw_pc_net_kbps: 0.0,
            top_disk_proc: "None".to_string(),
            top_disk_kbps: 0.0,
            top_io_debug: Vec::new(),
            stage1_display_off_allowed: false,
            defer_display_off_for_monitor_audio: false,
            monitor_off_triggered_this_idle: false,
            suggested_thresholds: None,
            best_perf_locked_since: now_s,
            last_media_prompt_notif: 0,

            toast_message: None,
            diag_session: DiagnosticSession::default(),
        };

        for _ in 0..HISTORY_LEN {
            s.history_agent_cpu.push_back(0.0);
            s.history_gpu.push_back(0.0);
            s.history_pc_cpu.push_back(0.0);
            s.history_pc_disk.push_back(0.0);
            s.history_pc_net.push_back(0.0);
        }

        s.load_energy_history();
        s.load_block_history();
        s
    }

    pub fn set_toast(&mut self, text: impl Into<String>) {
        self.toast_message = Some((text.into(), Instant::now()));
    }

    pub fn set_operational_mode(&mut self, mode: OperationalMode, duration_hours: Option<u64>) {
        self.config.set_operational_mode(mode);
        self.operational_mode = mode;
        self.config_dirty = true;
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if mode == OperationalMode::BestPerformance {
            self.best_perf_locked_since = now_s;
            self.eval_state.best_perf_until_epoch_secs =
                duration_hours.filter(|&h| h > 0).map(|h| now_s + h * 3600);
        } else {
            self.eval_state.best_perf_until_epoch_secs = None;
        }
        self.set_toast(format!("{}: {}", mode.name(), mode.status_note()));
    }

    pub fn apply_ignore_item(&mut self, kind: &str, target: &str) {
        let trimmed = target.trim().to_lowercase();
        if trimmed.is_empty() {
            return;
        }
        let field = match kind {
            "disk_noise" => &mut self.config.disk_noise_processes,
            "power_owner" => &mut self.config.ignored_power_owners,
            _ => &mut self.config.ignored_processes,
        };
        let mut list: Vec<String> = field
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty() && p.to_lowercase() != trimmed)
            .collect();
        list.push(trimmed.clone());
        *field = list.join(", ");
        self.config_dirty = true;
        self.set_toast(format!("Ignored '{}' ({}).", trimmed, kind));
    }

    pub fn apply_media_action(&mut self, action: MediaPromptAction) -> bool {
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let should_sleep = self.eval_state.apply_media_prompt_action(action, now_s);
        self.media_prompt_active = self.eval_state.media_prompt_active;
        self.media_lock_active = self.eval_state.media_lock_session_key.is_some();
        match action {
            MediaPromptAction::KeepAwake => {
                self.set_toast("Locked awake for this media playback session.");
            }
            MediaPromptAction::Hide => {
                self.set_toast("Media prompt hidden — playback keeps PC awake until stopped.");
            }
            MediaPromptAction::SleepNow => {
                self.set_toast("Sleeping immediately per media prompt choice.");
            }
        }
        should_sleep
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
        let now_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
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

    pub fn load_block_history(&mut self) {
        let path = get_block_history_path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            let now_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let retention_secs = (self.config.block_history_retention_days as u64) * 86400;
            let mut list = Vec::new();
            for line in content.lines() {
                if let Ok(rec) = serde_json::from_str::<BlockRecord>(line) {
                    if retention_secs == 0
                        || now_epoch.saturating_sub(rec.start_epoch_secs) <= retention_secs
                    {
                        list.push(rec);
                    }
                }
            }
            if list.len() > 2000 {
                let excess = list.len() - 2000;
                list.drain(0..excess);
            }
            self.block_history = list;
        }
    }

    pub fn record_block_event(
        &mut self,
        start_epoch: u64,
        end_epoch: u64,
        duration_secs: u64,
        primary_blocker: String,
        all_blockers: Vec<String>,
    ) {
        if !self.config.block_history_enabled || duration_secs < 2 {
            return;
        }

        let record = BlockRecord {
            start_epoch_secs: start_epoch,
            end_epoch_secs: end_epoch,
            duration_secs,
            primary_blocker,
            all_blockers,
        };

        let path = get_block_history_path();
        if let Ok(line) = serde_json::to_string(&record) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "{}", line);
            }
        }

        self.block_history.push(record);
        if self.block_history.len() > 2000 {
            self.block_history.remove(0);
        }
    }

    pub fn clear_block_history(&mut self) {
        self.block_history.clear();
        let path = get_block_history_path();
        let _ = std::fs::remove_file(path);
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

            let mut luid = LUID {
                LowPart: 0,
                HighPart: 0,
            };
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

            let res = SetSuspendState(false, false, false);
            // Settle time for drivers, WDDM, and display adapters upon resuming from sleep
            std::thread::sleep(Duration::from_millis(1500));
            res
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
        // 1. D-Bus org.freedesktop.ScreenSaver (GNOME, KDE Plasma, XFCE under Wayland & X11)
        if let Ok(output) = Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.ScreenSaver",
                "--type=method_call",
                "--print-reply",
                "/org/freedesktop/ScreenSaver",
                "org.freedesktop.ScreenSaver.GetSessionIdleTime",
            ])
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("uint32") {
                        if let Some(val_str) = trimmed.strip_prefix("uint32") {
                            if let Ok(ms) = val_str.trim().parse::<u64>() {
                                return (ms / 1000) as u32;
                            }
                        }
                    }
                }
            }
        }

        // 2. systemd-logind via loginctl
        if let Ok(output) = Command::new("loginctl")
            .args([
                "show-session",
                "self",
                "-p",
                "IdleSinceHintMonotonic",
                "--value",
            ])
            .output()
        {
            if output.status.success() {
                let s = String::from_utf8_lossy(&output.stdout);
                if let Ok(micros) = s.trim().parse::<u64>() {
                    if micros > 0 {
                        return (micros / 1_000_000) as u32;
                    }
                }
            }
        }

        // 3. Fallback to classic X11 xprintidle
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
    unsafe {
        // Native Win32 monitor power-off (SC_MONITORPOWER = 2) with abort-if-hung timeout;
        // avoids spawning powershell + on-the-fly C# compilation under user idle.
        let mut res: usize = 0;
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SYSCOMMAND,
            SC_MONITORPOWER as usize,
            2,
            SMTO_ABORTIFHUNG,
            1000,
            &mut res,
        );
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
    let _ = mins;
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
    let _ = monitor_timeout_mins;
    #[cfg(windows)]
    {
        let _ = Command::new("powercfg")
            .args(["/change", "standby-timeout-ac", "0"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        let _ = Command::new("powercfg")
            .args(["/change", "standby-timeout-dc", "0"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        apply_monitor_timeout(monitor_timeout_mins);
        let _ = Command::new("powercfg")
            .args([
                "/setacvalueindex",
                "SCHEME_CURRENT",
                "SUB_PROCESSOR",
                "PROCTHROTTLEMIN",
                "5",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        let _ = Command::new("powercfg")
            .args([
                "/setacvalueindex",
                "SCHEME_CURRENT",
                "SUB_PROCESSOR",
                "PROCTHROTTLEMAX",
                "100",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        let _ = Command::new("powercfg")
            .args(["/setactive", "SCHEME_CURRENT"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

pub fn override_audio_browser_power_requests() -> Result<usize, String> {
    #[cfg(windows)]
    {
        let callers = [
            ("DRIVER", "High Definition Audio Device"),
            ("DRIVER", "Realtek High Definition Audio"),
            ("DRIVER", "VB-Audio Virtual Cable (ROOT\\MEDIA\\0001)"),
            ("DRIVER", "VB-Audio VoiceMeeter VAIO"),
            ("DRIVER", "FxSound Audio Enhancer (ROOT\\MEDIA\\0000)"),
            ("DRIVER", "USB Audio 2.0"),
            ("DRIVER", "Legacy Kernel Caller"),
            ("PROCESS", "msedge.exe"),
            ("PROCESS", "chrome.exe"),
            ("PROCESS", "brave.exe"),
            ("PROCESS", "firefox.exe"),
            ("PROCESS", "IDMan.exe"),
        ];

        let mut ok_count = 0usize;
        let mut last_err = String::new();
        for (caller_type, name) in callers {
            match Command::new("powercfg")
                .args([
                    "/requestsoverride",
                    caller_type,
                    name,
                    "EXECUTION",
                    "SYSTEM",
                    "AWAYMODE",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
            {
                Ok(out) if out.status.success() => ok_count += 1,
                Ok(out) => {
                    let msg = format!(
                        "{} {}",
                        String::from_utf8_lossy(&out.stderr),
                        String::from_utf8_lossy(&out.stdout)
                    );
                    if !msg.trim().is_empty() {
                        last_err = msg.trim().to_string();
                    }
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }
        }
        if ok_count == 0 {
            let low = last_err.to_lowercase();
            if low.contains("administrator") || low.contains("access") || low.contains("denied") {
                return Err("Access denied: Run Agent Power Guard as Administrator once to register powercfg overrides.".to_string());
            }
            return Err(if last_err.is_empty() {
                "powercfg /requestsoverride failed (Administrator privileges required).".to_string()
            } else {
                format!("powercfg override failed: {}", last_err)
            });
        }
        Ok(ok_count)
    }
    #[cfg(not(windows))]
    {
        Ok(0)
    }
}

pub fn check_power_requests() -> (bool, bool) {
    let snap = probes::query_os_power_requests_once();
    (snap.display_active, snap.system_active)
}

pub fn set_startup_task(enable: bool) {
    let _ = enable;
    #[cfg(windows)]
    {
        let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
            .encode_utf16()
            .collect();
        let val_name: Vec<u16> = "AgentPowerGuard\0".encode_utf16().collect();

        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            ) == 0
            {
                if enable {
                    if let Ok(exe_path) = std::env::current_exe() {
                        let cmd_str = format!("\"{}\" --tray\0", exe_path.to_string_lossy());
                        let cmd_w: Vec<u16> = cmd_str.encode_utf16().collect();
                        let byte_len = (cmd_w.len() * 2) as u32;
                        RegSetValueExW(
                            hkey,
                            val_name.as_ptr(),
                            0,
                            REG_SZ,
                            cmd_w.as_ptr() as *const u8,
                            byte_len,
                        );
                    }
                } else {
                    RegDeleteValueW(hkey, val_name.as_ptr());
                }
                RegCloseKey(hkey);
            }
        }
    }
}

pub fn remove_old_v3_scheduled_task() {
    #[cfg(windows)]
    {
        let _ = Command::new("schtasks")
            .args(["/delete", "/tn", "AgentPowerGuard", "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        let _ = Command::new("schtasks")
            .args(["/delete", "/tn", "AgentPowerGuard_v3", "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
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
// Feature 1: GPU Activity Monitoring (delegates to probes::sample_gpu_probe)
// ----------------------------------------------------------------------------

pub fn get_gpu_utilization() -> f32 {
    probes::sample_gpu_probe().util_pct
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
                ) != 0
                {
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
        if let Ok(output) = Command::new("pactl")
            .args(["list", "short", "source-outputs"])
            .output()
        {
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

fn send_http_json(stream: &mut TcpStream, status_line: &str, body: &str, allow_cors: bool) {
    let cors_hdr = if allow_cors {
        "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Authorization, X-APG-Token, Content-Type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\n"
    } else {
        ""
    };
    let resp = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        status_line,
        cors_hdr,
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn handle_http_client(
    mut stream: TcpStream,
    state: Arc<Mutex<SharedState>>,
    rate_limiter: Arc<RateLimiter>,
) {
    let peer_ip = stream
        .peer_addr()
        .map(|a| a.ip())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let (allow_lan, local_ip, configured_token) = {
        let s = state.lock().unwrap();
        (
            s.config.allow_lan_access,
            s.local_ip.clone(),
            s.api_token.clone(),
        )
    };

    if !peer_ip.is_loopback() && !allow_lan {
        let body = r#"{"error":"forbidden","message":"LAN access is disabled by configuration"}"#;
        send_http_json(&mut stream, "403 Forbidden", body, false);
        return;
    }

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
    let method = parts[0].to_uppercase();
    let path = parts[1];

    let mut host_header = "";
    let mut origin_header = "";
    let mut auth_header = "";
    let mut x_token_header = "";

    for line in req_str.lines().skip(1) {
        let lower = line.to_lowercase();
        if lower.starts_with("host:") {
            host_header = line[5..].trim();
        } else if lower.starts_with("origin:") {
            origin_header = line[7..].trim();
        } else if lower.starts_with("authorization:") {
            auth_header = line[14..].trim();
        } else if lower.starts_with("x-apg-token:") {
            x_token_header = line[12..].trim();
        }
    }

    // Host validation (DNS rebinding protection)
    if !host_header.is_empty() {
        let host_name = host_header
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let is_valid_host = host_name == "localhost"
            || host_name == "127.0.0.1"
            || host_name == "[::1]"
            || host_name == "::1"
            || (!local_ip.is_empty() && host_name == local_ip.to_lowercase());
        if !is_valid_host {
            let body = r#"{"error":"forbidden_host","message":"Host header rejected for DNS rebinding protection"}"#;
            send_http_json(&mut stream, "403 Forbidden", body, false);
            return;
        }
    }

    // CORS preflight for authenticated Fleet View browser polling when LAN access is enabled
    if method == "OPTIONS" && allow_lan && path.starts_with("/api/") {
        let resp = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Authorization, X-APG-Token, Content-Type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    // Origin validation (Reject cross-origin browser requests unless LAN fleet mode + /api/* token auth)
    if !origin_header.is_empty() && !(allow_lan && path.starts_with("/api/")) {
        let stripped_origin = origin_header
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        let origin_host = stripped_origin
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let host_name = host_header
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !origin_host.is_empty() && origin_host != host_name {
            let body = r#"{"error":"cross_origin_forbidden","message":"Cross-origin requests are rejected"}"#;
            send_http_json(&mut stream, "403 Forbidden", body, false);
            return;
        }
    }

    if path == "/" || path == "/index.html" {
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            DASHBOARD_HTML.len(),
            DASHBOARD_HTML
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path == "/manifest.json" {
        let manifest = r##"{
  "name": "Agent Power Guard",
  "short_name": "PowerGuard",
  "start_url": "/",
  "display": "standalone",
  "background_color": "#0f141c",
  "theme_color": "#0f141c",
  "icons": [
    {
      "src": "/icon.png",
      "sizes": "192x192",
      "type": "image/png"
    },
    {
      "src": "/icon.png",
      "sizes": "512x512",
      "type": "image/png"
    }
  ]
}"##;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/manifest+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            manifest.len(),
            manifest
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path == "/icon.png"
        || path == "/apple-touch-icon.png"
        || path == "/apple-touch-icon-precomposed.png"
    {
        let icon_bytes = include_bytes!("../assets/icon_32.png");
        let resp_headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            icon_bytes.len()
        );
        let _ = stream.write_all(resp_headers.as_bytes());
        let _ = stream.write_all(icon_bytes);
        return;
    }

    // Auth verification for all /api/* endpoints
    if path.starts_with("/api/") {
        if !rate_limiter.check_allowed(peer_ip) {
            let body = r#"{"error":"too_many_requests","message":"Rate limit exceeded (max 10 failed auths/min)"}"#;
            let resp = format!("HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 60\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = stream.write_all(resp.as_bytes());
            return;
        }

        let mut supplied_token = None;
        if auth_header.to_lowercase().starts_with("bearer ") {
            supplied_token = Some(auth_header[7..].trim().to_string());
        } else if !x_token_header.is_empty() {
            supplied_token = Some(x_token_header.to_string());
        } else if let Some(t) = parse_query_param(path, "token") {
            supplied_token = Some(t.trim().to_string());
        }

        let token_valid = match supplied_token {
            Some(ref t) => constant_time_eq(t, &configured_token),
            None => false,
        };

        if !token_valid {
            rate_limiter.record_failure(peer_ip);
            let body = r#"{"error":"unauthorized","message":"Missing or invalid API token"}"#;
            let resp = format!("HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nWWW-Authenticate: Bearer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = stream.write_all(resp.as_bytes());
            return;
        }

        rate_limiter.record_success(peer_ip);
    }

    if path == "/api/status" {
        let json = {
            let s = state.lock().unwrap();
            let total_kwh: f64 = s.energy_history.iter().map(|e| e.kwh_saved).sum();
            let total_cost: f64 = s.energy_history.iter().map(|e| e.cost_saved).sum();
            let total_duration: u64 = s.energy_history.iter().map(|e| e.duration_secs).sum();

            let lease_active = s
                .agent_lease_until
                .map(|u| Instant::now() < u)
                .unwrap_or(false);
            let lease_remaining = s
                .agent_lease_until
                .and_then(|u| u.checked_duration_since(Instant::now()))
                .map(|d| d.as_secs())
                .unwrap_or(0);

            serde_json::json!({
                "guard_enabled": s.config.guard_enabled,
                "operational_mode": s.operational_mode.api_key(),
                "operational_mode_name": s.operational_mode.name(),
                "operational_mode_note": s.operational_mode.status_note(),
                "intent": s.intent.code(),
                "intent_label": s.intent.display_label(),
                "stage1_display_off_allowed": s.stage1_display_off_allowed,
                "defer_display_off_for_monitor_audio": s.defer_display_off_for_monitor_audio,
                "all_conditions_pass": s.all_conditions_pass,
                "countdown_secs": s.countdown_secs,
                "countdown_max": s.countdown_max,
                "blocker_banner_text": s.blocker_banner_text,
                "structured_blockers": s.structured_blockers.iter().map(|b| serde_json::json!({
                    "condition": b.condition,
                    "name": b.name,
                    "who": b.who,
                    "value": b.value,
                    "since_secs": b.since_secs,
                    "ignore_target": b.ignore_target,
                    "ignore_kind": b.ignore_kind,
                })).collect::<Vec<_>>(),
                "live_idle_secs": s.live_idle_secs,
                "live_agent_cpu": s.live_agent_cpu_pct,
                "live_agent_proc": s.live_agent_process_name,
                "live_gpu": s.live_gpu_pct,
                "live_download": s.live_download_kbps,
                "live_pc_cpu": s.live_pc_cpu_pct,
                "live_pc_disk": s.live_pc_disk_kbps,
                "live_pc_net": s.live_pc_net_kbps,
                "raw_pc_disk": s.raw_pc_disk_kbps,
                "raw_pc_net": s.raw_pc_net_kbps,
                "top_disk_proc": s.top_disk_proc,
                "top_disk_kbps": s.top_disk_kbps,
                "live_mic_cam_active": s.live_mic_active || s.live_cam_active,
                "live_mic_cam_app": s.live_mic_cam_app,
                "power_request_active": s.live_power_display_active,
                "spike_counter": s.pc_spike_sustained_counter,
                "spike_limit": s.config.spike_filter_secs,
                "cond7_text": s.cond7_spike_filter.value_text,
                "cond9_detail": s.cond9_power_requests.blocker_detail.as_deref().unwrap_or(&s.cond9_power_requests.value_text),
                "cond10_detail": s.cond10_gpu.blocker_detail.as_deref().unwrap_or(&s.cond10_gpu.value_text),
                "cond11_detail": s.cond11_mic_cam.blocker_detail.as_deref().unwrap_or(&s.cond11_mic_cam.value_text),
                "media": {
                    "prompt_active": s.media_prompt_active,
                    "app": s.media_prompt_app,
                    "title": s.media_prompt_title,
                    "lock_active": s.media_lock_active,
                    "lock_app": s.media_lock_app,
                },
                "mic_cam": {
                    "mic_active": s.live_mic_active,
                    "cam_active": s.live_cam_active,
                    "app": s.live_mic_cam_app,
                    "signal_state": s.mic_capture_signal_label,
                    "capture_peak": s.mic_capture_peak,
                },
                "power_requests": {
                    "display_active": s.live_power_display_active,
                    "suppressed_browser": s.suppressed_browser_request,
                    "unavailable": s.cond9_power_requests.probe_unavailable.is_some(),
                    "detail": s.cond9_power_requests.probe_unavailable.as_deref().unwrap_or(&s.cond9_power_requests.value_text),
                    "owners": s.active_power_owners.iter().map(|o| serde_json::json!({
                        "category": o.category,
                        "caller_type": o.caller_type,
                        "name": o.name,
                    })).collect::<Vec<_>>(),
                },
                "gpu": {
                    "util_pct": s.live_gpu_pct,
                    "floor_pct": s.gpu_idle_floor_pct,
                    "effective_threshold_pct": s.gpu_effective_threshold_pct,
                    "power_mw": s.gpu_power_mw,
                    "unavailable": s.cond10_gpu.probe_unavailable.is_some(),
                    "detail": s.cond10_gpu.probe_unavailable.as_deref().unwrap_or(&s.cond10_gpu.value_text),
                },
                "suggested_thresholds": s.suggested_thresholds.map(|sug| serde_json::json!({
                    "pc_cpu": sug.pc_cpu_pct,
                    "disk_kbps": sug.disk_kbps,
                    "net_kbps": sug.net_kbps,
                })),
                "top_io_debug": s.top_io_debug,
                "cond1_pass": s.cond1_idle.is_passing,
                "cond2_pass": s.cond2_agent_cpu.is_passing,
                "cond3_pass": s.cond3_download.is_passing,
                "cond4_pass": s.cond4_pc_cpu.is_passing,
                "cond5_pass": s.cond5_pc_disk.is_passing,
                "cond6_pass": s.cond6_pc_net.is_passing,
                "cond7_pass": s.cond7_spike_filter.is_passing,
                "cond8_pass": s.cond8_monitor_timeout.is_passing,
                "cond9_pass": s.cond9_power_requests.is_passing,
                "cond10_pass": s.cond10_gpu.is_passing,
                "cond11_pass": s.cond11_mic_cam.is_passing,
                "agent_lease_active": lease_active,
                "agent_lease_reason": s.agent_lease_reason,
                "agent_lease_remaining_secs": lease_remaining,
                "agent_lease_pid": s.agent_lease_pid,
                "local_ip": s.local_ip,
                "mac_address": s.mac_address,
                "power_mode": s.config.active_mode.name(),
                "currency_symbol": s.config.currency_symbol,
                "co2_factor": get_carbon_factor_kg(&s.config),
                "carbon_region": s.config.carbon_region,
                "battery": {
                    "has_battery": s.live_battery_status.has_battery,
                    "on_ac_power": s.live_battery_status.on_ac_power,
                    "percent": s.live_battery_status.percent,
                    "is_charging": s.live_battery_status.is_charging,
                },
                "stay_awake_until": s.config.stay_awake_until_epoch_secs,
                "top_processes": s.top_processes,
                "history": {
                    "agent_cpu": s.history_agent_cpu.iter().cloned().collect::<Vec<f32>>(),
                    "gpu": s.history_gpu.iter().cloned().collect::<Vec<f32>>(),
                    "pc_cpu": s.history_pc_cpu.iter().cloned().collect::<Vec<f32>>(),
                    "pc_disk": s.history_pc_disk.iter().cloned().collect::<Vec<f32>>(),
                    "pc_net": s.history_pc_net.iter().cloned().collect::<Vec<f32>>(),
                },
                "energy": {
                    "kwh_saved": total_kwh,
                    "cost_saved": total_cost,
                    "duration_secs": total_duration,
                    "co2_saved": total_kwh * get_carbon_factor_kg(&s.config),
                }
            })
            .to_string()
        };
        send_http_json(&mut stream, "200 OK", &json, allow_lan);
        return;
    }

    if path.starts_with("/api/sleep") {
        {
            let mut s = state.lock().unwrap();
            s.set_toast("Remote command: Sleep Now executed.");
        }
        let _ = sleep_now();
        let body = r#"{"status":"ok","message":"System sleep command issued"}"#;
        send_http_json(&mut stream, "200 OK", body, allow_lan);
        return;
    }

    if path.starts_with("/api/postpone") {
        let mins = parse_query_param(path, "mins")
            .and_then(|m| m.parse::<u64>().ok())
            .unwrap_or(15);
        {
            let mut s = state.lock().unwrap();
            s.postponed_until = Some(Instant::now() + Duration::from_secs(mins * 60));
            s.set_toast(&format!(
                "Sleep postponed by {} minutes via Remote/API.",
                mins
            ));
        }
        let body = format!(
            r#"{{"status":"ok","message":"Postponed for {} minutes","mins":{}}}"#,
            mins, mins
        );
        send_http_json(&mut stream, "200 OK", &body, allow_lan);
        return;
    }

    if path.starts_with("/api/toggle_guard") {
        let (enabled, mode_key) = {
            let mut s = state.lock().unwrap();
            let next_mode = if s.operational_mode == OperationalMode::SmartGuard {
                OperationalMode::GuardOff
            } else {
                OperationalMode::SmartGuard
            };
            s.set_operational_mode(next_mode, None);
            (s.config.guard_enabled, s.operational_mode.api_key())
        };
        let body = format!(
            r#"{{"status":"ok","guard_enabled":{},"operational_mode":"{}"}}"#,
            enabled, mode_key
        );
        send_http_json(&mut stream, "200 OK", &body, allow_lan);
        return;
    }

    if path.starts_with("/api/operational_mode") {
        let mode_str = parse_query_param(path, "mode").unwrap_or_default();
        let hours = parse_query_param(path, "hours").and_then(|h| h.parse::<u64>().ok());
        if let Some(mode) = OperationalMode::from_api_str(&mode_str) {
            let body = {
                let mut s = state.lock().unwrap();
                s.set_operational_mode(mode, hours);
                serde_json::json!({
                    "status": "ok",
                    "mode": mode.api_key(),
                    "name": mode.name(),
                    "note": mode.status_note(),
                    "guard_enabled": s.config.guard_enabled,
                })
                .to_string()
            };
            send_http_json(&mut stream, "200 OK", &body, allow_lan);
        } else {
            send_http_json(
                &mut stream,
                "400 Bad Request",
                r#"{"error":"invalid_mode","message":"Expected smart_guard, guard_off, or best_performance"}"#,
                allow_lan,
            );
        }
        return;
    }

    if path.starts_with("/api/media_prompt") {
        let action_str = parse_query_param(path, "action").unwrap_or_default();
        if let Some(action) = MediaPromptAction::from_api_str(&action_str) {
            let (should_sleep, body) = {
                let mut s = state.lock().unwrap();
                let ss = s.apply_media_action(action);
                let json = serde_json::json!({
                    "status": "ok",
                    "action": action_str,
                    "prompt_active": s.media_prompt_active,
                    "media_lock_active": s.media_lock_active,
                    "sleeping": ss,
                })
                .to_string();
                (ss, json)
            };
            if should_sleep {
                let _ = sleep_now();
            }
            send_http_json(&mut stream, "200 OK", &body, allow_lan);
        } else {
            send_http_json(
                &mut stream,
                "400 Bad Request",
                r#"{"error":"invalid_action","message":"Expected keep, sleep, or hide"}"#,
                allow_lan,
            );
        }
        return;
    }

    if path == "/api/guide" || path.starts_with("/api/guide?") {
        let sections: Vec<_> = get_info_guide_sections()
            .into_iter()
            .map(|sec| serde_json::json!({ "title": sec.title, "body": sec.body }))
            .collect();
        let body = serde_json::json!({ "status": "ok", "sections": sections }).to_string();
        send_http_json(&mut stream, "200 OK", &body, allow_lan);
        return;
    }

    if path.starts_with("/api/lease") {
        let mins = parse_query_param(path, "mins")
            .and_then(|m| m.parse::<u64>().ok())
            .unwrap_or(30);
        let reason =
            parse_query_param(path, "reason").unwrap_or_else(|| "Remote Lease".to_string());
        let pid = parse_query_param(path, "pid").and_then(|p| p.parse::<u32>().ok());
        let mut s = state.lock().unwrap();
        s.agent_lease_until = Some(Instant::now() + Duration::from_secs(mins * 60));
        s.agent_lease_reason = reason.clone();
        s.agent_lease_pid = pid;
        let pid_msg = pid.map(|p| format!(" (PID {})", p)).unwrap_or_default();
        s.set_toast(&format!(
            "Agent Lease granted for {}m: {}{}",
            mins, reason, pid_msg
        ));
        let body = format!(
            r#"{{"status":"ok","lease_mins":{},"reason":"{}","pid":{:?}}}"#,
            mins, reason, pid
        );
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/release") {
        let mut s = state.lock().unwrap();
        s.agent_lease_until = None;
        s.agent_lease_reason.clear();
        s.agent_lease_pid = None;
        s.set_toast("Agent Lease released via Remote/API.");
        let body = r#"{"status":"ok","message":"Agent lease cleared"}"#;
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/energy/clear") {
        let mut s = state.lock().unwrap();
        s.energy_history.clear();
        s.save_energy_history();
        s.set_toast("Energy history cleared via Remote/API.");
        let body = r#"{"status":"ok","message":"Energy records deleted"}"#;
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/history/clear") {
        let mut s = state.lock().unwrap();
        s.clear_block_history();
        s.set_toast("Block reason history cleared via Remote/API.");
        let body = r#"{"status":"ok","message":"Block reason history deleted"}"#;
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/history") {
        let s = state.lock().unwrap();
        let body = match serde_json::to_string(&s.block_history) {
            Ok(json) => json,
            Err(_) => "[]".to_string(),
        };
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/notify/test") {
        let s = state.lock().unwrap();
        send_notification(&s.config, &NotificationEvent::TestMessage);
        let body = r#"{"status":"ok","message":"Test notification dispatched"}"#;
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/mode") {
        let mode_str = parse_query_param(path, "name")
            .unwrap_or_else(|| "balanced".to_string())
            .to_lowercase();
        let mode = match mode_str.as_str() {
            "work" => PowerMode::Work,
            "night" => PowerMode::Night,
            "away" => PowerMode::Away,
            "custom" => PowerMode::Custom,
            _ => PowerMode::Balanced,
        };
        {
            let mut s = state.lock().unwrap();
            mode.apply_to_config(&mut s.config);
            s.config_dirty = true;
            s.set_toast(&format!("Power mode switched to {}", mode.name()));
        }
        let body = format!(r#"{{"status":"ok","mode":"{}"}}"#, mode.name());
        send_http_json(&mut stream, "200 OK", &body, allow_lan);
        return;
    }

    if path.starts_with("/api/schedule/stay_awake") {
        let mins = parse_query_param(path, "mins")
            .and_then(|m| m.parse::<u64>().ok())
            .unwrap_or(60);
        let mut s = state.lock().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        s.config.stay_awake_until_epoch_secs = Some(now + mins * 60);
        s.config_dirty = true;
        s.set_toast(&format!(
            "Scheduled stay-awake active for {} minutes.",
            mins
        ));
        let body = format!(r#"{{"status":"ok","stay_awake_mins":{}}}"#, mins);
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/wake_timer/add") {
        let time_str = parse_query_param(path, "time").unwrap_or_else(|| "02:00".to_string());
        let label =
            parse_query_param(path, "label").unwrap_or_else(|| "Scheduled Wake".to_string());
        let mut s = state.lock().unwrap();
        let id = format!(
            "{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        );
        let timer = WakeTimer {
            id: id.clone(),
            time_str: time_str.clone(),
            label: label.clone(),
            enabled: true,
        };
        schedule_os_wake(&timer);
        s.config.wake_timers.push(timer);
        s.config_dirty = true;
        s.set_toast(&format!("Wake timer scheduled at {}: {}", time_str, label));
        let body = format!(r#"{{"status":"ok","id":"{}","time":"{}"}}"#, id, time_str);
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/wake_timer/delete") {
        if let Some(id) = parse_query_param(path, "id") {
            let mut s = state.lock().unwrap();
            remove_os_wake(&id);
            s.config.wake_timers.retain(|t| t.id != id);
            s.config_dirty = true;
            s.set_toast("Wake timer removed.");
            let body = r#"{"status":"ok","message":"Wake timer deleted"}"#;
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = stream.write_all(resp.as_bytes());
            return;
        }
    }

    if path.starts_with("/api/ignore/add") {
        if let Some(proc) = parse_query_param(path, "process") {
            let kind = parse_query_param(path, "kind").unwrap_or_else(|| "process".to_string());
            let trimmed = proc.trim().to_lowercase();
            if !trimmed.is_empty() {
                let body = {
                    let mut s = state.lock().unwrap();
                    s.apply_ignore_item(&kind, &trimmed);
                    serde_json::json!({
                        "status": "ok",
                        "kind": kind,
                        "target": trimmed,
                        "ignored_processes": s.config.ignored_processes,
                        "disk_noise_processes": s.config.disk_noise_processes,
                        "ignored_power_owners": s.config.ignored_power_owners,
                    })
                    .to_string()
                };
                send_http_json(&mut stream, "200 OK", &body, allow_lan);
                return;
            }
        }
    }

    if path.starts_with("/api/ignore/remove") {
        if let Some(proc) = parse_query_param(path, "process") {
            let mut s = state.lock().unwrap();
            let trimmed = proc.trim().to_lowercase();
            let list: Vec<String> = s
                .config
                .ignored_processes
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty() && p.to_lowercase() != trimmed)
                .collect();
            s.config.ignored_processes = list.join(", ");
            s.config_dirty = true;
            s.set_toast(&format!("Removed '{}' from process ignore list.", trimmed));
            let body = format!(
                r#"{{"status":"ok","ignored_processes":"{}"}}"#,
                s.config.ignored_processes
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            return;
        }
    }

    // Phase 5.3: Multi-Machine Dashboard Peers API
    if path == "/api/peers" || path.starts_with("/api/peers?") {
        let s = state.lock().unwrap();
        let body = serde_json::to_string(&serde_json::json!({
            "status": "ok",
            "peers": s.config.peers,
        }))
        .unwrap_or_else(|_| "{}".to_string());
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/peers/add") {
        let name = parse_query_param(path, "name").unwrap_or_else(|| "Remote PC".to_string());
        let url = parse_query_param(path, "url").unwrap_or_default();
        let token = parse_query_param(path, "token").unwrap_or_default();
        let permission =
            parse_query_param(path, "permission").unwrap_or_else(|| "control".to_string());
        let mac = parse_query_param(path, "mac");

        if !url.is_empty() {
            let mut s = state.lock().unwrap();
            let id = parse_query_param(path, "id").unwrap_or_else(|| {
                format!(
                    "{:x}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                )
            });
            s.config.peers.retain(|p| p.id != id);
            s.config.peers.push(PeerMachine {
                id,
                name,
                url,
                token,
                permission,
                mac,
            });
            s.config_dirty = true;
            s.config.save();
            let body = serde_json::to_string(&serde_json::json!({
                "status": "ok",
                "peers": s.config.peers,
            }))
            .unwrap_or_else(|_| "{}".to_string());
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            return;
        }
    }

    if path.starts_with("/api/peers/remove") {
        if let Some(id) = parse_query_param(path, "id") {
            let mut s = state.lock().unwrap();
            s.config.peers.retain(|p| p.id != id);
            s.config_dirty = true;
            s.config.save();
            let body = serde_json::to_string(&serde_json::json!({
                "status": "ok",
                "peers": s.config.peers,
            }))
            .unwrap_or_else(|_| "{}".to_string());
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            return;
        }
    }

    // Phase 5.4: Wake-on-LAN API
    if path.starts_with("/api/wol") {
        if let Some(mac) = parse_query_param(path, "mac") {
            let bcast = parse_query_param(path, "bcast");
            match send_wol_packet(&mac, bcast.as_deref()) {
                Ok(()) => {
                    let body = serde_json::to_string(&serde_json::json!({
                        "status": "sent",
                        "mac": mac,
                        "bcast": bcast.unwrap_or_else(|| "255.255.255.255".to_string()),
                    }))
                    .unwrap_or_default();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    return;
                }
                Err(err) => {
                    let body = serde_json::to_string(&serde_json::json!({
                        "error": err,
                    }))
                    .unwrap_or_default();
                    let resp = format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    return;
                }
            }
        }
    }

    let body = r#"{"error":"not_found"}"#;
    let resp = format!("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
    let _ = stream.write_all(resp.as_bytes());
}

fn start_web_server(state: Arc<Mutex<SharedState>>, port: u16, rate_limiter: Arc<RateLimiter>) {
    std::thread::spawn(move || {
        let addr = format!("0.0.0.0:{}", port);
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(_) => return,
        };

        for stream in listener.incoming().flatten() {
            let state_clone = Arc::clone(&state);
            let limiter_clone = Arc::clone(&rate_limiter);
            std::thread::spawn(move || {
                handle_http_client(stream, state_clone, limiter_clone);
            });
        }
    });
}

fn send_cli_request(path: &str) -> Result<String, String> {
    let token = get_or_create_api_token();
    let port = AppConfig::load().web_server_port;
    let addr = format!("127.0.0.1:{}", port);
    let mut stream = TcpStream::connect(&addr)
        .map_err(|e| format!("Could not connect to running Agent Power Guard instance on {} (Is the app running?): {}", addr, e))?;
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
        path, addr, token
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut resp = String::new();
    stream
        .read_to_string(&mut resp)
        .map_err(|e| e.to_string())?;
    if let Some(idx) = resp.find("\r\n\r\n") {
        Ok(resp[idx + 4..].to_string())
    } else {
        Ok(resp)
    }
}

// Phase 1.1: Process-bound Run Wrapper
fn run_cli_wrapper(args: &[String]) {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(0xFFFFFFFF);
    }

    let mut max_mins: u64 = 60;
    let mut reason: Option<String> = None;
    let mut cmd_parts = Vec::new();

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--" {
            cmd_parts.extend_from_slice(&args[i + 1..]);
            break;
        } else if args[i] == "--max-mins" && i + 1 < args.len() {
            max_mins = args[i + 1].parse::<u64>().unwrap_or(60);
            i += 2;
        } else if args[i] == "--reason" && i + 1 < args.len() {
            reason = Some(args[i + 1].clone());
            i += 2;
        } else {
            cmd_parts.extend_from_slice(&args[i..]);
            break;
        }
    }

    if cmd_parts.is_empty() {
        eprintln!(
            "Usage: agent-power-guard run [--max-mins N] [--reason \"text\"] -- <command...>"
        );
        std::process::exit(1);
    }

    let program = &cmd_parts[0];
    let prog_args = &cmd_parts[1..];
    let lease_reason = reason.unwrap_or_else(|| cmd_parts.join(" "));

    println!(
        "[EXEC] [Agent Power Guard] Spawning child: {} {:?}",
        program, prog_args
    );
    let mut child = match Command::new(program).args(prog_args).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to spawn child command '{}': {}", program, e);
            std::process::exit(1);
        }
    };

    let pid = child.id();
    let encoded_reason = lease_reason.replace(' ', "%20");
    let path = format!(
        "/api/lease?mins={}&reason={}&pid={}",
        max_mins, encoded_reason, pid
    );
    if let Err(e) = send_cli_request(&path) {
        eprintln!(
            "Warning: Failed to acquire PID lease from Agent Power Guard watchdog: {}",
            e
        );
    } else {
        println!(
            "[LOCK] [Agent Power Guard] Acquired PID-bound lease (PID {}, {}m cap): {}",
            pid, max_mins, lease_reason
        );
    }

    let status = child.wait();
    let _ = send_cli_request("/api/release");
    println!("[RELEASE] [Agent Power Guard] Child process exited. Lease released.");

    match status {
        Ok(st) => {
            std::process::exit(st.code().unwrap_or(0));
        }
        Err(_) => {
            std::process::exit(1);
        }
    }
}

// Phase 1.2: MCP Stdio Server
fn run_mcp_server() {
    use std::io::{BufRead, BufReader};
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin);

    let tools_list = serde_json::json!({
        "tools": [
            {
                "name": "keep_awake",
                "description": "Acquire an agent lease to keep the workstation awake while running builds, tests, or AI workflows",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "minutes": { "type": "number", "description": "Number of minutes to keep machine awake (e.g. 15, 30, 60)" },
                        "reason": { "type": "string", "description": "Task description or agent step name" }
                    },
                    "required": ["minutes"]
                }
            },
            {
                "name": "release",
                "description": "Release the active agent lease and allow workstation to sleep normally",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "status",
                "description": "Get current watchdog status, countdown, idle metrics, and blockers",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "postpone",
                "description": "Postpone workstation sleep by N minutes without a named lease",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "minutes": { "type": "number", "description": "Number of minutes to postpone" }
                    }
                }
            }
        ]
    });

    let mut line = String::new();
    while let Ok(n) = reader.read_line(&mut line) {
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        if let Ok(req) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

            match method {
                "initialize" => {
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": "2024-11-05",
                            "capabilities": {
                                "tools": {}
                            },
                            "serverInfo": {
                                "name": "agent-power-guard",
                                "version": "1.1.0"
                            }
                        }
                    });
                    println!("{}", resp);
                }
                "notifications/initialized" => {}
                "tools/list" => {
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": tools_list
                    });
                    println!("{}", resp);
                }
                "tools/call" => {
                    let params = req.get("params");
                    let tool_name = params
                        .and_then(|p| p.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    let arguments = params
                        .and_then(|p| p.get("arguments"))
                        .cloned()
                        .unwrap_or(serde_json::json!({}));

                    let result_text = match tool_name {
                        "keep_awake" => {
                            let mins = arguments
                                .get("minutes")
                                .and_then(|m| m.as_u64())
                                .unwrap_or(30);
                            let reason = arguments
                                .get("reason")
                                .and_then(|r| r.as_str())
                                .unwrap_or("MCP Agent Task");
                            let path = format!(
                                "/api/lease?mins={}&reason={}",
                                mins,
                                reason.replace(' ', "%20")
                            );
                            send_cli_request(&path).unwrap_or_else(|e| format!("Error: {}", e))
                        }
                        "release" => send_cli_request("/api/release")
                            .unwrap_or_else(|e| format!("Error: {}", e)),
                        "status" => send_cli_request("/api/status")
                            .unwrap_or_else(|e| format!("Error: {}", e)),
                        "postpone" => {
                            let mins = arguments
                                .get("minutes")
                                .and_then(|m| m.as_u64())
                                .unwrap_or(15);
                            let path = format!("/api/postpone?mins={}", mins);
                            send_cli_request(&path).unwrap_or_else(|e| format!("Error: {}", e))
                        }
                        _ => format!("Unknown tool: {}", tool_name),
                    };

                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": result_text
                                }
                            ]
                        }
                    });
                    println!("{}", resp);
                }
                _ => {
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32601,
                            "message": format!("Method not found: {}", method)
                        }
                    });
                    println!("{}", resp);
                }
            }
        }
        line.clear();
    }
}

// Phase 1.3: Agent Profiles
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub keywords: String,
    pub agent_cpu_threshold: f32,
    pub gpu_threshold_pct: f32,
}

pub fn get_built_in_profiles() -> Vec<AgentProfile> {
    vec![
        AgentProfile {
            name: "Claude Code".to_string(),
            description: "Watches Claude Code CLI and background worker processes".to_string(),
            keywords: "claude, code".to_string(),
            agent_cpu_threshold: 2.0,
            gpu_threshold_pct: 5.0,
        },
        AgentProfile {
            name: "Cursor".to_string(),
            description: "Watches Cursor IDE and Language Server indexers".to_string(),
            keywords: "cursor, code, language_server".to_string(),
            agent_cpu_threshold: 2.0,
            gpu_threshold_pct: 5.0,
        },
        AgentProfile {
            name: "Antigravity".to_string(),
            description: "Watches Antigravity IDE agent runner and MCP sidecars".to_string(),
            keywords: "antigravity, language_server".to_string(),
            agent_cpu_threshold: 2.0,
            gpu_threshold_pct: 5.0,
        },
        AgentProfile {
            name: "Ollama & Local LLM".to_string(),
            description: "Watches Ollama, llama.cpp, and local GPU inference runners".to_string(),
            keywords: "ollama, llama, vllm".to_string(),
            agent_cpu_threshold: 5.0,
            gpu_threshold_pct: 10.0,
        },
        AgentProfile {
            name: "Docker & Containers".to_string(),
            description: "Watches Docker daemon, containerd, and WSL2 background tasks".to_string(),
            keywords: "docker, dockerd, containerd, wsl".to_string(),
            agent_cpu_threshold: 3.0,
            gpu_threshold_pct: 5.0,
        },
        AgentProfile {
            name: "Rust / Cargo Toolchain".to_string(),
            description: "Watches cargo builds, rustc, and rust-analyzer".to_string(),
            keywords: "cargo, rustc, rust-analyzer".to_string(),
            agent_cpu_threshold: 5.0,
            gpu_threshold_pct: 5.0,
        },
        AgentProfile {
            name: "Node / Web Toolchain".to_string(),
            description: "Watches node, npm, pnpm, bun, tsc, vite, and next dev servers"
                .to_string(),
            keywords: "node, npm, pnpm, bun, tsc, esbuild, vite".to_string(),
            agent_cpu_threshold: 3.0,
            gpu_threshold_pct: 5.0,
        },
        AgentProfile {
            name: "Python ML".to_string(),
            description: "Watches python training scripts, PyTorch, and Jupyter kernels"
                .to_string(),
            keywords: "python, torch, jupyter".to_string(),
            agent_cpu_threshold: 5.0,
            gpu_threshold_pct: 10.0,
        },
    ]
}

pub fn send_wol_packet(mac_str: &str, broadcast_ip: Option<&str>) -> Result<(), String> {
    let mac_clean = mac_str.replace([':', '-', '.'], "");
    if mac_clean.len() != 12 {
        return Err("Invalid MAC address length (expected 12 hex characters)".to_string());
    }
    let mut mac_bytes = [0u8; 6];
    for i in 0..6 {
        mac_bytes[i] = u8::from_str_radix(&mac_clean[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("Invalid hex digit in MAC address: {}", e))?;
    }
    let mut packet = [0u8; 102];
    packet[0..6].fill(0xFF);
    for i in 0..16 {
        let start = 6 + i * 6;
        packet[start..start + 6].copy_from_slice(&mac_bytes);
    }
    let dest = format!("{}:9", broadcast_ip.unwrap_or("255.255.255.255"));
    let socket =
        UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("Failed to bind UDP socket: {}", e))?;
    socket
        .set_broadcast(true)
        .map_err(|e| format!("Failed to set broadcast on UDP socket: {}", e))?;
    socket
        .send_to(&packet, &dest)
        .map_err(|e| format!("Failed to send magic packet: {}", e))?;
    Ok(())
}

fn handle_cli_command(args: &[String]) {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(0xFFFFFFFF);
    }
    let cmd = args[1].to_lowercase();
    match cmd.as_str() {
        "run" => {
            run_cli_wrapper(&args[2..]);
        }
        "mcp" => {
            run_mcp_server();
        }
        "lease" => {
            let mins = args
                .get(2)
                .and_then(|m| m.parse::<u32>().ok())
                .unwrap_or(30);
            let reason = args
                .get(3)
                .cloned()
                .unwrap_or_else(|| "CLI Lease".to_string());
            let encoded_reason = reason.replace(' ', "%20");
            match send_cli_request(&format!(
                "/api/lease?mins={}&reason={}",
                mins, encoded_reason
            )) {
                Ok(resp) => println!("Success: {}", resp),
                Err(err) => eprintln!("Error: {}", err),
            }
        }
        "release" => match send_cli_request("/api/release") {
            Ok(resp) => println!("Success: {}", resp),
            Err(err) => eprintln!("Error: {}", err),
        },
        "status" => match send_cli_request("/api/status") {
            Ok(resp) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                    println!("==================================================");
                    println!("           AGENT POWER GUARD - STATUS             ");
                    println!("==================================================");
                    println!("Guard Enabled    : {}", v["guard_enabled"]);
                    println!(
                        "All Conditions   : {}",
                        if v["all_conditions_pass"] == true {
                            "PASSING (Arming Sleep)"
                        } else {
                            "BLOCKED"
                        }
                    );
                    println!(
                        "Countdown        : {}s / {}s",
                        v["countdown_secs"], v["countdown_max"]
                    );
                    println!("Banner           : {}", v["blocker_banner_text"]);
                    println!("Human Idle       : {}s", v["live_idle_secs"]);
                    println!(
                        "Agent CPU        : {:.1}% ({})",
                        v["live_agent_cpu"], v["live_agent_proc"]
                    );
                    println!("GPU Compute      : {:.1}%", v["live_gpu"]);
                    println!("Whole-PC CPU     : {:.1}%", v["live_pc_cpu"]);
                    println!("Whole-PC Disk    : {:.0} KB/s", v["live_pc_disk"]);
                    println!("Whole-PC Network : {:.0} KB/s", v["live_pc_net"]);
                    println!("Mic/Cam In Use   : {}", v["live_mic_cam_active"]);
                    println!(
                        "Agent Lease      : {} ({})",
                        v["agent_lease_active"], v["agent_lease_reason"]
                    );
                    if let Some(pid) = v["agent_lease_pid"].as_u64() {
                        println!("Bound Process PID: {}", pid);
                    }
                    let port = AppConfig::load().web_server_port;
                    println!("Local Web URL    : http://{}:{}", v["local_ip"], port);
                    println!("==================================================");
                } else {
                    println!("{}", resp);
                }
            }
            Err(err) => eprintln!("Error: {}", err),
        },
        "sleep" => match send_cli_request("/api/sleep") {
            Ok(resp) => println!("Success: {}", resp),
            Err(err) => eprintln!("Error: {}", err),
        },
        "postpone" => {
            let mins = args
                .get(2)
                .and_then(|m| m.parse::<u32>().ok())
                .unwrap_or(15);
            match send_cli_request(&format!("/api/postpone?mins={}", mins)) {
                Ok(resp) => println!("Success: {}", resp),
                Err(err) => eprintln!("Error: {}", err),
            }
        }
        "wol" => {
            let mac = args.get(2).map(|s| s.as_str()).unwrap_or("");
            let bcast = args.get(3).map(|s| s.as_str());
            if mac.is_empty() {
                eprintln!("Usage: agent-power-guard wol <mac_address> [broadcast_ip]");
            } else {
                match send_wol_packet(mac, bcast) {
                    Ok(()) => println!(
                        "Magic packet successfully sent to {} (target broadcast: {})",
                        mac,
                        bcast.unwrap_or("255.255.255.255:9")
                    ),
                    Err(err) => eprintln!("Error sending WoL packet: {}", err),
                }
            }
        }
        "daemon" | "service-run" => {
            run_headless_daemon();
        }
        "service" => {
            let sub = args.get(2).map(|s| s.as_str()).unwrap_or("status");
            match sub {
                "install" => install_service(),
                "uninstall" => uninstall_service(),
                "run" => run_headless_daemon(),
                "status" => service_status(),
                _ => eprintln!("Usage: agent-power-guard service [install|uninstall|status|run]"),
            }
        }
        "relay" => {
            let port = args
                .get(2)
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(49282);
            run_wake_relay(port, None);
        }
        "help" | "--help" | "-h" => {
            println!("Agent Power Guard CLI");
            println!();
            println!("Usage:");
            println!("  agent-power-guard run [--max-mins N] [--reason \"text\"] -- <cmd...>  - Keep awake while command runs (process-bound lease)");
            println!("  agent-power-guard mcp                                              - Launch stdio Model Context Protocol (MCP) server");
            println!("  agent-power-guard lease [mins] [reason]                            - Block sleep with an active agent lease");
            println!("  agent-power-guard release                                          - Release active agent lease");
            println!("  agent-power-guard status                                           - Query watchdog status");
            println!("  agent-power-guard postpone [mins]                                  - Postpone sleep by N minutes");
            println!("  agent-power-guard sleep                                            - Trigger immediate system sleep");
            println!("  agent-power-guard wol <mac> [broadcast]                            - Send Wake-on-LAN magic packet to wake remote PC");
            println!("  agent-power-guard daemon                                           - Run headless background watchdog daemon");
            println!("  agent-power-guard service install                                  - Register autostart background service");
            println!("  agent-power-guard service uninstall                                - Unregister background service");
            println!("  agent-power-guard service status                                   - Check background service status");
            println!("  agent-power-guard relay [port]                                     - Launch Wake-on-LAN micro-relay server");
            println!("  agent-power-guard help                                             - Show this help message");
        }
        _ => {
            eprintln!(
                "Unknown command: '{}'. Run 'agent-power-guard help' for options.",
                cmd
            );
        }
    }
}

// ----------------------------------------------------------------------------
// Background Watchdog Engine (1 Hz Polling)
// ----------------------------------------------------------------------------

pub fn start_watchdog_thread(state: Arc<Mutex<SharedState>>, egui_ctx: Option<egui::Context>) {
    std::thread::spawn(move || {
        run_watchdog_loop(state, egui_ctx);
    });
}

pub fn run_watchdog_loop(state: Arc<Mutex<SharedState>>, egui_ctx: Option<egui::Context>) {
    let mut sys = System::new().expect("sysinfo System::new");
    let mut nets = Networks::new_with_refreshed_list().expect("Networks::new");
    let mut disks =
        Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_io_usage())
            .expect("Disks::new");

    // Warm up CPU usage
    sys.refresh_cpu_usage();
    std::thread::sleep(Duration::from_millis(500));

    // Populate MAC address once
    {
        let mut s = state.lock().unwrap();
        s.mac_address = get_primary_mac(&nets);
    }

    let audio_media_shared = probes::start_audio_media_worker();
    let power_req_shared = probes::start_power_request_worker();
    let wake_tracker = probes::WakeEventTracker::start();
    let wake_lock = probes::WakeLockController::start();
    let mut cached_default_nics = probes::query_default_route_nic_names();
    let mut last_nic_refresh = Instant::now();
    let mut last_best_perf_notif = 0u64;

    loop {
        let loop_start = Instant::now();

        if last_nic_refresh.elapsed() >= Duration::from_secs(10) {
            cached_default_nics = probes::query_default_route_nic_names();
            last_nic_refresh = Instant::now();
        }

        // 1. Sample OS Telemetry & Wall-Clock Time
        let now_wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let idle_secs = get_idle_seconds();

        sys.refresh_cpu_usage();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_disk_usage(),
        );

        disks.refresh_specifics(false, DiskRefreshKind::nothing().with_io_usage());
        nets.refresh(false);

        let whole_pc_cpu = sys.global_cpu_usage();

        let mut total_disk_r = 0u64;
        let mut total_disk_w = 0u64;
        for disk in &disks {
            let u = disk.usage();
            total_disk_r = total_disk_r.saturating_add(u.read_bytes);
            total_disk_w = total_disk_w.saturating_add(u.written_bytes);
        }
        let whole_pc_disk_kbps = (total_disk_r + total_disk_w) as f32 / 1024.0;

        let gpu_reading = probes::sample_gpu_probe();
        let (mic_held, cam_held, mic_cam_app) = check_mic_camera_active();
        let audio_reading = audio_media_shared
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let power_req_snap = power_req_shared
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let batt = get_battery_status();
        let (local_h, local_m) = get_local_hour_minute();

        let trigger_sleep;
        let mut trigger_monitor_off = false;
        let mut launch_diag_media = false;
        let hold_os_wake_lock;
        let mut wake_sleep_stabilize = false;

        {
            let mut s = state.lock().unwrap();

            let woke_now = s.last_wall_clock_secs > 0 && now_wall > s.last_wall_clock_secs + 10;
            if woke_now {
                s.monitor_off_triggered_this_idle = false;
                wake_sleep_stabilize = true;
            }
            s.last_wall_clock_secs = now_wall;
            if idle_secs < 10 {
                s.monitor_off_triggered_this_idle = false;
            }

            let wake_cls = if woke_now {
                let schedule_pairs: Vec<(bool, String)> = s
                    .config
                    .wake_timers
                    .iter()
                    .map(|t| (t.enabled, t.time_str.clone()))
                    .collect();
                let schedule_refs: Vec<(bool, &str)> = schedule_pairs
                    .iter()
                    .map(|(en, ts)| (*en, ts.as_str()))
                    .collect();
                Some(wake_tracker.classify_resume(
                    now_wall,
                    idle_secs,
                    &schedule_refs,
                    local_h,
                    local_m,
                ))
            } else {
                None
            };

            let split_csv = |raw: &str| -> Vec<String> {
                raw.split(',')
                    .map(|w| w.trim().to_lowercase())
                    .filter(|w| !w.is_empty())
                    .collect()
            };
            let agent_keywords = split_csv(&s.config.agent_keywords);
            let download_keywords = split_csv(&s.config.download_keywords);
            let ignored_keywords = split_csv(&s.config.ignored_processes);
            let disk_noise_keywords = split_csv(&s.config.disk_noise_processes);

            // PID-bound lease exit check
            if let Some(pid) = s.agent_lease_pid {
                let sys_pid = sysinfo::Pid::from_u32(pid);
                if !sys.processes().contains_key(&sys_pid) {
                    s.agent_lease_until = None;
                    s.agent_lease_reason.clear();
                    s.agent_lease_pid = None;
                    s.eval_state.agent_lease_until_epoch_secs = None;
                    s.eval_state.agent_lease_reason.clear();
                    s.eval_state.agent_lease_pid = None;
                    s.set_toast(&format!(
                        "PID-bound lease auto-released: Process {} exited.",
                        pid
                    ));
                }
            }

            let enriched = probes::sample_processes_enriched(
                &sys,
                &agent_keywords,
                &download_keywords,
                &ignored_keywords,
                &disk_noise_keywords,
                s.agent_lease_pid,
                &audio_reading.active_audio_pids,
                &mic_cam_app,
            );

            let (raw_pc_net_kbps, filtered_pc_net_kbps) = probes::compute_filtered_net_kbps(
                &nets,
                &cached_default_nics,
                &s.config.nic_allowlist_override,
            );

            s.top_processes = enriched
                .top_processes
                .iter()
                .map(|p| ProcessCpuInfo {
                    name: p.name.clone(),
                    cpu_pct: p.cpu_pct,
                    is_agent: p.is_agent,
                    is_ignored: p.is_ignored,
                })
                .collect();

            let self_adjusted_pc_cpu = (whole_pc_cpu - enriched.self_cpu_norm).max(0.0);
            let has_active_power_req = power_req_snap.display_active;

            // Update live telemetry snapshot
            s.live_idle_secs = idle_secs;
            s.live_agent_cpu_pct = enriched.max_agent_cpu;
            s.live_agent_process_name = enriched.max_agent_proc.clone();
            s.live_gpu_pct = gpu_reading.util_pct;
            s.gpu_power_mw = gpu_reading.power_mw;
            s.live_download_kbps = enriched.max_dl_kbps;
            s.live_download_process_name = enriched.max_dl_proc.clone();
            s.live_pc_cpu_pct = self_adjusted_pc_cpu;
            let effective_disk_now = enriched.attributed_disk_kbps.min(whole_pc_disk_kbps);
            s.live_pc_disk_kbps = effective_disk_now;
            s.raw_pc_disk_kbps = whole_pc_disk_kbps;
            s.top_disk_proc = enriched.top_disk_proc.clone();
            s.top_disk_kbps = enriched.top_disk_kbps.min(whole_pc_disk_kbps);
            s.top_io_debug = enriched.top_io_debug.clone();
            s.live_pc_net_kbps = filtered_pc_net_kbps;
            s.raw_pc_net_kbps = raw_pc_net_kbps;
            s.live_mic_active = mic_held;
            s.live_cam_active = cam_held;
            s.live_mic_cam_app = mic_cam_app.clone();
            s.mic_capture_peak = audio_reading.capture_peak;
            s.mic_capture_signal_label = audio_reading.capture_signal.label().to_string();
            s.live_power_display_active = has_active_power_req;
            s.live_battery_status = batt;

            // Update 120s rolling history
            if s.history_agent_cpu.len() >= HISTORY_LEN {
                s.history_agent_cpu.pop_front();
            }
            s.history_agent_cpu.push_back(enriched.max_agent_cpu);

            if s.history_gpu.len() >= HISTORY_LEN {
                s.history_gpu.pop_front();
            }
            s.history_gpu.push_back(gpu_reading.util_pct);

            if s.history_pc_cpu.len() >= HISTORY_LEN {
                s.history_pc_cpu.pop_front();
            }
            s.history_pc_cpu.push_back(self_adjusted_pc_cpu);

            if s.history_pc_disk.len() >= HISTORY_LEN {
                s.history_pc_disk.pop_front();
            }
            s.history_pc_disk.push_back(effective_disk_now);

            if s.history_pc_net.len() >= HISTORY_LEN {
                s.history_pc_net.pop_front();
            }
            s.history_pc_net.push_back(filtered_pc_net_kbps);

            if let Some(until) = s.config.stay_awake_until_epoch_secs {
                if now_wall >= until {
                    s.config.stay_awake_until_epoch_secs = None;
                    s.config_dirty = true;
                    s.set_toast("Scheduled stay-awake period ended.");
                }
            }

            let in_quiet = s.config.quiet_hours_enabled
                && is_in_quiet_hours(
                    local_h,
                    local_m,
                    &s.config.quiet_hours_start,
                    &s.config.quiet_hours_end,
                );
            let eval_cfg = s.config.to_eval_config(in_quiet);

            // Sync transient timer handles from UI/API into persistent s.eval_state
            s.eval_state.postponed_until_epoch_secs = s
                .postponed_until
                .map(|i| now_wall + i.saturating_duration_since(Instant::now()).as_secs());
            s.eval_state.agent_lease_until_epoch_secs = s
                .agent_lease_until
                .map(|i| now_wall + i.saturating_duration_since(Instant::now()).as_secs());
            s.eval_state.agent_lease_reason = s.agent_lease_reason.clone();
            s.eval_state.agent_lease_pid = s.agent_lease_pid;

            let metrics = Metrics {
                idle_secs,
                max_agent_cpu: enriched.max_agent_cpu,
                max_agent_proc: enriched.max_agent_proc,
                max_dl_kbps: enriched.max_dl_kbps,
                max_dl_proc: enriched.max_dl_proc,
                whole_pc_cpu: self_adjusted_pc_cpu,
                whole_pc_disk_kbps,
                whole_pc_net_kbps: raw_pc_net_kbps,
                gpu_util: gpu_reading.util_pct,
                mic_active: mic_held,
                cam_active: cam_held,
                mic_cam_app: mic_cam_app.clone(),
                display_request_active: has_active_power_req,
                sensor_error: false,
                now_epoch_secs: now_wall,
                local_hour: local_h,
                local_min: local_m,
                on_battery: batt.has_battery && !batt.on_ac_power,
                battery_pct: batt.percent,
                stay_awake_until: s.config.stay_awake_until_epoch_secs,
                gpu_power_mw: gpu_reading.power_mw,
                gpu_health: gpu_reading.health,
                attributed_disk_kbps: Some(enriched.attributed_disk_kbps),
                raw_physical_disk_available: true,
                top_disk_proc: enriched.top_disk_proc,
                top_disk_kbps: enriched.top_disk_kbps.min(whole_pc_disk_kbps),
                default_route_net_kbps: Some(filtered_pc_net_kbps),
                media: audio_reading.media.clone(),
                mic_cam: MicCamSnapshot {
                    health: audio_reading.capture_health,
                    mic_held,
                    cam_held,
                    holder_app: mic_cam_app,
                    holder_render_audio_active: enriched.holder_render_audio_active,
                    capture_peak: audio_reading.capture_peak,
                    capture_signal: audio_reading.capture_signal,
                },
                power_requests: power_req_snap.clone(),
                lease_tree_cpu_pct: enriched.lease_tree_cpu,
                lease_tree_disk_kbps: enriched.lease_tree_disk_kbps,
                wake_classification: wake_cls,
            };

            let prev_media_prompt = s.media_prompt_active;
            let mut state_ref = std::mem::take(&mut s.eval_state);
            let decision = evaluate(&eval_cfg, &metrics, &mut state_ref);
            s.eval_state = state_ref;

            // If evaluator cleared/auto-expired a hung lease, sync SharedState mirror
            if s.agent_lease_until.is_some() && s.eval_state.agent_lease_until_epoch_secs.is_none()
            {
                s.agent_lease_until = None;
                s.agent_lease_reason.clear();
                s.agent_lease_pid = None;
                s.set_toast("Idle zombie agent lease expired cleanly.");
            }

            if decision.best_performance_expired {
                s.set_operational_mode(OperationalMode::SmartGuard, None);
            }

            s.operational_mode = decision.operational_mode;
            s.intent = decision.intent;
            s.countdown_secs = decision.countdown_secs;
            s.countdown_max = decision.countdown_max;
            s.all_conditions_pass = decision.all_conditions_pass;
            s.blocker_banner_text = decision.blocker_banner_text;
            s.structured_blockers = decision.structured_blockers;
            s.pc_spike_sustained_counter = s.eval_state.pc_spike_sustained_counter;
            s.grace_active = decision.trigger_grace_banner;
            s.stage1_display_off_allowed = decision.stage1_display_off_allowed;
            s.defer_display_off_for_monitor_audio = decision.defer_display_off_for_monitor_audio;
            s.media_prompt_active = decision.media_prompt_active;
            s.media_prompt_app = decision.media_prompt_app;
            s.media_prompt_title = decision.media_prompt_title;
            s.media_lock_active = decision.media_lock_active;
            s.media_lock_app = decision.media_lock_app;
            s.active_power_owners = decision.active_power_owners;
            s.suppressed_browser_request = decision.suppressed_browser_request;
            s.gpu_effective_threshold_pct = decision.gpu_effective_threshold_pct;
            s.gpu_idle_floor_pct = decision.gpu_idle_floor_pct;
            s.suggested_thresholds = decision.suggested_thresholds;

            s.cond1_idle = decision.cond1_idle;
            s.cond2_agent_cpu = decision.cond2_agent_cpu;
            s.cond3_download = decision.cond3_download;
            s.cond4_pc_cpu = decision.cond4_pc_cpu;
            s.cond5_pc_disk = decision.cond5_pc_disk;
            s.cond6_pc_net = decision.cond6_pc_net;
            s.cond7_spike_filter = decision.cond7_spike_filter;
            s.cond8_monitor_timeout = decision.cond8_monitor_timeout;
            s.cond9_power_requests = decision.cond9_power_requests;
            s.cond10_gpu = decision.cond10_gpu;
            s.cond11_mic_cam = decision.cond11_mic_cam;

            trigger_sleep = decision.trigger_sleep;
            hold_os_wake_lock = decision.hold_os_wake_lock;

            if decision.stage1_display_off_allowed
                && s.config.cond8_monitor_timeout_enabled
                && !s.monitor_off_triggered_this_idle
                && !decision.defer_display_off_for_monitor_audio
                && idle_secs >= s.config.monitor_timeout_mins.saturating_mul(60)
                && !trigger_sleep
            {
                s.monitor_off_triggered_this_idle = true;
                trigger_monitor_off = true;
            }

            if let Some(dur) = decision.log_monitor_off_duration {
                s.log_energy_event("monitor_off", dur);
            }

            // Block History & Wakeup Logging (§5.9)
            if s.config.block_history_enabled {
                if let Some(cls) = decision.detected_wake_classification {
                    s.record_block_event(
                        now_wall,
                        now_wall + 2,
                        2,
                        format!("Wakeup: {}", cls.label()),
                        vec![format!("Wakeup source: {}", cls.label())],
                    );
                }

                let new_primary = decision.blockers.first().cloned();
                if new_primary != s.current_primary_blocker {
                    if let Some(start) = s.current_block_start_epoch_secs {
                        let duration = now_wall.saturating_sub(start);
                        if duration >= 2 {
                            if let Some(prev) = s.current_primary_blocker.take() {
                                let all = s.current_all_blockers.clone();
                                s.record_block_event(start, now_wall, duration, prev, all);
                            }
                        }
                    }
                    if let Some(b) = &new_primary {
                        s.current_block_start_epoch_secs = Some(now_wall);
                        s.current_primary_blocker = Some(b.clone());
                        s.current_all_blockers = decision.blockers.clone();
                    } else {
                        s.current_block_start_epoch_secs = None;
                        s.current_primary_blocker = None;
                        s.current_all_blockers.clear();
                    }
                }

                if now_wall % 60 == 0 {
                    check_and_rotate_block_history();
                }
            }

            // Notifications (§5.6, §5.9, §6)
            if decision.trigger_grace_banner
                && !s.last_grace_active
                && s.config.notify_on_sleep_imminent
                && now_wall >= s.last_sleep_imminent_notif + 60
            {
                s.last_sleep_imminent_notif = now_wall;
                send_notification(
                    &s.config,
                    &NotificationEvent::SleepImminent {
                        countdown_secs: decision.countdown_secs,
                    },
                );
            }
            s.last_grace_active = decision.trigger_grace_banner;

            if decision.woke_from_sleep_reset {
                let wake_desc = decision
                    .detected_wake_classification
                    .map(|c| c.label())
                    .unwrap_or("Unknown");
                s.set_toast(format!("System resumed ({}).", wake_desc));
                if s.config.notify_on_pc_woke && now_wall >= s.last_pc_woke_notif + 60 {
                    s.last_pc_woke_notif = now_wall;
                    send_notification(&s.config, &NotificationEvent::PcWoke);
                }
            }

            if let Some((ref lease_reason, idle_mins)) = decision.trigger_zombie_lease_alert {
                s.set_toast(format!(
                    "Agent lease '{}' looks hung ({}m at ~0% activity).",
                    lease_reason, idle_mins
                ));
                send_notification(
                    &s.config,
                    &NotificationEvent::ZombieLease {
                        reason: lease_reason.clone(),
                        idle_mins,
                    },
                );
            }

            if decision.media_prompt_active
                && !prev_media_prompt
                && now_wall >= s.last_media_prompt_notif + 300
            {
                s.last_media_prompt_notif = now_wall;
                send_notification(
                    &s.config,
                    &NotificationEvent::MediaPrompt {
                        app: s.media_prompt_app.clone(),
                        title: s.media_prompt_title.clone(),
                    },
                );
            }

            if let Some(start) = s.current_block_start_epoch_secs {
                let blocked_secs = now_wall.saturating_sub(start);
                let threshold_secs = (s.config.notify_blocked_long_hours as u64) * 3600;
                if blocked_secs >= threshold_secs
                    && now_wall >= s.last_blocked_long_notif + 3600
                    && s.config.notify_on_blocked_long
                {
                    s.last_blocked_long_notif = now_wall;
                    let blocker_name = s
                        .current_primary_blocker
                        .clone()
                        .unwrap_or_else(|| "Unknown Blocker".to_string());
                    send_notification(
                        &s.config,
                        &NotificationEvent::BlockedLong {
                            hours: s.config.notify_blocked_long_hours,
                            blocker: blocker_name,
                        },
                    );
                }
            }

            if s.operational_mode == OperationalMode::GuardOff {
                let paused_secs = now_wall.saturating_sub(s.guard_paused_since);
                let threshold_secs = (s.config.notify_guard_paused_hours as u64) * 3600;
                if paused_secs >= threshold_secs
                    && now_wall >= s.last_guard_paused_notif + 3600
                    && s.config.notify_on_guard_paused_long
                {
                    s.last_guard_paused_notif = now_wall;
                    send_notification(
                        &s.config,
                        &NotificationEvent::GuardPausedLong {
                            hours: s.config.notify_guard_paused_hours,
                        },
                    );
                }
            } else {
                s.guard_paused_since = now_wall;
            }

            if s.operational_mode == OperationalMode::BestPerformance {
                let locked_secs = now_wall.saturating_sub(s.best_perf_locked_since);
                let threshold_secs = (s.config.notify_guard_paused_hours as u64).max(1) * 3600;
                if locked_secs >= threshold_secs
                    && now_wall >= last_best_perf_notif + 3600
                    && s.config.notify_on_guard_paused_long
                {
                    last_best_perf_notif = now_wall;
                    send_notification(
                        &s.config,
                        &NotificationEvent::BestPerfLockedLong {
                            hours: s.config.notify_guard_paused_hours.max(1),
                        },
                    );
                }
            } else {
                s.best_perf_locked_since = now_wall;
            }

            // Phase 2.3: Opt-in Update Check
            if s.config.check_for_updates
                && now_wall >= s.config.last_update_check_epoch_secs + 86400
            {
                s.config.last_update_check_epoch_secs = now_wall;
                s.config_dirty = true;
                trigger_update_check_locked(&mut s, Arc::clone(&state));
            }

            // 120-Second Automated Diagnostic Session Tick
            if s.diag_session.active {
                let os_name = if s.detected_specs.os_name.trim().is_empty() {
                    #[cfg(windows)]
                    {
                        "Windows 11 / Windows 10".to_string()
                    }
                    #[cfg(target_os = "macos")]
                    {
                        "macOS".to_string()
                    }
                    #[cfg(target_os = "linux")]
                    {
                        "Linux".to_string()
                    }
                    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
                    {
                        "Unknown OS".to_string()
                    }
                } else {
                    s.detected_specs.os_name.clone()
                };
                let hw_summary = format!(
                    "CPU: {} ({}c) | RAM: {:.1} GB | GPU: {} | Storage: {} drive(s) | Audio: {} device(s) | Laptop: {}",
                    if s.detected_specs.cpu_brand.is_empty() { "Detected CPU" } else { &s.detected_specs.cpu_brand },
                    s.detected_specs.cpu_cores,
                    s.detected_specs.ram_gb,
                    if s.detected_specs.gpu_name.is_empty() { "Integrated/Discrete GPU" } else { &s.detected_specs.gpu_name },
                    s.detected_specs.storage_drives.len(),
                    s.detected_specs.audio_devices.len(),
                    s.detected_specs.is_laptop
                );
                let audio_devs = s.detected_specs.audio_devices.clone();
                let intent_display = s.intent.display_label();
                launch_diag_media = s.diag_session.tick_1s(
                    idle_secs,
                    Some(decision.countdown_secs),
                    &intent_display,
                    &decision.blockers,
                    audio_reading.media.render_active,
                    audio_reading.media.render_peak,
                    &audio_reading.media.render_app,
                    power_req_snap.display_active,
                    power_req_snap.system_active,
                    whole_pc_cpu,
                    gpu_reading.util_pct,
                    whole_pc_disk_kbps * 0.2,
                    whole_pc_disk_kbps * 0.8,
                    filtered_pc_net_kbps * 0.7,
                    filtered_pc_net_kbps * 0.3,
                    &os_name,
                    &hw_summary,
                    &audio_devs,
                );
            }

            // Auto-save configuration if dirty
            if s.config_dirty {
                s.config_dirty = false;
                s.config.save();
            }
        } // unlock state

        if launch_diag_media {
            open_browser_url("https://www.youtube.com/watch?v=aqz-KE-bpKQ");
        }

        // Sync OS wake-lock thread (Best Performance mode)
        wake_lock.set_locked(hold_os_wake_lock);

        if wake_sleep_stabilize {
            std::thread::sleep(Duration::from_millis(1200));
        }

        // Execute Stage 1 Display-Off outside mutex lock
        if trigger_monitor_off {
            turn_off_monitors_now();
        }

        // Execute sleep outside of mutex lock, measuring real wall-clock sleep duration
        if trigger_sleep {
            let before_sleep = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let ok = sleep_now();
            let after_sleep = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut s = state.lock().unwrap();
            if ok {
                let slept_secs = after_sleep.saturating_sub(before_sleep).max(60);
                s.log_energy_event("sleep", slept_secs);
            } else {
                s.set_toast(
                    "Sleep trigger returned false (Check OS sleep permissions / powercfg).",
                );
            }
        }

        if let Some(ctx) = &egui_ctx {
            ctx.request_repaint();
        }

        let elapsed = loop_start.elapsed();
        if elapsed < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_secs(1) - elapsed);
        }
    }
}

// ----------------------------------------------------------------------------
// Phase 5.1 & 5.4: Background Daemon, Service Management & Wake Relay
// ----------------------------------------------------------------------------

pub fn run_headless_daemon() {
    println!("==================================================");
    println!("   AGENT POWER GUARD - BACKGROUND WATCHDOG DAEMON ");
    println!("==================================================");
    let config = AppConfig::load();
    if config.run_at_startup {
        set_startup_task(true);
    }
    let port = config.web_server_port;
    let state = Arc::new(Mutex::new(SharedState::new(config)));
    let rate_limiter = Arc::new(RateLimiter::new());

    start_web_server(Arc::clone(&state), port, rate_limiter);
    let token = get_or_create_api_token();
    println!("Watchdog daemon active.");
    println!("REST API listening on: http://0.0.0.0:{}", port);
    println!("API Token: {}", token);
    println!("Press Ctrl+C to terminate daemon.");
    println!("==================================================");

    run_watchdog_loop(state, None);
}

pub fn install_service() {
    #[cfg(windows)]
    {
        if let Ok(exe_path) = std::env::current_exe() {
            let exe_str = exe_path.to_string_lossy().to_string();
            let status = Command::new("schtasks.exe")
                .args([
                    "/create",
                    "/tn",
                    "AgentPowerGuardService",
                    "/tr",
                    &format!("\"{}\" daemon", exe_str),
                    "/sc",
                    "onlogon",
                    "/rl",
                    "highest",
                    "/f",
                ])
                .status();
            match status {
                Ok(s) if s.success() => {
                    println!("Successfully installed Agent Power Guard background service task ('AgentPowerGuardService').");
                    println!("The watchdog daemon will now start automatically at logon with zero elevation prompts.");
                }
                Ok(s) => eprintln!("schtasks exited with code: {:?}", s.code()),
                Err(e) => eprintln!("Failed to invoke schtasks.exe: {}", e),
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let unit_dir = PathBuf::from(&home).join(".config/systemd/user");
        let _ = std::fs::create_dir_all(&unit_dir);
        let unit_file = unit_dir.join("agent-power-guard.service");
        let exe = std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("/usr/local/bin/agent-power-guard"));
        let unit_content = format!(
            "[Unit]\nDescription=Agent Power Guard Watchdog Service\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={} daemon\nRestart=on-failure\nRestartSec=5s\n\n[Install]\nWantedBy=default.target\n",
            exe.to_string_lossy()
        );
        if std::fs::write(&unit_file, unit_content).is_ok() {
            let _ = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status();
            let _ = Command::new("systemctl")
                .args(["--user", "enable", "--now", "agent-power-guard"])
                .status();
            println!(
                "Successfully installed and enabled systemd user service: {}",
                unit_file.display()
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let launch_dir = PathBuf::from(&home).join("Library/LaunchAgents");
        let _ = std::fs::create_dir_all(&launch_dir);
        let plist_file = launch_dir.join("com.agentpowerguard.watchdog.plist");
        let exe = std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("/usr/local/bin/agent-power-guard"));
        let plist_content = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n    <key>Label</key>\n    <string>com.agentpowerguard.watchdog</string>\n    <key>ProgramArguments</key>\n    <array>\n        <string>{}</string>\n        <string>daemon</string>\n    </array>\n    <key>KeepAlive</key>\n    <true/>\n    <key>RunAtLoad</key>\n    <true/>\n</dict>\n</plist>\n",
            exe.to_string_lossy()
        );
        if std::fs::write(&plist_file, plist_content).is_ok() {
            let _ = Command::new("launchctl")
                .args(["load", &plist_file.to_string_lossy()])
                .status();
            println!(
                "Successfully installed and loaded launchd agent: {}",
                plist_file.display()
            );
        }
    }
}

pub fn uninstall_service() {
    #[cfg(windows)]
    {
        let status = Command::new("schtasks.exe")
            .args(["/delete", "/tn", "AgentPowerGuardService", "/f"])
            .status();
        match status {
            Ok(s) if s.success() => {
                println!("Successfully uninstalled Agent Power Guard background service task.");
            }
            Ok(s) => eprintln!("schtasks exited with code: {:?}", s.code()),
            Err(e) => eprintln!("Failed to invoke schtasks.exe: {}", e),
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", "agent-power-guard"])
            .status();
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let unit_file = PathBuf::from(&home).join(".config/systemd/user/agent-power-guard.service");
        let _ = std::fs::remove_file(unit_file);
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        println!("Successfully uninstalled systemd user service.");
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let plist_file =
            PathBuf::from(&home).join("Library/LaunchAgents/com.agentpowerguard.watchdog.plist");
        let _ = Command::new("launchctl")
            .args(["unload", &plist_file.to_string_lossy()])
            .status();
        let _ = std::fs::remove_file(plist_file);
        println!("Successfully uninstalled launchd agent.");
    }
}

pub fn service_status() {
    let port = AppConfig::load().web_server_port;
    match send_cli_request("/api/status") {
        Ok(_) => {
            println!(
                "Agent Power Guard Background Watchdog: ACTIVE (Running on port {})",
                port
            );
        }
        Err(_) => {
            println!("Agent Power Guard Background Watchdog: STOPPED / NOT RUNNING");
        }
    }
}

pub fn run_wake_relay(port: u16, custom_token: Option<String>) {
    let auth_token = custom_token.unwrap_or_else(get_or_create_api_token);
    let addr = format!("0.0.0.0:{}", port);
    println!("==================================================");
    println!("        AGENT POWER GUARD - WAKE RELAY            ");
    println!("==================================================");
    println!("Relay listening on : http://{}", addr);
    println!("Auth Token         : {}", auth_token);
    println!("Ready to forward Wake-on-LAN packets on LAN.");
    println!("Endpoints:");
    println!("  GET  /relay/status");
    println!("  POST /relay/wake?mac=<mac>&bcast=<bcast_ip>");
    println!("==================================================");

    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind relay on {}: {}", addr, e);
            return;
        }
    };

    let start_time = Instant::now();
    for stream in listener.incoming().flatten() {
        let auth_token = auth_token.clone();
        std::thread::spawn(move || {
            handle_relay_client(stream, &auth_token, start_time);
        });
    }
}

fn handle_relay_client(mut stream: TcpStream, expected_token: &str, start_time: Instant) {
    let mut buffer = [0u8; 4096];
    let bytes_read = match stream.read(&mut buffer) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req_str = String::from_utf8_lossy(&buffer[..bytes_read]);
    let first_line = req_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0];
    let path = parts[1];

    if method == "OPTIONS" {
        let resp = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path == "/relay/status" {
        let body = serde_json::to_string(&serde_json::json!({
            "status": "ok",
            "role": "wake_relay",
            "uptime_secs": start_time.elapsed().as_secs(),
        }))
        .unwrap_or_default();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/relay/wake") {
        let auth_header = req_str
            .lines()
            .find(|line| line.to_lowercase().starts_with("authorization:"))
            .and_then(|line| line.split(':').nth(1))
            .map(|val| val.trim())
            .unwrap_or("");
        let query_token = parse_query_param(path, "token").unwrap_or_default();
        let is_authed = if auth_header.starts_with("Bearer ") {
            constant_time_eq(&auth_header[7..], expected_token)
        } else if !query_token.is_empty() {
            constant_time_eq(&query_token, expected_token)
        } else {
            false
        };

        if !is_authed {
            let body = r#"{"error":"unauthorized"}"#;
            let resp = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            return;
        }

        if let Some(mac) = parse_query_param(path, "mac") {
            let bcast = parse_query_param(path, "bcast");
            match send_wol_packet(&mac, bcast.as_deref()) {
                Ok(()) => {
                    let body = serde_json::to_string(&serde_json::json!({
                        "status": "sent",
                        "mac": mac,
                        "broadcast": bcast.unwrap_or_else(|| "255.255.255.255".to_string()),
                    }))
                    .unwrap_or_default();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    return;
                }
                Err(err) => {
                    let body = serde_json::to_string(&serde_json::json!({
                        "error": err,
                    }))
                    .unwrap_or_default();
                    let resp = format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    return;
                }
            }
        }
    }

    let body = r#"{"error":"not_found"}"#;
    let resp = format!("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
    let _ = stream.write_all(resp.as_bytes());
}

// ----------------------------------------------------------------------------
// GUI Application Implementation
// ----------------------------------------------------------------------------

pub fn open_url(url: &str) {
    #[cfg(windows)]
    let _ = Command::new("cmd")
        .args(["/c", "start", url])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    #[cfg(target_os = "linux")]
    let _ = Command::new("xdg-open").arg(url).status();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).status();
}

pub fn parse_releases_json(raw: &[u8]) -> Result<Vec<ReleaseEntry>, String> {
    let val: serde_json::Value = serde_json::from_slice(raw)
        .map_err(|e| format!("Failed to parse GitHub releases JSON: {}", e))?;

    if let Some(msg) = val.get("message").and_then(|m| m.as_str()) {
        return Err(format!("GitHub API: {}", msg));
    }

    let items: Vec<serde_json::Value> = if let Some(arr) = val.as_array() {
        arr.clone()
    } else if val.get("tag_name").is_some() {
        vec![val]
    } else {
        return Err("Unexpected GitHub Releases response format.".to_string());
    };

    let mut releases = Vec::new();
    for item in items {
        if item.get("draft").and_then(|d| d.as_bool()).unwrap_or(false) {
            continue;
        }
        let Some(tag) = item.get("tag_name").and_then(|t| t.as_str()) else {
            continue;
        };
        let prerelease = item
            .get("prerelease")
            .and_then(|p| p.as_bool())
            .unwrap_or(false);
        let name = item
            .get("name")
            .and_then(|n| n.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(tag)
            .to_string();
        let published_at = item
            .get("published_at")
            .and_then(|p| p.as_str())
            .map(|s| s.chars().take(10).collect::<String>())
            .unwrap_or_default();
        let body = item
            .get("body")
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_string();

        let mut assets = Vec::new();
        if let Some(arr) = item.get("assets").and_then(|a| a.as_array()) {
            for a in arr {
                if let (Some(aname), Some(url)) = (
                    a.get("name").and_then(|n| n.as_str()),
                    a.get("browser_download_url").and_then(|u| u.as_str()),
                ) {
                    assets.push(ReleaseAssetInfo {
                        name: aname.to_string(),
                        download_url: url.to_string(),
                        size_bytes: a.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                    });
                }
            }
        }

        releases.push(ReleaseEntry {
            tag: tag.to_string(),
            name,
            published_at,
            body,
            prerelease,
            assets,
        });
    }

    // Sort highest version first; non-prerelease takes precedence on equal base versions.
    releases.sort_by(|a, b| {
        if is_newer_version(&a.tag, &b.tag) {
            std::cmp::Ordering::Less
        } else if is_newer_version(&b.tag, &a.tag) {
            std::cmp::Ordering::Greater
        } else {
            a.prerelease.cmp(&b.prerelease)
        }
    });

    Ok(releases)
}

pub fn pick_latest_release(releases: &[ReleaseEntry]) -> Option<&ReleaseEntry> {
    releases
        .iter()
        .find(|r| !r.prerelease)
        .or_else(|| releases.first())
}

pub fn select_platform_asset(
    release: &ReleaseEntry,
    running_in_program_files: bool,
) -> Option<&ReleaseAssetInfo> {
    #[cfg(windows)]
    {
        if running_in_program_files {
            if let Some(msi) = release
                .assets
                .iter()
                .find(|a| a.name.to_lowercase().ends_with(".msi"))
            {
                return Some(msi);
            }
        }
        if let Some(exe) = release
            .assets
            .iter()
            .find(|a| a.name.to_lowercase().ends_with(".exe"))
        {
            return Some(exe);
        }
        release
            .assets
            .iter()
            .find(|a| a.name.to_lowercase().ends_with(".msi"))
    }
    #[cfg(target_os = "linux")]
    {
        let _ = running_in_program_files;
        release
            .assets
            .iter()
            .find(|a| a.name.contains("linux") && a.name.ends_with(".tar.gz"))
            .or_else(|| release.assets.iter().find(|a| a.name.ends_with(".deb")))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = running_in_program_files;
        let arch_key = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        };
        release
            .assets
            .iter()
            .find(|a| {
                a.name.contains("macos") && a.name.contains(arch_key) && a.name.ends_with(".tar.gz")
            })
            .or_else(|| release.assets.iter().find(|a| a.name.ends_with(".zip")))
    }
}

pub fn cleanup_old_update_backup() {
    if let Ok(exe) = std::env::current_exe() {
        let old = exe.with_extension("exe.old");
        if old.exists() {
            let _ = std::fs::remove_file(&old);
        }
        let old_unix = exe.with_extension("old");
        if old_unix.exists() {
            let _ = std::fs::remove_file(&old_unix);
        }
    }
}

fn fetch_github_releases_blocking() -> Result<Vec<ReleaseEntry>, String> {
    let mut cmd = Command::new("curl");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.args([
        "-sL",
        "--max-time",
        "12",
        "--connect-timeout",
        "6",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        &format!("User-Agent: AgentPowerGuard/{}", env!("CARGO_PKG_VERSION")),
        "https://api.github.com/repos/Subham-Maity/agent-power-guard/releases?per_page=20",
    ]);

    let output = cmd.output().map_err(|e| format!("curl failed: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "GitHub API request failed (exit {:?})",
            output.status.code()
        ));
    }
    parse_releases_json(&output.stdout)
}

pub fn trigger_update_check(state: Arc<Mutex<SharedState>>) {
    let mut s = state.lock().unwrap();
    trigger_update_check_locked(&mut s, Arc::clone(&state));
}

pub fn trigger_update_check_locked(s: &mut SharedState, state: Arc<Mutex<SharedState>>) {
    if s.update_check_in_progress {
        return;
    }
    s.update_check_in_progress = true;
    s.update_check_status_msg = Some("Fetching all releases from GitHub...".to_string());

    std::thread::spawn(move || {
        let res = fetch_github_releases_blocking();
        let mut s = state.lock().unwrap();
        s.update_check_in_progress = false;

        match res {
            Ok(releases) => {
                let current = env!("CARGO_PKG_VERSION");
                if let Some(latest) = pick_latest_release(&releases).cloned() {
                    let clean = latest.tag.trim_start_matches('v');
                    s.update_latest_tag = Some(latest.tag.clone());
                    if is_newer_version(clean, current) {
                        s.update_available = Some(latest.tag.clone());
                        s.update_check_status_msg = Some(format!(
                            "Update available: {} (Installed: v{}, {} releases found)",
                            latest.tag,
                            current,
                            releases.len()
                        ));
                    } else {
                        s.update_available = None;
                        s.update_check_status_msg = Some(format!(
                            "Up to date — Latest: {} (Installed: v{})",
                            latest.tag, current
                        ));
                    }
                } else {
                    s.update_check_status_msg = Some("No published releases found.".to_string());
                }
                s.available_releases = releases;
            }
            Err(err) => {
                s.update_check_status_msg = Some(err);
            }
        }
    });
}

pub fn trigger_update_download_locked(
    s: &mut SharedState,
    state: Arc<Mutex<SharedState>>,
    requested_tag: Option<String>,
) {
    if s.update_download_in_progress {
        return;
    }
    s.update_download_in_progress = true;
    s.update_downloaded_path = None;
    s.update_downloaded_version = None;
    s.update_install_prompt_open = false;
    s.update_check_status_msg = Some("Preparing background download...".to_string());
    let existing_releases = s.available_releases.clone();

    std::thread::spawn(move || {
        let releases = if existing_releases.is_empty() {
            match fetch_github_releases_blocking() {
                Ok(list) => list,
                Err(e) => {
                    let mut s = state.lock().unwrap();
                    s.update_download_in_progress = false;
                    s.update_check_status_msg = Some(format!("Download failed: {}", e));
                    return;
                }
            }
        } else {
            existing_releases
        };

        let target = if let Some(ref tag) = requested_tag {
            releases.iter().find(|r| &r.tag == tag).cloned()
        } else {
            pick_latest_release(&releases).cloned()
        };

        let Some(release) = target else {
            let mut s = state.lock().unwrap();
            s.update_download_in_progress = false;
            s.update_check_status_msg = Some("Target release not found on GitHub.".to_string());
            return;
        };

        let in_prog_files = std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_lowercase().contains("program files"))
            .unwrap_or(false);

        let Some(asset) = select_platform_asset(&release, in_prog_files).cloned() else {
            let mut s = state.lock().unwrap();
            s.update_download_in_progress = false;
            s.update_check_status_msg = Some(format!(
                "No downloadable asset found for this OS in {}.",
                release.tag
            ));
            return;
        };

        {
            let mut s = state.lock().unwrap();
            s.available_releases = releases;
            s.update_check_status_msg =
                Some(format!("Downloading {} ({})...", release.tag, asset.name));
        }

        let dir = std::env::temp_dir().join("agent-power-guard-updates");
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join(&asset.name);
        let dest_str = dest.to_string_lossy().to_string();

        let mut cmd = Command::new("curl");
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.args([
            "-fSL",
            "--max-time",
            "180",
            "--connect-timeout",
            "10",
            "-o",
            &dest_str,
            &asset.download_url,
        ]);

        let ok_dl = match cmd.output() {
            Ok(out) if out.status.success() => std::fs::metadata(&dest)
                .map(|m| m.len() > 100_000)
                .unwrap_or(false),
            _ => false,
        };

        let mut s = state.lock().unwrap();
        s.update_download_in_progress = false;
        if ok_dl {
            s.update_downloaded_path = Some(dest);
            s.update_downloaded_version = Some(release.tag.clone());
            s.update_install_prompt_open = true;
            s.update_check_status_msg = Some(format!(
                "Downloaded {} ({}) — click Install Now to finish updating.",
                release.tag, asset.name
            ));
        } else {
            s.update_check_status_msg = Some(format!(
                "Failed to download {}. Note: Windows Defender / SmartScreen may block unsigned binaries.",
                asset.name
            ));
        }
    });
}

pub fn install_downloaded_update(downloaded: &PathBuf) -> Result<(), String> {
    if !downloaded.exists() {
        return Err("Downloaded installer file no longer exists.".to_string());
    }
    let name_lower = downloaded
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    #[cfg(windows)]
    {
        if name_lower.ends_with(".msi") {
            Command::new("msiexec")
                .args(["/i", &downloaded.to_string_lossy(), "/passive"])
                .spawn()
                .map_err(|e| format!("Failed to launch MSI installer: {}", e))?;
            std::process::exit(0);
        }

        let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let backup_exe = current_exe.with_extension("exe.old");
        let _ = std::fs::remove_file(&backup_exe);

        if std::fs::rename(&current_exe, &backup_exe).is_ok() {
            if std::fs::copy(downloaded, &current_exe).is_ok() {
                let _ = Command::new(&current_exe).spawn();
                std::process::exit(0);
            } else {
                // Rollback if copy failed
                let _ = std::fs::rename(&backup_exe, &current_exe);
            }
        }

        // Fallback if installed folder needs elevation: elevated PowerShell swap & relaunch
        let ps_cmd = format!(
            "Start-Sleep -Seconds 1; Move-Item -LiteralPath '{}' -Destination '{}' -Force; Start-Process -FilePath '{}'",
            downloaded.to_string_lossy().replace('\'', "''"),
            current_exe.to_string_lossy().replace('\'', "''"),
            current_exe.to_string_lossy().replace('\'', "''"),
        );
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-WindowStyle','Hidden','-Command',\"{}\"",
                    ps_cmd.replace('"', "`\"")
                ),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Failed to apply update: {}", e))?;
        std::process::exit(0);
    }

    #[cfg(not(windows))]
    {
        if name_lower.ends_with(".tar.gz") {
            let extract_dir = std::env::temp_dir().join("apg-extract");
            let _ = std::fs::remove_dir_all(&extract_dir);
            let _ = std::fs::create_dir_all(&extract_dir);
            let status = Command::new("tar")
                .args([
                    "-xzf",
                    &downloaded.to_string_lossy(),
                    "-C",
                    &extract_dir.to_string_lossy(),
                ])
                .status()
                .map_err(|e| format!("tar failed: {}", e))?;
            if !status.success() {
                return Err("Failed to extract update archive.".to_string());
            }
            let cand1 = extract_dir
                .join("agent-power-guard")
                .join("agent-power-guard");
            let cand2 = extract_dir.join("agent-power-guard");
            let new_bin = if cand1.is_file() { cand1 } else { cand2 };
            if !new_bin.is_file() {
                return Err("Updated binary not found inside extracted archive.".to_string());
            }
            let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let backup_exe = current_exe.with_extension("old");
            let _ = std::fs::remove_file(&backup_exe);
            let _ = std::fs::rename(&current_exe, &backup_exe);
            std::fs::copy(&new_bin, &current_exe).map_err(|e| e.to_string())?;
            let _ = Command::new("chmod")
                .args(["+x", &current_exe.to_string_lossy()])
                .status();
            let _ = Command::new(&current_exe).spawn();
            std::process::exit(0);
        } else {
            open_url(&downloaded.to_string_lossy());
            Ok(())
        }
    }
}

#[derive(Default)]
struct PlatformHwDetails {
    motherboard: String,
    ram_details: String,
    gpu_name: String,
    gpu_vram_gb: f32,
    storage_drives: Vec<String>,
    monitors: Vec<String>,
    network_adapters: Vec<String>,
    bluetooth_devices: Vec<String>,
    audio_devices: Vec<String>,
    usb_peripherals: Vec<String>,
    cabinet_cooling: Vec<String>,
    is_laptop_override: Option<bool>,
}

/// Move detected USB ARGB/RGB/fan controllers (e.g. Gigabyte ITE Device, Corsair, NZXT, Lian Li)
/// out of generic USB peripherals into `cabinet_cooling` with readable labels.
fn classify_rgb_and_cooling_peripherals(
    usb_peripherals: &mut Vec<String>,
    cabinet_cooling: &mut Vec<String>,
) {
    let mut kept_usb = Vec::with_capacity(usb_peripherals.len());
    for item in usb_peripherals.drain(..) {
        let lower = item.to_ascii_lowercase();
        if lower == "ite device" || lower.contains("ite829") || lower.contains("ite570") {
            cabinet_cooling.push("Motherboard ARGB Controller (ITE Device)".to_string());
        } else if lower.contains("icue")
            || lower.contains("commander")
            || lower.contains("lighting node")
            || lower.contains("kraken")
            || lower.contains("nzxt")
            || lower.contains("lian li")
            || lower.contains("uni fan")
            || lower.contains("aura led")
            || lower.contains("mystic light")
            || lower.contains("rgb controller")
            || lower.contains("argb")
        {
            cabinet_cooling.push(item);
        } else {
            kept_usb.push(item);
        }
    }
    *usb_peripherals = dedupe_clean_list(kept_usb);
    *cabinet_cooling = dedupe_clean_list(std::mem::take(cabinet_cooling));
}

#[cfg(windows)]
fn fetch_platform_hw_details(ram_gb: f32) -> PlatformHwDetails {
    let mut hw = PlatformHwDetails::default();
    let ps = r#"
$ErrorActionPreference = 'SilentlyContinue'
$out = [ordered]@{}

$bb = Get-CimInstance Win32_BaseBoard | Select-Object -First 1 Manufacturer, Product
if ($bb) {
    $out.motherboard = ("$($bb.Manufacturer) $($bb.Product)").Trim()
}

$ddrMap = @{
    20 = 'DDR'; 21 = 'DDR2'; 22 = 'DDR2'; 24 = 'DDR3';
    26 = 'DDR4'; 27 = 'LPDDR'; 28 = 'LPDDR2'; 29 = 'LPDDR3';
    30 = 'LPDDR4'; 34 = 'DDR5'; 35 = 'LPDDR5'
}
$out.ram_sticks = @(Get-CimInstance Win32_PhysicalMemory | ForEach-Object {
    $gb = [math]::Round([double]$_.Capacity / 1GB, 1)
    $spd = if ($_.ConfiguredClockSpeed -gt 0) { [int]$_.ConfiguredClockSpeed } else { [int]$_.Speed }
    $gen = $ddrMap[[int]$_.SMBIOSMemoryType]
    if (-not $gen) { $gen = '' }
    [ordered]@{
        gb = $gb
        gen = $gen
        speed = $spd
        mfg = ("$($_.Manufacturer)").Trim()
        part = ("$($_.PartNumber)").Trim()
    }
})

$busMap = @{ 7='USB'; 8='RAID'; 11='SATA'; 12='SAS'; 17='NVMe' }
$mediaMap = @{ 3='HDD'; 4='SSD'; 5='SCM' }
$disks = @()
try {
    $pd = @(Get-CimInstance -Namespace root\Microsoft\Windows\Storage -ClassName MSFT_PhysicalDisk)
    foreach ($d in $pd) {
        $gb = [math]::Round([double]$d.Size / 1GB, 0)
        $bus = $busMap[[int]$d.BusType]
        $med = $mediaMap[[int]$d.MediaType]
        $kindStr = (@($bus, $med) | Where-Object { $_ }) -join ' '
        $name = ("$($d.FriendlyName)").Trim()
        if ($name -and $gb -gt 0 -and $name -notmatch 'Virtual Disk|Msft Virtual') {
            if ($kindStr) { $disks += "$name ($kindStr, $gb GB)" } else { $disks += "$name ($gb GB)" }
        }
    }
} catch {}
if ($disks.Count -eq 0) {
    foreach ($d in @(Get-CimInstance Win32_DiskDrive)) {
        $gb = [math]::Round([double]$d.Size / 1GB, 0)
        $name = ("$($d.Model)").Trim()
        $iface = ("$($d.InterfaceType)").Trim()
        if ($name -and $gb -gt 0 -and $name -notmatch 'Virtual Disk|Msft Virtual') {
            if ($iface) { $disks += "$name ($iface, $gb GB)" } else { $disks += "$name ($gb GB)" }
        }
    }
}
$out.storage_drives = @($disks | Select-Object -Unique)

$regVram = @{}
Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0*' -ErrorAction SilentlyContinue | ForEach-Object {
    $desc = "$($_.'DriverDesc')".Trim()
    $qw = $_.'HardwareInformation.qwMemorySize'
    if ($desc -and $qw -is [System.ValueType] -and [double]$qw -gt 0) {
        $regVram[$desc] = [math]::Round([double]$qw / 1GB, 1)
    }
}
$out.gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object {
    $gname = "$($_.Name)".Trim()
    $vgb = if ($regVram.ContainsKey($gname)) { $regVram[$gname] } else { [math]::Round([double]$_.AdapterRAM / 1GB, 1) }
    [ordered]@{ name = $gname; vram = $vgb }
})

$mons = @()
try {
    foreach ($m in @(Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorID)) {
        $n = ($m.UserFriendlyName | Where-Object { $_ -ne 0 } | ForEach-Object { [char]$_ }) -join ''
        if ($n.Trim()) { $mons += $n.Trim() }
    }
} catch {}
if ($mons.Count -eq 0) {
    $mons = @(Get-CimInstance Win32_DesktopMonitor | ForEach-Object { "$($_.Name)".Trim() } | Where-Object { $_ })
}
$out.monitors = @($mons | Select-Object -Unique)

$ch = (Get-CimInstance Win32_SystemEnclosure | Select-Object -First 1 -ExpandProperty ChassisTypes)
$out.chassis = @($ch)

$nets = @()
Get-CimInstance Win32_NetworkAdapter | Where-Object {
    $_.PhysicalAdapter -eq $true -and $_.Name -notmatch 'Virtual|Hyper-V|TAP|VPN|WAN Miniport|Bluetooth|Loopback|Kernel Debug'
} | ForEach-Object {
    $kind = if ($_.Name -match 'Wi-Fi|WiFi|Wireless|802\.11|WLAN|AX\d{3}|BE\d{3}' -or $_.NetConnectionID -match 'Wi-Fi|WLAN') { 'Wi-Fi' } else { 'LAN' }
    $st = if ($_.NetConnectionStatus -eq 2) { 'Connected' } else { 'Idle' }
    if ($_.Name) { $nets += "${kind}: $($_.Name.Trim()) [$st]" }
}
$out.network = @($nets | Select-Object -Unique)

$bt = @()
Get-CimInstance Win32_PnPEntity | Where-Object {
    $_.Present -eq $true -and ($_.PNPClass -eq 'Bluetooth' -or $_.Service -match 'BTH|RFCOMM') -and
    $_.Name -notmatch 'Enumerator|Transport|Protocol|Service|Profile|Device Identification|Avrcp|A2dp|Hands-Free|Standard Serial over Bluetooth|RFCOMM'
} | ForEach-Object {
    if ($_.Name) { $bt += $_.Name.Trim() }
}
$out.bluetooth = @($bt | Select-Object -Unique)

$aud = @()
Get-CimInstance Win32_SoundDevice | Where-Object { $_.Status -eq 'OK' } | ForEach-Object {
    if ($_.Name -and $_.Name -notmatch 'Virtual|VB-Audio|FxSound|^High Definition Audio Device$|^USB Audio 2\.0$') { $aud += $_.Name.Trim() }
}
Get-CimInstance Win32_PnPEntity | Where-Object {
    $_.Present -eq $true -and $_.PNPClass -eq 'AudioEndpoint' -and $_.Name -notmatch 'VB-Audio|FxSound'
} | ForEach-Object {
    if ($_.Name) { $aud += $_.Name.Trim() }
}
$out.audio = @($aud | Select-Object -Unique)

$periph = @()
Get-CimInstance Win32_PointingDevice | ForEach-Object {
    if ($_.Name -and $_.Name -notmatch 'HID-compliant mouse|PS/2|USB Input Device') { $periph += "Mouse: $($_.Name.Trim())" }
}
Get-CimInstance Win32_Keyboard | ForEach-Object {
    if ($_.Name -and $_.Name -notmatch 'Enhanced \(101|HID Keyboard Device|Standard PS/2') { $periph += "Keyboard: $($_.Name.Trim())" }
}
$usbParents = @(Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -match '^USB\\VID_' -and $_.InstanceId -notmatch '&MI_' })
$usbIds = @($usbParents | ForEach-Object { $_.InstanceId })
if ($usbIds.Count -gt 0) {
    Get-PnpDeviceProperty -InstanceId $usbIds -KeyName 'DEVPKEY_Device_BusReportedDeviceDesc' | ForEach-Object {
        if ($_.Data) {
            $d = "$($_.Data)".Trim()
            if ($d -and $d -notmatch 'USB Mass Storage|USB Composite|Generic USB|Root Hub|Input Device|UASP') {
                $periph += $d
            }
        }
    }
}
Get-CimInstance Win32_PnPEntity | Where-Object {
    $_.Present -eq $true -and ($_.PNPClass -eq 'Camera' -or $_.PNPClass -eq 'Image')
} | ForEach-Object {
    if ($_.Name) { $periph += "Camera: $($_.Name.Trim())" }
}
$out.peripherals = @($periph | Select-Object -Unique)

$out | ConvertTo-Json -Depth 4 -Compress
"#;

    let json_arr_str = |v: &serde_json::Value, key: &str| -> Vec<String> {
        dedupe_clean_list(
            v.get(key)
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        )
    };

    if let Ok(out) = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", ps])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        if out.status.success() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if let Some(mb) = v.get("motherboard").and_then(|x| x.as_str()) {
                    hw.motherboard = mb.trim().to_string();
                }
                if let Some(arr) = v.get("ram_sticks").and_then(|x| x.as_array()) {
                    let mut sticks = Vec::new();
                    for st in arr {
                        let gb = st.get("gb").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                        if gb <= 0.0 {
                            continue;
                        }
                        let gen = st
                            .get("gen")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let speed = st.get("speed").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                        let part = st
                            .get("part")
                            .and_then(|x| x.as_str())
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("unknown"))
                            .map(|s| s.to_string())
                            .or_else(|| {
                                st.get("mfg")
                                    .and_then(|x| x.as_str())
                                    .map(|s| s.trim())
                                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("unknown"))
                                    .map(|s| s.to_string())
                            })
                            .unwrap_or_default();
                        sticks.push((gb, gen, speed, part));
                    }
                    hw.ram_details = format_ram_sticks_summary(ram_gb, &sticks);
                }
                if let Some(arr) = v.get("gpus").and_then(|g| g.as_array()) {
                    let names: Vec<String> = arr
                        .iter()
                        .filter_map(|g| g.get("name").and_then(|n| n.as_str()).map(str::to_string))
                        .collect();
                    hw.gpu_name = names.join(" + ");
                    hw.gpu_vram_gb = arr
                        .iter()
                        .filter_map(|g| g.get("vram").and_then(|x| x.as_f64()))
                        .map(|x| x as f32)
                        .fold(0.0, f32::max);
                }
                hw.storage_drives = json_arr_str(&v, "storage_drives");
                hw.monitors = json_arr_str(&v, "monitors");
                hw.network_adapters = json_arr_str(&v, "network");
                hw.bluetooth_devices = json_arr_str(&v, "bluetooth");
                hw.audio_devices = json_arr_str(&v, "audio");
                hw.usb_peripherals = json_arr_str(&v, "peripherals");
                if let Some(arr) = v.get("chassis").and_then(|c| c.as_array()) {
                    for c in arr {
                        if let Some(code) = c.as_u64() {
                            if matches!(code, 8 | 9 | 10 | 14 | 30 | 31 | 32) {
                                hw.is_laptop_override = Some(true);
                            }
                        }
                    }
                }
            }
        }
    }
    hw
}

#[cfg(target_os = "linux")]
fn fetch_platform_hw_details(ram_gb: f32) -> PlatformHwDetails {
    let mut hw = PlatformHwDetails::default();
    let read_trim = |p: &str| -> String {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    // Motherboard / System model + chassis form factor
    let b_vendor = read_trim("/sys/class/dmi/id/board_vendor");
    let b_name = read_trim("/sys/class/dmi/id/board_name");
    let board = format!("{} {}", b_vendor, b_name).trim().to_string();
    if !board.is_empty() {
        hw.motherboard = board;
    } else {
        let s_vendor = read_trim("/sys/class/dmi/id/sys_vendor");
        let p_name = read_trim("/sys/class/dmi/id/product_name");
        hw.motherboard = format!("{} {}", s_vendor, p_name).trim().to_string();
    }
    if let Ok(ct) = read_trim("/sys/class/dmi/id/chassis_type").parse::<u64>() {
        if matches!(ct, 8 | 9 | 10 | 14 | 30 | 31 | 32) {
            hw.is_laptop_override = Some(true);
        }
    }

    // RAM summary (checks optional non-root lshw if installed, otherwise clear total)
    let mut ram_sticks: Vec<(f32, String, u32, String)> = Vec::new();
    if let Ok(out) = Command::new("lshw")
        .args(["-C", "memory", "-json", "-quiet"])
        .output()
    {
        if out.status.success() {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                fn collect_lshw_dimms(
                    v: &serde_json::Value,
                    out: &mut Vec<(f32, String, u32, String)>,
                ) {
                    if let Some(arr) = v.as_array() {
                        for item in arr {
                            collect_lshw_dimms(item, out);
                        }
                        return;
                    }
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if id.starts_with("bank") {
                        let size_bytes = v.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
                        if size_bytes > 0 {
                            let gb = (size_bytes as f64 / (1024.0 * 1024.0 * 1024.0)) as f32;
                            let desc = v.get("description").and_then(|x| x.as_str()).unwrap_or("");
                            let gen = ["DDR5", "DDR4", "DDR3", "LPDDR5", "LPDDR4"]
                                .iter()
                                .find(|g| desc.to_ascii_uppercase().contains(*g))
                                .unwrap_or(&"")
                                .to_string();
                            let clock_hz = v.get("clock").and_then(|x| x.as_u64()).unwrap_or(0);
                            let mhz = if clock_hz > 1_000_000 {
                                (clock_hz / 1_000_000) as u32
                            } else {
                                0
                            };
                            let part = v
                                .get("product")
                                .or_else(|| v.get("vendor"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            out.push((gb, gen, mhz, part));
                        }
                    }
                    if let Some(children) = v.get("children") {
                        collect_lshw_dimms(children, out);
                    }
                }
                collect_lshw_dimms(&val, &mut ram_sticks);
            }
        }
    }
    hw.ram_details = format_ram_sticks_summary(ram_gb, &ram_sticks);

    // Physical Storage Drives via lsblk -J (never mounted partitions)
    let mut drives = Vec::new();
    if let Ok(out) = Command::new("lsblk")
        .args(["-J", "-b", "-o", "NAME,MODEL,SIZE,ROTA,TYPE,TRAN"])
        .output()
    {
        if out.status.success() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if let Some(arr) = v.get("blockdevices").and_then(|b| b.as_array()) {
                    for dev in arr {
                        let dtype = dev.get("type").and_then(|x| x.as_str()).unwrap_or("");
                        if dtype != "disk" {
                            continue;
                        }
                        let name = dev.get("name").and_then(|x| x.as_str()).unwrap_or("");
                        let size_b = dev
                            .get("size")
                            .and_then(|x| x.as_u64().or_else(|| x.as_str()?.parse().ok()))
                            .unwrap_or(0);
                        if size_b < 1_000_000_000 {
                            continue;
                        }
                        let gb = (size_b as f64 / 1_000_000_000.0).round() as u64;
                        let model = dev
                            .get("model")
                            .and_then(|x| x.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .unwrap_or(name);
                        let tran = dev
                            .get("tran")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_ascii_uppercase();
                        let rota = dev.get("rota").and_then(|x| x.as_bool());
                        let media = match rota {
                            Some(false) => "SSD",
                            Some(true) => "HDD",
                            None => "",
                        };
                        let kind = [tran.as_str(), media]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if kind.is_empty() {
                            drives.push(format!("{} ({} GB)", model, gb));
                        } else {
                            drives.push(format!("{} ({}, {} GB)", model, kind, gb));
                        }
                    }
                }
            }
        }
    }
    if drives.is_empty() {
        if let Ok(entries) = std::fs::read_dir("/sys/block") {
            for entry in entries.flatten() {
                let bname = entry.file_name().to_string_lossy().to_string();
                if bname.starts_with("loop")
                    || bname.starts_with("ram")
                    || bname.starts_with("zram")
                {
                    continue;
                }
                let base = entry.path();
                let sectors: u64 = std::fs::read_to_string(base.join("size"))
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);
                let gb = (sectors * 512) / 1_000_000_000;
                if gb == 0 {
                    continue;
                }
                let model = std::fs::read_to_string(base.join("device/model"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let rota = std::fs::read_to_string(base.join("queue/rotational"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let media = if bname.starts_with("nvme") {
                    "NVMe SSD"
                } else if rota == "0" {
                    "SSD"
                } else if rota == "1" {
                    "HDD"
                } else {
                    "Disk"
                };
                let label = if model.is_empty() { &bname } else { &model };
                drives.push(format!("{} ({}, {} GB)", label, media, gb));
            }
        }
    }
    hw.storage_drives = dedupe_clean_list(drives);

    // GPU + optional VRAM via nvidia-smi / AMD sysfs
    let mut gpus = Vec::new();
    if let Ok(out) = Command::new("lspci").arg("-mm").output() {
        if out.status.success() {
            let txt = String::from_utf8_lossy(&out.stdout);
            for line in txt.lines() {
                if line.contains("VGA compatible controller")
                    || line.contains("3D controller")
                    || line.contains("Display controller")
                {
                    let fields: Vec<&str> = line.split('"').collect();
                    if fields.len() >= 6 {
                        gpus.push(format!("{} {}", fields[3], fields[5]).trim().to_string());
                    } else {
                        gpus.push(line.to_string());
                    }
                }
            }
        }
    }
    hw.gpu_name = dedupe_clean_list(gpus).join(" + ");
    if let Ok(out) = Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output()
    {
        if out.status.success() {
            if let Some(mb) = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .and_then(|l| l.trim().parse::<f32>().ok())
            {
                hw.gpu_vram_gb = (mb / 1024.0 * 10.0).round() / 10.0;
            }
        }
    }
    if hw.gpu_vram_gb <= 0.0 {
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
            for e in entries.flatten() {
                let p = e.path().join("device/mem_info_vram_total");
                if let Ok(bytes_str) = std::fs::read_to_string(p) {
                    if let Ok(b) = bytes_str.trim().parse::<f64>() {
                        hw.gpu_vram_gb = hw.gpu_vram_gb.max((b / 1073741824.0) as f32);
                    }
                }
            }
        }
    }

    // Monitors (xrandr + DRM sysfs fallback for Wayland)
    let mut mons = Vec::new();
    if let Ok(out) = Command::new("xrandr").arg("--listmonitors").output() {
        if out.status.success() {
            let txt = String::from_utf8_lossy(&out.stdout);
            for line in txt.lines().skip(1) {
                let t = line.trim();
                if !t.is_empty() {
                    mons.push(t.to_string());
                }
            }
        }
    }
    if mons.is_empty() {
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
            for e in entries.flatten() {
                let p = e.path();
                if read_trim(&p.join("status").to_string_lossy()) == "connected" {
                    let name = e.file_name().to_string_lossy().to_string();
                    mons.push(name);
                }
            }
        }
    }
    hw.monitors = dedupe_clean_list(mons);

    // Network (Wi-Fi / LAN)
    let mut nets = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for e in entries.flatten() {
            let ifname = e.file_name().to_string_lossy().to_string();
            if ifname == "lo"
                || ifname.starts_with("docker")
                || ifname.starts_with("veth")
                || ifname.starts_with("br-")
                || ifname.starts_with("virbr")
                || ifname.starts_with("tun")
                || ifname.starts_with("tap")
                || ifname.starts_with("wg")
            {
                continue;
            }
            let base = e.path();
            let is_wifi = base.join("wireless").exists();
            let dev_type = read_trim(&base.join("type").to_string_lossy());
            if !is_wifi && dev_type != "1" {
                continue;
            }
            let kind = if is_wifi { "Wi-Fi" } else { "LAN" };
            let state = if read_trim(&base.join("operstate").to_string_lossy()) == "up" {
                "Connected"
            } else {
                "Idle"
            };
            let driver = std::fs::read_to_string(base.join("device/uevent"))
                .ok()
                .and_then(|u| {
                    u.lines()
                        .find_map(|l| l.strip_prefix("DRIVER="))
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            if driver.is_empty() {
                nets.push(format!("{}: {} [{}]", kind, ifname, state));
            } else {
                nets.push(format!("{}: {} ({}) [{}]", kind, ifname, driver, state));
            }
        }
    }
    hw.network_adapters = dedupe_clean_list(nets);

    // Bluetooth
    let mut bt = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/bluetooth") {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.contains(':') {
                bt.push(format!("Bluetooth Adapter ({})", name));
            }
        }
    }
    if let Ok(out) = Command::new("bluetoothctl")
        .args(["devices", "Connected"])
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(rest) = line.strip_prefix("Device ") {
                    if let Some((_, dev_name)) = rest.split_once(' ') {
                        bt.push(dev_name.trim().to_string());
                    }
                }
            }
        }
    }
    hw.bluetooth_devices = dedupe_clean_list(bt);

    // Audio / Mic
    let mut aud = Vec::new();
    if let Ok(cards) = std::fs::read_to_string("/proc/asound/cards") {
        for line in cards.lines() {
            let t = line.trim();
            if t.starts_with(|c: char| c.is_ascii_digit()) {
                if let Some((_, desc)) = t.split_once("]: ") {
                    aud.push(desc.trim().to_string());
                }
            }
        }
    }
    hw.audio_devices = dedupe_clean_list(aud);

    // USB Peripherals, Mice & Keyboards
    let mut periph = Vec::new();
    if let Ok(out) = Command::new("lsusb").output() {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let lower = line.to_ascii_lowercase();
                if lower.contains("root hub") || lower.contains("linux foundation") {
                    continue;
                }
                if let Some(idx) = line.find("ID ") {
                    let after = &line[idx + 3..];
                    if let Some((_, name)) = after.split_once(' ') {
                        periph.push(name.trim().to_string());
                    }
                }
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir("/sys/class/input") {
        for e in entries.flatten() {
            let name_path = e.path().join("device/name");
            if let Ok(n) = std::fs::read_to_string(name_path) {
                let t = n.trim();
                let tl = t.to_ascii_lowercase();
                if t.is_empty()
                    || tl.contains("power button")
                    || tl.contains("sleep button")
                    || tl.contains("video bus")
                    || tl.contains("pc speaker")
                    || tl.contains("hda ")
                {
                    continue;
                }
                if tl.contains("mouse")
                    || tl.contains("keyboard")
                    || tl.contains("trackpad")
                    || tl.contains("touchpad")
                {
                    periph.push(t.to_string());
                }
            }
        }
    }
    hw.usb_peripherals = dedupe_clean_list(periph);
    hw
}

#[cfg(target_os = "macos")]
fn fetch_platform_hw_details(ram_gb: f32) -> PlatformHwDetails {
    let mut hw = PlatformHwDetails::default();
    let out = match Command::new("system_profiler")
        .args([
            "SPHardwareDataType",
            "SPMemoryDataType",
            "SPNVMeDataType",
            "SPSerialATADataType",
            "SPDisplaysDataType",
            "SPAudioDataType",
            "SPAirPortDataType",
            "SPEthernetDataType",
            "SPBluetoothDataType",
            "SPUSBDataType",
            "-json",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return hw,
    };

    let v: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(val) => val,
        Err(_) => return hw,
    };

    if let Some(first) = v
        .get("SPHardwareDataType")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
    {
        let mname = first
            .get("machine_name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let mmodel = first
            .get("machine_model")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        hw.motherboard = format!("{} ({})", mname, mmodel)
            .trim()
            .trim_end_matches("()")
            .trim()
            .to_string();
        if mname.to_ascii_lowercase().contains("book")
            || mmodel.to_ascii_lowercase().contains("book")
        {
            hw.is_laptop_override = Some(true);
        }
    }

    // RAM / Unified Memory
    let mut ram_sticks = Vec::new();
    let mut unified_desc = String::new();
    if let Some(arr) = v.get("SPMemoryDataType").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(dimms) = item.get("_items").and_then(|x| x.as_array()) {
                for d in dimms {
                    let size_s = d.get("dimm_size").and_then(|x| x.as_str()).unwrap_or("");
                    let gb: f32 = size_s
                        .split_whitespace()
                        .next()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0.0);
                    if gb <= 0.0 {
                        continue;
                    }
                    let gen = d
                        .get("dimm_type")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let spd_s = d.get("dimm_speed").and_then(|x| x.as_str()).unwrap_or("");
                    let mhz: u32 = spd_s
                        .split_whitespace()
                        .next()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    let mfg = d
                        .get("dimm_manufacturer")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    ram_sticks.push((gb, gen, mhz, mfg));
                }
            } else if let Some(d_type) = item.get("dimm_type").and_then(|x| x.as_str()) {
                let mfg = item
                    .get("dimm_manufacturer")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                unified_desc = format!("{:.1} GB ({} {})", ram_gb, mfg, d_type)
                    .trim()
                    .to_string();
            }
        }
    }
    hw.ram_details = if !ram_sticks.is_empty() {
        format_ram_sticks_summary(ram_gb, &ram_sticks)
    } else if !unified_desc.is_empty() {
        unified_desc
    } else {
        format!("{:.1} GB", ram_gb)
    };

    // Physical Storage Drives (NVMe + SATA)
    let mut drives = Vec::new();
    for (section_key, bus_lbl) in [("SPNVMeDataType", "NVMe"), ("SPSerialATADataType", "SATA")] {
        if let Some(controllers) = v.get(section_key).and_then(|x| x.as_array()) {
            for ctrl in controllers {
                if let Some(items) = ctrl.get("_items").and_then(|x| x.as_array()) {
                    for d in items {
                        let name = d
                            .get("device_model")
                            .or_else(|| d.get("_name"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .trim();
                        if name.is_empty() {
                            continue;
                        }
                        let size = d.get("size").and_then(|x| x.as_str()).unwrap_or("");
                        if size.is_empty() {
                            drives.push(format!("{} ({})", name, bus_lbl));
                        } else {
                            drives.push(format!("{} ({}, {})", name, bus_lbl, size));
                        }
                    }
                }
            }
        }
    }
    hw.storage_drives = dedupe_clean_list(drives);

    // GPUs & Monitors
    if let Some(arr) = v.get("SPDisplaysDataType").and_then(|d| d.as_array()) {
        let mut gpus = Vec::new();
        let mut mons = Vec::new();
        for card in arr {
            if let Some(m) = card.get("sppci_model").and_then(|x| x.as_str()) {
                gpus.push(m.to_string());
            }
            if let Some(vram_s) = card
                .get("_spdisplays_vram")
                .or_else(|| card.get("spdisplays_vram"))
                .and_then(|x| x.as_str())
            {
                if let Some(num) = vram_s
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<f32>().ok())
                {
                    let gb = if vram_s.contains("MB") {
                        num / 1024.0
                    } else {
                        num
                    };
                    hw.gpu_vram_gb = hw.gpu_vram_gb.max(gb);
                }
            }
            if let Some(ndrvs) = card.get("spdisplays_ndrvs").and_then(|d| d.as_array()) {
                for d in ndrvs {
                    if let Some(n) = d.get("_name").and_then(|x| x.as_str()) {
                        mons.push(n.to_string());
                    }
                }
            }
        }
        hw.gpu_name = dedupe_clean_list(gpus).join(" + ");
        hw.monitors = dedupe_clean_list(mons);
    }

    // Network (Wi-Fi & Ethernet LAN)
    let mut nets = Vec::new();
    if let Some(arr) = v.get("SPAirPortDataType").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(ifaces) = item
                .get("spairport_airport_interfaces")
                .and_then(|x| x.as_array())
            {
                for iface in ifaces {
                    let name = iface.get("_name").and_then(|x| x.as_str()).unwrap_or("en0");
                    let card_t = iface
                        .get("spairport_wireless_card_type")
                        .and_then(|x| x.as_str())
                        .unwrap_or("Wi-Fi");
                    nets.push(format!("Wi-Fi: {} ({})", card_t, name));
                }
            }
        }
    }
    if let Some(arr) = v.get("SPEthernetDataType").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(cards) = item.get("ethernet_cards").and_then(|x| x.as_array()) {
                for c in cards {
                    let name = c
                        .get("spethernet_device-name")
                        .or_else(|| c.get("_name"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("Ethernet");
                    nets.push(format!("LAN: {}", name));
                }
            } else if let Some(name) = item.get("_name").and_then(|x| x.as_str()) {
                nets.push(format!("LAN: {}", name));
            }
        }
    }
    hw.network_adapters = dedupe_clean_list(nets);

    // Bluetooth
    let mut bt = Vec::new();
    if let Some(arr) = v.get("SPBluetoothDataType").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(chip) = item
                .get("controller_properties")
                .and_then(|c| c.get("controller_chipset"))
                .and_then(|x| x.as_str())
            {
                bt.push(chip.to_string());
            }
            if let Some(devs) = item.get("device_connected").and_then(|x| x.as_array()) {
                for dev in devs {
                    if let Some(obj) = dev.as_object() {
                        for k in obj.keys() {
                            bt.push(k.clone());
                        }
                    }
                }
            }
        }
    }
    hw.bluetooth_devices = dedupe_clean_list(bt);

    // Audio / Mic
    let mut aud = Vec::new();
    if let Some(arr) = v.get("SPAudioDataType").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(devs) = item.get("_items").and_then(|x| x.as_array()) {
                for d in devs {
                    if let Some(n) = d.get("_name").and_then(|x| x.as_str()) {
                        aud.push(n.to_string());
                    }
                }
            }
        }
    }
    hw.audio_devices = dedupe_clean_list(aud);

    // USB Peripherals (recursive walk through nested hubs)
    let mut usb = Vec::new();
    fn walk_mac_usb(val: &serde_json::Value, out: &mut Vec<String>) {
        if let Some(arr) = val.as_array() {
            for item in arr {
                walk_mac_usb(item, out);
            }
            return;
        }
        if let Some(name) = val.get("_name").and_then(|x| x.as_str()) {
            let nl = name.to_ascii_lowercase();
            if !nl.contains("bus") && !nl.contains("root hub") && nl != "hub" {
                out.push(name.to_string());
            }
        }
        if let Some(children) = val.get("_items") {
            walk_mac_usb(children, out);
        }
    }
    if let Some(arr) = v.get("SPUSBDataType") {
        walk_mac_usb(arr, &mut usb);
    }
    hw.usb_peripherals = dedupe_clean_list(usb);
    hw
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn fetch_platform_hw_details(ram_gb: f32) -> PlatformHwDetails {
    PlatformHwDetails {
        ram_details: format!("{:.1} GB", ram_gb),
        ..Default::default()
    }
}

pub fn trigger_spec_fetch_locked(s: &mut SharedState, state: Arc<Mutex<SharedState>>) {
    if s.watt_calc.spec_fetch_in_progress {
        return;
    }
    s.watt_calc.spec_fetch_in_progress = true;
    let gpu_pwr_w = s.gpu_power_mw.map(|mw| mw as f32 / 1000.0);

    std::thread::spawn(move || {
        let (cpu_brand, cpu_cores, cpu_logical_threads, ram_gb) = match System::new() {
            Ok(mut sys) => {
                sys.refresh_cpu_list(sysinfo::CpuRefreshKind::everything());
                sys.refresh_memory();
                let brand = sys
                    .cpus()
                    .first()
                    .map(|c| c.brand().trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Unknown CPU".to_string());
                let threads = sys.cpus().len().max(1);
                let cores = System::physical_core_count().ok().unwrap_or(threads);
                let ram = (sys.total_memory() as f64 / (1024.0 * 1024.0 * 1024.0)) as f32;
                (brand, cores, threads.max(cores), ram)
            }
            Err(_) => {
                let cores = System::physical_core_count().unwrap_or(1);
                ("Unknown CPU".to_string(), cores, cores, 0.0)
            }
        };
        let os_name = System::long_os_version()
            .ok()
            .unwrap_or_else(|| std::env::consts::OS.to_string());

        let storage_summary = match Disks::new_with_refreshed_list() {
            Ok(disks) => {
                let disk_count = disks.len();
                let total_disk_gb: u64 =
                    disks.iter().map(|d| d.total_space()).sum::<u64>() / (1024 * 1024 * 1024);
                format!("{} volume(s) (~{} GB total)", disk_count, total_disk_gb)
            }
            Err(_) => "Unknown".to_string(),
        };

        let mut hw = fetch_platform_hw_details(ram_gb);
        classify_rgb_and_cooling_peripherals(&mut hw.usb_peripherals, &mut hw.cabinet_cooling);
        let is_laptop = hw
            .is_laptop_override
            .unwrap_or_else(|| get_battery_status().has_battery);
        let ram_details = if hw.ram_details.is_empty() {
            format!("{:.1} GB", ram_gb)
        } else {
            hw.ram_details
        };

        let detected_rgb_ctrl = !hw.cabinet_cooling.is_empty();

        let specs = DetectedSystemSpecs {
            fetched: true,
            os_name,
            motherboard: hw.motherboard,
            cpu_brand,
            cpu_cores,
            cpu_logical_threads,
            ram_gb,
            ram_details,
            gpu_name: hw.gpu_name,
            gpu_vram_gb: hw.gpu_vram_gb,
            gpu_live_power_w: gpu_pwr_w,
            storage_summary,
            storage_drives: hw.storage_drives,
            monitors: hw.monitors,
            network_adapters: hw.network_adapters,
            bluetooth_devices: hw.bluetooth_devices,
            audio_devices: hw.audio_devices,
            usb_peripherals: hw.usb_peripherals,
            cabinet_cooling: hw.cabinet_cooling,
            is_laptop,
        };

        let mut s = state.lock().unwrap();
        s.watt_calc.spec_fetch_in_progress = false;
        if detected_rgb_ctrl {
            s.watt_calc.has_rgb_strips_or_hub = true;
            if s.watt_calc.argb_fan_count == 0 && !specs.is_laptop {
                s.watt_calc.argb_fan_count = s.watt_calc.case_fan_count.max(3);
                if s.watt_calc.case_fan_count < s.watt_calc.argb_fan_count {
                    s.watt_calc.case_fan_count = s.watt_calc.argb_fan_count;
                }
            }
        }
        if !specs.monitors.is_empty() {
            let new_count = specs.monitors.len() as u32;
            if !s.detected_specs.fetched
                && s.watt_calc.monitor_count <= 1
                && new_count > 1
                && s.watt_calc.monitor_each_w > 55.0
            {
                // Initial monitor_each_w came from config.monitor_watts (total across monitors);
                // split evenly across newly discovered displays instead of multiplying total by N.
                s.watt_calc.monitor_each_w = (s.watt_calc.monitor_each_w / new_count as f32)
                    .round()
                    .clamp(5.0, 200.0);
            }
            s.watt_calc.monitor_count = new_count;
            s.watt_calc.monitor_models_note = specs.monitors.join(", ");
        }
        if s.watt_calc.extra_gear_note == WattCalculatorState::default().extra_gear_note {
            s.watt_calc.extra_gear_note = if specs.is_laptop {
                "Laptop built-in speakers/mic + external peripherals".to_string()
            } else {
                "Standard PC chassis cooling & desk accessories".to_string()
            };
        }
        if let Some(w) = gpu_pwr_w {
            if w >= 1.0 && w <= 150.0 {
                s.watt_calc.gpu_idle_w = w.round();
            }
        }
        if specs.is_laptop {
            s.watt_calc.smps_model_note = "Laptop Power Brick".to_string();
            s.watt_calc.psu_efficiency_pct = 88.0;
            s.watt_calc.case_fan_count = 0;
            s.watt_calc.argb_fan_count = 0;
            s.watt_calc.cooler_type = "Laptop Internal Cooling".to_string();
        }
        s.detected_specs = specs;
        let prompt = build_ai_watt_prompt(
            &s.detected_specs,
            &s.watt_calc,
            &s.config.currency_symbol,
            s.config.electricity_rate_kwh,
        );
        s.watt_calc.generated_prompt = prompt;
        s.set_toast("Detailed hardware specs, drives, RAM, network & peripherals fetched.");
    });
}

pub fn execute_software_uninstall(wipe_data: bool) -> Result<(), String> {
    set_startup_task(false);
    uninstall_service();

    if wipe_data {
        if let Some(dir) = get_energy_history_path().parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[cfg(windows)]
    {
        // Check Windows MSI Uninstall registry first
        let ps = r#"
$paths = @(
  'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*'
)
$item = Get-ItemProperty $paths -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -like '*Agent Power Guard*' } | Select-Object -First 1
if ($item -and $item.PSChildName -match '^\{[0-9A-Fa-f-]+\}$') { Write-Output $item.PSChildName }
"#;
        if let Ok(out) = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", ps])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let guid = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if guid.starts_with('{') && guid.ends_with('}') {
                Command::new("msiexec")
                    .args(["/x", &guid])
                    .spawn()
                    .map_err(|e| format!("Failed to start Windows Uninstaller: {}", e))?;
                std::process::exit(0);
            }
        }

        // Portable EXE self-delete after process exits
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Command::new("cmd")
            .args([
                "/C",
                &format!(
                    "ping 127.0.0.1 -n 3 > nul & del /f /q \"{}\"",
                    exe.to_string_lossy()
                ),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Failed to schedule binary cleanup: {}", e))?;
        std::process::exit(0);
    }

    #[cfg(not(windows))]
    {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        #[cfg(target_os = "macos")]
        {
            let s = exe.to_string_lossy();
            if let Some(idx) = s.find(".app/Contents/MacOS/") {
                let bundle = &s[..idx + 4];
                let _ = std::fs::remove_dir_all(bundle);
                std::process::exit(0);
            }
        }
        let _ = std::fs::remove_file(&exe);
        std::process::exit(0);
    }
}

fn format_time_compact(secs: u32) -> String {
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 {
        format!("{}m {:02}s", m, s)
    } else {
        format!("{}s", s)
    }
}

pub fn copy_text_reliable(ctx: &egui::Context, text: &str) {
    ctx.copy_text(text.to_string());
    let owned = text.to_string();
    std::thread::spawn(move || {
        use std::io::Write as _;
        #[cfg(windows)]
        {
            if let Ok(mut child) = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "[Console]::InputEncoding = [System.Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())",
                ])
                .stdin(std::process::Stdio::piped())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(owned.as_bytes());
                }
                let _ = child.wait();
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Ok(mut child) = Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(owned.as_bytes());
                }
                let _ = child.wait();
            }
        }
        #[cfg(target_os = "linux")]
        {
            let mut copied = false;
            if let Ok(mut child) = Command::new("xclip")
                .args(["-selection", "clipboard"])
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(owned.as_bytes());
                }
                if let Ok(st) = child.wait() {
                    copied = st.success();
                }
            }
            if !copied {
                if let Ok(mut child) = Command::new("wl-copy")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                {
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(owned.as_bytes());
                    }
                    let _ = child.wait();
                }
            }
        }
    });
}

pub fn read_os_clipboard() -> Option<String> {
    #[cfg(windows)]
    {
        if let Ok(out) = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; Get-Clipboard -Raw",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).to_string();
                if !s.trim().is_empty() {
                    return Some(s);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new("pbpaste").output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).to_string();
                if !s.trim().is_empty() {
                    return Some(s);
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        for (bin, args) in [
            ("xclip", vec!["-selection", "clipboard", "-o"]),
            ("wl-paste", vec![]),
        ] {
            if let Ok(out) = Command::new(bin).args(&args).output() {
                if out.status.success() {
                    let s = String::from_utf8_lossy(&out.stdout).to_string();
                    if !s.trim().is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

#[derive(Clone, Copy)]
pub enum UiIcon {
    Shield,
    Pause,
    Bolt,
    Info,
    Sleep,
    Clock,
    Sliders,
    Tray,
    Sun,
    Moon,
    History,
    Coin,
    Bell,
    Globe,
    Search,
    Copy,
    Folder,
    Trash,
    Download,
    Check,
    Warning,
    Lock,
    Battery,
    Cpu,
    Refresh,
    Activity,
}

fn paint_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32, icon: UiIcon) {
    let w = rect.width();
    let h = rect.height();
    let l = rect.left();
    let t = rect.top();
    let p = |x: f32, y: f32| egui::pos2(l + x * w, t + y * h);
    let st = egui::Stroke::new(1.5, color);

    match icon {
        UiIcon::Shield => {
            let pts = [
                p(0.50, 0.10),
                p(0.86, 0.24),
                p(0.82, 0.58),
                p(0.50, 0.90),
                p(0.18, 0.58),
                p(0.14, 0.24),
                p(0.50, 0.10),
            ];
            for seg in pts.windows(2) {
                painter.line_segment([seg[0], seg[1]], st);
            }
            painter.line_segment([p(0.35, 0.50), p(0.46, 0.62)], st);
            painter.line_segment([p(0.46, 0.62), p(0.68, 0.38)], st);
        }
        UiIcon::Pause => {
            painter.rect_filled(
                egui::Rect::from_min_max(p(0.24, 0.18), p(0.42, 0.82)),
                1.0,
                color,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(p(0.58, 0.18), p(0.76, 0.82)),
                1.0,
                color,
            );
        }
        UiIcon::Bolt => {
            painter.line_segment([p(0.58, 0.10), p(0.26, 0.52)], st);
            painter.line_segment([p(0.26, 0.52), p(0.52, 0.52)], st);
            painter.line_segment([p(0.52, 0.52), p(0.40, 0.90)], st);
            painter.line_segment([p(0.40, 0.90), p(0.74, 0.44)], st);
            painter.line_segment([p(0.74, 0.44), p(0.48, 0.44)], st);
            painter.line_segment([p(0.48, 0.44), p(0.58, 0.10)], st);
        }
        UiIcon::Info => {
            painter.circle_stroke(rect.center(), w * 0.40, st);
            painter.circle_filled(p(0.50, 0.30), 1.1, color);
            painter.line_segment([p(0.50, 0.44), p(0.50, 0.72)], st);
        }
        UiIcon::Sleep => {
            // Crescent moon + Z
            painter.circle_stroke(rect.center(), w * 0.38, st);
            painter.line_segment([p(0.36, 0.36), p(0.64, 0.36)], st);
            painter.line_segment([p(0.64, 0.36), p(0.36, 0.64)], st);
            painter.line_segment([p(0.36, 0.64), p(0.64, 0.64)], st);
        }
        UiIcon::Clock => {
            painter.circle_stroke(rect.center(), w * 0.40, st);
            painter.line_segment([p(0.50, 0.26), p(0.50, 0.52)], st);
            painter.line_segment([p(0.50, 0.52), p(0.68, 0.62)], st);
        }
        UiIcon::Sliders => {
            for y in [0.26, 0.50, 0.74] {
                painter.line_segment([p(0.14, y), p(0.86, y)], st);
            }
            painter.circle_filled(p(0.36, 0.26), 2.1, color);
            painter.circle_filled(p(0.66, 0.50), 2.1, color);
            painter.circle_filled(p(0.44, 0.74), 2.1, color);
        }
        UiIcon::Tray => {
            painter.line_segment([p(0.18, 0.60), p(0.18, 0.84)], st);
            painter.line_segment([p(0.18, 0.84), p(0.82, 0.84)], st);
            painter.line_segment([p(0.82, 0.84), p(0.82, 0.60)], st);
            painter.line_segment([p(0.50, 0.16), p(0.50, 0.64)], st);
            painter.line_segment([p(0.32, 0.48), p(0.50, 0.66)], st);
            painter.line_segment([p(0.68, 0.48), p(0.50, 0.66)], st);
        }
        UiIcon::Sun => {
            painter.circle_stroke(rect.center(), w * 0.22, st);
            for (dx, dy) in [
                (0.0, -1.0),
                (0.0, 1.0),
                (-1.0, 0.0),
                (1.0, 0.0),
                (-0.7, -0.7),
                (0.7, -0.7),
                (-0.7, 0.7),
                (0.7, 0.7),
            ] {
                let c = rect.center();
                let r1 = w * 0.30;
                let r2 = w * 0.43;
                painter.line_segment(
                    [
                        egui::pos2(c.x + dx * r1, c.y + dy * r1),
                        egui::pos2(c.x + dx * r2, c.y + dy * r2),
                    ],
                    st,
                );
            }
        }
        UiIcon::Moon => {
            let pts = [
                p(0.62, 0.16),
                p(0.42, 0.24),
                p(0.32, 0.42),
                p(0.34, 0.62),
                p(0.48, 0.78),
                p(0.68, 0.84),
                p(0.82, 0.78),
                p(0.64, 0.70),
                p(0.56, 0.52),
                p(0.58, 0.34),
                p(0.72, 0.20),
                p(0.62, 0.16),
            ];
            for seg in pts.windows(2) {
                painter.line_segment([seg[0], seg[1]], st);
            }
        }
        UiIcon::History => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.18, 0.16), p(0.82, 0.84)),
                2.0,
                st,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([p(0.32, 0.36), p(0.68, 0.36)], st);
            painter.line_segment([p(0.32, 0.52), p(0.68, 0.52)], st);
            painter.line_segment([p(0.32, 0.68), p(0.56, 0.68)], st);
        }
        UiIcon::Coin => {
            painter.line_segment([p(0.16, 0.78), p(0.84, 0.78)], st);
            painter.rect_filled(
                egui::Rect::from_min_max(p(0.22, 0.54), p(0.34, 0.74)),
                1.0,
                color,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(p(0.44, 0.38), p(0.56, 0.74)),
                1.0,
                color,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(p(0.66, 0.20), p(0.78, 0.74)),
                1.0,
                color,
            );
        }
        UiIcon::Bell => {
            painter.line_segment([p(0.28, 0.68), p(0.32, 0.32)], st);
            painter.line_segment([p(0.32, 0.32), p(0.50, 0.16)], st);
            painter.line_segment([p(0.50, 0.16), p(0.68, 0.32)], st);
            painter.line_segment([p(0.68, 0.32), p(0.72, 0.68)], st);
            painter.line_segment([p(0.20, 0.68), p(0.80, 0.68)], st);
            painter.circle_filled(p(0.50, 0.82), 1.6, color);
        }
        UiIcon::Globe => {
            painter.circle_stroke(rect.center(), w * 0.40, st);
            painter.line_segment([p(0.12, 0.50), p(0.88, 0.50)], st);
            painter.line_segment([p(0.50, 0.12), p(0.50, 0.88)], st);
        }
        UiIcon::Search => {
            painter.circle_stroke(p(0.43, 0.43), w * 0.27, st);
            painter.line_segment([p(0.63, 0.63), p(0.86, 0.86)], st);
        }
        UiIcon::Copy => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.32, 0.16), p(0.84, 0.68)),
                1.5,
                st,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([p(0.20, 0.34), p(0.20, 0.82)], st);
            painter.line_segment([p(0.20, 0.82), p(0.68, 0.82)], st);
        }
        UiIcon::Folder => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.14, 0.28), p(0.86, 0.80)),
                1.5,
                st,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([p(0.16, 0.28), p(0.24, 0.18)], st);
            painter.line_segment([p(0.24, 0.18), p(0.48, 0.18)], st);
            painter.line_segment([p(0.48, 0.18), p(0.56, 0.28)], st);
        }
        UiIcon::Trash => {
            painter.line_segment([p(0.20, 0.26), p(0.80, 0.26)], st);
            painter.line_segment([p(0.38, 0.16), p(0.62, 0.16)], st);
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.26, 0.28), p(0.74, 0.84)),
                1.5,
                st,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([p(0.42, 0.42), p(0.42, 0.72)], st);
            painter.line_segment([p(0.58, 0.42), p(0.58, 0.72)], st);
        }
        UiIcon::Download => {
            painter.line_segment([p(0.50, 0.14), p(0.50, 0.64)], st);
            painter.line_segment([p(0.32, 0.48), p(0.50, 0.66)], st);
            painter.line_segment([p(0.68, 0.48), p(0.50, 0.66)], st);
            painter.line_segment([p(0.18, 0.82), p(0.82, 0.82)], st);
        }
        UiIcon::Check => {
            painter.line_segment([p(0.18, 0.54), p(0.42, 0.76)], st);
            painter.line_segment([p(0.42, 0.76), p(0.84, 0.26)], st);
        }
        UiIcon::Warning => {
            painter.line_segment([p(0.50, 0.14), p(0.86, 0.82)], st);
            painter.line_segment([p(0.86, 0.82), p(0.14, 0.82)], st);
            painter.line_segment([p(0.14, 0.82), p(0.50, 0.14)], st);
            painter.line_segment([p(0.50, 0.38), p(0.50, 0.58)], st);
            painter.circle_filled(p(0.50, 0.70), 1.1, color);
        }
        UiIcon::Lock => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.22, 0.46), p(0.78, 0.86)),
                1.5,
                st,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([p(0.34, 0.46), p(0.34, 0.28)], st);
            painter.line_segment([p(0.34, 0.28), p(0.66, 0.28)], st);
            painter.line_segment([p(0.66, 0.28), p(0.66, 0.46)], st);
        }
        UiIcon::Battery => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.14, 0.28), p(0.80, 0.72)),
                1.5,
                st,
                egui::StrokeKind::Inside,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(p(0.80, 0.40), p(0.88, 0.60)),
                1.0,
                color,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(p(0.22, 0.36), p(0.60, 0.64)),
                1.0,
                color,
            );
        }
        UiIcon::Cpu => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.26, 0.26), p(0.74, 0.74)),
                1.5,
                st,
                egui::StrokeKind::Inside,
            );
            for pos in [0.38, 0.62] {
                painter.line_segment([p(pos, 0.12), p(pos, 0.26)], st);
                painter.line_segment([p(pos, 0.74), p(pos, 0.88)], st);
                painter.line_segment([p(0.12, pos), p(0.26, pos)], st);
                painter.line_segment([p(0.74, pos), p(0.88, pos)], st);
            }
        }
        UiIcon::Refresh => {
            painter.circle_stroke(rect.center(), w * 0.34, st);
            painter.line_segment([p(0.64, 0.18), p(0.84, 0.26)], st);
            painter.line_segment([p(0.84, 0.26), p(0.76, 0.44)], st);
        }
        UiIcon::Activity => {
            let pts = [
                p(0.10, 0.52),
                p(0.28, 0.52),
                p(0.38, 0.18),
                p(0.54, 0.86),
                p(0.66, 0.36),
                p(0.74, 0.52),
                p(0.90, 0.52),
            ];
            for seg in pts.windows(2) {
                painter.line_segment([seg[0], seg[1]], st);
            }
        }
    }
}

fn draw_icon_inline(ui: &mut egui::Ui, size: f32, color: egui::Color32, icon: UiIcon) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    paint_icon(&ui.painter_at(rect), rect, color, icon);
}

fn icon_button(
    ui: &mut egui::Ui,
    icon: UiIcon,
    icon_color: egui::Color32,
    label: impl Into<egui::RichText>,
    fill: Option<egui::Color32>,
) -> egui::Response {
    let rt: egui::RichText = label.into();
    let padded = egui::RichText::new(format!("    {}", rt.text()));
    let mut btn = egui::Button::new(if fill.is_some() {
        padded.strong().color(egui::Color32::WHITE)
    } else {
        padded
    });
    if let Some(bg) = fill {
        btn = btn.fill(bg);
    }
    let resp = ui.add(btn);
    let r = egui::Rect::from_center_size(
        egui::pos2(resp.rect.left() + 12.0, resp.rect.center().y),
        egui::vec2(13.0, 13.0),
    );
    let ic = if fill.is_some() {
        egui::Color32::WHITE
    } else {
        icon_color
    };
    paint_icon(&ui.painter_at(resp.rect), r, ic, icon);
    resp
}

fn icon_heading(ui: &mut egui::Ui, icon: UiIcon, color: egui::Color32, title: &str) {
    ui.horizontal(|ui| {
        draw_icon_inline(ui, 18.0, color, icon);
        ui.label(egui::RichText::new(title).strong().size(17.0));
    });
}

fn icon_card_title(ui: &mut egui::Ui, icon: UiIcon, color: egui::Color32, title: &str) {
    ui.horizontal(|ui| {
        draw_icon_inline(ui, 15.0, color, icon);
        ui.label(egui::RichText::new(title).strong().size(14.0));
    });
}

fn app_card_frame(ui: &egui::Ui) -> egui::Frame {
    let dark = ui.visuals().dark_mode;
    let fill = if dark {
        egui::Color32::from_rgb(20, 27, 38)
    } else {
        egui::Color32::from_rgb(248, 250, 252)
    };
    let border = if dark {
        egui::Color32::from_rgb(38, 49, 68)
    } else {
        egui::Color32::from_rgb(203, 213, 225)
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(16, 12))
}

fn app_subcard_frame(ui: &egui::Ui) -> egui::Frame {
    let dark = ui.visuals().dark_mode;
    let fill = if dark {
        egui::Color32::from_rgb(15, 22, 34)
    } else {
        egui::Color32::from_rgb(241, 245, 249)
    };
    let border = if dark {
        egui::Color32::from_rgb(34, 46, 66)
    } else {
        egui::Color32::from_rgb(203, 213, 225)
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(14, 10))
}

fn spec_bubble_colors(
    dark: bool,
    accent: egui::Color32,
) -> (egui::Color32, egui::Color32, egui::Color32) {
    if dark {
        let mix_dark =
            |c: u8, base: u8| -> u8 { ((base as f32) * 0.80 + (c as f32) * 0.20).round() as u8 };
        let fill = egui::Color32::from_rgb(
            mix_dark(accent.r(), 16),
            mix_dark(accent.g(), 22),
            mix_dark(accent.b(), 34),
        );
        let stroke = egui::Color32::from_rgb(
            ((accent.r() as u16 * 3) / 4) as u8,
            ((accent.g() as u16 * 3) / 4) as u8,
            ((accent.b() as u16 * 3) / 4) as u8,
        );
        let text = egui::Color32::from_rgb(235, 242, 250);
        (fill, stroke, text)
    } else {
        let mix_light = |c: u8| -> u8 { (255.0 * 0.84 + (c as f32) * 0.16).round() as u8 };
        let fill = egui::Color32::from_rgb(
            mix_light(accent.r()),
            mix_light(accent.g()),
            mix_light(accent.b()),
        );
        let stroke = egui::Color32::from_rgb(
            ((accent.r() as u16 * 4) / 5) as u8,
            ((accent.g() as u16 * 4) / 5) as u8,
            ((accent.b() as u16 * 4) / 5) as u8,
        );
        let text = egui::Color32::from_rgb(20, 30, 45);
        (fill, stroke, text)
    }
}

fn measure_text_px(ui: &egui::Ui, text: &str, size: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_string(),
            egui::FontId::proportional(size),
            egui::Color32::WHITE,
        )
        .size()
        .x
}

fn pack_pill_rows(widths: &[f32], max_row_w: f32, gap: f32) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut cur_row: Vec<usize> = Vec::new();
    let mut cur_w = 0.0f32;
    for (idx, &w) in widths.iter().enumerate() {
        let needed = if cur_row.is_empty() { w } else { gap + w };
        if !cur_row.is_empty() && cur_w + needed > max_row_w {
            rows.push(cur_row);
            cur_row = vec![idx];
            cur_w = w;
        } else {
            cur_w += needed;
            cur_row.push(idx);
        }
    }
    if !cur_row.is_empty() {
        rows.push(cur_row);
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn render_core_spec_pill(
    ui: &mut egui::Ui,
    dark: bool,
    max_row_w: f32,
    cat_key: &str,
    prefix: &str,
    accent: egui::Color32,
    value: &mut String,
    edit_target: &mut Option<(String, usize)>,
    edit_buf: &mut String,
) -> bool {
    let mut changed = false;
    let (fill, stroke, text_col) = spec_bubble_colors(dark, accent);
    let is_editing = edit_target
        .as_ref()
        .is_some_and(|(k, i)| k == cat_key && *i == 0);
    let shown = if value.trim().is_empty() {
        "Unknown (click to edit)".to_string()
    } else {
        value.clone()
    };
    let display_txt = if shown.chars().count() > 52 {
        let truncated: String = shown.chars().take(49).collect();
        format!("{}...", truncated)
    } else {
        shown.clone()
    };

    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke))
        .corner_radius(11.0)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(
                    egui::RichText::new(format!("{}:", prefix))
                        .strong()
                        .size(11.5)
                        .color(accent),
                );
                if is_editing {
                    let te = ui.add(
                        egui::TextEdit::singleline(edit_buf)
                            .desired_width((max_row_w - 170.0).clamp(140.0, 260.0))
                            .hint_text("Enter value..."),
                    );
                    let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.small_button("Save").clicked() || enter {
                        let cleaned = edit_buf.trim().to_string();
                        if !cleaned.is_empty() && *value != cleaned {
                            *value = cleaned;
                            changed = true;
                        }
                        *edit_target = None;
                    }
                    if ui.small_button("Cancel").clicked() {
                        *edit_target = None;
                    }
                } else {
                    let lbl = egui::Label::new(
                        egui::RichText::new(display_txt).size(11.5).color(text_col),
                    )
                    .sense(egui::Sense::click());
                    let resp = ui.add(lbl.extend());
                    if resp.on_hover_text(&shown).clicked() {
                        *edit_target = Some((cat_key.to_string(), 0));
                        *edit_buf = value.clone();
                    }
                }
            });
        });
    changed
}

#[allow(clippy::too_many_arguments)]
fn render_spec_bubble_category(
    ui: &mut egui::Ui,
    dark: bool,
    cat_key: &str,
    title: &str,
    accent: egui::Color32,
    items: &mut Vec<String>,
    edit_target: &mut Option<(String, usize)>,
    edit_buf: &mut String,
    empty_hint: &str,
    add_hint: &str,
) -> bool {
    let mut changed = false;
    let (fill, stroke, text_col) = spec_bubble_colors(dark, accent);
    let remove_col = if dark {
        egui::Color32::from_rgb(252, 165, 165)
    } else {
        egui::Color32::from_rgb(185, 28, 28)
    };
    let max_row_w = (ui.available_width() - 6.0).max(220.0);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            egui::RichText::new(format!("{} ({})", title, items.len()))
                .strong()
                .size(12.0)
                .color(accent),
        );
        if items.is_empty() && !edit_target.as_ref().is_some_and(|(k, _)| k == cat_key) {
            ui.label(egui::RichText::new(empty_hint).weak().size(11.5));
        }
    });

    // Token indices: 0..items.len() for existing bubbles, items.len() for the "+ Add" chip
    let add_token = items.len();
    let is_adding = edit_target
        .as_ref()
        .is_some_and(|(k, i)| k == cat_key && *i == usize::MAX);
    let mut widths: Vec<f32> = Vec::with_capacity(items.len() + 1);
    for (idx, item) in items.iter().enumerate() {
        let is_editing = edit_target
            .as_ref()
            .is_some_and(|(k, i)| k == cat_key && *i == idx);
        let w = if is_editing {
            285.0
        } else {
            (measure_text_px(ui, item, 11.5) + 42.0).min(max_row_w)
        };
        widths.push(w);
    }
    widths.push(if is_adding { 285.0 } else { 64.0 });

    let rows = pack_pill_rows(&widths, max_row_w, 6.0);
    let mut remove_idx: Option<usize> = None;

    for row in rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for tok in row {
                if tok == add_token {
                    if is_adding {
                        egui::Frame::new()
                            .fill(fill)
                            .stroke(egui::Stroke::new(1.0, stroke))
                            .corner_radius(11.0)
                            .inner_margin(egui::Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    let te = ui.add(
                                        egui::TextEdit::singleline(edit_buf)
                                            .desired_width(165.0)
                                            .hint_text(add_hint),
                                    );
                                    let enter = te.lost_focus()
                                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                    if ui.small_button("Add").clicked() || enter {
                                        let cleaned = edit_buf.trim().to_string();
                                        if !cleaned.is_empty()
                                            && !items
                                                .iter()
                                                .any(|x| x.eq_ignore_ascii_case(&cleaned))
                                        {
                                            items.push(cleaned);
                                            changed = true;
                                        }
                                        edit_buf.clear();
                                        *edit_target = None;
                                    }
                                    if ui.small_button("Cancel").clicked() {
                                        edit_buf.clear();
                                        *edit_target = None;
                                    }
                                });
                            });
                    } else if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("+ Add")
                                    .size(11.0)
                                    .strong()
                                    .color(accent),
                            )
                            .fill(fill)
                            .stroke(egui::Stroke::new(1.0, stroke))
                            .corner_radius(11.0),
                        )
                        .on_hover_text(format!("Add custom item to {}", title))
                        .clicked()
                    {
                        *edit_target = Some((cat_key.to_string(), usize::MAX));
                        edit_buf.clear();
                    }
                    continue;
                }

                let idx = tok;
                let is_editing = edit_target
                    .as_ref()
                    .is_some_and(|(k, i)| k == cat_key && *i == idx);
                let raw_item = items[idx].clone();
                let display_item = if raw_item.chars().count() > 52 {
                    let truncated: String = raw_item.chars().take(49).collect();
                    format!("{}...", truncated)
                } else {
                    raw_item.clone()
                };

                egui::Frame::new()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, stroke))
                    .corner_radius(11.0)
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if is_editing {
                                let te = ui.add(
                                    egui::TextEdit::singleline(edit_buf)
                                        .desired_width(170.0)
                                        .hint_text("Edit item..."),
                                );
                                let enter = te.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if ui.small_button("OK").clicked() || enter {
                                    let cleaned = edit_buf.trim().to_string();
                                    if cleaned.is_empty() {
                                        remove_idx = Some(idx);
                                    } else if items[idx] != cleaned {
                                        items[idx] = cleaned;
                                        changed = true;
                                    }
                                    *edit_target = None;
                                }
                                if ui.small_button("Cancel").clicked() {
                                    *edit_target = None;
                                }
                            } else {
                                let lbl = egui::Label::new(
                                    egui::RichText::new(display_item).size(11.5).color(text_col),
                                )
                                .sense(egui::Sense::click());
                                let resp = ui.add(lbl.extend());
                                if resp.on_hover_text(&raw_item).clicked() {
                                    *edit_target = Some((cat_key.to_string(), idx));
                                    *edit_buf = items[idx].clone();
                                }
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("x")
                                                .strong()
                                                .size(11.0)
                                                .color(remove_col),
                                        )
                                        .frame(false),
                                    )
                                    .on_hover_text("Remove from list")
                                    .clicked()
                                {
                                    remove_idx = Some(idx);
                                }
                            }
                        });
                    });
            }
        });
        ui.add_space(3.0);
    }

    if let Some(idx) = remove_idx {
        if idx < items.len() {
            items.remove(idx);
            changed = true;
        }
    }

    if changed {
        *items = dedupe_clean_list(std::mem::take(items));
    }
    changed
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
    tray_mode_smart_id: tray_icon::menu::MenuId,
    tray_mode_guard_off_id: tray_icon::menu::MenuId,
    tray_mode_best_perf_id: tray_icon::menu::MenuId,
    tray_sleep_id: tray_icon::menu::MenuId,
    tray_exit_id: tray_icon::menu::MenuId,
    tray_mode_balanced_id: tray_icon::menu::MenuId,
    tray_mode_work_id: tray_icon::menu::MenuId,
    tray_mode_night_id: tray_icon::menu::MenuId,
    tray_mode_away_id: tray_icon::menu::MenuId,
    tray_stay_awake_id: tray_icon::menu::MenuId,
    tray_postpone_id: tray_icon::menu::MenuId,
}

impl AgentPowerGuardApp {
    pub fn new(cc: &eframe::CreationContext<'_>, state: Arc<Mutex<SharedState>>) -> Self {
        cleanup_old_update_backup();
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
        let (
            tray_icon,
            show_id,
            ms_id,
            mgo_id,
            mbp_id,
            sleep_id,
            exit_id,
            mb_id,
            mw_id,
            mn_id,
            ma_id,
            sa_id,
            pp_id,
        ) = {
            let icon = load_tray_icon();
            let menu = tray_icon::menu::Menu::new();
            let show_item = tray_icon::menu::MenuItem::new("Show Agent Power Guard", true, None);
            let op_smart =
                tray_icon::menu::MenuItem::new("Smart Guard (Watchdog Active)", true, None);
            let op_off = tray_icon::menu::MenuItem::new("Guard Off (OS Power Plan)", true, None);
            let op_best =
                tray_icon::menu::MenuItem::new("Best Performance (Locked Awake)", true, None);
            let postpone_item = tray_icon::menu::MenuItem::new("Postpone Sleep (+15m)", true, None);
            let stay_awake_item = tray_icon::menu::MenuItem::new("Keep Awake (1 Hour)", true, None);
            let sep1 = tray_icon::menu::PredefinedMenuItem::separator();
            let mode_balanced =
                tray_icon::menu::MenuItem::new("Timeout Preset: Balanced (3m)", true, None);
            let mode_work =
                tray_icon::menu::MenuItem::new("Timeout Preset: Work (10m)", true, None);
            let mode_night =
                tray_icon::menu::MenuItem::new("Timeout Preset: Night (1m)", true, None);
            let mode_away =
                tray_icon::menu::MenuItem::new("Timeout Preset: Away (30s)", true, None);
            let sep2 = tray_icon::menu::PredefinedMenuItem::separator();
            let sleep_item = tray_icon::menu::MenuItem::new("Test Sleep Now", true, None);
            let exit_item = tray_icon::menu::MenuItem::new("Exit Agent Power Guard", true, None);

            let _ = menu.append_items(&[
                &show_item,
                &op_smart,
                &op_off,
                &op_best,
                &postpone_item,
                &stay_awake_item,
                &sep1,
                &mode_balanced,
                &mode_work,
                &mode_night,
                &mode_away,
                &sep2,
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
                op_smart.id().clone(),
                op_off.id().clone(),
                op_best.id().clone(),
                sleep_item.id().clone(),
                exit_item.id().clone(),
                mode_balanced.id().clone(),
                mode_work.id().clone(),
                mode_night.id().clone(),
                mode_away.id().clone(),
                stay_awake_item.id().clone(),
                postpone_item.id().clone(),
            )
        };

        // Launch embedded Web Server & Remote API with Rate Limiting
        let rate_limiter = Arc::new(RateLimiter::new());
        let web_port = state.lock().unwrap().config.web_server_port;
        start_web_server(Arc::clone(&state), web_port, rate_limiter);

        // Launch background watchdog thread
        start_watchdog_thread(Arc::clone(&state), Some(cc.egui_ctx.clone()));

        Self {
            state,
            was_minimized: false,
            tray_icon,
            tray_show_id: show_id,
            tray_mode_smart_id: ms_id,
            tray_mode_guard_off_id: mgo_id,
            tray_mode_best_perf_id: mbp_id,
            tray_sleep_id: sleep_id,
            tray_exit_id: exit_id,
            tray_mode_balanced_id: mb_id,
            tray_mode_work_id: mw_id,
            tray_mode_night_id: mn_id,
            tray_mode_away_id: ma_id,
            tray_stay_awake_id: sa_id,
            tray_postpone_id: pp_id,
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
            } else if menu_event.id == self.tray_mode_smart_id {
                let mut s = self.state.lock().unwrap();
                s.set_operational_mode(OperationalMode::SmartGuard, None);
            } else if menu_event.id == self.tray_mode_guard_off_id {
                let mut s = self.state.lock().unwrap();
                s.set_operational_mode(OperationalMode::GuardOff, None);
            } else if menu_event.id == self.tray_mode_best_perf_id {
                let mut s = self.state.lock().unwrap();
                s.set_operational_mode(OperationalMode::BestPerformance, None);
            } else if menu_event.id == self.tray_sleep_id {
                sleep_now();
            } else if menu_event.id == self.tray_mode_balanced_id {
                let mut s = self.state.lock().unwrap();
                PowerMode::Balanced.apply_to_config(&mut s.config);
                s.config_dirty = true;
                s.set_toast("Power mode switched to Balanced via Tray.");
            } else if menu_event.id == self.tray_mode_work_id {
                let mut s = self.state.lock().unwrap();
                PowerMode::Work.apply_to_config(&mut s.config);
                s.config_dirty = true;
                s.set_toast("Power mode switched to Work via Tray.");
            } else if menu_event.id == self.tray_mode_night_id {
                let mut s = self.state.lock().unwrap();
                PowerMode::Night.apply_to_config(&mut s.config);
                s.config_dirty = true;
                s.set_toast("Power mode switched to Night via Tray.");
            } else if menu_event.id == self.tray_mode_away_id {
                let mut s = self.state.lock().unwrap();
                PowerMode::Away.apply_to_config(&mut s.config);
                s.config_dirty = true;
                s.set_toast("Power mode switched to Away via Tray.");
            } else if menu_event.id == self.tray_stay_awake_id {
                let mut s = self.state.lock().unwrap();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                s.config.stay_awake_until_epoch_secs = Some(now + 3600);
                s.config_dirty = true;
                s.set_toast("Keep Awake active for 1 hour via Tray.");
            } else if menu_event.id == self.tray_postpone_id {
                let mut s = self.state.lock().unwrap();
                s.postponed_until = Some(Instant::now() + Duration::from_secs(15 * 60));
                s.set_toast("Sleep postponed for 15 minutes via Tray.");
            } else if menu_event.id == self.tray_exit_id {
                std::process::exit(0);
            }
        }

        // Update Tray Tooltip dynamically
        if let Some(tray) = &self.tray_icon {
            let s = self.state.lock().unwrap();
            let tooltip = match s.operational_mode {
                OperationalMode::BestPerformance => {
                    "Agent Power Guard [BEST PERFORMANCE - LOCKED AWAKE]".to_string()
                }
                OperationalMode::GuardOff => {
                    "Agent Power Guard [GUARD OFF - OS POWER PLAN]".to_string()
                }
                OperationalMode::SmartGuard => {
                    if s.all_conditions_pass {
                        format!(
                            "Agent Power Guard - Sleeping in {} / {}",
                            format_time_compact(s.countdown_secs),
                            format_time_compact(s.countdown_max)
                        )
                    } else {
                        format!(
                            "Agent Power Guard [{}] - {}",
                            s.intent.display_label(),
                            s.blocker_banner_text
                        )
                    }
                }
            };
            // Win32 szTip buffer limit is 128 characters including null terminator.
            let safe_tooltip: String = tooltip.chars().take(120).collect();
            let _ = tray.set_tooltip(Some(&safe_tooltip));
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
        // Non-focus-stealing topmost passive media prompt viewport (§5.3)
        let prompt_needed = { self.state.lock().unwrap().media_prompt_active };
        if prompt_needed {
            let state_clone = Arc::clone(&self.state);
            ui.ctx().show_viewport_deferred(
                egui::ViewportId::from_hash_of("apg_passive_media_prompt"),
                egui::ViewportBuilder::default()
                    .with_title("Agent Power Guard — Media Playback")
                    .with_inner_size([430.0, 155.0])
                    .with_resizable(false)
                    .with_always_on_top()
                    .with_active(false),
                move |vp_ui, _class| {
                    let (app, title) = {
                        let s = state_clone.lock().unwrap();
                        (s.media_prompt_app.clone(), s.media_prompt_title.clone())
                    };
                    vp_ui.vertical(|ui| {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(format!("Playback active in {}", app))
                                .strong()
                                .size(14.0),
                        );
                        if !title.is_empty() {
                            ui.label(egui::RichText::new(title).weak().size(11.0));
                        }
                        ui.label(
                            egui::RichText::new(
                                "You have been idle — keep this PC awake for this media session?",
                            )
                            .size(12.0),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if icon_button(
                                ui,
                                UiIcon::Check,
                                egui::Color32::from_rgb(52, 211, 153),
                                "Keep Awake (this session)",
                                None,
                            )
                            .clicked()
                            {
                                state_clone
                                    .lock()
                                    .unwrap()
                                    .apply_media_action(MediaPromptAction::KeepAwake);
                            }
                            if icon_button(
                                ui,
                                UiIcon::Sleep,
                                egui::Color32::from_rgb(147, 197, 253),
                                "Sleep Now",
                                None,
                            )
                            .clicked()
                            {
                                let should_sleep = state_clone
                                    .lock()
                                    .unwrap()
                                    .apply_media_action(MediaPromptAction::SleepNow);
                                if should_sleep {
                                    let _ = sleep_now();
                                }
                            }
                            if ui.button("Hide").clicked() {
                                state_clone
                                    .lock()
                                    .unwrap()
                                    .apply_media_action(MediaPromptAction::Hide);
                            }
                        });
                    });
                },
            );
        }

        // Fast-path: When hidden in the system tray, skip all widget layouts and OpenGL rendering
        if ui.ctx().input(|i| i.viewport().visible() == Some(false)) {
            return;
        }

        let mut s = self.state.lock().unwrap();

        if ui.visuals().dark_mode != s.config.dark_mode {
            ui.ctx().set_visuals(if s.config.dark_mode {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
        }

        // Modal: Info Guide (§6)
        let mut guide_open = s.show_info_guide;
        if guide_open {
            egui::Window::new("Agent Power Guard - Guide")
                .open(&mut guide_open)
                .default_size([600.0, 480.0])
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ui.ctx(), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for sec in get_info_guide_sections() {
                            app_card_frame(ui).show(ui, |ui| {
                                icon_card_title(
                                    ui,
                                    UiIcon::Info,
                                    egui::Color32::from_rgb(96, 165, 250),
                                    sec.title,
                                );
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(sec.body).size(12.0));
                            });
                            ui.add_space(6.0);
                        }
                    });
                });
            s.show_info_guide = guide_open;
        }

        // Modal: Update Ready to Install Prompt
        let mut install_open = s.update_install_prompt_open;
        if install_open {
            let dl_ver = s
                .update_downloaded_version
                .clone()
                .unwrap_or_else(|| "Latest".to_string());
            let dl_path_opt = s.update_downloaded_path.clone();
            egui::Window::new("Update Ready to Install")
                .open(&mut install_open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .default_size([450.0, 190.0])
                .show(ui.ctx(), |ui| {
                    icon_card_title(
                        ui,
                        UiIcon::Download,
                        egui::Color32::from_rgb(52, 211, 153),
                        &format!("Agent Power Guard {} downloaded", dl_ver),
                    );
                    ui.add_space(4.0);
                    if let Some(ref p) = dl_path_opt {
                        ui.label(
                            egui::RichText::new(format!("Saved to: {}", p.display()))
                                .weak()
                                .size(11.0),
                        );
                    }
                    ui.add_space(6.0);
                    ui.label("Install now and restart the app? Once restarted, the Updates tab will reflect the new version.");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if icon_button(
                            ui,
                            UiIcon::Check,
                            egui::Color32::WHITE,
                            "Install & Restart Now",
                            Some(egui::Color32::from_rgb(16, 185, 129)),
                        )
                        .clicked()
                        {
                            if let Some(ref p) = dl_path_opt {
                                if let Err(err) = install_downloaded_update(p) {
                                    s.update_check_status_msg = Some(err.clone());
                                    s.set_toast(err);
                                }
                            }
                        }
                        if ui.button("Later").clicked() {
                            s.update_install_prompt_open = false;
                        }
                    });
                });
            if !install_open {
                s.update_install_prompt_open = false;
            }
        }

        // Modal: Uninstall Software Confirmation
        let mut uninstall_open = s.show_uninstall_modal;
        if uninstall_open {
            egui::Window::new("Uninstall Agent Power Guard")
                .open(&mut uninstall_open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .default_size([450.0, 210.0])
                .show(ui.ctx(), |ui| {
                    icon_card_title(
                        ui,
                        UiIcon::Trash,
                        egui::Color32::from_rgb(248, 113, 113),
                        "Confirm Uninstall",
                    );
                    ui.add_space(4.0);
                    ui.label("This disables startup registration, stops/removes any background service, and launches the uninstaller (or deletes the portable executable).");
                    ui.add_space(6.0);
                    ui.checkbox(
                        &mut s.uninstall_wipe_data,
                        "Also delete saved configuration, API token, energy history, and block history",
                    );
                    if let Some(ref msg) = s.uninstall_status_msg {
                        ui.label(egui::RichText::new(msg).color(egui::Color32::RED));
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if icon_button(
                            ui,
                            UiIcon::Trash,
                            egui::Color32::WHITE,
                            "Confirm Uninstall",
                            Some(egui::Color32::from_rgb(185, 28, 28)),
                        )
                        .clicked()
                        {
                            if let Err(err) = execute_software_uninstall(s.uninstall_wipe_data) {
                                s.uninstall_status_msg = Some(err);
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            s.show_uninstall_modal = false;
                        }
                    });
                });
            if !uninstall_open {
                s.show_uninstall_modal = false;
            }
        }

        // Modal: Custom Hardware Watt Calculator & AI Estimator (Clean 4-Step Wizard)
        let mut calc_open = s.show_watt_calc_modal;
        if calc_open {
            let state_arc = self.state.clone();
            let content_rect = ui.ctx().content_rect();
            let modal_w = (content_rect.width() - 36.0).clamp(300.0, 820.0);
            let modal_h = (content_rect.height() - 40.0).clamp(340.0, 720.0);
            egui::Window::new("Hardware Watt Calculator & AI Estimator")
                .open(&mut calc_open)
                .collapsible(false)
                .constrain_to(content_rect)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .fixed_size([modal_w, modal_h])
                .show(ui.ctx(), |ui| {
                    let inner_w = (modal_w - 30.0).max(240.0);
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_width(inner_w);
                        ui.set_max_width(inner_w);
                        let dark = ui.visuals().dark_mode;
                        let mut any_spec_edited = false;

                        if !s.detected_specs.fetched && !s.watt_calc.manual_wizard_unlocked {
                            app_card_frame(ui).show(ui, |ui| {
                                ui.set_width((inner_w - 34.0).max(220.0));
                                ui.add_space(48.0);
                                ui.vertical_centered(|ui| {
                                    draw_icon_inline(ui, 34.0, egui::Color32::from_rgb(56, 189, 248), UiIcon::Cpu);
                                    ui.add_space(10.0);
                                    ui.label(
                                        egui::RichText::new("Fetch Your Device Hardware Specs")
                                            .strong()
                                            .size(18.0),
                                    );
                                    ui.add_space(6.0);
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(
                                                "Click below to scan motherboard, CPU, RAM sticks, physical storage drives, GPU, displays, Wi-Fi/LAN, Bluetooth, audio/mic, USB peripherals & RGB controllers.",
                                            )
                                            .weak()
                                            .size(12.5),
                                        )
                                        .wrap(),
                                    );
                                    ui.add_space(18.0);
                                    if s.watt_calc.spec_fetch_in_progress {
                                        ui.spinner();
                                        ui.add_space(6.0);
                                        ui.label(
                                            egui::RichText::new("Scanning device hardware... Please wait.")
                                                .strong()
                                                .color(egui::Color32::from_rgb(56, 189, 248)),
                                        );
                                    } else {
                                        let fetch_btn = ui.add_sized(
                                            [240.0, 42.0],
                                            egui::Button::new(
                                                egui::RichText::new("Fetch Device Details")
                                                    .strong()
                                                    .size(15.0)
                                                    .color(egui::Color32::WHITE),
                                            )
                                            .fill(egui::Color32::from_rgb(37, 99, 235))
                                            .corner_radius(10.0),
                                        );
                                        if fetch_btn.clicked() {
                                            trigger_spec_fetch_locked(&mut s, state_arc.clone());
                                        }
                                        ui.add_space(12.0);
                                        if ui
                                            .button(
                                                egui::RichText::new("Skip scan & enter specs manually")
                                                    .weak()
                                                    .size(12.0),
                                            )
                                            .clicked()
                                        {
                                            s.watt_calc.manual_wizard_unlocked = true;
                                        }
                                    }
                                });
                                ui.add_space(48.0);
                            });
                            return;
                        }

                        // STEP 1: Auto-Fetch & Color-Coded Editable Hardware Bubbles
                        app_card_frame(ui).show(ui, |ui| {
                            ui.set_width((inner_w - 34.0).max(220.0));
                            ui.horizontal_wrapped(|ui| {
                                icon_card_title(
                                    ui,
                                    UiIcon::Cpu,
                                    egui::Color32::from_rgb(56, 189, 248),
                                    "Step 1: System Specs & Editable Hardware Bubbles",
                                );
                                ui.add_space(8.0);
                                if s.watt_calc.spec_fetch_in_progress {
                                    ui.label("Detecting hardware...");
                                } else if icon_button(
                                    ui,
                                    UiIcon::Search,
                                    egui::Color32::WHITE,
                                    "Re-Fetch Device Specs",
                                    Some(egui::Color32::from_rgb(37, 99, 235)),
                                )
                                .clicked()
                                {
                                    trigger_spec_fetch_locked(&mut s, state_arc.clone());
                                }
                            });
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(
                                    "Click any colored bubble to edit, 'x' to remove unused items (e.g. paired phones/virtual audio), or '+ Add' to append fans, ARGB strips, speakers, or custom parts.",
                                )
                                .weak()
                                .size(11.0),
                            );
                            ui.add_space(6.0);

                            {
                                let guard = &mut *s;
                                let specs = &mut guard.detected_specs;
                                let calc = &mut guard.watt_calc;
                                let edit_target = &mut calc.spec_edit_target;
                                let edit_buf = &mut calc.spec_edit_buf;

                                if specs.fetched {
                                    let max_core_w = (ui.available_width() - 6.0).max(220.0);
                                    let os_text = format!(
                                        "{} ({})",
                                        specs.os_name,
                                        if specs.is_laptop { "Laptop" } else { "Desktop" }
                                    );
                                    if specs.ram_details.is_empty() && specs.ram_gb > 0.0 {
                                        specs.ram_details = format!("{:.1} GB", specs.ram_gb);
                                    }
                                    let core_pill_w = |cat: &str, prefix: &str, val: &str| -> f32 {
                                        if edit_target.as_ref().is_some_and(|(k, _)| k == cat) {
                                            340.0f32.min(max_core_w)
                                        } else {
                                            let shown = if val.trim().is_empty() {
                                                "Unknown (click to edit)"
                                            } else {
                                                val
                                            };
                                            (measure_text_px(ui, &format!("{}: {}", prefix, shown), 11.5) + 30.0)
                                                .min(max_core_w)
                                        }
                                    };
                                    let core_widths = [
                                        (measure_text_px(ui, &os_text, 11.5) + 22.0).min(max_core_w),
                                        core_pill_w("core:board", "Board", &specs.motherboard),
                                        core_pill_w("core:cpu", "CPU", &specs.cpu_brand),
                                        core_pill_w("core:ram", "RAM", &specs.ram_details),
                                        core_pill_w("core:gpu", "GPU", &specs.gpu_name),
                                    ];
                                    let core_rows = pack_pill_rows(&core_widths, max_core_w, 6.0);
                                    for row in core_rows {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 6.0;
                                            for tok in row {
                                                match tok {
                                                    0 => {
                                                        let (os_fill, os_stroke, os_txt) = spec_bubble_colors(
                                                            dark,
                                                            egui::Color32::from_rgb(100, 116, 139),
                                                        );
                                                        egui::Frame::new()
                                                            .fill(os_fill)
                                                            .stroke(egui::Stroke::new(1.0, os_stroke))
                                                            .corner_radius(11.0)
                                                            .inner_margin(egui::Margin::symmetric(9, 4))
                                                            .show(ui, |ui| {
                                                                ui.add(
                                                                    egui::Label::new(
                                                                        egui::RichText::new(&os_text)
                                                                            .size(11.5)
                                                                            .color(os_txt),
                                                                    )
                                                                    .extend(),
                                                                );
                                                            });
                                                    }
                                                    1 => {
                                                        any_spec_edited |= render_core_spec_pill(
                                                            ui,
                                                            dark,
                                                            max_core_w,
                                                            "core:board",
                                                            "Board",
                                                            egui::Color32::from_rgb(34, 211, 238),
                                                            &mut specs.motherboard,
                                                            edit_target,
                                                            edit_buf,
                                                        );
                                                    }
                                                    2 => {
                                                        any_spec_edited |= render_core_spec_pill(
                                                            ui,
                                                            dark,
                                                            max_core_w,
                                                            "core:cpu",
                                                            "CPU",
                                                            egui::Color32::from_rgb(59, 130, 246),
                                                            &mut specs.cpu_brand,
                                                            edit_target,
                                                            edit_buf,
                                                        );
                                                    }
                                                    3 => {
                                                        any_spec_edited |= render_core_spec_pill(
                                                            ui,
                                                            dark,
                                                            max_core_w,
                                                            "core:ram",
                                                            "RAM",
                                                            egui::Color32::from_rgb(139, 92, 246),
                                                            &mut specs.ram_details,
                                                            edit_target,
                                                            edit_buf,
                                                        );
                                                    }
                                                    _ => {
                                                        any_spec_edited |= render_core_spec_pill(
                                                            ui,
                                                            dark,
                                                            max_core_w,
                                                            "core:gpu",
                                                            "GPU",
                                                            egui::Color32::from_rgb(236, 72, 153),
                                                            &mut specs.gpu_name,
                                                            edit_target,
                                                            edit_buf,
                                                        );
                                                    }
                                                }
                                            }
                                        });
                                        ui.add_space(4.0);
                                    }
                                    ui.add_space(4.0);
                                }

                                any_spec_edited |= render_spec_bubble_category(
                                    ui,
                                    dark,
                                    "storage",
                                    "Storage Drives",
                                    egui::Color32::from_rgb(245, 158, 11),
                                    &mut specs.storage_drives,
                                    edit_target,
                                    edit_buf,
                                    "No physical drives listed yet",
                                    "e.g. Samsung 980 PRO 1TB NVMe",
                                );
                                ui.add_space(4.0);

                                let mons_changed = render_spec_bubble_category(
                                    ui,
                                    dark,
                                    "monitors",
                                    "Displays",
                                    egui::Color32::from_rgb(16, 185, 129),
                                    &mut specs.monitors,
                                    edit_target,
                                    edit_buf,
                                    "None auto-detected",
                                    "e.g. LG 27GP850 1440p",
                                );
                                if mons_changed {
                                    any_spec_edited = true;
                                    if !specs.monitors.is_empty() {
                                        calc.monitor_count = specs.monitors.len() as u32;
                                        calc.monitor_models_note = specs.monitors.join(", ");
                                    } else {
                                        calc.monitor_models_note = "None".to_string();
                                    }
                                }
                                ui.add_space(4.0);

                                any_spec_edited |= render_spec_bubble_category(
                                    ui,
                                    dark,
                                    "network",
                                    "Wi-Fi / LAN",
                                    egui::Color32::from_rgb(14, 165, 233),
                                    &mut specs.network_adapters,
                                    edit_target,
                                    edit_buf,
                                    "None detected",
                                    "e.g. Intel AX210 Wi-Fi 6E",
                                );
                                ui.add_space(4.0);

                                any_spec_edited |= render_spec_bubble_category(
                                    ui,
                                    dark,
                                    "bluetooth",
                                    "Bluetooth",
                                    egui::Color32::from_rgb(99, 102, 241),
                                    &mut specs.bluetooth_devices,
                                    edit_target,
                                    edit_buf,
                                    "None detected",
                                    "e.g. Bluetooth Adapter",
                                );
                                ui.add_space(4.0);

                                any_spec_edited |= render_spec_bubble_category(
                                    ui,
                                    dark,
                                    "audio",
                                    "Audio / Mic / DAC",
                                    egui::Color32::from_rgb(168, 85, 247),
                                    &mut specs.audio_devices,
                                    edit_target,
                                    edit_buf,
                                    "None detected",
                                    "e.g. Fifine Mic / Edifier Speakers",
                                );
                                ui.add_space(4.0);

                                any_spec_edited |= render_spec_bubble_category(
                                    ui,
                                    dark,
                                    "usb",
                                    "USB & Peripherals",
                                    egui::Color32::from_rgb(20, 184, 166),
                                    &mut specs.usb_peripherals,
                                    edit_target,
                                    edit_buf,
                                    "Standard mouse/keyboard",
                                    "e.g. Mechanical Keyboard / UPS",
                                );
                                ui.add_space(4.0);

                                any_spec_edited |= render_spec_bubble_category(
                                    ui,
                                    dark,
                                    "cabinet",
                                    "Cabinet Cooling, Fans & ARGB",
                                    egui::Color32::from_rgb(244, 63, 94),
                                    &mut specs.cabinet_cooling,
                                    edit_target,
                                    edit_buf,
                                    "Use Step 2 or + Add for case fans / ARGB / AIO",
                                    "e.g. 3x 120mm ARGB Fans",
                                );
                            }
                        });

                        ui.add_space(8.0);

                        // STEP 2: Missing Hardware Inputs (SMPS, Fans/ARGB), Custom Watt Distribution & Tariff
                        app_card_frame(ui).show(ui, |ui| {
                            ui.set_width((inner_w - 34.0).max(220.0));
                            icon_card_title(
                                ui,
                                UiIcon::Sliders,
                                egui::Color32::from_rgb(16, 185, 129),
                                "Step 2: Missing Details (SMPS, Case Fans / ARGB), Watt Distribution & Price",
                            );
                            ui.add_space(6.0);

                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("SMPS / PSU:").strong().size(12.0));
                                ui.add(egui::TextEdit::singleline(&mut s.watt_calc.smps_model_note).desired_width(210.0));
                                ui.horizontal(|ui| {
                                    ui.label("Rated:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.psu_wattage).range(45..=2000).suffix(" W"));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Efficiency:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.psu_efficiency_pct).range(60.0..=96.0).suffix("%"));
                                });
                            });
                            ui.add_space(5.0);

                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("Monitors:").strong().size(12.0));
                                ui.horizontal(|ui| {
                                    ui.label("Count:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.monitor_count).range(1..=6));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Models:");
                                    ui.add(egui::TextEdit::singleline(&mut s.watt_calc.monitor_models_note).desired_width(220.0));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Each:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.monitor_each_w).range(5.0..=200.0).suffix(" W"));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Wall Meter (0 = calc):");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.wall_meter_override_w).range(0.0..=1500.0).suffix(" W"));
                                });
                            });
                            ui.add_space(5.0);

                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("Cabinet Cooling & ARGB:").strong().size(12.0).color(egui::Color32::from_rgb(244, 63, 94)));
                                ui.horizontal(|ui| {
                                    ui.label("Case Fans:");
                                    if ui.add(egui::DragValue::new(&mut s.watt_calc.case_fan_count).range(0..=20)).changed() {
                                        any_spec_edited = true;
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("ARGB Fans:");
                                    if ui.add(egui::DragValue::new(&mut s.watt_calc.argb_fan_count).range(0..=20)).changed() {
                                        any_spec_edited = true;
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Cooler:");
                                    let cooler_selected = s.watt_calc.cooler_type.clone();
                                    egui::ComboBox::from_id_salt("calc_cooler_combo")
                                        .selected_text(cooler_selected)
                                        .width(175.0)
                                        .show_ui(ui, |ui| {
                                            for opt in [
                                                "Stock / Tower Air Cooler",
                                                "Dual-Tower Air Cooler",
                                                "120mm AIO Liquid Cooler",
                                                "240mm AIO Liquid Cooler",
                                                "360mm AIO Liquid Cooler",
                                                "Custom Water Loop",
                                                "Laptop Internal Cooling",
                                            ] {
                                                if ui.selectable_label(s.watt_calc.cooler_type == opt, opt).clicked() {
                                                    s.watt_calc.cooler_type = opt.to_string();
                                                    any_spec_edited = true;
                                                }
                                            }
                                        });
                                });
                                if ui.checkbox(&mut s.watt_calc.has_rgb_strips_or_hub, "ARGB Hub / LED Strips").changed() {
                                    any_spec_edited = true;
                                }
                            });
                            ui.add_space(4.0);

                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("Quick Add:").weak().size(11.5));
                                for (btn_lbl, target_cat, entry) in [
                                    ("+ 3x 120mm ARGB Fans", "cabinet", "3x 120mm ARGB Case Fans"),
                                    ("+ 240mm AIO", "cabinet", "240mm AIO Liquid Cooler"),
                                    ("+ LED Strip / Hub", "cabinet", "ARGB Controller Hub & LED Strips"),
                                    ("+ 2.1 Speakers", "audio", "2.1 Desktop Speakers"),
                                    ("+ UPS Backup", "usb", "Desktop UPS Battery Backup"),
                                ] {
                                    if ui.small_button(btn_lbl).clicked() {
                                        let dest = match target_cat {
                                            "cabinet" => &mut s.detected_specs.cabinet_cooling,
                                            "audio" => &mut s.detected_specs.audio_devices,
                                            _ => &mut s.detected_specs.usb_peripherals,
                                        };
                                        if !dest.iter().any(|x| x.eq_ignore_ascii_case(entry)) {
                                            dest.push(entry.to_string());
                                            any_spec_edited = true;
                                        }
                                    }
                                }
                            });
                            ui.add_space(5.0);

                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("Extra Notes:").strong().size(12.0));
                                let note_w = (ui.available_width() - 95.0).clamp(180.0, 560.0);
                                if ui.add(egui::TextEdit::singleline(&mut s.watt_calc.extra_gear_note).desired_width(note_w)).changed() {
                                    any_spec_edited = true;
                                }
                            });
                            ui.add_space(5.0);

                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("Component Idle (W):").strong().size(12.0));
                                ui.horizontal(|ui| {
                                    ui.label("CPU:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.cpu_idle_w).range(1.0..=300.0).suffix("W"));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("GPU:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.gpu_idle_w).range(0.0..=300.0).suffix("W"));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Board/RAM/Fans:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.mobo_ram_ssd_fans_w).range(2.0..=150.0).suffix("W"));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Extras:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.peripherals_w).range(0.0..=100.0).suffix("W"));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Sleep:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.sleep_watts).range(0.5..=50.0).suffix("W"));
                                });
                            });
                            ui.add_space(5.0);

                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("Usage & Electricity Rate:").strong().size(12.0));
                                ui.horizontal(|ui| {
                                    ui.label("Active/day:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.daily_active_hours).range(0.0..=24.0).speed(0.5).suffix("h"));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Sleep/day:");
                                    ui.add(egui::DragValue::new(&mut s.watt_calc.daily_idle_sleep_hours).range(0.0..=24.0).speed(0.5).suffix("h"));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Currency:");
                                    let selected_lbl = MAJOR_CURRENCIES
                                        .iter()
                                        .find(|c| c.symbol == s.config.currency_symbol)
                                        .map(|c| c.code.to_string())
                                        .unwrap_or_else(|| s.config.currency_symbol.clone());
                                    egui::ComboBox::from_id_salt("calc_currency_combo")
                                        .selected_text(selected_lbl)
                                        .width(86.0)
                                        .show_ui(ui, |ui| {
                                            for c in MAJOR_CURRENCIES {
                                                if ui.selectable_label(s.config.currency_symbol == c.symbol, c.label).clicked() {
                                                    s.config.currency_symbol = c.symbol.to_string();
                                                    s.custom_currency_input = c.symbol.to_string();
                                                    s.config_dirty = true;
                                                }
                                            }
                                        });
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Rate/kWh:");
                                    let csym = s.config.currency_symbol.clone();
                                    if ui.add(egui::DragValue::new(&mut s.config.electricity_rate_kwh).prefix(&csym).suffix("/kWh").range(0.01..=50000.0).speed(0.1)).changed() {
                                        s.config_dirty = true;
                                        any_spec_edited = true;
                                    }
                                });
                            });

                            if any_spec_edited && !s.watt_calc.generated_prompt.is_empty() {
                                s.watt_calc.generated_prompt = build_ai_watt_prompt(
                                    &s.detected_specs,
                                    &s.watt_calc,
                                    &s.config.currency_symbol,
                                    s.config.electricity_rate_kwh,
                                );
                            }

                            ui.add_space(6.0);
                            let bd = s.watt_calc.compute_breakdown();
                            let cur = s.config.currency_symbol.clone();
                            let co2_kg = get_carbon_factor_kg(&s.config);
                            let proj = compute_power_projection(
                                bd.total_active_watts,
                                bd.total_monitor_watts,
                                bd.sleep_watts,
                                s.config.electricity_rate_kwh,
                                co2_kg,
                                s.watt_calc.daily_active_hours,
                                s.watt_calc.daily_idle_sleep_hours,
                            );
                            app_subcard_frame(ui).show(ui, |ui| {
                                ui.set_width((inner_w - 64.0).max(180.0));
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "Active Total: {:.0} W (PC Wall {:.0} W + Monitors {:.0} W)  |  Display Off: {:.0} W  |  Sleep: {:.1} W",
                                            bd.total_active_watts, bd.wall_pc_watts, bd.total_monitor_watts, proj.display_off_watts, bd.sleep_watts
                                        ))
                                        .strong()
                                        .color(egui::Color32::from_rgb(52, 211, 153)),
                                    )
                                    .wrap(),
                                );
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "Daily Protected Cost: {}{:.2}/day ({:.2} kWh)  |  Est. Savings: {}{:.2}/day  |  {}{:.1}/month  |  {}{:.0}/year",
                                            cur, proj.daily_guard_cost, proj.daily_with_guard_kwh, cur, proj.daily_saved_cost, cur, proj.monthly_saved_cost, cur, proj.yearly_saved_cost
                                        ))
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(147, 197, 253)),
                                    )
                                    .wrap(),
                                );
                            });
                            ui.add_space(6.0);
                            if icon_button(
                                ui,
                                UiIcon::Check,
                                egui::Color32::WHITE,
                                "Apply Calculated Watts to Dashboard",
                                Some(egui::Color32::from_rgb(16, 185, 129)),
                            )
                            .clicked()
                            {
                                s.config.active_watts = bd.total_active_watts.max(20.0);
                                s.config.monitor_watts = bd.total_monitor_watts.max(0.0);
                                s.config.sleep_watts = bd.sleep_watts.max(0.5);
                                s.config_dirty = true;
                                s.set_toast("Applied custom wattage distribution to Energy Dashboard.");
                            }
                        });

                        ui.add_space(8.0);

                        // STEP 3: Generate Prompt & Reliable Clipboard Copy
                        app_card_frame(ui).show(ui, |ui| {
                            ui.set_width((inner_w - 34.0).max(220.0));
                            ui.horizontal_wrapped(|ui| {
                                icon_card_title(
                                    ui,
                                    UiIcon::Bolt,
                                    egui::Color32::from_rgb(245, 158, 11),
                                    "Step 3: Generate AI Prompt (Use Any AI Chat)",
                                );
                                ui.add_space(8.0);
                                if icon_button(
                                    ui,
                                    UiIcon::Refresh,
                                    egui::Color32::from_rgb(245, 158, 11),
                                    "Generate Prompt",
                                    None,
                                )
                                .clicked()
                                {
                                    let prompt = build_ai_watt_prompt(
                                        &s.detected_specs,
                                        &s.watt_calc,
                                        &s.config.currency_symbol,
                                        s.config.electricity_rate_kwh,
                                    );
                                    s.watt_calc.generated_prompt = prompt;
                                    s.set_toast("AI prompt updated with current specs & rates.");
                                }
                                if icon_button(
                                    ui,
                                    UiIcon::Copy,
                                    egui::Color32::WHITE,
                                    "Copy Prompt",
                                    Some(egui::Color32::from_rgb(37, 99, 235)),
                                )
                                .clicked()
                                {
                                    if s.watt_calc.generated_prompt.is_empty() {
                                        s.watt_calc.generated_prompt = build_ai_watt_prompt(
                                            &s.detected_specs,
                                            &s.watt_calc,
                                            &s.config.currency_symbol,
                                            s.config.electricity_rate_kwh,
                                        );
                                    }
                                    copy_text_reliable(ui.ctx(), &s.watt_calc.generated_prompt);
                                    s.set_toast("Prompt copied to system clipboard. Paste into ChatGPT, Claude, or Gemini.");
                                }
                            });
                            ui.add_space(4.0);
                            if s.watt_calc.generated_prompt.is_empty() {
                                s.watt_calc.generated_prompt = build_ai_watt_prompt(
                                    &s.detected_specs,
                                    &s.watt_calc,
                                    &s.config.currency_symbol,
                                    s.config.electricity_rate_kwh,
                                );
                            }
                            let box_w = (inner_w - 42.0).max(200.0);
                            egui::ScrollArea::vertical()
                                .id_salt("ai_prompt_scroll")
                                .max_height(130.0)
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut s.watt_calc.generated_prompt)
                                            .code_editor()
                                            .desired_rows(6)
                                            .desired_width(box_w),
                                    );
                                });
                        });

                        ui.add_space(8.0);

                        // STEP 4: Paste AI JSON Response & Auto-Apply
                        app_card_frame(ui).show(ui, |ui| {
                            ui.set_width((inner_w - 34.0).max(220.0));
                            ui.horizontal_wrapped(|ui| {
                                icon_card_title(
                                    ui,
                                    UiIcon::Download,
                                    egui::Color32::from_rgb(168, 85, 247),
                                    "Step 4: Paste AI Response & Calculate All Savings",
                                );
                                ui.add_space(8.0);
                                if icon_button(
                                    ui,
                                    UiIcon::Copy,
                                    egui::Color32::from_rgb(168, 85, 247),
                                    "Paste Clipboard",
                                    None,
                                )
                                .clicked()
                                {
                                    if let Some(clip_txt) = read_os_clipboard() {
                                        s.watt_calc.ai_response_input = clip_txt;
                                        s.set_toast("Pasted clipboard into AI response box.");
                                    } else {
                                        s.set_toast("Clipboard is empty.");
                                    }
                                }
                                if icon_button(
                                    ui,
                                    UiIcon::Check,
                                    egui::Color32::WHITE,
                                    "Apply AI Response",
                                    Some(egui::Color32::from_rgb(124, 58, 237)),
                                )
                                .clicked()
                                {
                                    let raw = s.watt_calc.ai_response_input.clone();
                                    match parse_ai_watt_response(&raw) {
                                        Ok(parsed) => {
                                            if let Some(v) = parsed.cpu_idle_w {
                                                s.watt_calc.cpu_idle_w = v;
                                            }
                                            if let Some(v) = parsed.gpu_idle_w {
                                                s.watt_calc.gpu_idle_w = v;
                                            }
                                            if let Some(v) = parsed.mobo_ram_ssd_fans_w {
                                                s.watt_calc.mobo_ram_ssd_fans_w = v;
                                            }
                                            if let Some(v) = parsed.peripherals_w {
                                                s.watt_calc.peripherals_w = v;
                                            }
                                            if let Some(v) = parsed.psu_efficiency_pct {
                                                s.watt_calc.psu_efficiency_pct = v;
                                            }
                                            if let Some(v) = parsed.active_watts {
                                                s.config.active_watts = v;
                                            }
                                            if let Some(v) = parsed.monitor_watts {
                                                s.config.monitor_watts = v;
                                                let cnt = s.watt_calc.monitor_count.max(1) as f32;
                                                s.watt_calc.monitor_each_w = v / cnt;
                                            }
                                            if let Some(v) = parsed.sleep_watts {
                                                s.config.sleep_watts = v;
                                                s.watt_calc.sleep_watts = v;
                                            }
                                            if let Some(sym) = parsed.currency_symbol {
                                                s.config.currency_symbol = normalize_currency_input(&sym);
                                                s.custom_currency_input = s.config.currency_symbol.clone();
                                            }
                                            if let Some(rate) = parsed.electricity_rate_kwh {
                                                s.config.electricity_rate_kwh = rate;
                                            }
                                            if let Some(notes) = parsed.notes {
                                                s.watt_calc.ai_notes = notes;
                                            }
                                            s.config_dirty = true;
                                            s.watt_calc.ai_parse_status = Some(format!(
                                                "Applied: Active {:.0}W, Monitor {:.0}W, Sleep {:.1}W, Rate {}{:.2}/kWh",
                                                s.config.active_watts,
                                                s.config.monitor_watts,
                                                s.config.sleep_watts,
                                                s.config.currency_symbol,
                                                s.config.electricity_rate_kwh
                                            ));
                                            s.set_toast("AI estimate parsed and applied to Energy Dashboard.");
                                        }
                                        Err(err) => {
                                            s.watt_calc.ai_parse_status = Some(format!("Error: {}", err));
                                        }
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            let box_w = (inner_w - 42.0).max(200.0);
                            egui::ScrollArea::vertical()
                                .id_salt("ai_resp_scroll")
                                .max_height(110.0)
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut s.watt_calc.ai_response_input)
                                            .code_editor()
                                            .desired_rows(5)
                                            .desired_width(box_w)
                                            .hint_text("Paste AI JSON response here, e.g. {\"active_watts\": 115, \"monitor_watts\": 25, \"sleep_watts\": 3, \"currency_symbol\": \"₹\", \"electricity_rate_kwh\": 9.0}"),
                                    );
                                });
                            if let Some(ref st) = s.watt_calc.ai_parse_status {
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(st).strong().color(egui::Color32::from_rgb(52, 211, 153)));
                            }
                            if !s.watt_calc.ai_notes.is_empty() {
                                ui.label(egui::RichText::new(&s.watt_calc.ai_notes).weak().size(11.5));
                            }
                        });
                    });
                });
            s.show_watt_calc_modal = calc_open;
        }

        // ====================================================================
        // TOP CONTROL BAR & NAVIGATION
        // ====================================================================
        egui::Panel::top("top_bar").resizable(false).show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                // 3-Mode Operational Segmented Selector (§6)
                let cur_mode = s.operational_mode;
                for (mode, label, active_rgb, icon) in [
                    (
                        OperationalMode::SmartGuard,
                        "Smart Guard",
                        egui::Color32::from_rgb(22, 163, 74),
                        UiIcon::Shield,
                    ),
                    (
                        OperationalMode::GuardOff,
                        "Guard Off",
                        egui::Color32::from_rgb(71, 85, 105),
                        UiIcon::Pause,
                    ),
                    (
                        OperationalMode::BestPerformance,
                        "Best Performance",
                        egui::Color32::from_rgb(217, 119, 6),
                        UiIcon::Bolt,
                    ),
                ] {
                    let selected = cur_mode == mode;
                    let fill = if selected { Some(active_rgb) } else { None };
                    if icon_button(ui, icon, active_rgb, label, fill).clicked() && !selected {
                        s.set_operational_mode(mode, None);
                    }
                }

                if cur_mode == OperationalMode::BestPerformance {
                    ui.label("Timer:");
                    if ui.small_button("1h").clicked() {
                        s.set_operational_mode(OperationalMode::BestPerformance, Some(1));
                    }
                    if ui.small_button("2h").clicked() {
                        s.set_operational_mode(OperationalMode::BestPerformance, Some(2));
                    }
                    if ui.small_button("Unlimited").clicked() {
                        s.set_operational_mode(OperationalMode::BestPerformance, None);
                    }
                }

                if icon_button(
                    ui,
                    UiIcon::Info,
                    egui::Color32::from_rgb(96, 165, 250),
                    "Guide",
                    None,
                )
                .on_hover_text("Open how Agent Power Guard modes & intent detection work")
                .clicked()
                {
                    s.show_info_guide = !s.show_info_guide;
                }

                ui.separator();

                // Live Countdown Progress Bar
                let fraction = if s.countdown_max > 0 {
                    (s.countdown_secs as f32 / s.countdown_max as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };

                let progress_label = match s.operational_mode {
                    OperationalMode::GuardOff => "GUARD OFF (OS Plan)".to_string(),
                    OperationalMode::BestPerformance => "LOCKED AWAKE".to_string(),
                    OperationalMode::SmartGuard => {
                        if s.all_conditions_pass {
                            format!(
                                "Sleeping in: {} / {}",
                                format_time_compact(s.countdown_secs),
                                format_time_compact(s.countdown_max)
                            )
                        } else {
                            format!("HOLD ({})", format_time_compact(s.countdown_max))
                        }
                    }
                };

                let pbar = egui::ProgressBar::new(fraction)
                    .text(egui::RichText::new(progress_label).strong())
                    .animate(s.all_conditions_pass && s.operational_mode == OperationalMode::SmartGuard);

                ui.add_sized([(ui.available_width() - 370.0).max(140.0), 22.0], pbar);

                ui.separator();

                // Quick Action Buttons
                if icon_button(ui, UiIcon::Sleep, egui::Color32::from_rgb(147, 197, 253), "Sleep", None).clicked() {
                    s.set_toast("Executing sleep now via OS power call...");
                    let ok = sleep_now();
                    if !ok {
                        s.set_toast("Sleep call failed. Check OS sleep permissions / powercfg.");
                    }
                }

                if icon_button(ui, UiIcon::Clock, egui::Color32::from_rgb(56, 189, 248), "15s Test", None).clicked() {
                    s.config.idle_threshold_secs = 15;
                    s.config.monitor_timeout_mins = 1;
                    s.config.active_mode = PowerMode::Custom;
                    s.countdown_secs = 15;
                    s.countdown_max = 15;
                    s.config_dirty = true;
                    apply_monitor_timeout(1);
                    s.set_toast("15s Test active (sleep 15s, display off 1m).");
                }

                if icon_button(ui, UiIcon::Sliders, egui::Color32::from_rgb(148, 163, 184), "Power Plan", None).clicked() {
                    apply_optimal_power_plan(s.config.monitor_timeout_mins);
                    s.set_toast("Optimal Power Plan applied.");
                }

                if icon_button(ui, UiIcon::Tray, egui::Color32::from_rgb(148, 163, 184), "Tray", None).clicked() {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    flush_working_set();
                    s.set_toast("Minimized to system tray. Active in background.");
                }

                let (theme_lbl, theme_ic, theme_col) = if s.config.dark_mode {
                    ("Light", UiIcon::Sun, egui::Color32::from_rgb(251, 191, 36))
                } else {
                    ("Dark", UiIcon::Moon, egui::Color32::from_rgb(99, 102, 241))
                };
                if icon_button(ui, theme_ic, theme_col, theme_lbl, None).clicked() {
                    s.config.dark_mode = !s.config.dark_mode;
                    s.config_dirty = true;
                }
            });

            // Status note directly under 3-mode selector + live Intent & Stage 1 badge (§6)
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(s.operational_mode.status_note())
                        .weak()
                        .size(11.5),
                );
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("Intent: {}", s.intent.display_label()))
                        .strong()
                        .color(egui::Color32::from_rgb(96, 165, 250))
                        .size(11.5),
                );
                ui.separator();
                let stage1_txt = if s.defer_display_off_for_monitor_audio {
                    "Display-off: Protected (Monitor Audio)".to_string()
                } else if s.stage1_display_off_allowed {
                    if !s.config.cond8_monitor_timeout_enabled {
                        "Display-off: Disabled".to_string()
                    } else if s.monitor_off_triggered_this_idle {
                        "Display-off: Triggered".to_string()
                    } else {
                        format!(
                            "Display-off: {}s / {}s",
                            s.live_idle_secs,
                            s.config.monitor_timeout_mins.saturating_mul(60)
                        )
                    }
                } else {
                    "Display-off: Held by Meeting / Active Video / Mode".to_string()
                };
                ui.label(egui::RichText::new(stage1_txt).weak().size(11.5));
            });

            // Best Performance explicit expiry reminder banner
            if s.operational_mode == OperationalMode::BestPerformance {
                let now_s = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let exp_msg = if let Some(u) = s.eval_state.best_perf_until_epoch_secs {
                    format!("expires in {}m", u.saturating_sub(now_s) / 60)
                } else {
                    "no auto-expiry".to_string()
                };
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(146, 64, 14))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(12, 5))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            draw_icon_inline(ui, 15.0, egui::Color32::WHITE, UiIcon::Bolt);
                            ui.label(
                                egui::RichText::new(format!(
                                    "BEST PERFORMANCE ACTIVE: Sleep & display-off are locked off ({}).",
                                    exp_msg
                                ))
                                .color(egui::Color32::WHITE)
                                .strong(),
                            );
                            if ui.button("Return to Smart Guard").clicked() {
                                s.set_operational_mode(OperationalMode::SmartGuard, None);
                            }
                        });
                    });
            }

            // Passive Media Prompt / Lock In-App Banner (§5.3)
            if s.media_prompt_active {
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(30, 58, 138))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(12, 5))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            draw_icon_inline(ui, 15.0, egui::Color32::from_rgb(191, 219, 254), UiIcon::Info);
                            let label = if s.media_prompt_title.is_empty() {
                                format!("Idle playback detected in {}. Keep PC awake for this session?", s.media_prompt_app)
                            } else {
                                format!("Playing \"{}\" in {}. Keep PC awake for this session?", s.media_prompt_title, s.media_prompt_app)
                            };
                            ui.label(egui::RichText::new(label).color(egui::Color32::WHITE).strong());
                            if icon_button(ui, UiIcon::Check, egui::Color32::from_rgb(52, 211, 153), "Keep Awake", None).clicked() {
                                s.apply_media_action(MediaPromptAction::KeepAwake);
                            }
                            if icon_button(ui, UiIcon::Sleep, egui::Color32::from_rgb(147, 197, 253), "Sleep Now", None).clicked() {
                                let should_sleep = s.apply_media_action(MediaPromptAction::SleepNow);
                                if should_sleep {
                                    let _ = sleep_now();
                                }
                            }
                            if ui.button("Hide").clicked() {
                                s.apply_media_action(MediaPromptAction::Hide);
                            }
                        });
                    });
            } else if s.media_lock_active {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(147, 197, 253), UiIcon::Lock);
                    ui.label(
                        egui::RichText::new(format!(
                            "Media session lock active ({}) — unlocks {}s after pause/stop",
                            s.media_lock_app, s.config.media_pause_grace_secs
                        ))
                        .color(egui::Color32::from_rgb(147, 197, 253))
                        .size(11.5),
                    );
                    if ui.small_button("Sleep Now / Release Lock").clicked() {
                        let should_sleep = s.apply_media_action(MediaPromptAction::SleepNow);
                        if should_sleep {
                            let _ = sleep_now();
                        }
                    }
                });
            }

            ui.add_space(4.0);

            // Phase 2.3: Update Available Banner
            if let Some(new_ver) = s.update_available.clone() {
                let state_arc = self.state.clone();
                let dl_ready = s.update_downloaded_version.as_deref() == Some(new_ver.as_str())
                    && s.update_downloaded_path.is_some();
                let dl_path = s.update_downloaded_path.clone();
                let dl_busy = s.update_download_in_progress;

                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(30, 58, 138))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            draw_icon_inline(ui, 15.0, egui::Color32::from_rgb(147, 197, 253), UiIcon::Download);
                            ui.label(
                                egui::RichText::new(format!(
                                    "New release available: {} (Installed: v{})",
                                    new_ver,
                                    env!("CARGO_PKG_VERSION")
                                ))
                                .strong()
                                .color(egui::Color32::from_rgb(191, 219, 254)),
                            );
                            if dl_ready {
                                if icon_button(ui, UiIcon::Check, egui::Color32::WHITE, "Install & Restart Now", Some(egui::Color32::from_rgb(16, 185, 129))).clicked() {
                                    if let Some(ref p) = dl_path {
                                        if let Err(err) = install_downloaded_update(p) {
                                            s.set_toast(err);
                                        }
                                    }
                                }
                            } else if dl_busy {
                                ui.label(
                                    egui::RichText::new("Downloading update in background...")
                                        .color(egui::Color32::from_rgb(191, 219, 254)),
                                );
                            } else if icon_button(ui, UiIcon::Download, egui::Color32::WHITE, "Update Now", Some(egui::Color32::from_rgb(37, 99, 235))).clicked() {
                                trigger_update_download_locked(&mut s, state_arc, Some(new_ver));
                            }
                            if ui.button("View Releases").clicked() {
                                open_url(
                                    "https://github.com/Subham-Maity/agent-power-guard/releases",
                                );
                            }
                        });
                    });
                ui.add_space(4.0);
            }

            // Tab Navigation Bar (with crisp vector icons)
            ui.horizontal_wrapped(|ui| {
                for (tab, label, icon, col) in [
                    (ActiveTab::Guard, "Watchdog & Presets", UiIcon::Shield, egui::Color32::from_rgb(52, 211, 153)),
                    (ActiveTab::History, "Block History", UiIcon::History, egui::Color32::from_rgb(245, 158, 11)),
                    (ActiveTab::Energy, "Energy Savings", UiIcon::Coin, egui::Color32::from_rgb(56, 189, 248)),
                    (ActiveTab::Diagnostic, "Test & Report", UiIcon::Activity, egui::Color32::from_rgb(244, 63, 94)),
                    (ActiveTab::Notifications, "Alerts & Updates", UiIcon::Bell, egui::Color32::from_rgb(168, 85, 247)),
                    (ActiveTab::Remote, "Remote", UiIcon::Globe, egui::Color32::from_rgb(96, 165, 250)),
                ] {
                    let active = s.active_tab == tab;
                    let rt = egui::RichText::new(format!("    {}", label)).strong();
                    let resp = ui.selectable_label(active, rt);
                    let r = egui::Rect::from_center_size(
                        egui::pos2(resp.rect.left() + 11.0, resp.rect.center().y),
                        egui::vec2(13.0, 13.0),
                    );
                    let ic = if active { egui::Color32::WHITE } else { col };
                    paint_icon(&ui.painter_at(resp.rect), r, ic, icon);
                    if resp.clicked() {
                        s.active_tab = tab;
                    }
                }
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
                        draw_icon_inline(ui, 15.0, egui::Color32::WHITE, UiIcon::Warning);
                        ui.label(
                            egui::RichText::new(format!(
                                "SLEEP IMMINENT: PC sleeping in {} seconds!",
                                s.countdown_secs
                            ))
                            .strong()
                            .color(egui::Color32::WHITE),
                        );
                        if icon_button(
                            ui,
                            UiIcon::Clock,
                            egui::Color32::from_rgb(15, 23, 42),
                            "Postpone (+15m)",
                            None,
                        )
                        .clicked()
                        {
                            s.postponed_until = Some(Instant::now() + Duration::from_secs(15 * 60));
                            s.set_toast("Sleep postponed for 15 minutes.");
                        }
                        if icon_button(
                            ui,
                            UiIcon::Bolt,
                            egui::Color32::WHITE,
                            "Sleep Immediately",
                            Some(egui::Color32::from_rgb(153, 27, 27)),
                        )
                        .clicked()
                        {
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
                            draw_icon_inline(ui, 14.0, egui::Color32::WHITE, UiIcon::Lock);
                            ui.label(
                                egui::RichText::new(format!(
                                    "ACTIVE AGENT LEASE: \"{}\" ({}s remaining)",
                                    s.agent_lease_reason, rem
                                ))
                                .color(egui::Color32::WHITE)
                                .strong(),
                            );
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
                    ui.horizontal(|ui| {
                        draw_icon_inline(ui, 13.0, egui::Color32::LIGHT_BLUE, UiIcon::Info);
                        ui.label(
                            egui::RichText::new(msg).color(egui::Color32::LIGHT_BLUE).size(12.0),
                        );
                    });
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

                        // Phase 3.1 & 3.3: Power Modes & Battery Status
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                icon_card_title(ui, UiIcon::Bolt, egui::Color32::from_rgb(245, 158, 11), "Power Mode");
                                if s.live_battery_status.has_battery {
                                    let batt_pct = s.live_battery_status.percent.unwrap_or(100);
                                    let batt_text = if s.live_battery_status.on_ac_power {
                                        format!("AC ({}%)", batt_pct)
                                    } else {
                                        format!("Battery ({}%)", batt_pct)
                                    };
                                    let batt_color = if !s.live_battery_status.on_ac_power && batt_pct <= s.config.low_battery_percent_threshold {
                                        egui::Color32::RED
                                    } else if !s.live_battery_status.on_ac_power {
                                        egui::Color32::YELLOW
                                    } else {
                                        egui::Color32::from_rgb(16, 185, 129)
                                    };
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(egui::RichText::new(batt_text).color(batt_color).strong().size(11.0));
                                        draw_icon_inline(ui, 13.0, batt_color, UiIcon::Battery);
                                    });
                                }
                            });
                            ui.label(egui::RichText::new("Active operational profile:").weak().size(11.0));
                            ui.horizontal_wrapped(|ui| {
                                let modes = [
                                    (PowerMode::Balanced, "Balanced (3m)"),
                                    (PowerMode::Work, "Work (10m)"),
                                    (PowerMode::Night, "Night (1m)"),
                                    (PowerMode::Away, "Away (30s)"),
                                ];
                                for (m, lbl) in modes {
                                    let is_active = s.config.active_mode == m;
                                    let btn = if is_active {
                                        egui::Button::new(egui::RichText::new(lbl).strong().color(egui::Color32::WHITE))
                                            .fill(egui::Color32::from_rgb(37, 99, 235))
                                    } else {
                                        egui::Button::new(lbl)
                                    };
                                    if ui.add(btn).clicked() {
                                        m.apply_to_config(&mut s.config);
                                        s.countdown_secs = s.config.idle_threshold_secs;
                                        s.countdown_max = s.config.idle_threshold_secs;
                                        s.config_dirty = true;
                                        apply_monitor_timeout(s.config.monitor_timeout_mins);
                                        s.set_toast(format!("Activated {} Power Mode.", m.name()));
                                    }
                                }
                            });

                            // Stay-Awake quick controls
                            ui.add_space(2.0);
                            let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                            if let Some(until) = s.config.stay_awake_until_epoch_secs {
                                if now_epoch < until {
                                    let rem_m = (until - now_epoch) / 60 + 1;
                                    ui.horizontal(|ui| {
                                        draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(245, 158, 11), UiIcon::Clock);
                                        ui.label(egui::RichText::new(format!("Keep Awake Active ({}m left)", rem_m)).color(egui::Color32::from_rgb(245, 158, 11)).strong());
                                        if ui.button("Cancel").clicked() {
                                            s.config.stay_awake_until_epoch_secs = None;
                                            s.config_dirty = true;
                                            s.set_toast("Keep awake cancelled.");
                                        }
                                    });
                                } else {
                                    s.config.stay_awake_until_epoch_secs = None;
                                }
                            } else {
                                ui.horizontal(|ui| {
                                    draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(148, 163, 184), UiIcon::Clock);
                                    ui.label(egui::RichText::new("Keep Awake:").weak());
                                    if ui.button("+30m").clicked() {
                                        s.config.stay_awake_until_epoch_secs = Some(now_epoch + 30 * 60);
                                        s.config_dirty = true;
                                        s.set_toast("Keep awake scheduled for 30 minutes.");
                                    }
                                    if ui.button("+1h").clicked() {
                                        s.config.stay_awake_until_epoch_secs = Some(now_epoch + 60 * 60);
                                        s.config_dirty = true;
                                        s.set_toast("Keep awake scheduled for 1 hour.");
                                    }
                                    if ui.button("+2h").clicked() {
                                        s.config.stay_awake_until_epoch_secs = Some(now_epoch + 120 * 60);
                                        s.config_dirty = true;
                                        s.set_toast("Keep awake scheduled for 2 hours.");
                                    }
                                });
                            }
                        });

                        ui.add_space(4.0);

                        // Quick Presets
                        ui.group(|ui| {
                            icon_card_title(ui, UiIcon::Clock, egui::Color32::from_rgb(56, 189, 248), "Quick Presets (Staged Power Strategy)");
                            ui.label(egui::RichText::new("Turns monitor off first, then puts PC to deep sleep.").weak().size(11.0));
                            ui.add_space(4.0);

                            ui.horizontal_wrapped(|ui| {
                                let presets: [(&str, u32, u32, &str); 10] = [
                                    ("15s Test", 15, 1, "15s Test Mode"),
                                    ("3m (Default)", 180, 2, "3m Default"),
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
                                        s.config.active_mode = PowerMode::Custom;
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

                        // Condition 10: Watched GPU % (AI Models & Compute — Baseline-Relative + Power)
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond10_gpu_enabled, "3. Watched GPU % (AI Models)").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if s.cond10_gpu.probe_unavailable.is_some() {
                                        ui.label(egui::RichText::new("PROBE UNAVAILABLE").color(egui::Color32::from_rgb(245, 158, 11)).strong().size(10.5));
                                    } else {
                                        let tag = if s.cond10_gpu.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                        ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                    }
                                });
                            });
                            if let Some(reason) = &s.cond10_gpu.probe_unavailable {
                                ui.horizontal(|ui| {
                                    draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(245, 158, 11), UiIcon::Warning);
                                    ui.label(egui::RichText::new(reason).color(egui::Color32::from_rgb(245, 158, 11)).size(10.5));
                                });
                            } else {
                                let pwr_str = s.gpu_power_mw.map(|mw| format!(", {:.1}W", mw as f32 / 1000.0)).unwrap_or_default();
                                ui.label(egui::RichText::new(format!(
                                    "Live GPU: {:.1}%{} (Idle floor: {:.1}%, Effective cutoff: {:.1}%)",
                                    s.live_gpu_pct, pwr_str, s.gpu_idle_floor_pct, s.gpu_effective_threshold_pct
                                )).weak().size(11.0));
                            }
                            ui.horizontal(|ui| {
                                ui.label("Floor Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.gpu_threshold_pct, 0.5..=90.0).suffix("%")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Baseline Margin:");
                                if ui.add(egui::Slider::new(&mut s.config.gpu_baseline_margin_pct, 3.0..=40.0).suffix("%")).changed() {
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
                            ui.label(egui::RichText::new(format!(
                                "Status: {} | Audio signal: {} ({:.3})",
                                s.cond11_mic_cam.value_text, s.mic_capture_signal_label, s.mic_capture_peak
                            )).weak().size(11.0));
                            ui.collapsing("Meeting vs Creative Apps (§5.4)", |ui| {
                                ui.label(egui::RichText::new("Strict Meeting Apps (block even when muted):").size(10.0));
                                if ui.text_edit_singleline(&mut s.config.mic_meeting_apps).changed() {
                                    s.config_dirty = true;
                                }
                                ui.label(egui::RichText::new("Creative Apps (block only while audio active):").size(10.0));
                                if ui.text_edit_singleline(&mut s.config.mic_creative_apps).changed() {
                                    s.config_dirty = true;
                                }
                            });
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
                            ui.label(egui::RichText::new(format!("Live Global CPU (minus self): {}", s.cond4_pc_cpu.value_text)).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.pc_cpu_threshold, 5.0..=90.0).suffix("%")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        // Condition 5: Attributed Disk I/O (§5.2)
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
                            ui.label(egui::RichText::new(format!(
                                "Effective Disk: {:.0} KB/s (Top: {} {:.0} KB/s | Raw Physical SSD: {:.0} KB/s)",
                                s.live_pc_disk_kbps, s.top_disk_proc, s.top_disk_kbps, s.raw_pc_disk_kbps
                            )).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.pc_disk_threshold_kbps, 500.0..=100000.0).suffix(" KB/s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.collapsing("Edit Background Disk Noise List", |ui| {
                                if ui.text_edit_multiline(&mut s.config.disk_noise_processes).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.collapsing("Debug: Physical Disk vs Process I/O (Top 5)", |ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Raw Physical SSD (IOCTL): {:.0} KB/s | Bounded Effective: {:.0} KB/s",
                                        s.raw_pc_disk_kbps, s.live_pc_disk_kbps
                                    ))
                                    .size(11.0),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Per-process GetProcessIoCounters includes pipes/IPC/network, so browsers/IDEs are bounded by physical disk I/O:",
                                    )
                                    .weak()
                                    .size(10.5),
                                );
                                for (pname, pkbps) in &s.top_io_debug {
                                    ui.label(
                                        egui::RichText::new(format!("• {}: {:.0} KB/s", pname, pkbps))
                                            .size(11.0),
                                    );
                                }
                            });
                        });

                        // Condition 6: Default-Route Network (§5.2)
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
                            ui.label(egui::RichText::new(format!(
                                "Default-Route Net: {:.0} KB/s (Raw incl. virtual/WSL: {:.0} KB/s)",
                                s.live_pc_net_kbps, s.raw_pc_net_kbps
                            )).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.pc_net_threshold_kbps, 20.0..=10000.0).suffix(" KB/s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.collapsing("NIC Allowlist Override", |ui| {
                                ui.label(egui::RichText::new("Optional comma-separated NIC names (empty = auto default-route):").size(10.0));
                                if ui.text_edit_singleline(&mut s.config.nic_allowlist_override).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        // Condition 7: Signal-Conditioning Spike Filter (§2)
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
                            ui.label(egui::RichText::new(format!("State: {}", s.cond7_spike_filter.value_text)).weak().size(11.0));
                            ui.horizontal(|ui| {
                                ui.label("Window:");
                                if ui.add(egui::Slider::new(&mut s.config.spike_filter_secs, 3..=60).suffix("s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Busy K:");
                                if ui.add(egui::DragValue::new(&mut s.config.filter_busy_count).range(2..=30)).changed() {
                                    s.config_dirty = true;
                                }
                                ui.label("Quiet Q:");
                                if ui.add(egui::DragValue::new(&mut s.config.filter_quiet_count).range(1..=10)).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        // Condition 8: Monitor Timeout & Audio Protection (§5.8)
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
                            if ui.checkbox(&mut s.config.protect_monitor_audio, "Keep display on when playing through monitor/HDMI speakers").changed() {
                                s.config_dirty = true;
                            }
                        });

                        // Condition 9: Power Requests & Ownership (§5.7)
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut s.config.cond9_power_requests_enabled, "11. Power Requests").changed() {
                                    s.config_dirty = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if s.cond9_power_requests.probe_unavailable.is_some() {
                                        ui.label(egui::RichText::new("PROBE UNAVAILABLE").color(egui::Color32::from_rgb(245, 158, 11)).strong().size(10.5));
                                    } else {
                                        let tag = if s.cond9_power_requests.is_passing { ("PASS (Quiet)", egui::Color32::from_rgb(34, 139, 34)) } else { ("BLOCKING SLEEP", egui::Color32::RED) };
                                        ui.label(egui::RichText::new(tag.0).color(tag.1).strong().size(11.0));
                                    }
                                });
                            });
                            if let Some(reason) = &s.cond9_power_requests.probe_unavailable {
                                ui.horizontal(|ui| {
                                    draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(245, 158, 11), UiIcon::Warning);
                                    ui.label(egui::RichText::new(format!("{} — Run once elevated to inspect driver/app locks", reason)).color(egui::Color32::from_rgb(245, 158, 11)).size(10.5));
                                });
                            }
                            if ui.checkbox(&mut s.config.respect_display_requests, "Respect DISPLAY Requests (Video Playback)").changed() {
                                s.config_dirty = true;
                            }
                            if let Some(supp) = &s.suppressed_browser_request {
                                ui.label(egui::RichText::new(format!("Suppressed stale silent browser lock: {}", supp)).color(egui::Color32::from_rgb(56, 189, 248)).size(10.5));
                            }
                            let mut ignore_owner: Option<String> = None;
                            for owner in &s.active_power_owners {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(format!("[{}] {}", owner.category, owner.name)).weak().size(10.5));
                                    if ui.small_button("Ignore").clicked() {
                                        ignore_owner = Some(owner.name.clone());
                                    }
                                });
                            }
                            if let Some(target) = ignore_owner {
                                s.apply_ignore_item("power_owner", &target);
                            }
                            ui.collapsing("Ignored Power Request Owners", |ui| {
                                if ui.text_edit_singleline(&mut s.config.ignored_power_owners).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            if ui.button("Override Audio/Browser Locks").clicked() {
                                match override_audio_browser_power_requests() {
                                    Ok(count) => s.set_toast(format!(
                                        "Registered {} powercfg audio/browser overrides.",
                                        count
                                    )),
                                    Err(err) => s.set_toast(err),
                                }
                            }
                        });

                        ui.collapsing("Passive Media, Zombie Leases & Wake", |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Media Prompt Delay:");
                                if ui.add(egui::Slider::new(&mut s.config.media_prompt_after_secs, 30..=1800).suffix("s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Media Pause Grace:");
                                if ui.add(egui::Slider::new(&mut s.config.media_pause_grace_secs, 10..=300).suffix("s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Zombie Lease Alert:");
                                if ui.add(egui::Slider::new(&mut s.config.lease_idle_mins, 5..=60).suffix(" min")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            if ui.checkbox(&mut s.config.lease_auto_expire_enabled, "Auto-expire hung PID leases after idle limit").changed() {
                                s.config_dirty = true;
                            }
                            if s.config.lease_auto_expire_enabled {
                                ui.horizontal(|ui| {
                                    ui.label("Auto-expire Limit:");
                                    if ui.add(egui::Slider::new(&mut s.config.lease_auto_expire_mins, 10..=120).suffix(" min")).changed() {
                                        s.config_dirty = true;
                                    }
                                });
                            }
                            ui.horizontal(|ui| {
                                ui.label("Unattended Wake Sleep:");
                                if ui.add(egui::Slider::new(&mut s.config.unattended_wake_secs, 30..=300).suffix("s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                        });

                        ui.add_space(6.0);
                        ui.separator();

                        // Phase 3.1: Quiet Hours & Schedules
                        ui.collapsing("Quiet Hours & Night Schedule", |ui| {
                            if ui.checkbox(&mut s.config.quiet_hours_enabled, "Enable Quiet Hours (Aggressive sleep during night)").changed() {
                                s.config_dirty = true;
                            }
                            ui.horizontal(|ui| {
                                ui.label("Start Time:");
                                if ui.text_edit_singleline(&mut s.config.quiet_hours_start).changed() {
                                    s.config_dirty = true;
                                }
                                ui.label("End Time:");
                                if ui.text_edit_singleline(&mut s.config.quiet_hours_end).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Night Mode:");
                                ui.selectable_value(&mut s.config.quiet_hours_mode, PowerMode::Night, "Night (60s)");
                                ui.selectable_value(&mut s.config.quiet_hours_mode, PowerMode::Away, "Away (30s)");
                            });
                            if ui.checkbox(&mut s.config.sleep_after_lease, "Sleep immediately after active agent lease ends").changed() {
                                s.config_dirty = true;
                            }
                        });

                        // Phase 3.3: Laptop & Battery Profile
                        ui.collapsing("Laptop & Battery Profile", |ui| {
                            if ui.checkbox(&mut s.config.battery_profile_enabled, "Enable DC Battery Profile (Applies automatically on battery)").changed() {
                                s.config_dirty = true;
                            }
                            ui.horizontal(|ui| {
                                ui.label("Battery Idle Threshold:");
                                if ui.add(egui::Slider::new(&mut s.config.battery_idle_threshold_secs, 10..=600).suffix("s")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Battery Agent CPU limit:");
                                if ui.add(egui::Slider::new(&mut s.config.battery_agent_cpu_threshold, 0.5..=5.0).suffix("%")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            if ui.checkbox(&mut s.config.low_battery_auto_sleep_enabled, "Emergency Sleep on Low Battery").changed() {
                                s.config_dirty = true;
                            }
                            ui.horizontal(|ui| {
                                ui.label("Low Battery Cutoff:");
                                if ui.add(egui::Slider::new(&mut s.config.low_battery_percent_threshold, 5..=30).suffix("%")).changed() {
                                    s.config_dirty = true;
                                }
                            });
                            if s.live_battery_status.has_battery {
                                ui.label(egui::RichText::new(format!(
                                    "Detected Battery: {}% ({})",
                                    s.live_battery_status.percent.unwrap_or(0),
                                    if s.live_battery_status.on_ac_power { "Plugged In / AC" } else { "Running on Battery" }
                                )).weak().size(11.0));
                            } else {
                                ui.label(egui::RichText::new("Desktop detected: No battery power supply found.").weak().size(11.0));
                            }
                        });

                        // Phase 3.2: Scheduled Wake Timers
                        ui.collapsing("Scheduled Wake Timers", |ui| {
                            ui.label(egui::RichText::new("Wake machine from sleep at specified times (e.g. for nightly backup or CI jobs), then let Watchdog put it back to sleep once idle.").weak().size(11.0));
                            let mut to_remove = Vec::new();
                            for timer in &s.config.wake_timers {
                                ui.horizontal(|ui| {
                                    draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(96, 165, 250), UiIcon::Clock);
                                    ui.label(egui::RichText::new(&timer.time_str).strong());
                                    ui.label(&timer.label);
                                    if icon_button(ui, UiIcon::Trash, egui::Color32::from_rgb(248, 113, 113), "Remove", None).clicked() {
                                        to_remove.push(timer.id.clone());
                                    }
                                });
                            }
                            for id in to_remove {
                                remove_os_wake(&id);
                                s.config.wake_timers.retain(|t| t.id != id);
                                s.config_dirty = true;
                                s.set_toast("Wake timer removed.");
                            }
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if ui.button("+ Add 02:00 AM (Nightly Backup Wake)").clicked() {
                                    let id = format!("{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs());
                                    let timer = WakeTimer {
                                        id: id.clone(),
                                        time_str: "02:00".to_string(),
                                        label: "Nightly Backup Wake".to_string(),
                                        enabled: true,
                                    };
                                    schedule_os_wake(&timer);
                                    s.config.wake_timers.push(timer);
                                    s.config_dirty = true;
                                    s.set_toast("Scheduled 02:00 AM Wake.");
                                }
                                if ui.button("+ Add 06:00 AM (Early Standby Wake)").clicked() {
                                    let id = format!("{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs());
                                    let timer = WakeTimer {
                                        id: id.clone(),
                                        time_str: "06:00".to_string(),
                                        label: "Early Standby Wake".to_string(),
                                        enabled: true,
                                    };
                                    schedule_os_wake(&timer);
                                    s.config.wake_timers.push(timer);
                                    s.config_dirty = true;
                                    s.set_toast("Scheduled 06:00 AM Wake.");
                                }
                            });
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
                    ui.label(
                        egui::RichText::new("— 1 Hz live sampling").color(egui::Color32::GRAY),
                    );
                });
                ui.separator();

                let reserved_h = if s.structured_blockers.is_empty() {
                    85.0
                } else {
                    165.0
                };
                let available_h = (ui.available_height() - reserved_h).max(200.0);
                let chart_h = (available_h / 5.0).clamp(50.0, 105.0);

                // Chart 1: Agent CPU %
                ui.label(
                    egui::RichText::new(format!(
                        "1. Agent CPU % (Live: {:.1}%)",
                        s.live_agent_cpu_pct
                    ))
                    .strong()
                    .size(11.0),
                );
                let agent_pts: PlotPoints = s
                    .history_agent_cpu
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
                        plot_ui.line(
                            Line::new("Agent CPU", agent_pts)
                                .color(egui::Color32::from_rgb(0, 191, 255)),
                        );
                        plot_ui.hline(
                            HLine::new("Threshold", s.config.agent_cpu_threshold as f64)
                                .color(egui::Color32::RED)
                                .style(LineStyle::dashed_loose()),
                        );
                    });

                // Chart 2: GPU %
                ui.label(
                    egui::RichText::new(format!("2. GPU % (Live: {:.1}%)", s.live_gpu_pct))
                        .strong()
                        .size(11.0),
                );
                let gpu_pts: PlotPoints = s
                    .history_gpu
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| [i as f64, v as f64])
                    .collect::<Vec<[f64; 2]>>()
                    .into();
                Plot::new("chart_gpu")
                    .height(chart_h)
                    .allow_drag(false)
                    .allow_zoom(false)
                    .allow_scroll(false)
                    .include_y(0.0)
                    .include_y(s.config.gpu_threshold_pct * 1.5)
                    .show(ui, |plot_ui| {
                        plot_ui.line(
                            Line::new("GPU Compute", gpu_pts)
                                .color(egui::Color32::from_rgb(16, 185, 129)),
                        );
                        plot_ui.hline(
                            HLine::new("Threshold", s.config.gpu_threshold_pct as f64)
                                .color(egui::Color32::RED)
                                .style(LineStyle::dashed_loose()),
                        );
                    });

                // Chart 3: Whole-PC CPU %
                ui.label(
                    egui::RichText::new(format!(
                        "3. Whole-PC CPU % (Live: {:.1}%)",
                        s.live_pc_cpu_pct
                    ))
                    .strong()
                    .size(11.0),
                );
                let pc_cpu_pts: PlotPoints = s
                    .history_pc_cpu
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
                        plot_ui.line(
                            Line::new("PC CPU", pc_cpu_pts)
                                .color(egui::Color32::from_rgb(147, 112, 219)),
                        );
                        plot_ui.hline(
                            HLine::new("Threshold", s.config.pc_cpu_threshold as f64)
                                .color(egui::Color32::RED)
                                .style(LineStyle::dashed_loose()),
                        );
                    });

                // Chart 4: Disk I/O
                ui.label(
                    egui::RichText::new(format!(
                        "4. Total Disk I/O (Live: {:.0} KB/s)",
                        s.live_pc_disk_kbps
                    ))
                    .strong()
                    .size(11.0),
                );
                let disk_pts: PlotPoints = s
                    .history_pc_disk
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
                        plot_ui.line(
                            Line::new("Disk KB/s", disk_pts)
                                .color(egui::Color32::from_rgb(255, 165, 0)),
                        );
                        plot_ui.hline(
                            HLine::new("Threshold", s.config.pc_disk_threshold_kbps as f64)
                                .color(egui::Color32::RED)
                                .style(LineStyle::dashed_loose()),
                        );
                    });

                // Chart 5: Network I/O
                ui.label(
                    egui::RichText::new(format!(
                        "5. Total Network I/O (Live: {:.0} KB/s)",
                        s.live_pc_net_kbps
                    ))
                    .strong()
                    .size(11.0),
                );
                let net_pts: PlotPoints = s
                    .history_pc_net
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
                    .include_y(s.config.pc_net_threshold_kbps * 1.3)
                    .show(ui, |plot_ui| {
                        plot_ui.line(
                            Line::new("Net KB/s", net_pts)
                                .color(egui::Color32::from_rgb(50, 205, 50)),
                        );
                        plot_ui.hline(
                            HLine::new("Threshold", s.config.pc_net_threshold_kbps as f64)
                                .color(egui::Color32::RED)
                                .style(LineStyle::dashed_loose()),
                        );
                    });

                ui.add_space(6.0);

                // Adaptive Baseline Suggestion Banner (§8)
                if let Some(sug) = s.suggested_thresholds {
                    let needs_bump = sug.pc_cpu_pct > s.config.pc_cpu_threshold
                        || sug.disk_kbps > s.config.pc_disk_threshold_kbps
                        || sug.net_kbps > s.config.pc_net_threshold_kbps;
                    if needs_bump {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(23, 37, 84))
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::symmetric(12, 6))
                            .show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    draw_icon_inline(ui, 14.0, egui::Color32::from_rgb(147, 197, 253), UiIcon::Info);
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Suggested idle floors from quiet periods: CPU {:.0}%, Disk {:.0} KB/s, Net {:.0} KB/s",
                                            sug.pc_cpu_pct, sug.disk_kbps, sug.net_kbps
                                        ))
                                        .color(egui::Color32::from_rgb(191, 219, 254))
                                        .size(11.5),
                                    );
                                    if ui.small_button("Apply Suggestions").clicked() {
                                        s.config.pc_cpu_threshold = s.config.pc_cpu_threshold.max(sug.pc_cpu_pct);
                                        s.config.pc_disk_threshold_kbps =
                                            s.config.pc_disk_threshold_kbps.max(sug.disk_kbps);
                                        s.config.pc_net_threshold_kbps =
                                            s.config.pc_net_threshold_kbps.max(sug.net_kbps);
                                        s.config_dirty = true;
                                        s.set_toast("Applied adaptive idle baseline thresholds.");
                                    }
                                });
                            });
                        ui.add_space(4.0);
                    }
                }

                // Blocker Banner & Structured Blocker Transparency Table (§6)
                let banner_fill = match s.operational_mode {
                    OperationalMode::GuardOff => egui::Color32::from_rgb(60, 60, 60),
                    OperationalMode::BestPerformance => egui::Color32::from_rgb(146, 64, 14),
                    OperationalMode::SmartGuard => {
                        if s.all_conditions_pass {
                            egui::Color32::from_rgb(20, 100, 20)
                        } else {
                            egui::Color32::from_rgb(139, 26, 26)
                        }
                    }
                };

                let banner_frame = egui::Frame::new()
                    .fill(banner_fill)
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(14, 8));

                banner_frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&s.blocker_banner_text)
                                .color(egui::Color32::WHITE)
                                .strong(),
                        );
                    });
                });

                if !s.structured_blockers.is_empty() {
                    ui.add_space(4.0);
                    let mut pending_ignore: Option<(String, String)> = None;
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("Active Blockers — What / Who / Value / Since").strong().size(11.5));
                        egui::Grid::new("active_blockers_grid")
                            .striped(true)
                            .min_col_width(70.0)
                            .show(ui, |ui| {
                                for b in s.structured_blockers.iter().take(5) {
                                    ui.label(egui::RichText::new(format!("#{} {}", b.condition, b.name)).strong().size(11.0));
                                    ui.label(egui::RichText::new(&b.who).color(egui::Color32::from_rgb(251, 191, 36)).size(11.0));
                                    ui.label(egui::RichText::new(&b.value).weak().size(11.0));
                                    ui.label(egui::RichText::new(format!("{}s", b.since_secs)).weak().size(11.0));
                                    if let Some(target) = &b.ignore_target {
                                        let kind = b.ignore_kind.as_deref().unwrap_or("process");
                                        if ui.small_button("+ Ignore").clicked() {
                                            pending_ignore = Some((kind.to_string(), target.clone()));
                                        }
                                    } else {
                                        ui.label("");
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                    if let Some((kind, target)) = pending_ignore {
                        s.apply_ignore_item(&kind, &target);
                    }
                }
            });
        }
        // ====================================================================
        // TAB 2: BLOCK HISTORY ("WHAT KEPT MY PC AWAKE") VIEW
        // ====================================================================
        else if s.active_tab == ActiveTab::History {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    icon_heading(
                        ui,
                        UiIcon::History,
                        egui::Color32::from_rgb(245, 158, 11),
                        "What Kept My PC Awake (Block History)",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(
                            ui,
                            UiIcon::Trash,
                            egui::Color32::from_rgb(248, 113, 113),
                            "Clear History",
                            None,
                        )
                        .clicked()
                        {
                            s.clear_block_history();
                            s.set_toast("Block reason history cleared.");
                        }
                        if icon_button(
                            ui,
                            UiIcon::Copy,
                            egui::Color32::from_rgb(147, 197, 253),
                            "Copy CSV",
                            None,
                        )
                        .clicked()
                        {
                            let csv = export_block_history_csv(&s.block_history);
                            copy_text_reliable(ui.ctx(), &csv);
                            s.set_toast("History CSV copied to clipboard.");
                        }
                        if icon_button(
                            ui,
                            UiIcon::Copy,
                            egui::Color32::from_rgb(147, 197, 253),
                            "Copy JSON",
                            None,
                        )
                        .clicked()
                        {
                            let json = export_block_history_json(&s.block_history);
                            copy_text_reliable(ui.ctx(), &json);
                            s.set_toast("History JSON copied to clipboard.");
                        }
                    });
                });

                ui.add_space(8.0);
                let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

                // 1. Metric Summary Cards (3 even-width cards)
                let top_blockers = calculate_top_blockers(&s.block_history);
                let total_blocked_secs: u64 = s.block_history.iter().map(|r| r.duration_secs).sum();
                let last_night = calculate_last_night_summary(&s.block_history, now_epoch);

                ui.columns(3, |cols| {
                    app_card_frame(&cols[0]).show(&mut cols[0], |ui| {
                        ui.horizontal(|ui| {
                            draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(245, 158, 11), UiIcon::Clock);
                            ui.label(egui::RichText::new("Total Blocked Time").weak().size(12.0));
                        });
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new(format_human_duration(total_blocked_secs)).strong().size(18.0).color(egui::Color32::from_rgb(245, 158, 11)));
                    });

                    app_card_frame(&cols[1]).show(&mut cols[1], |ui| {
                        ui.horizontal(|ui| {
                            draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(239, 68, 68), UiIcon::Warning);
                            ui.label(egui::RichText::new("Top Blocker").weak().size(12.0));
                        });
                        ui.add_space(2.0);
                        let top_name = top_blockers.first().map(|t| t.name.as_str()).unwrap_or("None");
                        let top_time = top_blockers.first().map(|t| format_human_duration(t.total_secs)).unwrap_or_default();
                        let top_txt = if top_time.is_empty() {
                            top_name.to_string()
                        } else {
                            format!("{} ({})", top_name, top_time)
                        };
                        ui.label(egui::RichText::new(top_txt).strong().size(14.0).color(egui::Color32::from_rgb(248, 113, 113)));
                    });

                    app_card_frame(&cols[2]).show(&mut cols[2], |ui| {
                        ui.horizontal(|ui| {
                            draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(147, 197, 253), UiIcon::Moon);
                            ui.label(egui::RichText::new("Last Night (Past 12h)").weak().size(12.0));
                        });
                        ui.add_space(2.0);
                        let top = last_night.top_blocker.unwrap_or_else(|| "All Quiet".to_string());
                        ui.label(egui::RichText::new(format!("Blocked: {} | Top: {}", format_human_duration(last_night.total_blocked_secs), top)).strong().size(13.0).color(egui::Color32::from_rgb(147, 197, 253)));
                    });
                });

                ui.add_space(8.0);

                // Config Controls Row
                ui.horizontal_wrapped(|ui| {
                    if ui.checkbox(&mut s.config.block_history_enabled, egui::RichText::new("Record Block History").strong()).changed() {
                        s.config_dirty = true;
                    }
                    ui.separator();
                    ui.label("Retention (Days):");
                    if ui.add(egui::DragValue::new(&mut s.config.block_history_retention_days).range(1..=90)).changed() {
                        s.config_dirty = true;
                    }
                    let storage_path = get_block_history_path();
                    ui.separator();
                    ui.label(egui::RichText::new(format!("File: {}", storage_path.display())).weak().size(11.0));
                });

                ui.add_space(8.0);

                // Top Blockers Table
                if !top_blockers.is_empty() {
                    ui.label(egui::RichText::new("Top Blockers by Cumulative Hold Time:").strong());
                    egui::Grid::new("top_blockers_grid").striped(true).min_col_width(120.0).show(ui, |ui| {
                        ui.label(egui::RichText::new("Block Reason").strong());
                        ui.label(egui::RichText::new("Cumulative Duration").strong());
                        ui.label(egui::RichText::new("Occurrences").strong());
                        ui.label(egui::RichText::new("% of Total").strong());
                        ui.end_row();

                        for b in top_blockers.iter().take(5) {
                            ui.label(&b.name);
                            ui.label(format_human_duration(b.total_secs));
                            ui.label(b.count.to_string());
                            let pct = if total_blocked_secs > 0 {
                                (b.total_secs as f64 / total_blocked_secs as f64) * 100.0
                            } else {
                                0.0
                            };
                            ui.label(format!("{:.1}%", pct));
                            ui.end_row();
                        }
                    });
                    ui.add_space(10.0);
                }

                // Timeline Log
                ui.label(egui::RichText::new("Event Timeline (Recent Block Intervals):").strong());
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if s.block_history.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new("No block events recorded yet. When a process, download, or sensor blocks automatic sleep, it will appear here.").weak());
                        });
                    } else {
                        egui::Grid::new("block_history_timeline_grid").striped(true).min_col_width(100.0).show(ui, |ui| {
                            ui.label(egui::RichText::new("Time").strong());
                            ui.label(egui::RichText::new("Duration").strong());
                            ui.label(egui::RichText::new("Primary Blocker").strong());
                            ui.label(egui::RichText::new("Other Blockers").strong());
                            ui.end_row();

                            for r in s.block_history.iter().rev() {
                                ui.label(format_epoch_local(r.start_epoch_secs));
                                ui.label(format_human_duration(r.duration_secs));
                                ui.label(egui::RichText::new(&r.primary_blocker).color(egui::Color32::from_rgb(245, 158, 11)));
                                let others = if r.all_blockers.len() > 1 {
                                    r.all_blockers[1..].join(", ")
                                } else {
                                    "-".to_string()
                                };
                                ui.label(others);
                                ui.end_row();
                            }
                        });
                    }
                });
            });
        }
        // ====================================================================
        // TAB 3: ENERGY & COST SAVINGS DASHBOARD VIEW
        // ====================================================================
        else if s.active_tab == ActiveTab::Energy {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    icon_heading(
                        ui,
                        UiIcon::Coin,
                        egui::Color32::from_rgb(56, 189, 248),
                        "Energy & Cost Savings Dashboard",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(
                            ui,
                            UiIcon::Bolt,
                            egui::Color32::WHITE,
                            "Watt Calculator & AI Estimator",
                            Some(egui::Color32::from_rgb(37, 99, 235)),
                        )
                        .clicked()
                        {
                            s.show_watt_calc_modal = true;
                        }
                    });
                });
                ui.label(egui::RichText::new("Track electricity and money saved when Agent Power Guard puts your monitor and workstation into deep standby.").weak().size(12.0));
                ui.separator();

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

                let clean_zero = |v: f64| -> f64 {
                    if v <= 0.0 || v.abs() < 0.0005 { 0.0 } else { v }
                };
                let total_kwh = clean_zero(filtered_records.iter().map(|r| r.kwh_saved).sum());
                let total_cost = clean_zero(filtered_records.iter().map(|r| r.cost_saved).sum());
                let total_secs: u64 = filtered_records.iter().map(|r| r.duration_secs).sum();
                let co2_factor = get_carbon_factor_kg(&s.config);
                let total_co2_kg = clean_zero(total_kwh * co2_factor);

                // 1. Metric Summary Cards (4 even columns)
                let cur = s.config.currency_symbol.clone();
                ui.columns(4, |cols| {
                    let stat_card = |ui: &mut egui::Ui, title: &str, val: String, color: egui::Color32| {
                        app_card_frame(ui).show(ui, |ui| {
                            ui.label(egui::RichText::new(title).size(11.5).weak());
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new(val).size(20.0).strong().color(color));
                        });
                    };
                    stat_card(&mut cols[0], "Money Saved", format!("{}{:.2}", cur, total_cost), egui::Color32::from_rgb(16, 185, 129));
                    stat_card(&mut cols[1], "Electricity Saved", format!("{:.1} kWh", total_kwh), egui::Color32::from_rgb(59, 130, 246));
                    stat_card(&mut cols[2], "Standby Hours", format!("{:.1} hrs", total_secs as f64 / 3600.0), egui::Color32::from_rgb(245, 158, 11));
                    stat_card(&mut cols[3], "CO₂ Avoided", format!("{:.1} kg", total_co2_kg), egui::Color32::from_rgb(168, 85, 247));
                });

                ui.add_space(6.0);

                // Daily Power Projection Strip
                let proj = compute_power_projection(
                    s.config.active_watts,
                    s.config.monitor_watts,
                    s.config.sleep_watts,
                    s.config.electricity_rate_kwh,
                    co2_factor,
                    s.watt_calc.daily_active_hours,
                    s.watt_calc.daily_idle_sleep_hours,
                );
                let cur_proj = s.config.currency_symbol.clone();
                app_subcard_frame(ui).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        draw_icon_inline(ui, 14.0, egui::Color32::from_rgb(52, 211, 153), UiIcon::Bolt);
                        ui.label(egui::RichText::new(format!(
                            "Power Profile: Active {:.0}W  |  Display Off {:.0}W  |  Sleep {:.1}W",
                            proj.active_watts, proj.display_off_watts, proj.sleep_watts
                        )).strong().size(12.0));
                        ui.separator();
                        ui.label(egui::RichText::new(format!(
                            "Daily Cost ({:.0}h active / {:.0}h sleep): {}{:.2}/day ({:.2} kWh)",
                            s.watt_calc.daily_active_hours,
                            s.watt_calc.daily_idle_sleep_hours,
                            cur_proj,
                            proj.daily_guard_cost,
                            proj.daily_with_guard_kwh
                        )).size(12.0));
                        ui.separator();
                        ui.label(egui::RichText::new(format!(
                            "Est. Savings: {}{:.2}/day · {}{:.1}/mo · {}{:.0}/yr ({:.0} kWh, {:.0} kg CO₂/yr)",
                            cur_proj,
                            proj.daily_saved_cost,
                            cur_proj,
                            proj.monthly_saved_cost,
                            cur_proj,
                            proj.yearly_saved_cost,
                            proj.yearly_saved_kwh,
                            proj.yearly_co2_kg
                        )).strong().color(egui::Color32::from_rgb(52, 211, 153)).size(12.0));
                    });
                });

                ui.add_space(6.0);

                // 2. Settings Card (Power Draw, Tariff, 26+ Currencies & Custom Symbol, Carbon Region)
                app_card_frame(ui).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui.checkbox(&mut s.config.record_energy_data, egui::RichText::new("Record Energy Data").strong()).changed() {
                            s.config_dirty = true;
                        }
                        ui.separator();
                        ui.label("Active Power:");
                        if ui.add(egui::DragValue::new(&mut s.config.active_watts).suffix(" W").range(10.0..=2000.0)).changed() {
                            s.config_dirty = true;
                        }
                        ui.label("Monitor Power:");
                        if ui.add(egui::DragValue::new(&mut s.config.monitor_watts).suffix(" W").range(0.0..=500.0)).changed() {
                            s.config_dirty = true;
                        }
                        ui.label("Sleep Power:");
                        if ui.add(egui::DragValue::new(&mut s.config.sleep_watts).suffix(" W").range(0.5..=100.0).speed(0.5)).changed() {
                            s.config_dirty = true;
                        }
                        ui.label("Electricity Rate:");
                        let cur_str = s.config.currency_symbol.clone();
                        if ui.add(egui::DragValue::new(&mut s.config.electricity_rate_kwh).prefix(&cur_str).suffix("/kWh").range(0.01..=50000.0).speed(0.1)).changed() {
                            s.config_dirty = true;
                        }
                    });

                    ui.add_space(4.0);

                    ui.horizontal_wrapped(|ui| {
                        ui.label("Currency:");
                        let selected_label = MAJOR_CURRENCIES
                            .iter()
                            .find(|c| c.symbol == s.config.currency_symbol)
                            .map(|c| c.label.to_string())
                            .unwrap_or_else(|| format!("Custom ({})", s.config.currency_symbol));
                        egui::ComboBox::from_id_salt("currency_combo")
                            .selected_text(selected_label)
                            .show_ui(ui, |ui| {
                                for c in MAJOR_CURRENCIES {
                                    if ui.selectable_label(s.config.currency_symbol == c.symbol, c.label).clicked() {
                                        s.config.currency_symbol = c.symbol.to_string();
                                        s.custom_currency_input = c.symbol.to_string();
                                        s.config_dirty = true;
                                    }
                                }
                            });
                        for &sym in &["₹", "$", "€", "£", "¥", "C$", "A$"] {
                            if ui.selectable_label(s.config.currency_symbol == sym, sym).clicked() {
                                s.config.currency_symbol = sym.to_string();
                                s.custom_currency_input = sym.to_string();
                                s.config_dirty = true;
                            }
                        }
                        ui.label("Custom:");
                        ui.add(egui::TextEdit::singleline(&mut s.custom_currency_input).desired_width(52.0));
                        if ui.small_button("Set").clicked() {
                            let cleaned = normalize_currency_input(&s.custom_currency_input);
                            if !cleaned.is_empty() {
                                s.config.currency_symbol = cleaned;
                                s.config_dirty = true;
                                s.set_toast("Custom currency applied.");
                            }
                        }
                        ui.separator();
                        ui.label("Grid Carbon:");
                        let regions = get_carbon_regions();
                        let current_region = s.config.carbon_region.clone();
                        egui::ComboBox::from_id_salt("carbon_region_combo")
                            .selected_text(&current_region)
                            .show_ui(ui, |ui| {
                                for reg in regions {
                                    if ui.selectable_label(s.config.carbon_region == reg.name, format!("{} ({:.0} g/kWh) - {}", reg.name, reg.g_per_kwh, reg.source)).clicked() {
                                        s.config.carbon_region = reg.name.to_string();
                                        s.config_dirty = true;
                                    }
                                }
                            });
                        if s.config.carbon_region == "Custom" {
                            if ui.add(egui::DragValue::new(&mut s.config.custom_carbon_g_kwh).range(10.0..=1200.0).suffix(" g/kWh")).changed() {
                                s.config_dirty = true;
                            }
                        }
                    });
                });

                ui.add_space(6.0);

                // 3. Storage Location
                let storage_path = get_energy_history_path();
                let storage_str = storage_path.to_string_lossy().to_string();
                app_subcard_frame(ui).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        draw_icon_inline(ui, 13.0, egui::Color32::LIGHT_BLUE, UiIcon::Folder);
                        ui.label(egui::RichText::new("Data File:").strong().size(11.5));
                        ui.label(egui::RichText::new(&storage_str).monospace().color(egui::Color32::LIGHT_BLUE).size(11.0));
                        if icon_button(ui, UiIcon::Folder, egui::Color32::from_rgb(245, 158, 11), "Open Folder", None).clicked() {
                            if let Some(parent) = storage_path.parent() {
                                #[cfg(windows)]
                                let _ = Command::new("explorer").arg(parent).status();
                                #[cfg(target_os = "linux")]
                                let _ = Command::new("xdg-open").arg(parent).status();
                                #[cfg(target_os = "macos")]
                                let _ = Command::new("open").arg(parent).status();
                            }
                        }
                        if icon_button(ui, UiIcon::Copy, egui::Color32::from_rgb(147, 197, 253), "Copy Path", None).clicked() {
                            copy_text_reliable(ui.ctx(), &storage_str);
                            s.set_toast("Path copied to clipboard.");
                        }
                    });
                });

                ui.add_space(6.0);

                // 4. Filter Buttons & Delete Actions (wrapped cleanly so Delete buttons never clip!)
                ui.horizontal_wrapped(|ui| {
                    ui.label("Filter Period:");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::All, "All Time");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::Today, "Today");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::Last7Days, "Last 7 Days");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::ThisMonth, "This Month");
                    ui.selectable_value(&mut s.energy_filter, EnergyFilter::ThisYear, "This Year");

                    ui.separator();

                    if s.energy_filter != EnergyFilter::All {
                        if icon_button(ui, UiIcon::Trash, egui::Color32::from_rgb(248, 113, 113), "Delete Selected Period", None).clicked() {
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

                    if icon_button(
                        ui,
                        UiIcon::Trash,
                        egui::Color32::WHITE,
                        "Delete All Data",
                        Some(egui::Color32::from_rgb(185, 28, 28)),
                    )
                    .clicked()
                    {
                        s.energy_history.clear();
                        s.save_energy_history();
                        s.set_toast("All energy history deleted.");
                    }
                });

                ui.separator();

                // 5. History Table
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if filtered_records.is_empty() {
                        ui.add_space(20.0);
                        ui.label(egui::RichText::new("No energy events recorded for this period yet. Leave the guard running to automatically accumulate power savings!").weak());
                    } else {
                        let table_cur = s.config.currency_symbol.clone();
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
                                    let tag = if record.event_type == "sleep" { "Deep Sleep" } else { "Monitor Standby" };
                                    ui.label(tag);
                                    ui.label(format_time_compact(record.duration_secs as u32));
                                    ui.label(format!("{:.0} W", record.watts_saved_avg));
                                    ui.label(format!("{:.3} kWh", record.kwh_saved));
                                    ui.label(format!("{}{:.2}", table_cur, record.cost_saved));
                                    ui.end_row();
                                }
                            });
                    }
                });
            });
        }
        // ====================================================================
        // TAB 4: 120-SECOND AUTOMATED TEST & ISSUE REPORTER
        // ====================================================================
        else if s.active_tab == ActiveTab::Diagnostic {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        icon_heading(
                            ui,
                            UiIcon::Activity,
                            egui::Color32::from_rgb(244, 63, 94),
                            "120s Automated Diagnostic & Issue Reporter",
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if s.diag_session.active {
                                if icon_button(
                                    ui,
                                    UiIcon::Trash,
                                    egui::Color32::from_rgb(248, 113, 113),
                                    "Cancel Test",
                                    None,
                                )
                                .clicked()
                                {
                                    s.diag_session.cancel("User cancelled test");
                                    s.set_toast("Diagnostic cancelled.");
                                }
                            } else if s.diag_session.phase == DiagnosticPhase::Completed {
                                if icon_button(
                                    ui,
                                    UiIcon::Refresh,
                                    egui::Color32::from_rgb(56, 189, 248),
                                    "New Diagnostic",
                                    None,
                                )
                                .clicked()
                                {
                                    s.diag_session = DiagnosticSession::default();
                                }
                            }
                        });
                    });
                    ui.label(
                        egui::RichText::new(
                            "Autonomous 120-second end-to-end diagnostic suite that validates sleep countdowns, tests audio render pipelines, inspects OS DISPLAY keep-awake requests, and flags zombie driver locks.",
                        )
                        .weak()
                        .size(12.0),
                    );
                    ui.separator();

                    let dark = ui.visuals().dark_mode;
                    let now_epoch = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    // --------------------------------------------------------
                    // STATE 1: IDLE / CANCELLED (Pre-Flight & Start Test)
                    // --------------------------------------------------------
                    if !s.diag_session.active && s.diag_session.phase != DiagnosticPhase::Completed {
                        app_card_frame(ui).show(ui, |ui| {
                            ui.set_width((ui.available_width() - 4.0).max(280.0));
                            icon_card_title(
                                ui,
                                UiIcon::Sliders,
                                egui::Color32::from_rgb(56, 189, 248),
                                "Automated 120-Second Test Routine",
                            );
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(
                                    "When started, the diagnostic engine executes 5 synchronized test phases to verify whether your system behaves nominally or suffers from premature sleep or sleep refusal:",
                                )
                                .size(12.0),
                            );
                            ui.add_space(6.0);

                            let phases_info = [
                                ("1. Arming & Baseline Idle (0-25s)", "Verifies user input is idle, monitors baseline CPU/GPU/Disk, and checks sleep countdown continuity."),
                                ("2. Automated Media Playback (25-55s)", "Automatically triggers a test video in your browser. Verifies whether CoreAudio/WASAPI render hooks and OS DISPLAY requests activate. (Diagnoses why laptops sleep while watching movies!)."),
                                ("3. Post-Media Release & Grace (55-85s)", "Tests audio stop/close. Verifies drivers release sound sessions cleanly without leaving zombie sleep locks."),
                                ("4. Hardware Blocker Deep-Dive (85-110s)", "Isolates physical drive activity from browser IPC cache noise, and checks powercfg driver assertions."),
                                ("5. Synthesis & Report (110-120s)", "Synthesizes sensor samples into root cause diagnosis and outputs a full GitHub-ready Markdown report."),
                            ];

                            for (title, desc) in phases_info {
                                ui.horizontal(|ui| {
                                    draw_icon_inline(ui, 12.0, egui::Color32::from_rgb(52, 211, 153), UiIcon::Check);
                                    ui.label(egui::RichText::new(title).strong().size(12.0));
                                });
                                ui.label(egui::RichText::new(desc).weak().size(11.5));
                                ui.add_space(3.0);
                            }

                            ui.add_space(10.0);

                            // Warning Banner: Hands off keyboard & mouse!
                            let warn_fill = if dark {
                                egui::Color32::from_rgb(69, 26, 3)
                            } else {
                                egui::Color32::from_rgb(254, 243, 199)
                            };
                            let warn_stroke = if dark {
                                egui::Color32::from_rgb(180, 83, 9)
                            } else {
                                egui::Color32::from_rgb(245, 158, 11)
                            };
                            egui::Frame::new()
                                .fill(warn_fill)
                                .stroke(egui::Stroke::new(1.0, warn_stroke))
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::symmetric(12, 10))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        draw_icon_inline(ui, 18.0, egui::Color32::from_rgb(245, 158, 11), UiIcon::Warning);
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new("HANDS-OFF REQUIREMENT (120 SECONDS)")
                                                    .strong()
                                                    .size(12.5)
                                                    .color(if dark { egui::Color32::from_rgb(253, 230, 138) } else { egui::Color32::from_rgb(146, 64, 14) }),
                                            );
                                            ui.label(
                                                egui::RichText::new(
                                                    "Do NOT press any keys or move the mouse once the test begins. The watchdog requires uninterrupted human idle time to evaluate sensor accuracy and sleep thresholds.",
                                                )
                                                .size(11.5)
                                                .color(if dark { egui::Color32::from_rgb(254, 243, 199) } else { egui::Color32::from_rgb(120, 53, 15) }),
                                            );
                                        });
                                    });
                                });

                            ui.add_space(14.0);

                            ui.vertical_centered(|ui| {
                                let start_btn = ui.add_sized(
                                    [320.0, 44.0],
                                    egui::Button::new(
                                        egui::RichText::new("Start 120s Automated Diagnostic")
                                            .strong()
                                            .size(15.0)
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(225, 29, 72))
                                    .corner_radius(10.0),
                                );
                                if start_btn.clicked() {
                                    s.diag_session.start(now_epoch);
                                    s.set_toast("120s Diagnostic started: Please release keyboard and mouse!");
                                }
                            });
                            ui.add_space(8.0);
                        });
                    }

                    // --------------------------------------------------------
                    // STATE 2: ACTIVE DIAGNOSTIC (Live 120s Progress & Sensors)
                    // --------------------------------------------------------
                    else if s.diag_session.active {
                        let sec = s.diag_session.elapsed_secs;
                        let phase = s.diag_session.phase;
                        let pct = (sec as f32 / 120.0).clamp(0.0, 1.0);

                        // Urgent Hands-Off Banner
                        let alert_fill = if dark {
                            egui::Color32::from_rgb(76, 5, 25)
                        } else {
                            egui::Color32::from_rgb(255, 228, 230)
                        };
                        let alert_stroke = if dark {
                            egui::Color32::from_rgb(225, 29, 72)
                        } else {
                            egui::Color32::from_rgb(244, 63, 94)
                        };
                        egui::Frame::new()
                            .fill(alert_fill)
                            .stroke(egui::Stroke::new(1.5, alert_stroke))
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    draw_icon_inline(ui, 20.0, egui::Color32::from_rgb(244, 63, 94), UiIcon::Activity);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("DIAGNOSTIC IN PROGRESS: {}s / 120s", sec))
                                                .strong()
                                                .size(14.0)
                                                .color(if dark { egui::Color32::WHITE } else { egui::Color32::from_rgb(159, 18, 57) }),
                                        );
                                        ui.label(
                                            egui::RichText::new("PLEASE DO NOT TOUCH MOUSE OR KEYBOARD: Monitoring sleep countdown, media hooks, and driver assertions...")
                                                .size(12.0)
                                                .color(if dark { egui::Color32::from_rgb(254, 205, 211) } else { egui::Color32::from_rgb(136, 19, 55) }),
                                        );
                                    });
                                });
                            });

                        ui.add_space(8.0);

                        // Progress Bar
                        ui.add(
                            egui::ProgressBar::new(pct)
                                .text(format!("{}s / 120s — {}", sec, phase.label()))
                                .animate(true),
                        );

                        ui.add_space(8.0);

                        // Current Phase Description Card
                        app_card_frame(ui).show(ui, |ui| {
                            ui.set_width((ui.available_width() - 4.0).max(280.0));
                            ui.horizontal(|ui| {
                                draw_icon_inline(ui, 14.0, egui::Color32::from_rgb(56, 189, 248), UiIcon::Clock);
                                ui.label(egui::RichText::new(phase.label()).strong().size(13.0));
                            });
                            ui.label(egui::RichText::new(phase.description()).size(12.0));
                            if s.diag_session.user_interrupted {
                                ui.label(
                                    egui::RichText::new("Notice: Human input detected during test. Idle countdown was reset.")
                                        .color(egui::Color32::from_rgb(245, 158, 11))
                                        .size(11.5),
                                );
                            }
                        });

                        ui.add_space(8.0);

                        // Live Multi-Sensor Readout Cards
                        ui.label(egui::RichText::new("Real-Time Diagnostic Sensors:").strong().size(12.5));
                        ui.add_space(4.0);

                        let card_w = ((ui.available_width() - 24.0) / 4.0).max(140.0);

                        let last_smp = s.diag_session.samples.last();
                        let live_idle = last_smp.map(|sm| sm.idle_secs).unwrap_or(s.live_idle_secs);
                        let live_cd = last_smp.and_then(|sm| sm.countdown_secs).unwrap_or(s.countdown_secs);
                        let live_aud_act = last_smp.map(|sm| sm.audio_render_active).unwrap_or(s.media_lock_active);
                        let live_peak = last_smp.map(|sm| sm.audio_peak).unwrap_or(s.mic_capture_peak);
                        let live_disp = last_smp.map(|sm| sm.display_power_request).unwrap_or(!s.cond9_power_requests.is_passing);
                        let live_sys = last_smp.map(|sm| sm.system_power_request).unwrap_or(false);
                        let intent_str = s.intent.display_label();

                        ui.horizontal_wrapped(|ui| {
                            // Card 1: Input Idle
                            egui::Frame::new()
                                .fill(if dark { egui::Color32::from_rgb(20, 27, 38) } else { egui::Color32::from_rgb(241, 245, 249) })
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.set_width(card_w);
                                    ui.label(egui::RichText::new("Input Idle").weak().size(11.0));
                                    let idle_txt = format!("{}s", live_idle);
                                    ui.label(egui::RichText::new(idle_txt).strong().size(16.0).color(egui::Color32::from_rgb(52, 211, 153)));
                                    ui.label(egui::RichText::new("Threshold: 180s").weak().size(10.5));
                                });

                            // Card 2: Sleep Countdown
                            egui::Frame::new()
                                .fill(if dark { egui::Color32::from_rgb(20, 27, 38) } else { egui::Color32::from_rgb(241, 245, 249) })
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.set_width(card_w);
                                    ui.label(egui::RichText::new("Sleep Countdown").weak().size(11.0));
                                    let cd_txt = if s.all_conditions_pass {
                                        format!("{}s", live_cd)
                                    } else {
                                        "HOLD".to_string()
                                    };
                                    let cd_col = if s.all_conditions_pass { egui::Color32::from_rgb(56, 189, 248) } else { egui::Color32::from_rgb(245, 158, 11) };
                                    ui.label(egui::RichText::new(cd_txt).strong().size(16.0).color(cd_col));
                                    ui.label(egui::RichText::new(&intent_str).weak().size(10.5));
                                });

                            // Card 3: Audio Render Pipeline
                            egui::Frame::new()
                                .fill(if dark { egui::Color32::from_rgb(20, 27, 38) } else { egui::Color32::from_rgb(241, 245, 249) })
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.set_width(card_w);
                                    ui.label(egui::RichText::new("Audio Render").weak().size(11.0));
                                    let aud_txt = if live_aud_act { "Active" } else { "Idle / Muted" };
                                    let aud_col = if live_aud_act { egui::Color32::from_rgb(52, 211, 153) } else { egui::Color32::from_rgb(148, 163, 184) };
                                    ui.label(egui::RichText::new(aud_txt).strong().size(16.0).color(aud_col));
                                    let peak_pct = (live_peak * 100.0).round();
                                    ui.label(egui::RichText::new(format!("Peak: {:.0}%", peak_pct)).weak().size(10.5));
                                });

                            // Card 4: OS Power Requests
                            egui::Frame::new()
                                .fill(if dark { egui::Color32::from_rgb(20, 27, 38) } else { egui::Color32::from_rgb(241, 245, 249) })
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.set_width(card_w);
                                    ui.label(egui::RichText::new("OS Keep-Awake").weak().size(11.0));
                                    let req_txt = if live_disp { "DISPLAY: YES" } else { "DISPLAY: NO" };
                                    let req_col = if live_disp { egui::Color32::from_rgb(52, 211, 153) } else { egui::Color32::from_rgb(248, 113, 113) };
                                    ui.label(egui::RichText::new(req_txt).strong().size(15.0).color(req_col));
                                    let sys_txt = if live_sys { "SYSTEM: YES" } else { "SYSTEM: NO" };
                                    ui.label(egui::RichText::new(sys_txt).weak().size(10.5));
                                });
                        });

                        ui.add_space(10.0);

                        // Live Event Timeline Table (Last 10 samples)
                        app_card_frame(ui).show(ui, |ui| {
                            ui.set_width((ui.available_width() - 4.0).max(280.0));
                            icon_card_title(
                                ui,
                                UiIcon::History,
                                egui::Color32::from_rgb(168, 85, 247),
                                "Live Event Log Stream",
                            );
                            ui.add_space(4.0);

                            egui::ScrollArea::vertical()
                                .max_height(140.0)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for smp in s.diag_session.samples.iter().rev().take(10) {
                                        let note_display = if smp.note.is_empty() {
                                            format!("{}: Audio={}, CD={:?}", smp.phase.label(), smp.audio_render_active, smp.countdown_secs)
                                        } else {
                                            smp.note.clone()
                                        };
                                        ui.horizontal(|ui| {
                                            ui.label(egui::RichText::new(format!("[{:02}s]", smp.second)).monospace().weak());
                                            ui.label(egui::RichText::new(note_display).size(11.5));
                                        });
                                    }
                                });
                        });
                    }

                    // --------------------------------------------------------
                    // STATE 3: COMPLETED (Report & GitHub 1-Click Submission)
                    // --------------------------------------------------------
                    else if s.diag_session.phase == DiagnosticPhase::Completed {
                        // Hero Completion Banner
                        let comp_fill = if dark {
                            egui::Color32::from_rgb(6, 78, 59)
                        } else {
                            egui::Color32::from_rgb(209, 250, 229)
                        };
                        let comp_stroke = if dark {
                            egui::Color32::from_rgb(16, 185, 129)
                        } else {
                            egui::Color32::from_rgb(5, 150, 105)
                        };
                        egui::Frame::new()
                            .fill(comp_fill)
                            .stroke(egui::Stroke::new(1.0, comp_stroke))
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    draw_icon_inline(ui, 20.0, egui::Color32::from_rgb(16, 185, 129), UiIcon::Check);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new("120S AUTOMATED DIAGNOSTIC COMPLETE")
                                                .strong()
                                                .size(13.5)
                                                .color(if dark { egui::Color32::WHITE } else { egui::Color32::from_rgb(6, 95, 70) }),
                                        );
                                        ui.label(
                                            egui::RichText::new("Sensor timeline captured, root cause analysis synthesized, and GitHub report generated.")
                                                .size(11.5)
                                                .color(if dark { egui::Color32::from_rgb(167, 243, 208) } else { egui::Color32::from_rgb(4, 120, 87) }),
                                        );
                                    });
                                });
                            });

                        ui.add_space(8.0);

                        // Findings & Diagnosis Cards
                        app_card_frame(ui).show(ui, |ui| {
                            ui.set_width((ui.available_width() - 4.0).max(280.0));
                            icon_card_title(
                                ui,
                                UiIcon::Shield,
                                egui::Color32::from_rgb(56, 189, 248),
                                "Diagnostic Findings & Root Cause Analysis",
                            );
                            ui.add_space(6.0);

                            for finding in &s.diag_session.findings {
                                let (badge_col, border_col) = match finding.severity {
                                    DiagSeverity::Critical => (egui::Color32::from_rgb(244, 63, 94), egui::Color32::from_rgb(159, 18, 57)),
                                    DiagSeverity::Warning => (egui::Color32::from_rgb(245, 158, 11), egui::Color32::from_rgb(180, 83, 9)),
                                    DiagSeverity::Info => (egui::Color32::from_rgb(52, 211, 153), egui::Color32::from_rgb(5, 150, 105)),
                                };

                                egui::Frame::new()
                                    .fill(if dark { egui::Color32::from_rgb(20, 27, 38) } else { egui::Color32::from_rgb(248, 250, 252) })
                                    .stroke(egui::Stroke::new(1.0, border_col))
                                    .corner_radius(8.0)
                                    .inner_margin(egui::Margin::symmetric(10, 8))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(egui::RichText::new(finding.severity.badge()).strong().color(badge_col));
                                            ui.label(egui::RichText::new(&finding.title).strong().size(12.5));
                                        });
                                        ui.label(egui::RichText::new(&finding.description).size(11.5));
                                        ui.add_space(2.0);
                                        ui.horizontal(|ui| {
                                            ui.label(egui::RichText::new("Recommendation:").strong().size(11.5).color(egui::Color32::from_rgb(56, 189, 248)));
                                            ui.label(egui::RichText::new(&finding.recommendation).size(11.5));
                                        });
                                    });
                                ui.add_space(4.0);
                            }
                        });

                        ui.add_space(8.0);

                        // Markdown Report Card & Action Buttons
                        app_card_frame(ui).show(ui, |ui| {
                            ui.set_width((ui.available_width() - 4.0).max(280.0));
                            icon_card_title(
                                ui,
                                UiIcon::Copy,
                                egui::Color32::from_rgb(168, 85, 247),
                                "Generated Diagnostic Report (Markdown)",
                            );
                            ui.add_space(4.0);

                            ui.horizontal_wrapped(|ui| {
                                if icon_button(
                                    ui,
                                    UiIcon::Copy,
                                    egui::Color32::WHITE,
                                    "Copy Diagnostic Report",
                                    Some(egui::Color32::from_rgb(37, 99, 235)),
                                )
                                .clicked()
                                {
                                    copy_text_reliable(ui.ctx(), &s.diag_session.generated_report);
                                    s.set_toast("Diagnostic report copied to clipboard!");
                                }

                                if icon_button(
                                    ui,
                                    UiIcon::Globe,
                                    egui::Color32::WHITE,
                                    "Report Issue on GitHub",
                                    Some(egui::Color32::from_rgb(16, 185, 129)),
                                )
                                .on_hover_text("Opens pre-filled GitHub issue in your browser (no API key required)")
                                .clicked()
                                {
                                    let url = build_github_issue_url(&s.diag_session.generated_report, None);
                                    open_browser_url(&url);
                                    s.set_toast("Opening GitHub issue creator in your browser...");
                                }

                                if icon_button(
                                    ui,
                                    UiIcon::Refresh,
                                    egui::Color32::from_rgb(148, 163, 184),
                                    "Run New Test",
                                    None,
                                )
                                .clicked()
                                {
                                    s.diag_session = DiagnosticSession::default();
                                }
                            });

                            ui.add_space(6.0);

                            egui::ScrollArea::vertical()
                                .max_height(260.0)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let mut report_clone = s.diag_session.generated_report.clone();
                                    ui.add(
                                        egui::TextEdit::multiline(&mut report_clone)
                                            .desired_width(f32::INFINITY)
                                            .font(egui::TextStyle::Monospace)
                                            .interactive(true),
                                    );
                                });
                        });
                    }

                    ui.add_space(20.0);
                });
            });
        }
        // ====================================================================
        // TAB 5: ALERTS, UPDATES & SOFTWARE MANAGEMENT VIEW
        // ====================================================================
        else if s.active_tab == ActiveTab::Notifications {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        icon_heading(
                            ui,
                            UiIcon::Bell,
                            egui::Color32::from_rgb(168, 85, 247),
                            "Alerts, Updates & Software Management",
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(
                                ui,
                                UiIcon::Bell,
                                egui::Color32::WHITE,
                                "Send Test Notification",
                                Some(egui::Color32::from_rgb(124, 58, 237)),
                            )
                            .clicked()
                            {
                                send_notification(&s.config, &NotificationEvent::TestMessage);
                                s.set_toast("Test notification dispatched across configured channels.");
                            }
                        });
                    });

                    ui.add_space(8.0);

                    // Card 1: Software Updates (In-Panel Auto-Updater — GitHub Public Releases API)
                    let state_arc = self.state.clone();
                    app_card_frame(ui).show(ui, |ui| {
                        icon_card_title(
                            ui,
                            UiIcon::Download,
                            egui::Color32::from_rgb(56, 189, 248),
                            "Software Updates (GitHub Public Releases)",
                        );
                        ui.add_space(4.0);
                        let latest_label = s
                            .update_latest_tag
                            .as_deref()
                            .unwrap_or("Not checked yet (click 'Check & List Releases')");
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new("Installed:").weak());
                            ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).strong().color(egui::Color32::from_rgb(96, 165, 250)));
                            ui.separator();
                            ui.label(egui::RichText::new("Latest on GitHub:").weak());
                            ui.label(egui::RichText::new(latest_label).strong().color(egui::Color32::from_rgb(52, 211, 153)));
                        });
                        ui.add_space(6.0);

                        let dl_ready_path = s.update_downloaded_path.clone();
                        let dl_ready_ver = s.update_downloaded_version.clone();
                        let dl_busy = s.update_download_in_progress;
                        let chk_busy = s.update_check_in_progress;
                        let upd_avail = s.update_available.clone();

                        ui.horizontal_wrapped(|ui| {
                            if chk_busy {
                                ui.label("Checking GitHub releases...");
                            } else if icon_button(
                                ui,
                                UiIcon::Search,
                                egui::Color32::WHITE,
                                "Check & List Releases",
                                Some(egui::Color32::from_rgb(37, 99, 235)),
                            )
                            .clicked()
                            {
                                trigger_update_check_locked(&mut s, state_arc.clone());
                            }

                            if let Some(ref p) = dl_ready_path {
                                let ver_lbl = dl_ready_ver.as_deref().unwrap_or("Update");
                                if icon_button(
                                    ui,
                                    UiIcon::Check,
                                    egui::Color32::WHITE,
                                    format!("Install {} & Restart", ver_lbl),
                                    Some(egui::Color32::from_rgb(16, 185, 129)),
                                )
                                .clicked()
                                {
                                    if let Err(err) = install_downloaded_update(p) {
                                        s.set_toast(err);
                                    }
                                }
                            } else if dl_busy {
                                ui.label("Downloading update in background...");
                            } else if let Some(ref new_ver) = upd_avail {
                                if icon_button(
                                    ui,
                                    UiIcon::Download,
                                    egui::Color32::WHITE,
                                    format!("Update Now ({})", new_ver),
                                    Some(egui::Color32::from_rgb(16, 185, 129)),
                                )
                                .clicked()
                                {
                                    trigger_update_download_locked(&mut s, state_arc.clone(), Some(new_ver.clone()));
                                }
                            }

                            if ui.checkbox(&mut s.config.check_for_updates, "Auto-check daily via GitHub API").changed() {
                                s.config_dirty = true;
                            }
                        });

                        if let Some(msg) = &s.update_check_status_msg {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(msg).color(egui::Color32::from_rgb(147, 197, 253)).size(12.0));
                        }
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Windows Defender note: Release binaries are unsigned. If downloading manually in Chrome/Edge, click Keep -> Show more -> Keep anyway (or More info -> Run anyway).")
                                .weak()
                                .size(11.0),
                        );

                        if !s.available_releases.is_empty() {
                            ui.add_space(6.0);
                            ui.collapsing(format!("All Published Releases ({} versions)", s.available_releases.len()), |ui| {
                                let latest_tag = s.update_latest_tag.clone();
                                let current_clean = env!("CARGO_PKG_VERSION");
                                let releases_snapshot = s.available_releases.clone();
                                let mut download_tag_req: Option<String> = None;

                                egui::ScrollArea::vertical().max_height(210.0).show(ui, |ui| {
                                    for rel in &releases_snapshot {
                                        let is_latest = latest_tag.as_deref() == Some(rel.tag.as_str());
                                        let is_installed = rel.tag.trim_start_matches('v') == current_clean;
                                        app_subcard_frame(ui).show(ui, |ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(egui::RichText::new(&rel.tag).strong());
                                                if is_latest {
                                                    ui.label(egui::RichText::new("[LATEST]").color(egui::Color32::from_rgb(52, 211, 153)).strong().size(11.0));
                                                }
                                                if is_installed {
                                                    ui.label(egui::RichText::new("[INSTALLED]").color(egui::Color32::from_rgb(96, 165, 250)).strong().size(11.0));
                                                }
                                                if !rel.published_at.is_empty() {
                                                    ui.label(egui::RichText::new(&rel.published_at).weak().size(11.0));
                                                }
                                                if !is_installed && !dl_busy && ui.small_button("Download & Install").clicked() {
                                                    download_tag_req = Some(rel.tag.clone());
                                                }
                                            });
                                            let first_line = rel
                                                .body
                                                .lines()
                                                .find(|l| !l.trim().is_empty() && !l.trim().starts_with("---"))
                                                .unwrap_or(&rel.name);
                                            ui.label(egui::RichText::new(first_line).weak().size(11.0));
                                        });
                                        ui.add_space(4.0);
                                    }
                                });

                                if let Some(tag_req) = download_tag_req {
                                    trigger_update_download_locked(&mut s, state_arc.clone(), Some(tag_req));
                                }
                            });
                        }
                    });

                    ui.add_space(10.0);

                    // Card 2: Outbound Notification Channels (aligned 2-column grid)
                    app_card_frame(ui).show(ui, |ui| {
                        icon_card_title(
                            ui,
                            UiIcon::Bell,
                            egui::Color32::from_rgb(168, 85, 247),
                            "Outbound Notification Channels",
                        );
                        ui.add_space(6.0);
                        egui::Grid::new("notify_channels_grid").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                            ui.label(egui::RichText::new("ntfy URL:").strong());
                            if ui.add(egui::TextEdit::singleline(&mut s.config.notify_ntfy_url).desired_width(420.0).hint_text("https://ntfy.sh/your_private_topic")).changed() {
                                s.config_dirty = true;
                            }
                            ui.end_row();

                            ui.label("ntfy Token (optional):");
                            if ui.add(egui::TextEdit::singleline(&mut s.config.notify_ntfy_token).desired_width(420.0).password(true).hint_text("Bearer token if topic is protected")).changed() {
                                s.config_dirty = true;
                            }
                            ui.end_row();

                            ui.label(egui::RichText::new("Telegram Bot Token:").strong());
                            if ui.add(egui::TextEdit::singleline(&mut s.config.notify_telegram_token).desired_width(420.0).password(true).hint_text("123456789:ABCdefGhIJKlmNoPQRsTUVwxyZ")).changed() {
                                s.config_dirty = true;
                            }
                            ui.end_row();

                            ui.label(egui::RichText::new("Telegram Chat ID:").strong());
                            if ui.add(egui::TextEdit::singleline(&mut s.config.notify_telegram_chat_id).desired_width(420.0).hint_text("-100123456789 or user id")).changed() {
                                s.config_dirty = true;
                            }
                            ui.end_row();

                            ui.label(egui::RichText::new("Webhook URL (POST):").strong());
                            if ui.add(egui::TextEdit::singleline(&mut s.config.notify_webhook_url).desired_width(420.0).hint_text("https://discord.com/api/webhooks/... or custom server")).changed() {
                                s.config_dirty = true;
                            }
                            ui.end_row();
                        });
                    });

                    ui.add_space(10.0);

                    // Card 3: Event Subscriptions
                    app_card_frame(ui).show(ui, |ui| {
                        icon_card_title(
                            ui,
                            UiIcon::Bolt,
                            egui::Color32::from_rgb(245, 158, 11),
                            "Trigger Events",
                        );
                        ui.add_space(6.0);

                        if ui.checkbox(&mut s.config.notify_on_sleep_imminent, "Sleep Imminent (Grace period countdown reaches 30 seconds)").changed() {
                            s.config_dirty = true;
                        }

                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.notify_on_blocked_long, "Machine Blocked for longer than:").changed() {
                                s.config_dirty = true;
                            }
                            if ui.add(egui::DragValue::new(&mut s.config.notify_blocked_long_hours).range(1..=12)).changed() {
                                s.config_dirty = true;
                            }
                            ui.label("hours");
                        });

                        if ui.checkbox(&mut s.config.notify_on_lease_ended, "Agent Lease Concluded / Expired").changed() {
                            s.config_dirty = true;
                        }

                        if ui.checkbox(&mut s.config.notify_on_pc_woke, "System Resumed from Sleep").changed() {
                            s.config_dirty = true;
                        }

                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut s.config.notify_on_guard_paused_long, "Master Guard Disabled for longer than:").changed() {
                                s.config_dirty = true;
                            }
                            if ui.add(egui::DragValue::new(&mut s.config.notify_guard_paused_hours).range(1..=24)).changed() {
                                s.config_dirty = true;
                            }
                            ui.label("hours");
                        });
                    });

                    ui.add_space(10.0);

                    // Card 4: Privacy Guarantee
                    app_subcard_frame(ui).show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            draw_icon_inline(ui, 14.0, egui::Color32::from_rgb(52, 211, 153), UiIcon::Lock);
                            ui.label(egui::RichText::new("Privacy Guarantee: No telemetry is sent. Channels start blank and secrets stay strictly local on your machine.").weak().size(12.0));
                        });
                    });

                    ui.add_space(10.0);

                    // Card 5: Software Management & Uninstall
                    app_card_frame(ui).show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            icon_card_title(
                                ui,
                                UiIcon::Trash,
                                egui::Color32::from_rgb(248, 113, 113),
                                "Software Management",
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if icon_button(
                                    ui,
                                    UiIcon::Trash,
                                    egui::Color32::WHITE,
                                    "Uninstall Software...",
                                    Some(egui::Color32::from_rgb(185, 28, 28)),
                                )
                                .clicked()
                                {
                                    s.uninstall_status_msg = None;
                                    s.show_uninstall_modal = true;
                                }
                            });
                        });
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Cleanly remove startup tasks, unregister background service, optionally wipe saved config/history, and launch the uninstaller.").weak().size(12.0));
                    });
                });
            });
        }
        // ====================================================================
        // TAB 5: WEB & MOBILE REMOTE VIEW
        // ====================================================================
        else if s.active_tab == ActiveTab::Remote {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(4.0);
                    icon_heading(
                        ui,
                        UiIcon::Globe,
                        egui::Color32::from_rgb(96, 165, 250),
                        "Web & Mobile Remote Dashboard",
                    );
                    ui.label(egui::RichText::new("Control Agent Power Guard from your phone, tablet, or another browser on your local Wi-Fi.").weak().size(12.0));
                    ui.separator();

                    // Card 1: LAN Access Toggle & API Security
                    app_card_frame(ui).show(ui, |ui| {
                        icon_card_title(
                            ui,
                            UiIcon::Lock,
                            egui::Color32::from_rgb(52, 211, 153),
                            "API & Network Security",
                        );
                        ui.add_space(4.0);

                        if ui.checkbox(&mut s.config.allow_lan_access, egui::RichText::new("Allow access from local network (LAN)").strong()).changed() {
                            s.config_dirty = true;
                        }
                        ui.label(egui::RichText::new("When disabled (default), only this computer (127.0.0.1) can reach the API and dashboard. When enabled, other devices on your Wi-Fi/LAN can connect.").weak().size(12.0));

                        ui.add_space(8.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new("API Token:").strong());
                            let masked_token = format!("{}...{}", &s.api_token[..8], &s.api_token[56..]);
                            ui.label(egui::RichText::new(&masked_token).monospace().color(egui::Color32::from_rgb(245, 158, 11)));

                            if icon_button(ui, UiIcon::Copy, egui::Color32::from_rgb(147, 197, 253), "Copy Token", None).clicked() {
                                copy_text_reliable(ui.ctx(), &s.api_token);
                                s.set_toast("API token copied to clipboard.");
                            }

                            if icon_button(ui, UiIcon::Refresh, egui::Color32::from_rgb(245, 158, 11), "Regenerate Token", None).clicked() {
                                let new_token = regenerate_api_token();
                                s.api_token = new_token;
                                s.set_toast("API token regenerated successfully.");
                            }
                        });
                    });

                    ui.add_space(10.0);

                    let local_url = format!("http://{}:{}/#t={}", s.local_ip, s.config.web_server_port, s.api_token);

                    // Card 2: Mobile Remote URL & QR Pairing
                    app_card_frame(ui).show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            icon_card_title(
                                ui,
                                UiIcon::Globe,
                                egui::Color32::from_rgb(52, 211, 153),
                                "Remote Mobile URL",
                            );
                            ui.label(egui::RichText::new(&local_url).strong().size(13.0).color(egui::Color32::from_rgb(52, 211, 153)));

                            if icon_button(ui, UiIcon::Globe, egui::Color32::WHITE, "Open in Browser", Some(egui::Color32::from_rgb(37, 99, 235))).clicked() {
                                #[cfg(windows)]
                                let _ = Command::new("cmd").args(["/c", "start", &local_url]).creation_flags(CREATE_NO_WINDOW).status();
                                #[cfg(target_os = "linux")]
                                let _ = Command::new("xdg-open").arg(&local_url).status();
                                #[cfg(target_os = "macos")]
                                let _ = Command::new("open").arg(&local_url).status();
                            }

                            if icon_button(ui, UiIcon::Copy, egui::Color32::from_rgb(147, 197, 253), "Copy Link with Token", None).clicked() {
                                copy_text_reliable(ui.ctx(), &local_url);
                                s.set_toast("Mobile remote link with token copied to clipboard.");
                            }
                        });

                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Open this URL on your phone while connected to the same Wi-Fi. The token fragment auto-authenticates your phone so you can view live condition graphs, monitor countdown, put the PC to sleep immediately, postpone sleep, or grant/release agent leases.").weak());

                        // Phase 4.2: QR Code for Pairing
                        if let Ok(qr) = crate::qr::QrCode::encode_text(&local_url, crate::qr::QrEcLevel::L) {
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let qr_pixels = 160.0;
                                let (rect, _response) = ui.allocate_exact_size(egui::Vec2::splat(qr_pixels), egui::Sense::hover());
                                let painter = ui.painter_at(rect);
                                painter.rect_filled(rect, 4.0, egui::Color32::WHITE);
                                let cell = qr_pixels / qr.size as f32;
                                for y in 0..qr.size {
                                    for x in 0..qr.size {
                                        if qr.get_module(x, y) {
                                            let min = rect.min + egui::vec2(x as f32 * cell, y as f32 * cell);
                                            let cell_rect = egui::Rect::from_min_size(min, egui::Vec2::splat(cell));
                                            painter.rect_filled(cell_rect, 0.0, egui::Color32::BLACK);
                                        }
                                    }
                                }

                                ui.vertical(|ui| {
                                    ui.add_space(4.0);
                                    icon_card_title(
                                        ui,
                                        UiIcon::Search,
                                        egui::Color32::from_rgb(96, 165, 250),
                                        "Scan with Phone Camera to Pair",
                                    );
                                    ui.label(egui::RichText::new("Scan this QR code with your camera app to open the remote dashboard with your authentication token pre-configured.").weak().size(12.0));
                                    ui.add_space(8.0);
                                    ui.horizontal(|ui| {
                                        draw_icon_inline(ui, 13.0, egui::Color32::from_rgb(245, 158, 11), UiIcon::Warning);
                                        ui.label(egui::RichText::new("Security Warning:").strong().color(egui::Color32::from_rgb(245, 158, 11)));
                                    });
                                    ui.label(egui::RichText::new("This QR code grants full control over your PC's power management and embeds your secret API token. Do not share, publish, or take public screenshots of this code.").weak().size(11.0));
                                });
                            });
                        }
                    });

                    ui.add_space(10.0);

                    // Card 3: AI Agent Profiles One-Click Selector
                    app_card_frame(ui).show(ui, |ui| {
                        icon_card_title(
                            ui,
                            UiIcon::Cpu,
                            egui::Color32::from_rgb(56, 189, 248),
                            "AI Agent Profiles (One-Click Presets)",
                        );
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Apply tailored process watchlists and thresholds for specific AI coding tools and workloads:").weak().size(12.0));
                        ui.add_space(8.0);

                        let profiles = get_built_in_profiles();
                        ui.horizontal_wrapped(|ui| {
                            for p in &profiles {
                                if icon_button(ui, UiIcon::Bolt, egui::Color32::from_rgb(245, 158, 11), &p.name, None).clicked() {
                                    s.config.agent_keywords = p.keywords.clone();
                                    s.config.agent_cpu_threshold = p.agent_cpu_threshold;
                                    s.config.gpu_threshold_pct = p.gpu_threshold_pct;
                                    s.config_dirty = true;
                                    s.set_toast(format!("Applied profile: {} ({})", p.name, p.keywords));
                                }
                            }
                        });
                    });

                    ui.add_space(10.0);

                    // Card 4: Wake-on-LAN Remote Turn-On
                    app_subcard_frame(ui).show(ui, |ui| {
                        icon_card_title(
                            ui,
                            UiIcon::Bolt,
                            egui::Color32::from_rgb(245, 158, 11),
                            "Turning PC Back On (Wake-on-LAN)",
                        );
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("When your PC enters sleep, the CPU is suspended and the web server pauses. To wake it remotely, send a standard Wake-on-LAN (WoL) magic packet from your phone, router, or the CLI relay.").weak());
                        ui.add_space(8.0);

                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new("Target PC MAC Address:").strong());
                            ui.label(egui::RichText::new(&s.mac_address).monospace().color(egui::Color32::from_rgb(96, 165, 250)));
                            if icon_button(ui, UiIcon::Copy, egui::Color32::from_rgb(147, 197, 253), "Copy MAC", None).clicked() {
                                copy_text_reliable(ui.ctx(), &s.mac_address);
                                s.set_toast("MAC address copied.");
                            }

                            ui.separator();
                            ui.label(egui::RichText::new("Port:").strong());
                            ui.label(egui::RichText::new("9 (Standard WoL UDP)").monospace());
                        });
                    });

                    ui.add_space(10.0);

                    // Card 5: HTTP API Endpoints Reference
                    app_card_frame(ui).show(ui, |ui| {
                        icon_card_title(
                            ui,
                            UiIcon::Sliders,
                            egui::Color32::from_rgb(147, 197, 253),
                            "Lightweight Agent Heartbeat API Reference",
                        );
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
            });
        }
    }
}

// ----------------------------------------------------------------------------
// Application Entry Point
// ----------------------------------------------------------------------------

fn main() -> eframe::Result<()> {
    // 0. Set up Diagnostic Panic Logger
    std::panic::set_hook(Box::new(|info| {
        let log_file = get_config_path()
            .parent()
            .unwrap_or(&PathBuf::from("."))
            .join("crash.log");
        let payload = format!(
            "[{:?}] PANIC: {}\nLocation: {:?}\n",
            SystemTime::now(),
            info,
            info.location()
        );
        if let Some(parent) = log_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&log_file, payload);
    }));

    // 1. Check for CLI Subcommands & Flags
    let args: Vec<String> = std::env::args().collect();
    let mut start_in_tray = false;
    if args.len() > 1 {
        let first_arg = args[1].to_lowercase();
        if first_arg == "--tray" || first_arg == "--minimized" || first_arg == "-t" {
            start_in_tray = true;
        } else {
            handle_cli_command(&args);
            return Ok(());
        }
    }

    // 2. Load Configuration & Initialize State
    let config = AppConfig::load();
    if config.run_at_startup {
        set_startup_task(true);
    }
    let state = Arc::new(Mutex::new(SharedState::new(config)));

    // 3. Configure GUI Viewport
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Agent Power Guard")
            .with_inner_size([1040.0, 740.0])
            .with_min_inner_size([780.0, 520.0])
            .with_visible(!start_in_tray)
            .with_active(!start_in_tray),
        ..Default::default()
    };

    let res = eframe::run_native(
        "Agent Power Guard",
        native_options,
        Box::new(move |cc| Ok(Box::new(AgentPowerGuardApp::new(cc, state)))),
    );
    if let Err(e) = &res {
        let log_file = get_config_path()
            .parent()
            .unwrap_or(&PathBuf::from("."))
            .join("error.log");
        let _ = std::fs::write(log_file, format!("run_native error: {:?}\n", e));
    }
    res
}

// ----------------------------------------------------------------------------
// Unit Tests (Decision Evaluator & Security)
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_config() -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.idle_threshold_secs = 180;
        cfg.agent_cpu_threshold = 2.0;
        cfg.download_threshold_kbps = 100.0;
        cfg.pc_cpu_threshold = 30.0;
        cfg.pc_disk_threshold_kbps = 5000.0;
        cfg.pc_net_threshold_kbps = 100.0;
        cfg.spike_filter_secs = 15;
        cfg.gpu_threshold_pct = 5.0;
        cfg.gpu_baseline_margin_pct = 5.0;
        cfg.grace_period_secs = 30;
        cfg.guard_enabled = true;
        cfg.cond7_spike_filter_enabled = false;
        cfg
    }

    fn eval_app(cfg: &AppConfig, metrics: &Metrics, state: &mut EvaluationState) -> Decision {
        let eval_cfg = cfg.to_eval_config(false);
        evaluate(&eval_cfg, metrics, state)
    }

    fn make_passing_metrics() -> Metrics {
        Metrics {
            idle_secs: 200,
            max_agent_cpu: 0.5,
            max_agent_proc: "None".to_string(),
            max_dl_kbps: 10.0,
            max_dl_proc: "None".to_string(),
            whole_pc_cpu: 5.0,
            whole_pc_disk_kbps: 50.0,
            whole_pc_net_kbps: 20.0,
            gpu_util: 1.0,
            mic_active: false,
            cam_active: false,
            mic_cam_app: String::new(),
            display_request_active: false,
            sensor_error: false,
            now_epoch_secs: 1000,
            on_battery: false,
            battery_pct: None,
            stay_awake_until: None,
            local_hour: 12,
            local_min: 0,
            ..Default::default()
        }
    }

    #[test]
    fn test_all_conditions_pass() {
        let cfg = make_test_config();
        let metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        let decision = eval_app(&cfg, &metrics, &mut state);
        assert!(decision.all_conditions_pass);
        assert!(decision.blockers.is_empty());
        assert_eq!(decision.countdown_secs, 179);
    }

    #[test]
    fn test_cond1_human_idle_boundary() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        // 179s idle -> blocked
        metrics.idle_secs = 179;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert!(!d.cond1_idle.is_passing);

        // 180s idle -> passes
        metrics.idle_secs = 180;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(d.cond1_idle.is_passing);
    }

    #[test]
    fn test_cond2_agent_cpu_boundary() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        // 1.9% -> pass
        metrics.max_agent_cpu = 1.9;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(d.cond2_agent_cpu.is_passing);

        // 2.1% -> blocked
        metrics.max_agent_cpu = 2.1;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond2_agent_cpu.is_passing);
        assert!(!d.all_conditions_pass);
    }

    #[test]
    fn test_cond3_download_boundary() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        // 99 KB/s -> pass
        metrics.max_dl_kbps = 99.0;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(d.cond3_download.is_passing);

        // 101 KB/s -> blocked
        metrics.max_dl_kbps = 101.0;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond3_download.is_passing);
        assert!(!d.all_conditions_pass);
    }

    #[test]
    fn test_cond4_5_6_pc_metrics() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        // PC CPU high
        metrics.whole_pc_cpu = 45.0;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond4_pc_cpu.is_passing);

        // PC Disk high
        metrics.whole_pc_cpu = 5.0;
        metrics.whole_pc_disk_kbps = 6000.0;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond5_pc_disk.is_passing);

        // PC Net high
        metrics.whole_pc_disk_kbps = 50.0;
        metrics.whole_pc_net_kbps = 200.0;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond6_pc_net.is_passing);
    }

    #[test]
    fn test_cond7_sustained_spike_filter() {
        let mut cfg = make_test_config(); // spike_filter_secs = 15, filter_busy_count = 8, filter_quiet_count = 2
        cfg.cond7_spike_filter_enabled = true;
        let mut metrics = make_passing_metrics();
        metrics.whole_pc_cpu = 50.0; // Spiking

        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        // First 7 ticks (< busy_count=8): Pending -> does not reset or block sleep
        for i in 0..7 {
            metrics.now_epoch_secs = 1000 + i;
            let d = eval_app(&cfg, &metrics, &mut state);
            assert!(d.cond7_spike_filter.is_passing);
            assert!(d.all_conditions_pass);
        }

        // 8th busy tick: signal conditioning filter transitions to Busy -> blocks!
        metrics.now_epoch_secs = 1007;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond7_spike_filter.is_passing);
        assert!(!d.all_conditions_pass);

        // Once quiet over rolling window down to <= quiet_count (2), hysteresis clears cleanly
        metrics.whole_pc_cpu = 5.0;
        let mut cleared = false;
        for i in 0..15 {
            metrics.now_epoch_secs = 1008 + i;
            let d = eval_app(&cfg, &metrics, &mut state);
            if i == 14 {
                cleared = d.cond7_spike_filter.is_passing;
            }
        }
        assert!(
            cleared,
            "Hysteresis should clear after quiet samples drain busy window"
        );
    }

    #[test]
    fn test_cond9_display_power_request() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        metrics.display_request_active = true;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond9_power_requests.is_passing);
        assert!(!d.all_conditions_pass);
    }

    #[test]
    fn test_cond10_gpu_boundary() {
        let cfg = make_test_config(); // gpu_threshold_pct = 5.0, gpu_baseline_margin_pct = 5.0
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        // 4.9% passive idle sample trains baseline floor to 4.9% -> passes
        metrics.gpu_util = 4.9;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(d.cond10_gpu.is_passing);

        // 12.0% > effective cutoff (4.9% floor + 5.0% margin = 9.9%) -> blocks!
        metrics.gpu_util = 12.0;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond10_gpu.is_passing);
        assert!(!d.all_conditions_pass);
    }

    #[test]
    fn test_cond11_mic_cam_active() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        metrics.mic_active = true;
        metrics.mic_cam_app = "zoom.exe".to_string();
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond11_mic_cam.is_passing);
        assert!(!d.all_conditions_pass);

        metrics.mic_active = false;
        metrics.cam_active = true;
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.cond11_mic_cam.is_passing);
        assert!(!d.all_conditions_pass);
    }

    #[test]
    fn test_disabled_conditions() {
        let mut cfg = make_test_config();
        cfg.cond1_idle_enabled = false;
        cfg.cond2_agent_cpu_enabled = false;
        cfg.cond10_gpu_enabled = false;
        cfg.cond11_mic_cam_enabled = false;

        let mut metrics = make_passing_metrics();
        metrics.idle_secs = 10;
        metrics.max_agent_cpu = 20.0;
        metrics.gpu_util = 80.0;
        metrics.mic_active = true;

        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(d.all_conditions_pass);
    }

    #[test]
    fn test_active_lease_and_postpone() {
        let cfg = make_test_config();
        let metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            agent_lease_until_epoch_secs: Some(1500),
            agent_lease_reason: "Heavy compilation".to_string(),
            ..Default::default()
        };

        // Active lease blocks
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert!(d.blockers.iter().any(|b| b.contains("Heavy compilation")));

        // Expired lease clears and allows
        state.agent_lease_until_epoch_secs = Some(900);
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(d.all_conditions_pass);
        assert!(state.agent_lease_until_epoch_secs.is_none());

        // Postpone blocks
        state.postponed_until_epoch_secs = Some(1500);
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert!(d.blockers.iter().any(|b| b.contains("Postponed")));
    }

    #[test]
    fn test_master_guard_disabled() {
        let mut cfg = make_test_config();
        cfg.set_operational_mode(OperationalMode::GuardOff);
        let metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 50,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert_eq!(d.operational_mode, OperationalMode::GuardOff);
        assert_eq!(d.intent.code(), "GuardOff");
        assert_eq!(state.countdown_secs, cfg.idle_threshold_secs);
        assert!(d.blocker_banner_text.contains("watchdog is off"));
    }

    #[test]
    fn test_resume_from_sleep_reset() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        metrics.idle_secs = 2; // User input wake
        let mut state = EvaluationState {
            countdown_secs: 10,
            last_wall_clock_secs: 950, // Jumped by 50 seconds
            pc_spike_sustained_counter: 12,
            ..Default::default()
        };

        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(d.woke_from_sleep_reset);
        assert_eq!(state.countdown_secs, cfg.idle_threshold_secs);
        assert_eq!(state.pc_spike_sustained_counter, 0);
    }

    #[test]
    fn test_grace_period_trigger() {
        let cfg = make_test_config();
        let metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 31,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        let d = eval_app(&cfg, &metrics, &mut state);
        assert_eq!(d.countdown_secs, 30);
        assert!(d.trigger_grace_banner);
    }

    #[test]
    fn test_sensor_error_blocks_sleep() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        metrics.sensor_error = true;
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert!(d.blockers.iter().any(|b| b.contains("Sensor Failure")));
    }

    #[test]
    fn test_constant_time_eq() {
        let token = "a1b2c3d4e5f67890123456789012345678901234567890123456789012345678";
        assert!(constant_time_eq(token, token));
        assert!(!constant_time_eq(token, "wrong"));
        assert!(!constant_time_eq(
            token,
            "a1b2c3d4e5f67890123456789012345678901234567890123456789012345679"
        ));
    }

    #[test]
    fn test_rate_limiter() {
        let limiter = RateLimiter::new();
        let ip = "127.0.0.1".parse().unwrap();

        for _ in 0..9 {
            assert!(limiter.check_allowed(ip));
            limiter.record_failure(ip);
        }
        limiter.record_failure(ip);
        assert!(!limiter.check_allowed(ip));

        limiter.record_success(ip);
        assert!(limiter.check_allowed(ip));
    }

    #[test]
    fn test_is_newer_version() {
        assert!(is_newer_version("1.2.0", "1.1.0"));
        assert!(is_newer_version("v1.1.1", "1.1.0"));
        assert!(is_newer_version("2.0.0", "1.9.9"));
        assert!(!is_newer_version("1.1.0", "1.1.0"));
        assert!(!is_newer_version("v1.1.0", "1.1.0"));
        assert!(!is_newer_version("1.0.9", "1.1.0"));
        assert!(!is_newer_version("0.9.0", "1.0.0"));
        assert!(is_newer_version("v1.2.4-rc1", "1.2.3"));
    }

    #[test]
    fn test_parse_releases_json_picks_highest_version_and_asset() {
        let json = br#"[
            {"tag_name": "v1.2.2", "name": "v1.2.2", "draft": false, "prerelease": false, "published_at": "2026-11-11T10:00:00Z", "body": "old", "assets": []},
            {"tag_name": "v1.2.5-rc1", "name": "rc", "draft": true, "prerelease": true, "published_at": "2026-11-12T12:00:00Z", "body": "draft", "assets": []},
            {"tag_name": "v1.2.4", "name": "Release v1.2.4", "draft": false, "prerelease": false, "published_at": "2026-11-12T15:00:00Z", "body": "latest", "assets": [
                {"name": "AgentPowerGuard-windows-x64.msi", "browser_download_url": "https://example.com/app.msi", "size": 5000000},
                {"name": "agent-power-guard-windows-x64.exe", "browser_download_url": "https://example.com/app.exe", "size": 4500000},
                {"name": "agent-power-guard-linux-x86_64.tar.gz", "browser_download_url": "https://example.com/linux.tar.gz", "size": 4000000},
                {"name": "agent-power-guard-macos-aarch64.tar.gz", "browser_download_url": "https://example.com/mac-arm.tar.gz", "size": 4000000},
                {"name": "agent-power-guard-macos-x86_64.tar.gz", "browser_download_url": "https://example.com/mac-x64.tar.gz", "size": 4000000}
            ]}
        ]"#;
        let list = parse_releases_json(json).expect("releases json");
        assert_eq!(list.len(), 2);
        let latest = pick_latest_release(&list).expect("latest release");
        assert_eq!(latest.tag, "v1.2.4");
        #[cfg(windows)]
        {
            assert_eq!(
                select_platform_asset(latest, false).unwrap().name,
                "agent-power-guard-windows-x64.exe"
            );
            assert_eq!(
                select_platform_asset(latest, true).unwrap().name,
                "AgentPowerGuard-windows-x64.msi"
            );
        }
    }

    #[test]
    fn test_top_blockers_and_last_night_summary() {
        let history = vec![
            BlockRecord {
                start_epoch_secs: 1000,
                end_epoch_secs: 1300,
                duration_secs: 300,
                primary_blocker: "Agent CPU (node 4.5%)".to_string(),
                all_blockers: vec!["Agent CPU (node 4.5%)".to_string()],
            },
            BlockRecord {
                start_epoch_secs: 1400,
                end_epoch_secs: 1500,
                duration_secs: 100,
                primary_blocker: "Active Download (idman)".to_string(),
                all_blockers: vec!["Active Download (idman)".to_string()],
            },
            BlockRecord {
                start_epoch_secs: 1600,
                end_epoch_secs: 1800,
                duration_secs: 200,
                primary_blocker: "Agent CPU (node 4.5%)".to_string(),
                all_blockers: vec!["Agent CPU (node 4.5%)".to_string()],
            },
        ];

        let top = calculate_top_blockers(&history);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].name, "Agent CPU (node 4.5%)");
        assert_eq!(top[0].total_secs, 500);
        assert_eq!(top[0].count, 2);

        assert_eq!(top[1].name, "Active Download (idman)");
        assert_eq!(top[1].total_secs, 100);
        assert_eq!(top[1].count, 1);

        let summary = calculate_last_night_summary(&history, 2000);
        assert_eq!(summary.total_blocked_secs, 600);
        assert_eq!(
            summary.top_blocker.as_deref(),
            Some("Agent CPU (node 4.5%)")
        );
        assert_eq!(summary.event_count, 3);
    }

    #[test]
    fn test_block_history_csv_export() {
        let history = vec![BlockRecord {
            start_epoch_secs: 1000,
            end_epoch_secs: 1060,
            duration_secs: 60,
            primary_blocker: "Whole-PC CPU (45%)".to_string(),
            all_blockers: vec!["Whole-PC CPU (45%)".to_string(), "Network".to_string()],
        }];
        let csv = export_block_history_csv(&history);
        assert!(csv.contains("Start_Epoch,End_Epoch"));
        assert!(csv.contains("Whole-PC CPU (45%)"));
    }

    #[test]
    fn test_notification_event_formatting() {
        let e1 = NotificationEvent::SleepImminent { countdown_secs: 30 };
        assert_eq!(e1.event_name(), "sleep_imminent");
        let (title1, msg1) = e1.title_and_message();
        assert!(title1.contains("Sleep Imminent"));
        assert!(msg1.contains("30 seconds"));

        let e2 = NotificationEvent::BlockedLong {
            hours: 2,
            blocker: "Agent CPU".to_string(),
        };
        assert_eq!(e2.event_name(), "blocked_long");
        let (title2, msg2) = e2.title_and_message();
        assert!(title2.contains("Machine Blocked"));
        assert!(msg2.contains("Agent CPU"));

        let e3 = NotificationEvent::TestMessage;
        assert_eq!(e3.event_name(), "test");
    }

    #[test]
    fn test_power_modes_apply() {
        let mut cfg = AppConfig::default();

        PowerMode::Work.apply_to_config(&mut cfg);
        assert_eq!(cfg.active_mode, PowerMode::Work);
        assert_eq!(cfg.idle_threshold_secs, 600);
        assert_eq!(cfg.monitor_timeout_mins, 5);
        assert_eq!(cfg.agent_cpu_threshold, 1.0);

        PowerMode::Night.apply_to_config(&mut cfg);
        assert_eq!(cfg.active_mode, PowerMode::Night);
        assert_eq!(cfg.idle_threshold_secs, 60);
        assert_eq!(cfg.monitor_timeout_mins, 1);

        PowerMode::Away.apply_to_config(&mut cfg);
        assert_eq!(cfg.active_mode, PowerMode::Away);
        assert_eq!(cfg.idle_threshold_secs, 30);

        PowerMode::Balanced.apply_to_config(&mut cfg);
        assert_eq!(cfg.active_mode, PowerMode::Balanced);
        assert_eq!(cfg.idle_threshold_secs, 180);
    }

    #[test]
    fn test_quiet_hours_calculation() {
        // Overnight range: 23:00 to 07:00
        assert!(is_in_quiet_hours(23, 0, "23:00", "07:00"));
        assert!(is_in_quiet_hours(23, 45, "23:00", "07:00"));
        assert!(is_in_quiet_hours(2, 30, "23:00", "07:00"));
        assert!(is_in_quiet_hours(6, 59, "23:00", "07:00"));
        assert!(!is_in_quiet_hours(7, 0, "23:00", "07:00"));
        assert!(!is_in_quiet_hours(14, 0, "23:00", "07:00"));
        assert!(!is_in_quiet_hours(22, 59, "23:00", "07:00"));

        // Daytime range: 09:00 to 17:00
        assert!(is_in_quiet_hours(9, 0, "09:00", "17:00"));
        assert!(is_in_quiet_hours(12, 30, "09:00", "17:00"));
        assert!(!is_in_quiet_hours(8, 59, "09:00", "17:00"));
        assert!(!is_in_quiet_hours(17, 0, "09:00", "17:00"));
        assert!(!is_in_quiet_hours(22, 0, "09:00", "17:00"));
    }

    #[test]
    fn test_carbon_factor_calculation() {
        let mut cfg = AppConfig::default();

        cfg.carbon_region = "Global Average".to_string();
        assert!((get_carbon_factor_kg(&cfg) - 0.436).abs() < 0.001);

        cfg.carbon_region = "United States (Avg)".to_string();
        assert!((get_carbon_factor_kg(&cfg) - 0.368).abs() < 0.001);

        cfg.carbon_region = "France".to_string();
        assert!((get_carbon_factor_kg(&cfg) - 0.052).abs() < 0.001);

        cfg.carbon_region = "Custom".to_string();
        cfg.custom_carbon_g_kwh = 250.0;
        assert!((get_carbon_factor_kg(&cfg) - 0.250).abs() < 0.001);
    }

    #[test]
    fn test_battery_override_and_critical_sleep() {
        let mut cfg = make_test_config();
        cfg.battery_profile_enabled = true;
        cfg.battery_idle_threshold_secs = 60;
        cfg.low_battery_auto_sleep_enabled = true;
        cfg.low_battery_percent_threshold = 15;

        let mut metrics = make_passing_metrics();
        metrics.idle_secs = 70; // 70s idle: < 180s on AC, but >= 60s on battery!

        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        // On AC power -> not idle enough (needs 180s)
        metrics.on_battery = false;
        let d_ac = eval_app(&cfg, &metrics, &mut state);
        assert!(!d_ac.all_conditions_pass);

        // On Battery -> 70s passes the 60s battery threshold!
        metrics.on_battery = true;
        metrics.battery_pct = Some(80);
        let d_batt = eval_app(&cfg, &metrics, &mut state);
        assert!(d_batt.all_conditions_pass);

        // Critical low battery: 10% <= 15% -> forces emergency sleep
        metrics.battery_pct = Some(10);
        let d_crit = eval_app(&cfg, &metrics, &mut state);
        assert!(d_crit.trigger_sleep);
        assert!(d_crit.blocker_banner_text.contains("CRITICAL BATTERY"));
    }

    #[test]
    fn test_stay_awake_until_in_evaluator() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            ..Default::default()
        };

        // Stay awake active (future timestamp: 1500 > now: 1000)
        metrics.stay_awake_until = Some(1500);
        let d = eval_app(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert!(d
            .blockers
            .iter()
            .any(|b| b.contains("Scheduled Stay-Awake")));

        // Stay awake expired (past timestamp: 900 <= now: 1000)
        metrics.stay_awake_until = Some(900);
        let d2 = eval_app(&cfg, &metrics, &mut state);
        assert!(d2.all_conditions_pass);
    }

    #[test]
    fn test_sleep_after_lease() {
        let mut cfg = make_test_config();
        cfg.sleep_after_lease = true;

        let metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            agent_lease_until_epoch_secs: Some(999), // Lease just expired at 999 <= now 1000
            agent_lease_reason: "Training run".to_string(),
            ..Default::default()
        };

        let d = eval_app(&cfg, &metrics, &mut state);
        // With sleep_after_lease = true, expiration sets countdown to 0 and triggers sleep immediately!
        assert!(d.trigger_sleep);
        assert_eq!(state.countdown_secs, cfg.idle_threshold_secs);
    }

    #[test]
    fn test_apply_ignore_item_and_mode_mapping() {
        let mut s = SharedState::new(AppConfig::default());
        s.apply_ignore_item("disk_noise", "custombackup.exe");
        assert!(s.config.disk_noise_processes.contains("custombackup.exe"));
        s.apply_ignore_item("power_owner", "chrome.exe");
        assert!(s.config.ignored_power_owners.contains("chrome.exe"));

        s.set_operational_mode(OperationalMode::BestPerformance, Some(2));
        assert_eq!(s.operational_mode, OperationalMode::BestPerformance);
        assert!(s.eval_state.best_perf_until_epoch_secs.is_some());
        s.set_operational_mode(OperationalMode::SmartGuard, None);
        assert!(s.eval_state.best_perf_until_epoch_secs.is_none());
        assert!(s.config.guard_enabled);
    }

    #[test]
    fn test_wol_mac_validation() {
        // Valid MAC formats
        assert!(send_wol_packet("00:11:22:33:44:55", Some("127.0.0.1")).is_ok());
        assert!(send_wol_packet("aa-bb-cc-dd-ee-ff", Some("127.0.0.1")).is_ok());
        assert!(send_wol_packet("001122334455", Some("127.0.0.1")).is_ok());

        // Invalid MAC formats
        assert!(send_wol_packet("00:11:22:33:44", Some("127.0.0.1")).is_err());
        assert!(send_wol_packet("00:11:22:33:44:55:66", Some("127.0.0.1")).is_err());
        assert!(send_wol_packet("00:11:22:33:44:GG", Some("127.0.0.1")).is_err());
        assert!(send_wol_packet("not-a-mac-addr", Some("127.0.0.1")).is_err());
    }

    #[test]
    fn test_ignore_list_filtering() {
        let ignored_str = "system, registry, smss, myagent";
        let ignored_keywords: Vec<String> = ignored_str
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        let proc1 = "myagent.exe".to_lowercase();
        let proc2 = "cargo.exe".to_lowercase();
        let proc3 = "system".to_lowercase();

        assert!(ignored_keywords.iter().any(|k| proc1.contains(k)));
        assert!(!ignored_keywords.iter().any(|k| proc2.contains(k)));
        assert!(ignored_keywords.iter().any(|k| proc3.contains(k)));
    }

    #[test]
    fn test_peer_machine_serialization() {
        let mut cfg = make_test_config();
        assert!(cfg.peers.is_empty());
        cfg.peers.push(PeerMachine {
            id: "server-1".to_string(),
            name: "Home Server".to_string(),
            url: "http://192.168.1.100:49281".to_string(),
            token: "0123456789abcdef".to_string(),
            permission: "control".to_string(),
            mac: Some("00:11:22:33:44:55".to_string()),
        });

        let json = serde_json::to_string(&cfg).expect("Config with peers should serialize");
        let deserialized: AppConfig =
            serde_json::from_str(&json).expect("Config should deserialize");
        assert_eq!(deserialized.peers.len(), 1);
        assert_eq!(deserialized.peers[0].name, "Home Server");
        assert_eq!(deserialized.peers[0].permission, "control");
        assert_eq!(
            deserialized.peers[0].mac.as_deref(),
            Some("00:11:22:33:44:55")
        );
    }

    #[test]
    fn test_wayland_dbus_idle_parsing() {
        let sample_dbus_reply = "method return time=1700000000.123 sender=:1.45 -> destination=:1.99\n   uint32 185000\n";
        let mut parsed_idle_secs = 0u32;
        for line in sample_dbus_reply.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("uint32") {
                if let Some(val_str) = trimmed.strip_prefix("uint32") {
                    if let Ok(ms) = val_str.trim().parse::<u64>() {
                        parsed_idle_secs = (ms / 1000) as u32;
                    }
                }
            }
        }
        assert_eq!(parsed_idle_secs, 185);
    }
}
