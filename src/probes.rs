use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sysinfo::{Networks, System};

use crate::intent::{
    MediaSessionSnapshot, PowerRequestOwner, PowerRequestSnapshot, WakeClassification,
};
use crate::signal::{MicSignalClassifier, MicSignalState, ProbeHealth};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// ============================================================================
// 1. Dedicated OS Wake Lock Worker Thread (§6 Best Performance Mode)
// ============================================================================

/// Holds an OS-level wake lock (`SetThreadExecutionState` on Windows,
/// `systemd-inhibit` / `caffeinate` on Unix) on a dedicated long-lived thread.
/// Win32 execution-state flags are per-thread, so dedicating a thread guarantees
/// they persist while `Best Performance` is enabled and clear cleanly on disable/exit.
pub struct WakeLockController {
    inner: Arc<(Mutex<(bool, bool)>, Condvar)>, // (desired_locked, shutdown)
}

impl WakeLockController {
    pub fn start() -> Self {
        let pair = Arc::new((Mutex::new((false, false)), Condvar::new()));
        let worker = Arc::clone(&pair);

        std::thread::spawn(move || {
            let mut currently_locked = false;
            #[cfg(not(windows))]
            let mut child_guard: Option<std::process::Child> = None;

            loop {
                let (desired, shutdown) = {
                    let (lock, cvar) = &*worker;
                    let mut guard = lock.lock().unwrap();
                    while guard.0 == currently_locked && !guard.1 {
                        guard = cvar.wait(guard).unwrap();
                    }
                    (guard.0, guard.1)
                };

                if shutdown {
                    if currently_locked {
                        #[cfg(windows)]
                        unsafe {
                            windows_sys::Win32::System::Power::SetThreadExecutionState(
                                windows_sys::Win32::System::Power::ES_CONTINUOUS,
                            );
                        }
                        #[cfg(not(windows))]
                        if let Some(mut c) = child_guard.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                    }
                    break;
                }

                if desired != currently_locked {
                    #[cfg(windows)]
                    unsafe {
                        use windows_sys::Win32::System::Power::{
                            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED,
                            ES_SYSTEM_REQUIRED,
                        };
                        if desired {
                            SetThreadExecutionState(
                                ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
                            );
                        } else {
                            SetThreadExecutionState(ES_CONTINUOUS);
                        }
                    }

                    #[cfg(target_os = "macos")]
                    {
                        if desired {
                            child_guard =
                                Command::new("caffeinate").args(["-d", "-i"]).spawn().ok();
                        } else if let Some(mut c) = child_guard.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                    }

                    #[cfg(target_os = "linux")]
                    {
                        if desired {
                            child_guard = Command::new("systemd-inhibit")
                                .args([
                                    "--what=idle:sleep:handle-lid-switch",
                                    "--who=AgentPowerGuard",
                                    "--why=Best Performance mode locked awake",
                                    "sleep",
                                    "infinity",
                                ])
                                .spawn()
                                .ok();
                        } else if let Some(mut c) = child_guard.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                    }

                    currently_locked = desired;
                }
            }
        });

        Self { inner: pair }
    }

    pub fn set_locked(&self, locked: bool) {
        let (lock, cvar) = &*self.inner;
        let mut g = lock.lock().unwrap();
        if g.0 != locked {
            g.0 = locked;
            cvar.notify_one();
        }
    }
}

impl Drop for WakeLockController {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.inner;
        let mut g = lock.lock().unwrap();
        g.0 = false;
        g.1 = true;
        cvar.notify_one();
    }
}

// ============================================================================
// 2. Wake Event Classifier (§5.9)
// ============================================================================

#[derive(Default)]
pub struct WakeEventTracker {
    last_auto_resume_epoch: AtomicU64,
    last_user_resume_epoch: AtomicU64,
    registered: AtomicBool,
}

impl WakeEventTracker {
    pub fn start() -> Arc<Self> {
        let tracker = Arc::new(Self {
            last_auto_resume_epoch: AtomicU64::new(0),
            last_user_resume_epoch: AtomicU64::new(0),
            registered: AtomicBool::new(false),
        });

        #[cfg(windows)]
        {
            let weak = Arc::downgrade(&tracker);
            let boxed = Box::into_raw(Box::new(weak));
            unsafe {
                use windows_sys::Win32::Foundation::HANDLE;
                use windows_sys::Win32::System::Power::{
                    PowerRegisterSuspendResumeNotification, DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS,
                };
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND,
                };

                unsafe extern "system" fn power_cb(
                    context: *const core::ffi::c_void,
                    event_type: u32,
                    _setting: *const core::ffi::c_void,
                ) -> u32 {
                    if !context.is_null() {
                        let weak_ref = &*(context as *const std::sync::Weak<WakeEventTracker>);
                        if let Some(arc) = weak_ref.upgrade() {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            if event_type == PBT_APMRESUMEAUTOMATIC {
                                arc.last_auto_resume_epoch.store(now, Ordering::Relaxed);
                            } else if event_type == PBT_APMRESUMESUSPEND {
                                arc.last_user_resume_epoch.store(now, Ordering::Relaxed);
                            }
                        }
                    }
                    0
                }

                let params = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                    Callback: Some(power_cb),
                    Context: boxed as *mut core::ffi::c_void,
                };
                let mut reg_handle: *mut core::ffi::c_void = std::ptr::null_mut();
                let res = PowerRegisterSuspendResumeNotification(
                    DEVICE_NOTIFY_CALLBACK,
                    &params as *const _ as HANDLE,
                    &mut reg_handle,
                );
                if res == 0 {
                    tracker.registered.store(true, Ordering::Relaxed);
                } else {
                    drop(Box::from_raw(boxed));
                }
            }
        }

        tracker
    }

    pub fn classify_resume(
        &self,
        now_epoch: u64,
        idle_secs: u32,
        scheduled_wake_hhmm: &[(bool, &str)],
        cur_hour: u32,
        cur_min: u32,
    ) -> WakeClassification {
        let user_resume = self.last_user_resume_epoch.load(Ordering::Relaxed);
        if idle_secs < 10 || (user_resume > 0 && now_epoch.saturating_sub(user_resume) <= 15) {
            return WakeClassification::UserInput;
        }

        // Check if current local minute is within +-3 mins of an enabled wake timer
        let cur_total = (cur_hour * 60 + cur_min) as i32;
        for (enabled, hhmm) in scheduled_wake_hhmm {
            if !*enabled {
                continue;
            }
            if let Some((h, m)) = hhmm.split_once(':') {
                if let (Ok(th), Ok(tm)) = (h.trim().parse::<i32>(), m.trim().parse::<i32>()) {
                    let target_total = th * 60 + tm;
                    let diff = (cur_total - target_total).abs();
                    if diff <= 3 || (1440 - diff) <= 3 {
                        return WakeClassification::ScheduledTimer;
                    }
                }
            }
        }

        WakeClassification::UnattendedUnknown
    }
}

// ============================================================================
// 3. NVML GPU Utilization + Board Power Draw Probe (§5.1)
// ============================================================================

#[derive(Debug, Clone)]
pub struct GpuProbeReading {
    pub util_pct: f32,
    pub power_mw: Option<u32>,
    pub health: ProbeHealth,
}

pub fn sample_gpu_probe() -> GpuProbeReading {
    #[cfg(windows)]
    {
        unsafe {
            use windows_sys::Win32::Foundation::FreeLibrary;
            use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

            let nvml_lib = LoadLibraryA(b"nvml.dll\0".as_ptr());
            if nvml_lib.is_null() {
                return GpuProbeReading {
                    util_pct: 0.0,
                    power_mw: None,
                    health: ProbeHealth::Unavailable("NVML driver library not present".to_string()),
                };
            }

            type NvmlInit = unsafe extern "C" fn() -> u32;
            type NvmlShutdown = unsafe extern "C" fn() -> u32;
            type NvmlDeviceGetCount = unsafe extern "C" fn(*mut u32) -> u32;
            type NvmlDeviceGetHandleByIndex = unsafe extern "C" fn(u32, *mut usize) -> u32;
            type NvmlDeviceGetPowerUsage = unsafe extern "C" fn(usize, *mut u32) -> u32;
            #[repr(C)]
            struct NvmlUtilization {
                gpu: u32,
                memory: u32,
            }
            type NvmlDeviceGetUtilizationRates =
                unsafe extern "C" fn(usize, *mut NvmlUtilization) -> u32;

            let init_fn: Option<NvmlInit> =
                std::mem::transmute(GetProcAddress(nvml_lib, b"nvmlInit_v2\0".as_ptr()));
            let count_fn: Option<NvmlDeviceGetCount> = std::mem::transmute(GetProcAddress(
                nvml_lib,
                b"nvmlDeviceGetCount_v2\0".as_ptr(),
            ));
            let get_handle_fn: Option<NvmlDeviceGetHandleByIndex> = std::mem::transmute(
                GetProcAddress(nvml_lib, b"nvmlDeviceGetHandleByIndex_v2\0".as_ptr()),
            );
            let get_rates_fn: Option<NvmlDeviceGetUtilizationRates> = std::mem::transmute(
                GetProcAddress(nvml_lib, b"nvmlDeviceGetUtilizationRates\0".as_ptr()),
            );
            let get_power_fn: Option<NvmlDeviceGetPowerUsage> = std::mem::transmute(
                GetProcAddress(nvml_lib, b"nvmlDeviceGetPowerUsage\0".as_ptr()),
            );
            let shutdown_fn: Option<NvmlShutdown> =
                std::mem::transmute(GetProcAddress(nvml_lib, b"nvmlShutdown\0".as_ptr()));

            let mut out = GpuProbeReading {
                util_pct: 0.0,
                power_mw: None,
                health: ProbeHealth::Unavailable("NVML symbols unavailable".to_string()),
            };

            if let (Some(init), Some(get_handle), Some(get_rates), Some(shutdown)) =
                (init_fn, get_handle_fn, get_rates_fn, shutdown_fn)
            {
                if init() == 0 {
                    let mut dev_count: u32 = 1;
                    if let Some(cf) = count_fn {
                        let mut c = 0u32;
                        if cf(&mut c) == 0 {
                            dev_count = c;
                        }
                    }

                    if dev_count == 0 {
                        out.health =
                            ProbeHealth::Unavailable("No discrete NVIDIA GPU detected".to_string());
                    } else {
                        let mut max_util = 0.0f32;
                        let mut max_power: Option<u32> = None;
                        let mut any_ok = false;

                        for idx in 0..dev_count {
                            let mut device: usize = 0;
                            if get_handle(idx, &mut device) == 0 {
                                let mut util = NvmlUtilization { gpu: 0, memory: 0 };
                                if get_rates(device, &mut util) == 0 {
                                    any_ok = true;
                                    max_util = max_util.max(util.gpu as f32);
                                }
                                if let Some(pf) = get_power_fn {
                                    let mut mw = 0u32;
                                    if pf(device, &mut mw) == 0 {
                                        max_power = Some(max_power.unwrap_or(0).max(mw));
                                    }
                                }
                            }
                        }

                        if any_ok {
                            out = GpuProbeReading {
                                util_pct: max_util,
                                power_mw: max_power,
                                health: ProbeHealth::Available,
                            };
                        } else {
                            out.health = ProbeHealth::Unavailable("NVML query failed".to_string());
                        }
                    }
                    shutdown();
                } else {
                    out.health = ProbeHealth::Unavailable("nvmlInit_v2 failed".to_string());
                }
            }
            FreeLibrary(nvml_lib);
            return out;
        }
    }

    #[cfg(target_os = "linux")]
    {
        for card in 0..4 {
            let path = format!("/sys/class/drm/card{}/device/gpu_busy_percent", card);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(val) = content.trim().parse::<f32>() {
                    return GpuProbeReading {
                        util_pct: val,
                        power_mw: None,
                        health: ProbeHealth::Available,
                    };
                }
            }
        }
        return GpuProbeReading {
            util_pct: 0.0,
            power_mw: None,
            health: ProbeHealth::Unavailable("No DRM GPU busy counter exposed".to_string()),
        };
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    {
        GpuProbeReading {
            util_pct: 0.0,
            power_mw: None,
            health: ProbeHealth::Unavailable(
                "GPU utilization probe not supported on this OS".to_string(),
            ),
        }
    }
}

// ============================================================================
// 4. Default-Route Network Allowlist (§5.2 Network Condition 6)
// ============================================================================

/// Returns lowercase adapter friendly names/descriptions carrying a default route (0.0.0.0/0 or ::/0).
pub fn query_default_route_nic_names() -> Vec<String> {
    #[cfg(windows)]
    {
        unsafe {
            use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS, NO_ERROR};
            use windows_sys::Win32::NetworkManagement::IpHelper::{
                FreeMibTable, GetAdaptersAddresses, GetIpForwardTable2, GAA_FLAG_INCLUDE_GATEWAYS,
                IP_ADAPTER_ADDRESSES_LH, MIB_IPFORWARD_TABLE2,
            };
            use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC};

            let mut table_ptr: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
            let status = GetIpForwardTable2(AF_UNSPEC, &mut table_ptr);
            if status != NO_ERROR || table_ptr.is_null() {
                return Vec::new();
            }

            let num_entries = (*table_ptr).NumEntries as usize;
            let rows = std::slice::from_raw_parts((*table_ptr).Table.as_ptr(), num_entries);

            let mut route_if_indices = HashSet::new();
            let mut route_luids = HashSet::new();
            for row in rows {
                if row.Loopback {
                    continue;
                }
                let prefix = &row.DestinationPrefix;
                if prefix.PrefixLength != 0 {
                    continue;
                }
                let fam = prefix.Prefix.si_family;
                let is_default = if fam == AF_INET {
                    prefix.Prefix.Ipv4.sin_addr.S_un.S_addr == 0
                } else if fam == AF_INET6 {
                    prefix.Prefix.Ipv6.sin6_addr.u.Byte == [0u8; 16]
                } else {
                    false
                };
                if is_default {
                    route_if_indices.insert(row.InterfaceIndex);
                    route_luids.insert(row.InterfaceLuid.Value);
                }
            }
            FreeMibTable(table_ptr as *const core::ffi::c_void);

            if route_if_indices.is_empty() && route_luids.is_empty() {
                return Vec::new();
            }

            let mut buf_size: u32 = 16_384;
            let mut buf = vec![0u8; buf_size as usize];
            let mut ret = GetAdaptersAddresses(
                AF_UNSPEC as u32,
                GAA_FLAG_INCLUDE_GATEWAYS,
                std::ptr::null(),
                buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
                &mut buf_size,
            );
            if ret == ERROR_BUFFER_OVERFLOW {
                buf.resize(buf_size as usize, 0);
                ret = GetAdaptersAddresses(
                    AF_UNSPEC as u32,
                    GAA_FLAG_INCLUDE_GATEWAYS,
                    std::ptr::null(),
                    buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
                    &mut buf_size,
                );
            }
            if ret != ERROR_SUCCESS {
                return Vec::new();
            }

            let mut names = Vec::new();
            let mut cur = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
            while !cur.is_null() {
                let adapter = &*cur;
                let idx = adapter.Anonymous1.Anonymous.IfIndex;
                let luid = adapter.Luid.Value;
                if route_if_indices.contains(&idx) || route_luids.contains(&luid) {
                    let friendly = pwstr_lossy(adapter.FriendlyName);
                    let desc = pwstr_lossy(adapter.Description);
                    if !friendly.is_empty() {
                        names.push(friendly.to_lowercase());
                    }
                    if !desc.is_empty() {
                        names.push(desc.to_lowercase());
                    }
                }
                cur = adapter.Next;
            }
            return names;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut names = Vec::new();
        if let Ok(content) = std::fs::read_to_string("/proc/net/route") {
            for line in content.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 2 && cols[1] == "00000000" {
                    names.push(cols[0].to_lowercase());
                }
            }
        }
        return names;
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Vec::new()
    }
}

#[cfg(windows)]
unsafe fn pwstr_lossy(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
        if len > 512 {
            break;
        }
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

pub fn is_virtual_or_loopback_nic(nic_name: &str) -> bool {
    let lower = nic_name.to_lowercase();
    [
        "loopback",
        "vethernet",
        "wsl",
        "hyper-v",
        "docker",
        "veth",
        "br-",
        "virbr",
        "vmware",
        "virtualbox",
        "vboxnet",
        "isatap",
        "teredo",
    ]
    .iter()
    .any(|v| lower.contains(v))
        || lower == "lo"
}

/// Computes network KB/s using either default-route NICs allowlist, manual NIC override,
/// or fallback virtual/loopback exclusion (§5.2).
pub fn compute_filtered_net_kbps(
    nets: &Networks,
    default_route_nics: &[String],
    manual_override_csv: &str,
) -> (f32, f32) {
    let manual: Vec<String> = manual_override_csv
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let mut raw_total = 0u64;
    let mut filtered_total = 0u64;

    for (name, net) in nets {
        let bytes = net.received().saturating_add(net.transmitted());
        raw_total = raw_total.saturating_add(bytes);
        let lower = name.to_lowercase();

        let include = if !manual.is_empty() {
            manual.iter().any(|m| lower.contains(m))
        } else if !default_route_nics.is_empty() {
            default_route_nics
                .iter()
                .any(|d| d == &lower || lower.contains(d) || d.contains(&lower))
        } else {
            !is_virtual_or_loopback_nic(&lower)
        };

        if include {
            filtered_total = filtered_total.saturating_add(bytes);
        }
    }

    (raw_total as f32 / 1024.0, filtered_total as f32 / 1024.0)
}

// ============================================================================
// 5. Power Requests Worker Thread (Off 1 Hz Loop, Elevation-Aware, §5.7)
// ============================================================================

pub fn parse_powercfg_requests(text: &str, stderr: &str, exit_ok: bool) -> PowerRequestSnapshot {
    let combined_err = format!("{}\n{}", text, stderr).to_lowercase();
    if !exit_ok
        || combined_err.contains("requires administrator")
        || combined_err.contains("access is denied")
        || combined_err.contains("not have permissions")
    {
        return PowerRequestSnapshot {
            health: ProbeHealth::Unavailable(
                "Requires Administrator privilege (powercfg /requests)".to_string(),
            ),
            display_active: false,
            system_active: false,
            owners: Vec::new(),
        };
    }

    let mut current_category = String::new();
    let mut owners = Vec::new();
    let mut display_active = false;
    let mut system_active = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "None." {
            continue;
        }
        if trimmed.starts_with("DISPLAY:") {
            current_category = "DISPLAY".to_string();
        } else if trimmed.starts_with("SYSTEM:") {
            current_category = "SYSTEM".to_string();
        } else if trimmed.starts_with("AWAYMODE:") {
            current_category = "AWAYMODE".to_string();
        } else if trimmed.starts_with("EXECUTION:") {
            current_category = "EXECUTION".to_string();
        } else if trimmed.starts_with("PERFBOOST:") || trimmed.starts_with("ACTIVELOCKSCREEN:") {
            current_category.clear();
        } else if !current_category.is_empty() && trimmed.starts_with('[') {
            // Line looks like: [PROCESS] \Device\HarddiskVolume3\...\chrome.exe
            // or [DRIVER] High Definition Audio Device
            if let Some(end_bracket) = trimmed.find(']') {
                let caller_type = trimmed[1..end_bracket].trim().to_string();
                let raw_name = trimmed[end_bracket + 1..].trim();
                let clean_name = raw_name
                    .rsplit('\\')
                    .next()
                    .unwrap_or(raw_name)
                    .trim()
                    .to_string();
                if current_category == "DISPLAY" {
                    display_active = true;
                } else if current_category == "SYSTEM" {
                    system_active = true;
                }
                owners.push(PowerRequestOwner {
                    category: current_category.clone(),
                    caller_type,
                    name: if clean_name.is_empty() {
                        raw_name.to_string()
                    } else {
                        clean_name
                    },
                });
            }
        }
    }

    PowerRequestSnapshot {
        health: ProbeHealth::Available,
        display_active,
        system_active,
        owners,
    }
}

pub fn start_power_request_worker() -> Arc<Mutex<PowerRequestSnapshot>> {
    let shared = Arc::new(Mutex::new(PowerRequestSnapshot::default()));
    let cloned = Arc::clone(&shared);

    std::thread::spawn(move || loop {
        let snap = query_os_power_requests_once();
        if let Ok(mut g) = cloned.lock() {
            *g = snap;
        }
        std::thread::sleep(Duration::from_secs(3));
    });

    shared
}

pub fn query_os_power_requests_once() -> PowerRequestSnapshot {
    #[cfg(windows)]
    {
        match Command::new("powercfg")
            .arg("/requests")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            Ok(out) => parse_powercfg_requests(
                &String::from_utf8_lossy(&out.stdout),
                &String::from_utf8_lossy(&out.stderr),
                out.status.success(),
            ),
            Err(e) => PowerRequestSnapshot {
                health: ProbeHealth::Unavailable(format!("powercfg exec failed: {}", e)),
                ..Default::default()
            },
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new("pmset").args(["-g", "assertions"]).output() {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut owners = Vec::new();
            let mut display_active = false;
            for line in text.lines() {
                if line.contains("PreventUserIdleDisplaySleep") && line.contains("named:") {
                    display_active = true;
                    let name = line
                        .split("named:")
                        .nth(1)
                        .unwrap_or("macOS Assertion")
                        .trim()
                        .trim_matches('"')
                        .to_string();
                    owners.push(PowerRequestOwner {
                        category: "DISPLAY".to_string(),
                        caller_type: "PROCESS".to_string(),
                        name,
                    });
                }
            }
            return PowerRequestSnapshot {
                health: ProbeHealth::Available,
                display_active,
                system_active: false,
                owners,
            };
        }
        PowerRequestSnapshot {
            health: ProbeHealth::Unavailable("pmset unavailable".to_string()),
            ..Default::default()
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        PowerRequestSnapshot {
            health: ProbeHealth::Available,
            ..Default::default()
        }
    }
}

// ============================================================================
// 6. Audio Render / Capture / SMTC COM+WinRT Worker Thread (§5.3, §5.4, §5.8)
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct AudioMediaReading {
    pub media: MediaSessionSnapshot,
    pub capture_peak: f32,
    pub capture_signal: MicSignalState,
    pub active_audio_pids: Vec<u32>,
    pub capture_health: ProbeHealth,
}

pub fn start_audio_media_worker() -> Arc<Mutex<AudioMediaReading>> {
    let shared = Arc::new(Mutex::new(AudioMediaReading::default()));
    let cloned = Arc::clone(&shared);

    std::thread::spawn(move || {
        let mut mic_classifier = MicSignalClassifier::default();

        #[cfg(windows)]
        {
            use windows::core::{Interface, GUID, PWSTR};
            use windows::Media::Control::{
                GlobalSystemMediaTransportControlsSessionManager,
                GlobalSystemMediaTransportControlsSessionPlaybackStatus,
            };
            use windows::Win32::Foundation::PROPERTYKEY;
            use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
            use windows::Win32::Media::Audio::{
                eCapture, eConsole, eRender, AudioSessionStateActive, DigitalAudioDisplayDevice,
                IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
                MMDeviceEnumerator,
            };
            use windows::Win32::System::Com::StructuredStorage::{
                PropVariantClear, PropVariantToUInt32,
            };
            use windows::Win32::System::Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
                COINIT_MULTITHREADED, STGM_READ,
            };

            const PKEY_AUDIO_ENDPOINT_FORM_FACTOR: PROPERTYKEY = PROPERTYKEY {
                fmtid: GUID::from_u128(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e),
                pid: 0,
            };

            unsafe fn pwstr_free(p: PWSTR) -> String {
                if p.is_null() {
                    return String::new();
                }
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
                s
            }

            let com_hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            let should_uninit = com_hr.is_ok();
            let mut smtc_mgr: Option<GlobalSystemMediaTransportControlsSessionManager> = None;

            loop {
                if smtc_mgr.is_none() {
                    if let Ok(op) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                    {
                        smtc_mgr = op.join().ok();
                    }
                }

                let mut snap = MediaSessionSnapshot::default();
                let mut active_audio_pids = Vec::new();
                let mut capture_peak = 0.0f32;
                let mut capture_health = ProbeHealth::Available;

                // --- A. Core Audio Render & Capture Endpoints ---
                let enum_res: windows::core::Result<IMMDeviceEnumerator> =
                    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) };

                match enum_res {
                    Ok(enumerator) => {
                        // 1. Default Render Endpoint (§5.3 + §5.8 FormFactor)
                        if let Ok(render_dev) =
                            unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
                        {
                            // Check if monitor / HDMI / DisplayPort speaker (§5.8)
                            if let Ok(store) = unsafe { render_dev.OpenPropertyStore(STGM_READ) } {
                                if let Ok(mut pv) =
                                    unsafe { store.GetValue(&PKEY_AUDIO_ENDPOINT_FORM_FACTOR) }
                                {
                                    let ff = unsafe { PropVariantToUInt32(&pv) }.unwrap_or(0);
                                    let _ = unsafe { PropVariantClear(&mut pv) };
                                    if ff == (DigitalAudioDisplayDevice.0 as u32) {
                                        snap.monitor_speaker_active = true;
                                    }
                                }
                            }

                            if let Ok(meter) = unsafe {
                                render_dev.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None)
                            } {
                                if let Ok(peak) = unsafe { meter.GetPeakValue() } {
                                    snap.render_peak = peak;
                                    if peak > 0.002 {
                                        snap.render_active = true;
                                    }
                                }
                            }

                            // Per-session render mapping (PID, session_instance, browser audio)
                            if let Ok(mgr) = unsafe {
                                render_dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)
                            } {
                                if let Ok(sess_enum) = unsafe { mgr.GetSessionEnumerator() } {
                                    let count = unsafe { sess_enum.GetCount() }.unwrap_or(0);
                                    let mut best_peak = 0.0f32;
                                    for i in 0..count {
                                        if let Ok(ctrl) = unsafe { sess_enum.GetSession(i) } {
                                            let st = unsafe { ctrl.GetState() };
                                            if st == Ok(AudioSessionStateActive) {
                                                let s_peak = ctrl
                                                    .cast::<IAudioMeterInformation>()
                                                    .ok()
                                                    .and_then(|m| unsafe { m.GetPeakValue() }.ok())
                                                    .unwrap_or(0.0);
                                                if s_peak > 0.002 {
                                                    if let Ok(c2) =
                                                        ctrl.cast::<IAudioSessionControl2>()
                                                    {
                                                        let pid = unsafe { c2.GetProcessId() }
                                                            .unwrap_or(0);
                                                        if pid > 0 {
                                                            active_audio_pids.push(pid);
                                                            let exe = pid_exe_name_win(pid);
                                                            if crate::intent::is_browser_process(
                                                                &exe,
                                                            ) {
                                                                snap.browser_audio_active = true;
                                                            }
                                                            if s_peak > best_peak {
                                                                best_peak = s_peak;
                                                                snap.render_app = exe.clone();
                                                                let inst = unsafe {
                                                                    c2.GetSessionInstanceIdentifier(
                                                                    )
                                                                }
                                                                .map(|p| unsafe { pwstr_free(p) })
                                                                .unwrap_or_default();
                                                                if snap.session_key.is_empty() {
                                                                    snap.session_key = format!(
                                                                        "render::{}::{}",
                                                                        exe.to_lowercase(),
                                                                        inst
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // 2. Default Capture Endpoint (Mic Acoustic Peak, §5.4)
                        match unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) } {
                            Ok(cap_dev) => {
                                if let Ok(meter) = unsafe {
                                    cap_dev.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None)
                                } {
                                    match unsafe { meter.GetPeakValue() } {
                                        Ok(p) => capture_peak = p,
                                        Err(_) => {
                                            capture_health = ProbeHealth::Unavailable(
                                                "Capture meter read failed".to_string(),
                                            );
                                        }
                                    }
                                } else {
                                    capture_health = ProbeHealth::Unavailable(
                                        "IAudioMeterInformation unavailable".to_string(),
                                    );
                                }
                            }
                            Err(_) => {
                                capture_health = ProbeHealth::Unavailable(
                                    "No default capture microphone".to_string(),
                                );
                            }
                        }
                    }
                    Err(_) => {
                        capture_health =
                            ProbeHealth::Unavailable("MMDeviceEnumerator unavailable".to_string());
                        snap.health =
                            ProbeHealth::Unavailable("Core Audio COM unavailable".to_string());
                    }
                }

                // --- B. WinRT SMTC Media Session ---
                if let Some(ref mgr) = smtc_mgr {
                    if let Ok(sessions) = mgr.GetSessions() {
                        let n = sessions.Size().unwrap_or(0);
                        for i in 0..n {
                            if let Ok(sess) = sessions.GetAt(i) {
                                if let Ok(info) = sess.GetPlaybackInfo() {
                                    if let Ok(pb) = info.PlaybackStatus() {
                                        let app_id = sess
                                            .SourceAppUserModelId()
                                            .map(|h| h.to_string_lossy())
                                            .unwrap_or_default();
                                        let title = sess
                                            .TryGetMediaPropertiesAsync()
                                            .ok()
                                            .and_then(|op| op.join().ok())
                                            .and_then(|p| p.Title().ok())
                                            .map(|h| h.to_string_lossy())
                                            .unwrap_or_default();

                                        if pb
                                            == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing
                                        {
                                            snap.smtc_playing = true;
                                            snap.smtc_app = app_id.clone();
                                            snap.smtc_title = title;
                                            // Key strictly on app identity, NEVER title (§5.3)!
                                            snap.session_key =
                                                format!("smtc::{}", app_id.to_lowercase());
                                            break;
                                        } else if pb
                                            == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused
                                            || pb
                                                == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Stopped
                                        {
                                            snap.smtc_paused_or_stopped = true;
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        smtc_mgr = None;
                    }
                }

                let signal = mic_classifier.observe(capture_peak);
                if let Ok(mut g) = cloned.lock() {
                    *g = AudioMediaReading {
                        media: snap,
                        capture_peak,
                        capture_signal: signal,
                        active_audio_pids,
                        capture_health,
                    };
                }

                std::thread::sleep(Duration::from_secs(1));
            }

            #[allow(unreachable_code)]
            if should_uninit {
                unsafe { CoUninitialize() };
            }
        }

        #[cfg(not(windows))]
        {
            loop {
                #[cfg(target_os = "linux")]
                let reading = {
                    let render_active = Command::new("pactl")
                        .args(["list", "short", "sink-inputs"])
                        .output()
                        .map(|o| o.status.success() && !o.stdout.is_empty())
                        .unwrap_or(false);
                    AudioMediaReading {
                        media: MediaSessionSnapshot {
                            render_active,
                            render_app: if render_active {
                                "PulseAudio Stream".to_string()
                            } else {
                                String::new()
                            },
                            session_key: if render_active {
                                "linux::pulseaudio".to_string()
                            } else {
                                String::new()
                            },
                            ..Default::default()
                        },
                        capture_signal: mic_classifier.observe(0.0),
                        capture_health: ProbeHealth::Available,
                        ..Default::default()
                    }
                };
                #[cfg(not(target_os = "linux"))]
                let reading = {
                    let _ = mic_classifier.observe(0.0);
                    AudioMediaReading {
                        media: MediaSessionSnapshot {
                            health: ProbeHealth::Unavailable(
                                "Media session probe not supported on this OS".to_string(),
                            ),
                            ..Default::default()
                        },
                        capture_health: ProbeHealth::Unavailable(
                            "Capture meter probe not supported on this OS".to_string(),
                        ),
                        ..Default::default()
                    }
                };

                if let Ok(mut g) = cloned.lock() {
                    *g = reading;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    });

    shared
}

#[cfg(windows)]
fn pid_exe_name_win(pid: u32) -> String {
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return format!("pid:{}", pid);
        }
        let mut buf = [0u16; 260];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            0 as PROCESS_NAME_FORMAT,
            buf.as_mut_ptr(),
            &mut size,
        );
        CloseHandle(handle);
        if ok != 0 && size > 0 {
            let full = String::from_utf16_lossy(&buf[..size as usize]);
            full.rsplit('\\').next().unwrap_or(&full).to_string()
        } else {
            format!("pid:{}", pid)
        }
    }
}

// ============================================================================
// 7. Process Tree Attribution & Enriched System Sampler (§5.2, §5.5, §5.6)
// ============================================================================

#[derive(Debug, Clone)]
pub struct EnrichedProcessSummary {
    pub max_agent_cpu: f32,
    pub max_agent_proc: String,
    pub max_dl_kbps: f32,
    pub max_dl_proc: String,
    pub attributed_disk_kbps: f32,
    pub top_disk_proc: String,
    pub top_disk_kbps: f32,
    pub self_cpu_norm: f32,
    pub lease_tree_cpu: f32,
    pub lease_tree_disk_kbps: f32,
    pub holder_render_audio_active: bool,
    pub top_processes: Vec<ProcessSampleRow>,
}

#[derive(Debug, Clone)]
pub struct ProcessSampleRow {
    pub name: String,
    pub cpu_pct: f32,
    pub is_agent: bool,
    pub is_ignored: bool,
}

const IDE_SHELL_HOSTS: &[&str] = &[
    "code.exe",
    "code",
    "cursor.exe",
    "cursor",
    "antigravity.exe",
    "antigravity",
    "devenv.exe",
    "idea64.exe",
    "windsurf.exe",
    "windsurf",
];

fn is_ide_shell_host(lower_name: &str) -> bool {
    IDE_SHELL_HOSTS.iter().any(|s| lower_name == *s)
}

pub fn sample_processes_enriched(
    sys: &System,
    agent_keywords: &[String],
    download_keywords: &[String],
    ignored_keywords: &[String],
    disk_noise_keywords: &[String],
    lease_pid: Option<u32>,
    active_audio_pids: &[u32],
    mic_holder_app: &str,
) -> EnrichedProcessSummary {
    let cores = sys.cpus().len().max(1) as f32;
    let self_pid = std::process::id();

    struct Node {
        raw_name: String,
        lower: String,
        parent_pid: Option<u32>,
        cpu_norm: f32,
        disk_kbps: f32,
        is_ignored: bool,
    }

    let mut nodes: HashMap<u32, Node> = HashMap::new();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();

    for (pid, proc_ref) in sys.processes() {
        let p_u32 = pid.as_u32();
        let raw_name = proc_ref.name().to_string_lossy().to_string();
        let lower = raw_name.to_lowercase();
        let cpu_norm = proc_ref.cpu_usage() / cores;
        let du = proc_ref.disk_usage();
        let disk_kbps = (du.read_bytes.saturating_add(du.written_bytes)) as f32 / 1024.0;
        let is_ignored = ignored_keywords.iter().any(|k| lower.contains(k));
        let parent_pid = proc_ref.parent().map(|p| p.as_u32());

        if let Some(pp) = parent_pid {
            children.entry(pp).or_default().push(p_u32);
        }

        nodes.insert(
            p_u32,
            Node {
                raw_name,
                lower,
                parent_pid,
                cpu_norm,
                disk_kbps,
                is_ignored,
            },
        );
    }

    let self_cpu_norm = nodes.get(&self_pid).map(|n| n.cpu_norm).unwrap_or(0.0);

    // Helper: accumulate descendant CPU/disk excluding ignored & self
    let collect_subtree = |root_pid: u32, skip_root_shell: bool| -> (f32, f32, String) {
        let mut stack = vec![root_pid];
        let mut visited = HashSet::new();
        let mut total_cpu = 0.0f32;
        let mut total_disk = 0.0f32;
        let mut hottest_name = String::new();
        let mut hottest_cpu = 0.0f32;

        while let Some(curr) = stack.pop() {
            if !visited.insert(curr) {
                continue;
            }
            if let Some(n) = nodes.get(&curr) {
                if curr != self_pid && !n.is_ignored {
                    let count_self = !(curr == root_pid && skip_root_shell);
                    if count_self {
                        total_cpu += n.cpu_norm;
                        total_disk += n.disk_kbps;
                        if n.cpu_norm > hottest_cpu {
                            hottest_cpu = n.cpu_norm;
                            hottest_name = n.raw_name.clone();
                        }
                    }
                }
                if let Some(ch) = children.get(&curr) {
                    for &c in ch {
                        stack.push(c);
                    }
                }
            }
        }
        (total_cpu, total_disk, hottest_name)
    };

    let mut max_agent_cpu = 0.0f32;
    let mut max_agent_proc = "None".to_string();
    let mut max_dl_kbps = 0.0f32;
    let mut max_dl_proc = "None".to_string();
    let mut attributed_disk_kbps = 0.0f32;
    let mut top_disk_proc = "None".to_string();
    let mut top_disk_kbps = 0.0f32;
    let mut top_processes = Vec::new();
    let mut holder_render_audio_active = false;
    let holder_lower = mic_holder_app.to_lowercase();

    for (&pid, n) in &nodes {
        if pid == self_pid {
            continue;
        }
        let is_agent = agent_keywords.iter().any(|k| n.lower.contains(k));
        if n.cpu_norm > 0.1 || is_agent {
            top_processes.push(ProcessSampleRow {
                name: n.raw_name.clone(),
                cpu_pct: n.cpu_norm,
                is_agent,
                is_ignored: n.is_ignored,
            });
        }

        if !holder_lower.is_empty()
            && active_audio_pids.contains(&pid)
            && (n.lower.contains(&holder_lower) || holder_lower.contains(&n.lower))
        {
            holder_render_audio_active = true;
        }

        if n.is_ignored {
            continue;
        }

        // Process tree attribution for Agent CPU (§5.5):
        // IDE shells (`code.exe`, `cursor.exe`) only contribute CPU via worker children!
        if is_agent {
            let is_shell = is_ide_shell_host(&n.lower);
            let (tree_cpu, _, hottest_child) = collect_subtree(pid, is_shell);
            if tree_cpu > max_agent_cpu {
                max_agent_cpu = tree_cpu;
                max_agent_proc = if !hottest_child.is_empty() {
                    hottest_child
                } else {
                    n.raw_name.clone()
                };
            }
        }

        // Downloaders
        if download_keywords.iter().any(|k| n.lower.contains(k)) && n.disk_kbps > max_dl_kbps {
            max_dl_kbps = n.disk_kbps;
            max_dl_proc = n.raw_name.clone();
        }

        // Disk I/O Attribution (§5.2) excluding Defender / SearchIndexer / servicing noise
        let is_disk_noise = disk_noise_keywords.iter().any(|k| n.lower.contains(k));
        if !is_disk_noise && n.disk_kbps > 0.5 {
            attributed_disk_kbps += n.disk_kbps;
            if n.disk_kbps > top_disk_kbps {
                top_disk_kbps = n.disk_kbps;
                top_disk_proc = n.raw_name.clone();
            }
        }
    }

    // Also mark if any parent was unmatched/recycled safely (suppress unused field warning)
    let _ = nodes.values().next().map(|n| n.parent_pid);

    top_processes.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top_processes.truncate(8);

    let (lease_tree_cpu, lease_tree_disk_kbps, _) = if let Some(lpid) = lease_pid {
        collect_subtree(lpid, false)
    } else {
        (0.0, 0.0, String::new())
    };

    EnrichedProcessSummary {
        max_agent_cpu,
        max_agent_proc,
        max_dl_kbps,
        max_dl_proc,
        attributed_disk_kbps,
        top_disk_proc,
        top_disk_kbps,
        self_cpu_norm,
        lease_tree_cpu,
        lease_tree_disk_kbps,
        holder_render_audio_active,
        top_processes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_powercfg_requests_parsing_and_elevation_detection() {
        let unelev = "";
        let err = "The following requires administrator privileges.";
        let s1 = parse_powercfg_requests(unelev, err, false);
        assert!(!s1.health.is_available());

        let elevated_out = "\
DISPLAY:\r\n\
[PROCESS] \\Device\\HarddiskVolume3\\Program Files\\Google\\Chrome\\Application\\chrome.exe\r\n\
\r\n\
SYSTEM:\r\n\
[DRIVER] Realtek High Definition Audio\r\n\
\r\n\
AWAYMODE:\r\n\
None.\r\n";
        let s2 = parse_powercfg_requests(elevated_out, "", true);
        assert!(s2.health.is_available());
        assert!(s2.display_active);
        assert!(s2.system_active);
        assert_eq!(s2.owners.len(), 2);
        assert_eq!(s2.owners[0].name, "chrome.exe");
        assert_eq!(s2.owners[0].category, "DISPLAY");
    }

    #[test]
    fn test_virtual_nic_filter() {
        assert!(is_virtual_or_loopback_nic("vEthernet (WSL)"));
        assert!(is_virtual_or_loopback_nic("Loopback Pseudo-Interface 1"));
        assert!(is_virtual_or_loopback_nic("docker0"));
        assert!(!is_virtual_or_loopback_nic("Ethernet"));
        assert!(!is_virtual_or_loopback_nic("Wi-Fi"));
    }
}
