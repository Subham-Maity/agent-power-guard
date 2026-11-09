#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Agent Power Guard - Native Entry Point

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
use windows_sys::Win32::Foundation::{CloseHandle, FreeLibrary, HANDLE, LUID};
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
    RegCloseKey, RegDeleteValueW, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ,
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
    pub allow_lan_access: bool,         // default false (loopback only)
    pub ignored_processes: String,      // comma-separated ignore list

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

fn get_block_history_path() -> PathBuf {
    get_config_path().parent().unwrap_or(&PathBuf::from(".")).join("block_history.jsonl")
}

fn get_api_token_path() -> PathBuf {
    get_config_path().parent().unwrap_or(&PathBuf::from(".")).join("api_token.txt")
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
        use windows_sys::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
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
    let mut list: Vec<TopBlockerStat> = map.into_iter().map(|(name, (total_secs, count))| {
        TopBlockerStat { name, total_secs, count }
    }).collect();
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
    let overnight_records: Vec<&BlockRecord> = history.iter()
        .filter(|r| r.end_epoch_secs >= window_start)
        .collect();

    let total_secs: u64 = overnight_records.iter().map(|r| r.duration_secs).sum();
    let mut map: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for r in &overnight_records {
        *map.entry(r.primary_blocker.clone()).or_insert(0) += r.duration_secs;
    }
    let top_blocker = map.into_iter().max_by_key(|(_, secs)| *secs).map(|(name, _)| name);

    LastNightSummary {
        total_blocked_secs: total_secs,
        top_blocker,
        event_count: overnight_records.len(),
    }
}

pub fn export_block_history_csv(history: &[BlockRecord]) -> String {
    let mut csv = String::from("Start_Epoch,End_Epoch,Duration_Secs,Primary_Blocker,All_Blockers\n");
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
        s.trim_start_matches('v')
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
    PcWoke,
    GuardPausedLong { hours: u32 },
    TestMessage,
}

impl NotificationEvent {
    pub fn title_and_message(&self) -> (String, String) {
        match self {
            NotificationEvent::SleepImminent { countdown_secs } => (
                "Agent Power Guard: Sleep Imminent".to_string(),
                format!("All conditions pass. PC will sleep in {} seconds.", countdown_secs),
            ),
            NotificationEvent::BlockedLong { hours, blocker } => (
                "Agent Power Guard: Machine Blocked".to_string(),
                format!("PC has been kept awake for > {} hours by: {}", hours, blocker),
            ),
            NotificationEvent::LeaseEnded { reason } => (
                "Agent Power Guard: Lease Concluded".to_string(),
                format!("Agent lease '{}' has concluded.", reason),
            ),
            NotificationEvent::PcWoke => (
                "Agent Power Guard: System Resumed".to_string(),
                "PC has resumed from sleep and monitoring is active.".to_string(),
            ),
            NotificationEvent::GuardPausedLong { hours } => (
                "Agent Power Guard: Guard Paused".to_string(),
                format!("Master guard toggle has been disabled for > {} hours.", hours),
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
            NotificationEvent::PcWoke => "pc_woke",
            NotificationEvent::GuardPausedLong { .. } => "guard_paused_long",
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

    if ntfy_url.is_empty() && (tg_token.is_empty() || tg_chat.is_empty()) && webhook_url.is_empty() {
        return;
    }

    let (title, message) = event.title_and_message();
    let event_name = event.event_name().to_string();
    let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

    std::thread::spawn(move || {
        // 1. ntfy
        if !ntfy_url.is_empty() {
            let mut cmd = Command::new("curl");
            #[cfg(windows)]
            cmd.creation_flags(CREATE_NO_WINDOW);
            cmd.args(["-s", "--max-time", "10", "--connect-timeout", "5", "-X", "POST"]);
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
            cmd.args(["-s", "--max-time", "10", "--connect-timeout", "5", "-X", "POST", &url]);
            cmd.args(["-d", &format!("chat_id={}", tg_chat)]);
            cmd.args(["--data-urlencode", &format!("text={}\n\n{}", title, message)]);
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
            cmd.args(["-s", "--max-time", "10", "--connect-timeout", "5", "-X", "POST", &webhook_url]);
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

#[derive(Debug, Clone, Default)]
pub struct ConditionStatus {
    pub enabled: bool,
    pub is_passing: bool,
    pub value_text: String,
    pub threshold_text: String,
    pub blocker_detail: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub idle_secs: u32,
    pub max_agent_cpu: f32,
    pub max_agent_proc: String,
    pub max_dl_kbps: f32,
    pub max_dl_proc: String,
    pub whole_pc_cpu: f32,
    pub whole_pc_disk_kbps: f32,
    pub whole_pc_net_kbps: f32,
    pub gpu_util: f32,
    pub mic_active: bool,
    pub cam_active: bool,
    pub mic_cam_app: String,
    pub display_request_active: bool,
    pub sensor_error: bool,
    pub now_epoch_secs: u64,
}

#[derive(Debug, Clone, Default)]
pub struct EvaluationState {
    pub countdown_secs: u32,
    pub pc_spike_sustained_counter: u32,
    pub postponed_until_epoch_secs: Option<u64>,
    pub agent_lease_until_epoch_secs: Option<u64>,
    pub agent_lease_reason: String,
    pub agent_lease_pid: Option<u32>,
    pub last_wall_clock_secs: u64,
    pub monitor_standby_start_epoch_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Decision {
    pub all_conditions_pass: bool,
    pub blockers: Vec<String>,
    pub blocker_banner_text: String,
    pub countdown_secs: u32,
    pub countdown_max: u32,
    pub trigger_sleep: bool,
    pub trigger_grace_banner: bool,
    pub log_monitor_off_duration: Option<u64>,
    pub woke_from_sleep_reset: bool,

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
}

pub fn evaluate(config: &AppConfig, metrics: &Metrics, state: &mut EvaluationState) -> Decision {
    let mut woke_from_sleep_reset = false;
    let mut log_monitor_off_duration = None;

    // 1. Check Wall-Clock Wakeup
    if state.last_wall_clock_secs > 0 && metrics.now_epoch_secs > state.last_wall_clock_secs + 10 {
        state.countdown_secs = config.idle_threshold_secs;
        state.pc_spike_sustained_counter = 0;
        woke_from_sleep_reset = true;
    }
    state.last_wall_clock_secs = metrics.now_epoch_secs;

    let mut blockers = Vec::new();

    // Safety First: Global Rule 7 - sensor failure counts as blocking
    if metrics.sensor_error {
        blockers.push("Sensor Failure (Safety hold active)".to_string());
    }

    // Condition 1: Human Input Idle
    let cond1_pass = metrics.idle_secs >= config.idle_threshold_secs;
    let cond1_idle = ConditionStatus {
        enabled: config.cond1_idle_enabled,
        is_passing: cond1_pass,
        value_text: format!("{}s", metrics.idle_secs),
        threshold_text: format!(">= {}s", config.idle_threshold_secs),
        blocker_detail: if !cond1_pass && config.cond1_idle_enabled {
            Some(format!(
                "Human Activity (User active, idle only {}s < {}s)",
                metrics.idle_secs, config.idle_threshold_secs
            ))
        } else {
            None
        },
    };
    if config.cond1_idle_enabled && !cond1_pass {
        blockers.push(format!(
            "Human Idle ({}s < {}s)",
            metrics.idle_secs, config.idle_threshold_secs
        ));
    }

    // Condition 2: Watched Agent CPU %
    let cond2_pass = metrics.max_agent_cpu < config.agent_cpu_threshold;
    let cond2_agent_cpu = ConditionStatus {
        enabled: config.cond2_agent_cpu_enabled,
        is_passing: cond2_pass,
        value_text: format!("{:.1}% ({})", metrics.max_agent_cpu, metrics.max_agent_proc),
        threshold_text: format!("< {:.1}%", config.agent_cpu_threshold),
        blocker_detail: if !cond2_pass && config.cond2_agent_cpu_enabled {
            Some(format!(
                "Agent CPU ({} at {:.1}% >= {:.1}%)",
                metrics.max_agent_proc, metrics.max_agent_cpu, config.agent_cpu_threshold
            ))
        } else {
            None
        },
    };
    if config.cond2_agent_cpu_enabled && !cond2_pass {
        blockers.push(format!(
            "Agent CPU ({} at {:.1}% >= {:.1}%)",
            metrics.max_agent_proc, metrics.max_agent_cpu, config.agent_cpu_threshold
        ));
    }

    // Condition 3: Active Download / IDM
    let cond3_pass = metrics.max_dl_kbps < config.download_threshold_kbps;
    let cond3_download = ConditionStatus {
        enabled: config.cond3_download_enabled,
        is_passing: cond3_pass,
        value_text: format!("{:.0} KB/s ({})", metrics.max_dl_kbps, metrics.max_dl_proc),
        threshold_text: format!("< {:.0} KB/s", config.download_threshold_kbps),
        blocker_detail: if !cond3_pass && config.cond3_download_enabled {
            Some(format!(
                "Active Download ({} at {:.0} KB/s >= {:.0} KB/s)",
                metrics.max_dl_proc, metrics.max_dl_kbps, config.download_threshold_kbps
            ))
        } else {
            None
        },
    };
    if config.cond3_download_enabled && !cond3_pass {
        blockers.push(format!(
            "Active Download ({} at {:.0} KB/s)",
            metrics.max_dl_proc, metrics.max_dl_kbps
        ));
    }

    // Condition 4, 5, 6: Whole-PC Metrics
    let cond4_pass = metrics.whole_pc_cpu < config.pc_cpu_threshold;
    let cond5_pass = metrics.whole_pc_disk_kbps < config.pc_disk_threshold_kbps;
    let cond6_pass = metrics.whole_pc_net_kbps < config.pc_net_threshold_kbps;

    let cond4_pc_cpu = ConditionStatus {
        enabled: config.cond4_pc_cpu_enabled,
        is_passing: cond4_pass,
        value_text: format!("{:.1}%", metrics.whole_pc_cpu),
        threshold_text: format!("< {:.1}%", config.pc_cpu_threshold),
        blocker_detail: if !cond4_pass && config.cond4_pc_cpu_enabled {
            Some(format!(
                "Whole-PC CPU ({:.1}% >= {:.1}%)",
                metrics.whole_pc_cpu, config.pc_cpu_threshold
            ))
        } else {
            None
        },
    };

    let cond5_pc_disk = ConditionStatus {
        enabled: config.cond5_pc_disk_enabled,
        is_passing: cond5_pass,
        value_text: format!("{:.0} KB/s", metrics.whole_pc_disk_kbps),
        threshold_text: format!("< {:.0} KB/s", config.pc_disk_threshold_kbps),
        blocker_detail: if !cond5_pass && config.cond5_pc_disk_enabled {
            Some(format!(
                "Whole-PC Disk I/O ({:.0} KB/s >= {:.0} KB/s)",
                metrics.whole_pc_disk_kbps, config.pc_disk_threshold_kbps
            ))
        } else {
            None
        },
    };

    let cond6_pc_net = ConditionStatus {
        enabled: config.cond6_pc_net_enabled,
        is_passing: cond6_pass,
        value_text: format!("{:.0} KB/s", metrics.whole_pc_net_kbps),
        threshold_text: format!("< {:.0} KB/s", config.pc_net_threshold_kbps),
        blocker_detail: if !cond6_pass && config.cond6_pc_net_enabled {
            Some(format!(
                "Whole-PC Network ({:.0} KB/s >= {:.0} KB/s)",
                metrics.whole_pc_net_kbps, config.pc_net_threshold_kbps
            ))
        } else {
            None
        },
    };

    // Condition 7: Sustained Spike Filter
    let any_pc_metric_tripped = (config.cond4_pc_cpu_enabled && !cond4_pass)
        || (config.cond5_pc_disk_enabled && !cond5_pass)
        || (config.cond6_pc_net_enabled && !cond6_pass);

    if any_pc_metric_tripped {
        state.pc_spike_sustained_counter = state.pc_spike_sustained_counter.saturating_add(1);
    } else {
        state.pc_spike_sustained_counter = 0;
    }

    let spike_sustained = state.pc_spike_sustained_counter >= config.spike_filter_secs;
    let cond7_spike_filter = ConditionStatus {
        enabled: config.cond7_spike_filter_enabled,
        is_passing: !any_pc_metric_tripped || !spike_sustained,
        value_text: format!("{}s / {}s", state.pc_spike_sustained_counter, config.spike_filter_secs),
        threshold_text: format!("Debounce {}s", config.spike_filter_secs),
        blocker_detail: if any_pc_metric_tripped && spike_sustained && config.cond7_spike_filter_enabled {
            Some(format!(
                "Sustained PC Spike (Activity exceeded {}s debounce filter)",
                config.spike_filter_secs
            ))
        } else {
            None
        },
    };

    if config.cond7_spike_filter_enabled {
        if any_pc_metric_tripped && spike_sustained {
            if config.cond4_pc_cpu_enabled && !cond4_pass {
                blockers.push(format!("Whole-PC CPU ({:.1}%)", metrics.whole_pc_cpu));
            }
            if config.cond5_pc_disk_enabled && !cond5_pass {
                blockers.push(format!("Whole-PC Disk ({:.0} KB/s)", metrics.whole_pc_disk_kbps));
            }
            if config.cond6_pc_net_enabled && !cond6_pass {
                blockers.push(format!("Whole-PC Network ({:.0} KB/s)", metrics.whole_pc_net_kbps));
            }
        }
    } else if any_pc_metric_tripped {
        if config.cond4_pc_cpu_enabled && !cond4_pass {
            blockers.push(format!("Whole-PC CPU ({:.1}%)", metrics.whole_pc_cpu));
        }
        if config.cond5_pc_disk_enabled && !cond5_pass {
            blockers.push(format!("Whole-PC Disk ({:.0} KB/s)", metrics.whole_pc_disk_kbps));
        }
        if config.cond6_pc_net_enabled && !cond6_pass {
            blockers.push(format!("Whole-PC Network ({:.0} KB/s)", metrics.whole_pc_net_kbps));
        }
    }

    // Condition 8: Monitor Timeout Tracking
    let cond8_monitor_timeout = ConditionStatus {
        enabled: config.cond8_monitor_timeout_enabled,
        is_passing: true,
        value_text: format!("{}m", config.monitor_timeout_mins),
        threshold_text: "Standby sync".to_string(),
        blocker_detail: None,
    };

    let monitor_timeout_secs = (config.monitor_timeout_mins as u64) * 60;
    if (metrics.idle_secs as u64) >= monitor_timeout_secs {
        if state.monitor_standby_start_epoch_secs.is_none() {
            state.monitor_standby_start_epoch_secs = Some(metrics.now_epoch_secs);
        }
    } else if let Some(start) = state.monitor_standby_start_epoch_secs.take() {
        let dur = metrics.now_epoch_secs.saturating_sub(start);
        if dur > 10 {
            log_monitor_off_duration = Some(dur);
        }
    }

    // Condition 9: Windows Power Requests (DISPLAY lock)
    let cond9_pass = !metrics.display_request_active;
    let cond9_power_requests = ConditionStatus {
        enabled: config.cond9_power_requests_enabled && config.respect_display_requests,
        is_passing: cond9_pass,
        value_text: if metrics.display_request_active {
            "Active (Video)".to_string()
        } else {
            "None".to_string()
        },
        threshold_text: "Respect DISPLAY".to_string(),
        blocker_detail: if !cond9_pass && config.cond9_power_requests_enabled && config.respect_display_requests {
            Some("Windows DISPLAY Request (Active video playback or media lock)".to_string())
        } else {
            None
        },
    };
    if config.cond9_power_requests_enabled && config.respect_display_requests && metrics.display_request_active {
        blockers.push("DISPLAY Request (Video playback active)".to_string());
    }

    // Condition 10: Watched GPU % (AI Models & Compute)
    let cond10_pass = !config.cond10_gpu_enabled || metrics.gpu_util < config.gpu_threshold_pct;
    let cond10_gpu = ConditionStatus {
        enabled: config.cond10_gpu_enabled,
        is_passing: cond10_pass,
        value_text: format!("{:.1}%", metrics.gpu_util),
        threshold_text: format!("< {:.1}%", config.gpu_threshold_pct),
        blocker_detail: if !cond10_pass && config.cond10_gpu_enabled {
            Some(format!("Watched GPU ({:.1}%)", metrics.gpu_util))
        } else {
            None
        },
    };
    if config.cond10_gpu_enabled && !cond10_pass {
        blockers.push(format!("Watched GPU ({:.1}%)", metrics.gpu_util));
    }

    // Condition 11: Microphone & Camera Active (Meetings & Calls)
    let cond11_pass = !config.cond11_mic_cam_enabled || (!metrics.mic_active && !metrics.cam_active);
    let cond11_mic_cam = ConditionStatus {
        enabled: config.cond11_mic_cam_enabled,
        is_passing: cond11_pass,
        value_text: if metrics.mic_active || metrics.cam_active {
            format!("In Use ({})", if metrics.mic_cam_app.is_empty() { "Call/Meeting" } else { &metrics.mic_cam_app })
        } else {
            "Quiet".to_string()
        },
        threshold_text: "Require Idle".to_string(),
        blocker_detail: if !cond11_pass && config.cond11_mic_cam_enabled {
            Some(format!("Microphone/Camera in use ({})", metrics.mic_cam_app))
        } else {
            None
        },
    };
    if config.cond11_mic_cam_enabled && !cond11_pass {
        blockers.push(format!(
            "Mic/Cam In Use ({})",
            if metrics.mic_cam_app.is_empty() { "Meeting" } else { &metrics.mic_cam_app }
        ));
    }

    // Check Agent Lease
    if let Some(until) = state.agent_lease_until_epoch_secs {
        if metrics.now_epoch_secs < until {
            let rem = until - metrics.now_epoch_secs;
            let reason = if state.agent_lease_reason.is_empty() {
                "Agent Task".to_string()
            } else {
                state.agent_lease_reason.clone()
            };
            let pid_info = if let Some(pid) = state.agent_lease_pid {
                format!(" [PID {}]", pid)
            } else {
                String::new()
            };
            blockers.push(format!("Agent Lease: \"{}\"{} ({}s remaining)", reason, pid_info, rem));
        } else {
            state.agent_lease_until_epoch_secs = None;
            state.agent_lease_reason.clear();
            state.agent_lease_pid = None;
        }
    }

    // Check Postpone
    if let Some(until) = state.postponed_until_epoch_secs {
        if metrics.now_epoch_secs < until {
            let rem = until - metrics.now_epoch_secs;
            blockers.push(format!("Postponed ({}s remaining)", rem));
        } else {
            state.postponed_until_epoch_secs = None;
        }
    }

    // Overall Decision & Countdown
    let mut trigger_sleep = false;
    let all_conditions_pass;
    let blocker_banner_text;

    let countdown_max = config.idle_threshold_secs;

    if !config.guard_enabled {
        all_conditions_pass = false;
        state.countdown_secs = config.idle_threshold_secs;
        blocker_banner_text = "PAUSED: Master Guard toggle is OFF. Automatic sleep disabled.".to_string();
    } else if blockers.is_empty() {
        all_conditions_pass = true;
        if state.countdown_secs > 0 {
            state.countdown_secs -= 1;
        }
        blocker_banner_text = format!(
            "ALL CONDITIONS PASS -> PC Sleeping in {}s / {}s",
            state.countdown_secs, countdown_max
        );

        if state.countdown_secs == 0 {
            trigger_sleep = true;
            state.countdown_secs = countdown_max;
        }
    } else {
        all_conditions_pass = false;
        state.countdown_secs = config.idle_threshold_secs;
        let first = &blockers[0];
        let extra = if blockers.len() > 1 {
            format!(" (+{} other conditions)", blockers.len() - 1)
        } else {
            String::new()
        };
        blocker_banner_text = format!("BLOCKED BY: {}{}", first, extra);
    }

    let trigger_grace_banner = all_conditions_pass
        && state.countdown_secs <= config.grace_period_secs
        && state.countdown_secs > 0;

    Decision {
        all_conditions_pass,
        blockers,
        blocker_banner_text,
        countdown_secs: state.countdown_secs,
        countdown_max,
        trigger_sleep,
        trigger_grace_banner,
        log_monitor_off_duration,
        woke_from_sleep_reset,
        cond1_idle,
        cond2_agent_cpu,
        cond3_download,
        cond4_pc_cpu,
        cond5_pc_disk,
        cond6_pc_net,
        cond7_spike_filter,
        cond8_monitor_timeout,
        cond9_power_requests,
        cond10_gpu,
        cond11_mic_cam,
    }
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

    // Phase 2.3: Update Check
    pub update_available: Option<String>,
    pub update_check_in_progress: bool,
    pub update_check_status_msg: Option<String>,

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
            guard_paused_since: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),

            update_available: None,
            update_check_in_progress: false,
            update_check_status_msg: None,

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
        s.load_block_history();
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

    pub fn load_block_history(&mut self) {
        let path = get_block_history_path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let retention_secs = (self.config.block_history_retention_days as u64) * 86400;
            let mut list = Vec::new();
            for line in content.lines() {
                if let Ok(rec) = serde_json::from_str::<BlockRecord>(line) {
                    if retention_secs == 0 || now_epoch.saturating_sub(rec.start_epoch_secs) <= retention_secs {
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
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
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
        let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0".encode_utf16().collect();
        let val_name: Vec<u16> = "AgentPowerGuard\0".encode_utf16().collect();

        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) == 0 {
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

        let mut res = None;
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
                res = Some(max_util);
            }
        }
        FreeLibrary(nvml_lib);
        res
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

fn handle_http_client(mut stream: TcpStream, state: Arc<Mutex<SharedState>>, rate_limiter: Arc<RateLimiter>) {
    let peer_ip = stream.peer_addr().map(|a| a.ip()).unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let (allow_lan, local_ip, configured_token) = {
        let s = state.lock().unwrap();
        (s.config.allow_lan_access, s.local_ip.clone(), s.api_token.clone())
    };

    if !peer_ip.is_loopback() && !allow_lan {
        let body = r#"{"error":"forbidden","message":"LAN access is disabled by configuration"}"#;
        let resp = format!("HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
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
        let host_name = host_header.split(':').next().unwrap_or("").trim().to_lowercase();
        let is_valid_host = host_name == "localhost"
            || host_name == "127.0.0.1"
            || host_name == "[::1]"
            || host_name == "::1"
            || (!local_ip.is_empty() && host_name == local_ip.to_lowercase());
        if !is_valid_host {
            let body = r#"{"error":"forbidden_host","message":"Host header rejected for DNS rebinding protection"}"#;
            let resp = format!("HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = stream.write_all(resp.as_bytes());
            return;
        }
    }

    // Origin validation (Reject cross-origin browser requests)
    if !origin_header.is_empty() {
        let stripped_origin = origin_header
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        let origin_host = stripped_origin.split(':').next().unwrap_or("").trim().to_lowercase();
        let host_name = host_header.split(':').next().unwrap_or("").trim().to_lowercase();
        if !origin_host.is_empty() && origin_host != host_name {
            let body = r#"{"error":"cross_origin_forbidden","message":"Cross-origin requests are rejected"}"#;
            let resp = format!("HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = stream.write_all(resp.as_bytes());
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
                "agent_lease_pid": s.agent_lease_pid,
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
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/postpone") {
        let mins = parse_query_param(path, "mins").and_then(|m| m.parse::<u64>().ok()).unwrap_or(15);
        let mut s = state.lock().unwrap();
        s.postponed_until = Some(Instant::now() + Duration::from_secs(mins * 60));
        s.set_toast(&format!("Sleep postponed by {} minutes via Remote/API.", mins));
        let body = format!(r#"{{"status":"ok","message":"Postponed for {} minutes","mins":{}}}"#, mins, mins);
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/toggle_guard") {
        let mut s = state.lock().unwrap();
        s.config.guard_enabled = !s.config.guard_enabled;
        s.config_dirty = true;
        let body = format!(r#"{{"status":"ok","guard_enabled":{}}}"#, s.config.guard_enabled);
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path.starts_with("/api/lease") {
        let mins = parse_query_param(path, "mins").and_then(|m| m.parse::<u64>().ok()).unwrap_or(30);
        let reason = parse_query_param(path, "reason").unwrap_or_else(|| "Remote Lease".to_string());
        let pid = parse_query_param(path, "pid").and_then(|p| p.parse::<u32>().ok());
        let mut s = state.lock().unwrap();
        s.agent_lease_until = Some(Instant::now() + Duration::from_secs(mins * 60));
        s.agent_lease_reason = reason.clone();
        s.agent_lease_pid = pid;
        let pid_msg = pid.map(|p| format!(" (PID {})", p)).unwrap_or_default();
        s.set_toast(&format!("Agent Lease granted for {}m: {}{}", mins, reason, pid_msg));
        let body = format!(r#"{{"status":"ok","lease_mins":{},"reason":"{}","pid":{:?}}}"#, mins, reason, pid);
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
    let mut stream = TcpStream::connect("127.0.0.1:49281")
        .map_err(|e| format!("Could not connect to running Agent Power Guard instance on 127.0.0.1:49281 (Is the app running?): {}", e))?;
    let req = format!("GET {} HTTP/1.1\r\nHost: 127.0.0.1:49281\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", path, token);
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp).map_err(|e| e.to_string())?;
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
        eprintln!("Usage: agent-power-guard run [--max-mins N] [--reason \"text\"] -- <command...>");
        std::process::exit(1);
    }

    let program = &cmd_parts[0];
    let prog_args = &cmd_parts[1..];
    let lease_reason = reason.unwrap_or_else(|| cmd_parts.join(" "));

    println!("⚡ [Agent Power Guard] Spawning child: {} {:?}", program, prog_args);
    let mut child = match Command::new(program).args(prog_args).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to spawn child command '{}': {}", program, e);
            std::process::exit(1);
        }
    };

    let pid = child.id();
    let encoded_reason = lease_reason.replace(' ', "%20");
    let path = format!("/api/lease?mins={}&reason={}&pid={}", max_mins, encoded_reason, pid);
    if let Err(e) = send_cli_request(&path) {
        eprintln!("Warning: Failed to acquire PID lease from Agent Power Guard watchdog: {}", e);
    } else {
        println!("🔒 [Agent Power Guard] Acquired PID-bound lease (PID {}, {}m cap): {}", pid, max_mins, lease_reason);
    }

    let status = child.wait();
    let _ = send_cli_request("/api/release");
    println!("🔓 [Agent Power Guard] Child process exited. Lease released.");

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
        if n == 0 { break; }
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
                    let tool_name = params.and_then(|p| p.get("name")).and_then(|n| n.as_str()).unwrap_or("");
                    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(serde_json::json!({}));

                    let result_text = match tool_name {
                        "keep_awake" => {
                            let mins = arguments.get("minutes").and_then(|m| m.as_u64()).unwrap_or(30);
                            let reason = arguments.get("reason").and_then(|r| r.as_str()).unwrap_or("MCP Agent Task");
                            let path = format!("/api/lease?mins={}&reason={}", mins, reason.replace(' ', "%20"));
                            send_cli_request(&path).unwrap_or_else(|e| format!("Error: {}", e))
                        }
                        "release" => {
                            send_cli_request("/api/release").unwrap_or_else(|e| format!("Error: {}", e))
                        }
                        "status" => {
                            send_cli_request("/api/status").unwrap_or_else(|e| format!("Error: {}", e))
                        }
                        "postpone" => {
                            let mins = arguments.get("minutes").and_then(|m| m.as_u64()).unwrap_or(15);
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
            description: "Watches node, npm, pnpm, bun, tsc, vite, and next dev servers".to_string(),
            keywords: "node, npm, pnpm, bun, tsc, esbuild, vite".to_string(),
            agent_cpu_threshold: 3.0,
            gpu_threshold_pct: 5.0,
        },
        AgentProfile {
            name: "Python ML".to_string(),
            description: "Watches python training scripts, PyTorch, and Jupyter kernels".to_string(),
            keywords: "python, torch, jupyter".to_string(),
            agent_cpu_threshold: 5.0,
            gpu_threshold_pct: 10.0,
        },
    ]
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
                        if let Some(pid) = v["agent_lease_pid"].as_u64() {
                            println!("Bound Process PID: {}", pid);
                        }
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
            println!("  agent-power-guard run [--max-mins N] [--reason \"text\"] -- <cmd...>  - Keep awake while command runs (process-bound lease)");
            println!("  agent-power-guard mcp                                              - Launch stdio Model Context Protocol (MCP) server");
            println!("  agent-power-guard lease [mins] [reason]                            - Block sleep with an active agent lease");
            println!("  agent-power-guard release                                          - Release active agent lease");
            println!("  agent-power-guard status                                           - Query watchdog status");
            println!("  agent-power-guard postpone [mins]                                  - Postpone sleep by N minutes");
            println!("  agent-power-guard sleep                                            - Trigger immediate system sleep");
            println!("  agent-power-guard help                                             - Show this help message");
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

        loop {
            let loop_start = Instant::now();

            // 1. Check Wall-Clock Wakeup
            let now_wall = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let mut woke_from_sleep = false;
            {
                let mut s = state.lock().unwrap();
                if s.last_wall_clock_secs > 0 && now_wall > s.last_wall_clock_secs + 10 {
                    s.countdown_secs = s.countdown_max;
                    s.set_toast("System woke from sleep! Watchdog countdown reset.");
                    s.pc_spike_sustained_counter = 0;
                    woke_from_sleep = true;
                }
                s.last_wall_clock_secs = now_wall;
            }

            if woke_from_sleep {
                std::thread::sleep(Duration::from_millis(1500));
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

            let trigger_sleep;

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

                let ignored_keywords: Vec<String> = s
                    .config
                    .ignored_processes
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

                    // Phase 1.3: Ignore list
                    if ignored_keywords.iter().any(|k| proc_name.contains(k)) {
                        continue;
                    }

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

                // Phase 1.1: Check PID aliveness for PID-bound lease
                if let Some(pid) = s.agent_lease_pid {
                    let sys_pid = sysinfo::Pid::from_u32(pid);
                    if !sys.processes().contains_key(&sys_pid) {
                        s.agent_lease_until = None;
                        s.agent_lease_reason.clear();
                        s.agent_lease_pid = None;
                        s.set_toast(&format!("PID-bound lease auto-released: Process {} exited.", pid));
                    }
                }

                // Phase 0.2: Pure Condition Evaluation via evaluate()
                let metrics = Metrics {
                    idle_secs,
                    max_agent_cpu,
                    max_agent_proc: max_agent_proc.clone(),
                    max_dl_kbps,
                    max_dl_proc: max_dl_proc.clone(),
                    whole_pc_cpu,
                    whole_pc_disk_kbps,
                    whole_pc_net_kbps,
                    gpu_util,
                    mic_active,
                    cam_active,
                    mic_cam_app: mic_cam_app.clone(),
                    display_request_active: cached_display_request,
                    sensor_error: false,
                    now_epoch_secs: now_wall,
                };

                let mut eval_state = EvaluationState {
                    countdown_secs: s.countdown_secs,
                    pc_spike_sustained_counter: s.pc_spike_sustained_counter,
                    postponed_until_epoch_secs: s.postponed_until.map(|i| {
                        let rem = i.saturating_duration_since(Instant::now()).as_secs();
                        now_wall + rem
                    }),
                    agent_lease_until_epoch_secs: s.agent_lease_until.map(|i| {
                        let rem = i.saturating_duration_since(Instant::now()).as_secs();
                        now_wall + rem
                    }),
                    agent_lease_reason: s.agent_lease_reason.clone(),
                    agent_lease_pid: s.agent_lease_pid,
                    last_wall_clock_secs: s.last_wall_clock_secs,
                    monitor_standby_start_epoch_secs: s.monitor_standby_start.map(|i| {
                        let dur = i.elapsed().as_secs();
                        now_wall.saturating_sub(dur)
                    }),
                };

                let decision = evaluate(&s.config, &metrics, &mut eval_state);

                s.countdown_secs = decision.countdown_secs;
                s.countdown_max = decision.countdown_max;
                s.all_conditions_pass = decision.all_conditions_pass;
                s.blocker_banner_text = decision.blocker_banner_text;
                s.pc_spike_sustained_counter = eval_state.pc_spike_sustained_counter;
                s.grace_active = decision.trigger_grace_banner;
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

                if let Some(dur) = decision.log_monitor_off_duration {
                    s.log_energy_event("monitor_off", dur);
                }

                // Phase 2.1: Block History Tracking
                if s.config.block_history_enabled {
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

                // Phase 2.2: Notifications
                if decision.trigger_grace_banner && !s.last_grace_active {
                    if s.config.notify_on_sleep_imminent && now_wall >= s.last_sleep_imminent_notif + 60 {
                        s.last_sleep_imminent_notif = now_wall;
                        send_notification(&s.config, &NotificationEvent::SleepImminent { countdown_secs: decision.countdown_secs });
                    }
                }
                s.last_grace_active = decision.trigger_grace_banner;

                if decision.woke_from_sleep_reset {
                    if s.config.notify_on_pc_woke && now_wall >= s.last_pc_woke_notif + 60 {
                        s.last_pc_woke_notif = now_wall;
                        send_notification(&s.config, &NotificationEvent::PcWoke);
                    }
                }

                if let Some(start) = s.current_block_start_epoch_secs {
                    let blocked_secs = now_wall.saturating_sub(start);
                    let threshold_secs = (s.config.notify_blocked_long_hours as u64) * 3600;
                    if blocked_secs >= threshold_secs && now_wall >= s.last_blocked_long_notif + 3600 {
                        if s.config.notify_on_blocked_long {
                            s.last_blocked_long_notif = now_wall;
                            let blocker_name = s.current_primary_blocker.clone().unwrap_or_else(|| "Unknown Blocker".to_string());
                            send_notification(&s.config, &NotificationEvent::BlockedLong {
                                hours: s.config.notify_blocked_long_hours,
                                blocker: blocker_name,
                            });
                        }
                    }
                }

                if !s.config.guard_enabled {
                    let paused_secs = now_wall.saturating_sub(s.guard_paused_since);
                    let threshold_secs = (s.config.notify_guard_paused_hours as u64) * 3600;
                    if paused_secs >= threshold_secs && now_wall >= s.last_guard_paused_notif + 3600 {
                        if s.config.notify_on_guard_paused_long {
                            s.last_guard_paused_notif = now_wall;
                            send_notification(&s.config, &NotificationEvent::GuardPausedLong {
                                hours: s.config.notify_guard_paused_hours,
                            });
                        }
                    }
                } else {
                    s.guard_paused_since = now_wall;
                }

                // Phase 2.3: Opt-in Update Check
                if s.config.check_for_updates && now_wall >= s.config.last_update_check_epoch_secs + 86400 {
                    s.config.last_update_check_epoch_secs = now_wall;
                    s.config_dirty = true;
                    trigger_update_check(state.clone());
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

pub fn open_url(url: &str) {
    #[cfg(windows)]
    let _ = Command::new("cmd").args(["/c", "start", url]).creation_flags(CREATE_NO_WINDOW).status();
    #[cfg(target_os = "linux")]
    let _ = Command::new("xdg-open").arg(url).status();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).status();
}

pub fn trigger_update_check(state: Arc<Mutex<SharedState>>) {
    {
        let mut s = state.lock().unwrap();
        if s.update_check_in_progress {
            return;
        }
        s.update_check_in_progress = true;
        s.update_check_status_msg = Some("Checking GitHub Releases...".to_string());
    }

    std::thread::spawn(move || {
        let mut cmd = Command::new("curl");
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.args([
            "-s", "--max-time", "10", "--connect-timeout", "5",
            "-H", &format!("User-Agent: AgentPowerGuard/{}", env!("CARGO_PKG_VERSION")),
            "https://api.github.com/repos/Subham-Maity/agent-power-guard/releases/latest"
        ]);

        let res = cmd.output();
        let mut s = state.lock().unwrap();
        s.update_check_in_progress = false;

        match res {
            Ok(output) if output.status.success() => {
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                    if let Some(tag) = val["tag_name"].as_str() {
                        let clean = tag.trim_start_matches('v');
                        let current = env!("CARGO_PKG_VERSION");
                        if is_newer_version(clean, current) {
                            s.update_available = Some(tag.to_string());
                            s.update_check_status_msg = Some(format!("New release available: {}", tag));
                        } else {
                            s.update_available = None;
                            s.update_check_status_msg = Some(format!("Up to date (v{})", current));
                        }
                    } else {
                        s.update_check_status_msg = Some("No release tag found.".to_string());
                    }
                } else {
                    s.update_check_status_msg = Some("Failed to parse release response.".to_string());
                }
            }
            Ok(_) => {
                s.update_check_status_msg = Some("Update check received non-200 response.".to_string());
            }
            Err(e) => {
                s.update_check_status_msg = Some(format!("Update check failed: {}", e));
            }
        }
    });
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

        // Launch embedded Web Server & Remote API with Rate Limiting
        let rate_limiter = Arc::new(RateLimiter::new());
        start_web_server(Arc::clone(&state), 49281, rate_limiter);

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

            // Phase 2.3: Update Available Banner
            if let Some(new_ver) = &s.update_available {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(30, 58, 138))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("🚀 New release available: {} (Current: v{})", new_ver, env!("CARGO_PKG_VERSION"))).strong().color(egui::Color32::from_rgb(147, 197, 253)));
                            if ui.button("View on GitHub").clicked() {
                                open_url("https://github.com/Subham-Maity/agent-power-guard/releases");
                            }
                        });
                    });
                ui.add_space(4.0);
            }

            // Tab Navigation Bar
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.active_tab, ActiveTab::Guard, egui::RichText::new("🛡️ Watchdog & Presets").strong());
                ui.selectable_value(&mut s.active_tab, ActiveTab::History, egui::RichText::new("📜 Block History").strong());
                ui.selectable_value(&mut s.active_tab, ActiveTab::Energy, egui::RichText::new("💰 Energy Savings").strong());
                ui.selectable_value(&mut s.active_tab, ActiveTab::Notifications, egui::RichText::new("🔔 Alerts & Updates").strong());
                ui.selectable_value(&mut s.active_tab, ActiveTab::Remote, egui::RichText::new("🌐 Remote").strong());
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
        // TAB 2: BLOCK HISTORY ("WHAT KEPT MY PC AWAKE") VIEW
        // ====================================================================
        else if s.active_tab == ActiveTab::History {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading("📜 What Kept My PC Awake (Block History)");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("🗑️ Clear History").clicked() {
                            s.clear_block_history();
                            s.set_toast("Block reason history cleared.");
                        }
                        if ui.button("📋 Copy CSV").clicked() {
                            let csv = export_block_history_csv(&s.block_history);
                            ui.ctx().copy_text(csv);
                            s.set_toast("History CSV copied to clipboard.");
                        }
                        if ui.button("📋 Copy JSON").clicked() {
                            let json = export_block_history_json(&s.block_history);
                            ui.ctx().copy_text(json);
                            s.set_toast("History JSON copied to clipboard.");
                        }
                    });
                });

                ui.add_space(6.0);
                let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

                // 1. Metric Summary Cards
                let top_blockers = calculate_top_blockers(&s.block_history);
                let total_blocked_secs: u64 = s.block_history.iter().map(|r| r.duration_secs).sum();
                let last_night = calculate_last_night_summary(&s.block_history, now_epoch);

                ui.horizontal(|ui| {
                    // Card 1: Total Blocked Time
                    egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                        ui.label(egui::RichText::new("⏱️ Total Blocked Time").weak().size(12.0));
                        ui.label(egui::RichText::new(format_human_duration(total_blocked_secs)).strong().size(18.0).color(egui::Color32::from_rgb(245, 158, 11)));
                    });

                    // Card 2: Top Blocker Overall
                    egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                        ui.label(egui::RichText::new("🛑 Top Blocker").weak().size(12.0));
                        let top_name = top_blockers.first().map(|t| t.name.as_str()).unwrap_or("None");
                        let top_time = top_blockers.first().map(|t| format_human_duration(t.total_secs)).unwrap_or_default();
                        ui.label(egui::RichText::new(format!("{} ({})", top_name, top_time)).strong().size(14.0).color(egui::Color32::from_rgb(239, 68, 68)));
                    });

                    // Card 3: Last Night Summary
                    egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                        ui.label(egui::RichText::new("🌙 Last Night (Past 12h)").weak().size(12.0));
                        let top = last_night.top_blocker.unwrap_or_else(|| "All Quiet".to_string());
                        ui.label(egui::RichText::new(format!("Blocked: {} | Top: {}", format_human_duration(last_night.total_blocked_secs), top)).strong().size(13.0).color(egui::Color32::from_rgb(147, 197, 253)));
                    });
                });

                ui.add_space(8.0);

                // Config Controls Row
                ui.horizontal(|ui| {
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
        // TAB 4: ALERTS, NOTIFICATIONS & OPT-IN UPDATES VIEW
        // ====================================================================
        else if s.active_tab == ActiveTab::Notifications {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading("🔔 Alerts, Webhooks & Opt-in Updates");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("🔔 Send Test Notification").clicked() {
                            send_notification(&s.config, &NotificationEvent::TestMessage);
                            s.set_toast("Test notification dispatched across configured channels.");
                        }
                    });
                });

                ui.add_space(6.0);
                egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                    ui.label(egui::RichText::new("🔒 Privacy Guarantee: Outbound notifications and update checks are completely opt-in and off by default. Secrets and tokens are stored locally on your machine and never logged or sent to any telemetry service.").weak().size(12.0));
                });

                ui.add_space(10.0);

                // Section 1: Notification Channels
                egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                    ui.label(egui::RichText::new("📡 Outbound Channels").strong().size(14.0));
                    ui.add_space(6.0);

                    // 1. ntfy
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("ntfy URL:").strong().size(12.0));
                        if ui.add(egui::TextEdit::singleline(&mut s.config.notify_ntfy_url).hint_text("https://ntfy.sh/your_private_topic")).changed() {
                            s.config_dirty = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("ntfy Token (optional):").size(12.0));
                        if ui.add(egui::TextEdit::singleline(&mut s.config.notify_ntfy_token).password(true).hint_text("Bearer token if topic is protected")).changed() {
                            s.config_dirty = true;
                        }
                    });

                    ui.separator();

                    // 2. Telegram
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Telegram Bot Token:").strong().size(12.0));
                        if ui.add(egui::TextEdit::singleline(&mut s.config.notify_telegram_token).password(true).hint_text("123456789:ABCdefGhIJKlmNoPQRsTUVwxyZ")).changed() {
                            s.config_dirty = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Telegram Chat ID:").strong().size(12.0));
                        if ui.add(egui::TextEdit::singleline(&mut s.config.notify_telegram_chat_id).hint_text("-100123456789 or user id")).changed() {
                            s.config_dirty = true;
                        }
                    });

                    ui.separator();

                    // 3. Generic Webhook
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Webhook URL (POST):").strong().size(12.0));
                        if ui.add(egui::TextEdit::singleline(&mut s.config.notify_webhook_url).hint_text("https://discord.com/api/webhooks/... or custom server")).changed() {
                            s.config_dirty = true;
                        }
                    });
                });

                ui.add_space(10.0);

                // Section 2: Event Subscriptions
                egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                    ui.label(egui::RichText::new("⚡ Trigger Events").strong().size(14.0));
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

                // Section 3: Opt-in Update Check
                egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                    ui.label(egui::RichText::new("🚀 Software Updates").strong().size(14.0));
                    ui.add_space(6.0);

                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut s.config.check_for_updates, "Check for updates automatically (daily check via GitHub Releases API)").changed() {
                            s.config_dirty = true;
                        }

                        if ui.button("🔍 Check Now").clicked() {
                            let state_clone = self.state.clone();
                            trigger_update_check(state_clone);
                        }
                    });

                    ui.add_space(4.0);
                    if let Some(msg) = &s.update_check_status_msg {
                        ui.label(egui::RichText::new(format!("Update Status: {}", msg)).color(egui::Color32::from_rgb(147, 197, 253)));
                    } else {
                        ui.label(egui::RichText::new(format!("Current Version: v{}", env!("CARGO_PKG_VERSION"))).weak());
                    }
                });
            });
        }

        // ====================================================================
        // TAB 5: WEB & MOBILE REMOTE VIEW
        // ====================================================================
        else if s.active_tab == ActiveTab::Remote {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.add_space(4.0);
                ui.heading("🌐 Web & Mobile Remote Dashboard");
                ui.label(egui::RichText::new("Control Agent Power Guard from your phone, tablet, or another browser on your local Wi-Fi.").weak());
                ui.separator();

                // 0. LAN Access Toggle & API Security
                egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(10.0).inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
                    ui.heading("🔒 API & Network Security");
                    ui.add_space(4.0);

                    if ui.checkbox(&mut s.config.allow_lan_access, egui::RichText::new("Allow access from local network (LAN)").strong()).changed() {
                        s.config_dirty = true;
                    }
                    ui.label(egui::RichText::new("When disabled (default), only this computer (127.0.0.1) can reach the API and dashboard. When enabled, other devices on your Wi-Fi/LAN can connect.").weak().size(12.0));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("API Token:").strong());
                        let masked_token = format!("{}...{}", &s.api_token[..8], &s.api_token[56..]);
                        ui.label(egui::RichText::new(&masked_token).monospace().color(egui::Color32::from_rgb(245, 158, 11)));

                        if ui.button("📋 Copy Token").clicked() {
                            ui.ctx().copy_text(s.api_token.clone());
                            s.set_toast("API token copied to clipboard.");
                        }

                        if ui.button("🔄 Regenerate Token").clicked() {
                            let new_token = regenerate_api_token();
                            s.api_token = new_token;
                            s.set_toast("API token regenerated successfully.");
                        }
                    });
                });

                ui.add_space(10.0);

                let local_url = format!("http://{}:{}/#t={}", s.local_ip, s.config.web_server_port, s.api_token);

                egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(10.0).inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("📱 Remote Mobile URL:").strong().size(14.0));
                        ui.label(egui::RichText::new(&local_url).strong().size(14.0).color(egui::Color32::from_rgb(52, 211, 153)));

                        if ui.button("🌐 Open in Browser").clicked() {
                            #[cfg(windows)]
                            let _ = Command::new("cmd").args(["/c", "start", &local_url]).creation_flags(CREATE_NO_WINDOW).status();
                            #[cfg(target_os = "linux")]
                            let _ = Command::new("xdg-open").arg(&local_url).status();
                            #[cfg(target_os = "macos")]
                            let _ = Command::new("open").arg(&local_url).status();
                        }

                        if ui.button("📋 Copy Link with Token").clicked() {
                            ui.ctx().copy_text(local_url.clone());
                            s.set_toast("Mobile remote link with token copied to clipboard.");
                        }
                    });

                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Open this URL on your phone while connected to the same Wi-Fi. The token fragment auto-authenticates your phone so you can view live condition graphs, monitor countdown, put the PC to sleep immediately, postpone sleep, or grant/release agent leases!").weak());
                });

                ui.add_space(10.0);

                // Phase 1.3: Agent Profiles One-Click Selector
                egui::Frame::new().fill(egui::Color32::from_rgb(22, 29, 39)).corner_radius(10.0).inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
                    ui.heading("🤖 AI Agent Profiles (One-Click Presets)");
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Apply tailored process watchlists and thresholds for specific AI coding tools and workloads:").weak().size(12.0));
                    ui.add_space(8.0);

                    let profiles = get_built_in_profiles();
                    ui.horizontal_wrapped(|ui| {
                        for p in &profiles {
                            if ui.button(format!("⚡ {}", p.name)).clicked() {
                                s.config.agent_keywords = p.keywords.clone();
                                s.config.agent_cpu_threshold = p.agent_cpu_threshold;
                                s.config.gpu_threshold_pct = p.gpu_threshold_pct;
                                s.config_dirty = true;
                                s.set_toast(format!("Applied profile: {} ({})", p.name, p.keywords));
                            }
                        }
                    });
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
    // 0. Set up Diagnostic Panic Logger
    std::panic::set_hook(Box::new(|info| {
        let log_file = get_config_path().parent().unwrap_or(&PathBuf::from(".")).join("crash.log");
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
        let log_file = get_config_path().parent().unwrap_or(&PathBuf::from(".")).join("error.log");
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
        cfg.grace_period_secs = 30;
        cfg.guard_enabled = true;
        cfg
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

        let decision = evaluate(&cfg, &metrics, &mut state);
        assert!(decision.all_conditions_pass);
        assert!(decision.blockers.is_empty());
        assert_eq!(decision.countdown_secs, 179);
    }

    #[test]
    fn test_cond1_human_idle_boundary() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };

        // 179s idle -> blocked
        metrics.idle_secs = 179;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert!(!d.cond1_idle.is_passing);

        // 180s idle -> passes
        metrics.idle_secs = 180;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(d.cond1_idle.is_passing);
    }

    #[test]
    fn test_cond2_agent_cpu_boundary() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };

        // 1.9% -> pass
        metrics.max_agent_cpu = 1.9;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(d.cond2_agent_cpu.is_passing);

        // 2.1% -> blocked
        metrics.max_agent_cpu = 2.1;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.cond2_agent_cpu.is_passing);
        assert!(!d.all_conditions_pass);
    }

    #[test]
    fn test_cond3_download_boundary() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };

        // 99 KB/s -> pass
        metrics.max_dl_kbps = 99.0;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(d.cond3_download.is_passing);

        // 101 KB/s -> blocked
        metrics.max_dl_kbps = 101.0;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.cond3_download.is_passing);
        assert!(!d.all_conditions_pass);
    }

    #[test]
    fn test_cond4_5_6_pc_metrics() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };

        // PC CPU high
        metrics.whole_pc_cpu = 45.0;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.cond4_pc_cpu.is_passing);

        // PC Disk high
        metrics.whole_pc_cpu = 5.0;
        metrics.whole_pc_disk_kbps = 6000.0;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.cond5_pc_disk.is_passing);

        // PC Net high
        metrics.whole_pc_disk_kbps = 50.0;
        metrics.whole_pc_net_kbps = 200.0;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.cond6_pc_net.is_passing);
    }

    #[test]
    fn test_cond7_sustained_spike_filter() {
        let cfg = make_test_config(); // spike_filter_secs = 15
        let mut metrics = make_passing_metrics();
        metrics.whole_pc_cpu = 50.0; // Spiking

        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };

        // 14 ticks: spike not yet sustained -> spike filter passes!
        for _ in 0..14 {
            let d = evaluate(&cfg, &metrics, &mut state);
            assert!(d.cond7_spike_filter.is_passing);
            assert!(d.all_conditions_pass);
        }

        // 15th tick: sustained spike trips!
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.cond7_spike_filter.is_passing);
        assert!(!d.all_conditions_pass);

        // Activity subsides: counter resets immediately
        metrics.whole_pc_cpu = 5.0;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(d.cond7_spike_filter.is_passing);
        assert_eq!(state.pc_spike_sustained_counter, 0);
    }

    #[test]
    fn test_cond9_display_power_request() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };

        metrics.display_request_active = true;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.cond9_power_requests.is_passing);
        assert!(!d.all_conditions_pass);
    }

    #[test]
    fn test_cond10_gpu_boundary() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };

        metrics.gpu_util = 4.9;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(d.cond10_gpu.is_passing);

        metrics.gpu_util = 5.1;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.cond10_gpu.is_passing);
        assert!(!d.all_conditions_pass);
    }

    #[test]
    fn test_cond11_mic_cam_active() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };

        metrics.mic_active = true;
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.cond11_mic_cam.is_passing);
        assert!(!d.all_conditions_pass);

        metrics.mic_active = false;
        metrics.cam_active = true;
        let d = evaluate(&cfg, &metrics, &mut state);
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

        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };
        let d = evaluate(&cfg, &metrics, &mut state);
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
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert!(d.blockers.iter().any(|b| b.contains("Heavy compilation")));

        // Expired lease clears and allows
        state.agent_lease_until_epoch_secs = Some(900);
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(d.all_conditions_pass);
        assert!(state.agent_lease_until_epoch_secs.is_none());

        // Postpone blocks
        state.postponed_until_epoch_secs = Some(1500);
        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert!(d.blockers.iter().any(|b| b.contains("Postponed")));
    }

    #[test]
    fn test_master_guard_disabled() {
        let mut cfg = make_test_config();
        cfg.guard_enabled = false;
        let metrics = make_passing_metrics();
        let mut state = EvaluationState { countdown_secs: 50, last_wall_clock_secs: 999, ..Default::default() };

        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert_eq!(state.countdown_secs, cfg.idle_threshold_secs);
        assert!(d.blocker_banner_text.contains("PAUSED: Master Guard"));
    }

    #[test]
    fn test_resume_from_sleep_reset() {
        let cfg = make_test_config();
        let metrics = make_passing_metrics();
        let mut state = EvaluationState {
            countdown_secs: 10,
            last_wall_clock_secs: 950, // Jumped by 50 seconds
            pc_spike_sustained_counter: 12,
            ..Default::default()
        };

        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(d.woke_from_sleep_reset);
        assert_eq!(state.countdown_secs, cfg.idle_threshold_secs - 1);
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

        let d = evaluate(&cfg, &metrics, &mut state);
        assert_eq!(d.countdown_secs, 30);
        assert!(d.trigger_grace_banner);
    }

    #[test]
    fn test_sensor_error_blocks_sleep() {
        let cfg = make_test_config();
        let mut metrics = make_passing_metrics();
        metrics.sensor_error = true;
        let mut state = EvaluationState { countdown_secs: 180, last_wall_clock_secs: 999, ..Default::default() };

        let d = evaluate(&cfg, &metrics, &mut state);
        assert!(!d.all_conditions_pass);
        assert!(d.blockers.iter().any(|b| b.contains("Sensor Failure")));
    }

    #[test]
    fn test_constant_time_eq() {
        let token = "a1b2c3d4e5f67890123456789012345678901234567890123456789012345678";
        assert!(constant_time_eq(token, token));
        assert!(!constant_time_eq(token, "wrong"));
        assert!(!constant_time_eq(token, "a1b2c3d4e5f67890123456789012345678901234567890123456789012345679"));
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
        assert_eq!(summary.top_blocker.as_deref(), Some("Agent CPU (node 4.5%)"));
        assert_eq!(summary.event_count, 3);
    }

    #[test]
    fn test_block_history_csv_export() {
        let history = vec![
            BlockRecord {
                start_epoch_secs: 1000,
                end_epoch_secs: 1060,
                duration_secs: 60,
                primary_blocker: "Whole-PC CPU (45%)".to_string(),
                all_blockers: vec!["Whole-PC CPU (45%)".to_string(), "Network".to_string()],
            },
        ];
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

        let e2 = NotificationEvent::BlockedLong { hours: 2, blocker: "Agent CPU".to_string() };
        assert_eq!(e2.event_name(), "blocked_long");
        let (title2, msg2) = e2.title_and_message();
        assert!(title2.contains("Machine Blocked"));
        assert!(msg2.contains("Agent CPU"));

        let e3 = NotificationEvent::TestMessage;
        assert_eq!(e3.event_name(), "test");
    }
}
