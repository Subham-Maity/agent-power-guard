// ============================================================================
// Pure Decision Evaluator & Intent-Aware Sleep Engine
// ============================================================================
//
// Verified Assumptions from codebase audit (§1):
//   A1: Previously, Condition 10 (GPU) fed `all_conditions_pass` directly and bypassed
//       Condition 7, causing any 1s GPU blip >= 5% to reset the countdown. Fixed by routing
//       Cond 10 through the §2 rolling duty-cycle filter and §5.1 baseline-relative threshold.
//   A2: Previously, Cond 4/5/6 evaluated raw 1-second samples while Condition 7 acted only
//       as a fragile secondary gate. Fixed: Condition 7 is now the unified signal-conditioning
//       layer governing Conds 2, 3, 4, 5, 6, and 10. Raw 1s blips never reset countdown.
//   A3: Previously, `pc_spike_sustained_counter` reset to 0 on the very first quiet second.
//       Replaced with rolling ring buffer + duty-cycle + hysteresis (`SignalFilter`).
//   A4: Previously, any failing condition reset `countdown_secs` immediately. Now only
//       filtered `Busy` analog signals or true Digital blockers reset countdown; `Pending` never resets it.
//   A5: Verified in `check_power_requests`: `powercfg /requests` requires Administrator when run
//       as `AsInvoker` and previously failed silently as `(false, false)`. Fixed: probe now reports
//       `ProbeHealth::Unavailable` when unelevated (Cond 9 passes cleanly and shows a UI badge),
//       parses owners when available, and suppresses stale silent browser requests (>60s).
//   A6: Verified resume handler: wall-clock jumps >10s previously reset countdown unconditionally to
//       full `idle_threshold_secs`. Fixed with wake classification (`UserInput`, `ScheduledTimer`,
//       `UnattendedUnknown`) and a short ~120s unattended-wake return timer.
//   A7: Open egui window (`glow` 1Hz repaint) generates tiny desktop compositor GPU activity (~1-12%).
//       Hidden tray mode skips layout/rendering; baseline-relative GPU tracking handles open window idle too.

use std::collections::HashMap;

use crate::intent::{
    classify_mic_app, is_browser_process, BlockerInfo, MediaPromptAction, MediaSessionSnapshot,
    MicAppCategory, MicCamSnapshot, OperationalMode, PowerRequestOwner, PowerRequestSnapshot,
    UserIntent, WakeClassification,
};
use crate::signal::{
    AdaptiveBaselineSuggester, GpuBaselineTracker, MicSignalState, ProbeHealth, SignalFilter,
    SignalFilterConfig, SignalState, SuggestedThresholds,
};

/// Plain subset of `AppConfig` needed for pure decision evaluation.
#[derive(Debug, Clone)]
pub struct EvalConfig {
    pub operational_mode: OperationalMode,
    pub guard_enabled: bool,

    // Thresholds
    pub idle_threshold_secs: u32,
    pub agent_cpu_threshold: f32,
    pub download_threshold_kbps: f32,
    pub pc_cpu_threshold: f32,
    pub pc_disk_threshold_kbps: f32,
    pub pc_net_threshold_kbps: f32,
    pub spike_filter_secs: u32,
    pub monitor_timeout_mins: u32,
    pub respect_display_requests: bool,
    pub gpu_threshold_pct: f32,
    pub gpu_baseline_margin_pct: f32,
    pub grace_period_secs: u32,

    // Signal filter tuning
    pub filter_busy_count: u32,
    pub filter_quiet_count: u32,
    pub fast_filter_window_secs: u32,
    pub fast_filter_busy_count: u32,
    pub fast_filter_quiet_count: u32,

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
    pub cond10_gpu_enabled: bool,
    pub cond11_mic_cam_enabled: bool,

    // Quiet hours, scheduling, battery
    pub in_quiet_hours: bool,
    pub quiet_hours_mode_id: u8, // 0=Other, 1=Night, 2=Away
    pub sleep_after_lease: bool,
    pub battery_profile_enabled: bool,
    pub battery_idle_threshold_secs: u32,
    pub battery_agent_cpu_threshold: f32,
    pub low_battery_auto_sleep_enabled: bool,
    pub low_battery_percent_threshold: u8,

    // Scenario 5.3 Passive Media
    pub media_prompt_after_secs: u32,
    pub media_pause_grace_secs: u32,
    pub media_max_lease_hours: u32,

    // Scenario 5.4 Mic/Cam app categories
    pub mic_meeting_apps: String,
    pub mic_creative_apps: String,

    // Scenario 5.6 Zombie Leases
    pub lease_idle_mins: u32,
    pub lease_auto_expire_enabled: bool,
    pub lease_auto_expire_mins: u32,

    // Scenario 5.7 & 5.8 & 5.9
    pub ignored_power_owners: String,
    pub protect_monitor_audio: bool,
    pub unattended_wake_secs: u32,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            operational_mode: OperationalMode::SmartGuard,
            guard_enabled: true,
            idle_threshold_secs: 180,
            agent_cpu_threshold: 2.0,
            download_threshold_kbps: 100.0,
            pc_cpu_threshold: 30.0,
            pc_disk_threshold_kbps: 10000.0,
            pc_net_threshold_kbps: 200.0,
            spike_filter_secs: 15,
            monitor_timeout_mins: 2,
            respect_display_requests: true,
            gpu_threshold_pct: 5.0,
            gpu_baseline_margin_pct: 12.0,
            grace_period_secs: 30,

            filter_busy_count: 8,
            filter_quiet_count: 2,
            fast_filter_window_secs: 10,
            fast_filter_busy_count: 4,
            fast_filter_quiet_count: 1,

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
            cond11_mic_cam_enabled: true,

            in_quiet_hours: false,
            quiet_hours_mode_id: 1,
            sleep_after_lease: false,
            battery_profile_enabled: true,
            battery_idle_threshold_secs: 60,
            battery_agent_cpu_threshold: 1.0,
            low_battery_auto_sleep_enabled: true,
            low_battery_percent_threshold: 15,

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

#[derive(Debug, Clone, Default)]
pub struct ConditionStatus {
    pub enabled: bool,
    pub is_passing: bool,
    pub value_text: String,
    pub threshold_text: String,
    pub blocker_detail: Option<String>,
    pub probe_unavailable: Option<String>,
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

    // Phase 3: Battery & Scheduling
    pub on_battery: bool,
    pub battery_pct: Option<u8>,
    pub stay_awake_until: Option<u64>,
    pub local_hour: u32,
    pub local_min: u32,
    pub is_laptop: bool,

    // Intent-aware enriched probe inputs
    pub gpu_power_mw: Option<u32>,
    pub gpu_health: ProbeHealth,
    pub raw_physical_disk_available: bool,
    pub attributed_disk_kbps: Option<f32>,
    pub top_disk_proc: String,
    pub top_disk_kbps: f32,
    pub default_route_net_kbps: Option<f32>,
    pub media: MediaSessionSnapshot,
    pub mic_cam: MicCamSnapshot,
    pub power_requests: PowerRequestSnapshot,
    pub lease_tree_cpu_pct: f32,
    pub lease_tree_disk_kbps: f32,
    pub wake_classification: Option<WakeClassification>,
}

impl Metrics {
    pub fn effective_disk_kbps(&self) -> f32 {
        match self.attributed_disk_kbps {
            Some(attr) if self.raw_physical_disk_available => attr.min(self.whole_pc_disk_kbps),
            Some(attr) => attr,
            None => self.whole_pc_disk_kbps,
        }
    }

    pub fn effective_net_kbps(&self) -> f32 {
        self.default_route_net_kbps
            .unwrap_or(self.whole_pc_net_kbps)
    }
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

    // Per-signal conditioning filters (§2)
    pub agent_filter: SignalFilter,
    pub download_filter: SignalFilter,
    pub pc_cpu_filter: SignalFilter,
    pub disk_filter: SignalFilter,
    pub net_filter: SignalFilter,
    pub gpu_filter: SignalFilter,

    // GPU adaptive idle baseline (§5.1)
    pub gpu_baseline: GpuBaselineTracker,

    // Adaptive baseline threshold suggestions (§8)
    pub baseline_suggester: AdaptiveBaselineSuggester,

    // Passive media state machine (§5.3)
    pub media_idle_since_epoch: Option<u64>,
    pub media_prompt_active: bool,
    pub media_prompt_shown_for_session: Option<String>,
    pub media_prompt_since_epoch: Option<u64>,
    pub media_lock_session_key: Option<String>,
    pub media_lock_app: String,
    pub media_lock_since_epoch: Option<u64>,
    pub media_paused_since_epoch: Option<u64>,
    pub media_sleep_now_session_key: Option<String>,

    // Browser power request suppression (§5.7)
    pub browser_silent_since_epoch: Option<u64>,

    // Mic/Cam quiet or unknown observation tracking (§5.4)
    pub mic_held_since_epoch: Option<u64>,
    pub unknown_mic_active_since_epoch: Option<u64>,

    // Zombie lease idle detection (§5.6)
    pub lease_idle_since_epoch: Option<u64>,
    pub lease_hung_alert_sent: bool,

    // Unattended wake quick sleep timer (§5.9)
    pub unattended_wake_active: bool,
    pub last_wake_classification: Option<WakeClassification>,

    // Best Performance expiry (§6)
    pub best_perf_until_epoch_secs: Option<u64>,

    // Blocker first-seen map for "since when" transparency (§6)
    pub blocker_first_seen: HashMap<String, u64>,
}

impl EvaluationState {
    pub fn apply_media_prompt_action(&mut self, action: MediaPromptAction, now_epoch: u64) -> bool {
        match action {
            MediaPromptAction::KeepAwake | MediaPromptAction::Hide => {
                if let Some(key) = self.media_prompt_shown_for_session.clone() {
                    self.media_lock_session_key = Some(key);
                    self.media_lock_since_epoch = Some(now_epoch);
                }
                self.media_prompt_active = false;
                self.media_prompt_since_epoch = None;
                false
            }
            MediaPromptAction::SleepNow => {
                self.media_sleep_now_session_key = self.media_prompt_shown_for_session.clone();
                self.media_lock_session_key = None;
                self.media_lock_since_epoch = None;
                self.media_prompt_active = false;
                self.media_prompt_since_epoch = None;
                true
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Decision {
    pub operational_mode: OperationalMode,
    pub intent: UserIntent,
    pub all_conditions_pass: bool,
    pub blockers: Vec<String>,
    pub structured_blockers: Vec<BlockerInfo>,
    pub blocker_banner_text: String,
    pub countdown_secs: u32,
    pub countdown_max: u32,
    pub trigger_sleep: bool,
    pub trigger_grace_banner: bool,
    pub stage1_display_off_allowed: bool,
    pub defer_display_off_for_monitor_audio: bool,
    pub hold_os_wake_lock: bool,
    pub best_performance_expired: bool,
    pub log_monitor_off_duration: Option<u64>,
    pub woke_from_sleep_reset: bool,
    pub detected_wake_classification: Option<WakeClassification>,

    // Passive media UI/API state (§5.3)
    pub media_prompt_active: bool,
    pub media_prompt_app: String,
    pub media_prompt_title: String,
    pub media_lock_active: bool,
    pub media_lock_app: String,

    // Zombie lease alert (§5.6)
    pub trigger_zombie_lease_alert: Option<(String, u64)>,

    // Cond 9 & 10 enriched metadata
    pub active_power_owners: Vec<PowerRequestOwner>,
    pub suppressed_browser_request: Option<String>,
    pub gpu_effective_threshold_pct: f32,
    pub gpu_idle_floor_pct: f32,

    // Adaptive suggestion (§8)
    pub suggested_thresholds: Option<SuggestedThresholds>,

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

pub fn evaluate(config: &EvalConfig, metrics: &Metrics, state: &mut EvaluationState) -> Decision {
    let mut woke_from_sleep_reset = false;
    let mut detected_wake_classification = None;
    let mut log_monitor_off_duration = None;

    // Resolve operational mode (supports legacy `guard_enabled == false` callers too)
    let mut best_performance_expired = false;
    let mode = if !config.guard_enabled && config.operational_mode == OperationalMode::SmartGuard {
        OperationalMode::GuardOff
    } else if config.operational_mode == OperationalMode::BestPerformance {
        if let Some(exp) = state.best_perf_until_epoch_secs {
            if metrics.now_epoch_secs >= exp {
                state.best_perf_until_epoch_secs = None;
                best_performance_expired = true;
                OperationalMode::SmartGuard
            } else {
                OperationalMode::BestPerformance
            }
        } else {
            OperationalMode::BestPerformance
        }
    } else {
        config.operational_mode
    };

    // Dynamic effective thresholds (Quiet Hours + Laptop Battery)
    let mut effective_idle_threshold = config.idle_threshold_secs;
    let mut effective_agent_cpu_threshold = config.agent_cpu_threshold;

    if config.in_quiet_hours {
        match config.quiet_hours_mode_id {
            1 => {
                effective_idle_threshold = effective_idle_threshold.min(60);
                effective_agent_cpu_threshold = effective_agent_cpu_threshold.min(1.0);
            }
            2 => {
                effective_idle_threshold = effective_idle_threshold.min(30);
                effective_agent_cpu_threshold = effective_agent_cpu_threshold.min(1.0);
            }
            _ => {}
        }
    }

    if metrics.on_battery && config.battery_profile_enabled {
        effective_idle_threshold = effective_idle_threshold.min(config.battery_idle_threshold_secs);
        effective_agent_cpu_threshold =
            effective_agent_cpu_threshold.min(config.battery_agent_cpu_threshold);
    }

    // 1. Check Wall-Clock Wakeup & Classify Wake Source (§5.9)
    if state.last_wall_clock_secs > 0 && metrics.now_epoch_secs > state.last_wall_clock_secs + 10 {
        woke_from_sleep_reset = true;
        state.agent_filter.reset();
        state.download_filter.reset();
        state.pc_cpu_filter.reset();
        state.disk_filter.reset();
        state.net_filter.reset();
        state.gpu_filter.reset();
        state.pc_spike_sustained_counter = 0;

        let cls = metrics.wake_classification.unwrap_or_else(|| {
            if metrics.idle_secs < 10 {
                WakeClassification::UserInput
            } else {
                WakeClassification::UnattendedUnknown
            }
        });
        detected_wake_classification = Some(cls);
        state.last_wake_classification = Some(cls);

        match cls {
            WakeClassification::UnattendedUnknown => {
                state.unattended_wake_active = true;
                state.countdown_secs = config
                    .unattended_wake_secs
                    .max(15)
                    .min(effective_idle_threshold);
            }
            WakeClassification::UserInput | WakeClassification::ScheduledTimer => {
                state.unattended_wake_active = false;
                state.countdown_secs = effective_idle_threshold;
            }
        }
    }
    state.last_wall_clock_secs = metrics.now_epoch_secs;

    // If user subsequently touches keyboard/mouse after an unattended wake, switch back to normal countdown
    if state.unattended_wake_active && metrics.idle_secs < 5 {
        state.unattended_wake_active = false;
        state.countdown_secs = effective_idle_threshold;
    }

    let countdown_max = if state.unattended_wake_active {
        config
            .unattended_wake_secs
            .max(15)
            .min(effective_idle_threshold)
    } else {
        effective_idle_threshold
    };

    // Update GPU baseline and adaptive baseline suggester
    state
        .gpu_baseline
        .observe(metrics.gpu_util, metrics.gpu_power_mw, metrics.idle_secs);
    let effective_disk_kbps = metrics.effective_disk_kbps();
    let effective_net_kbps = metrics.effective_net_kbps();
    state.baseline_suggester.observe_idle(
        metrics.whole_pc_cpu,
        effective_disk_kbps,
        effective_net_kbps,
        metrics.idle_secs,
    );
    let suggested_thresholds = state.baseline_suggester.suggested_thresholds();

    // Signal Conditioning Filters (§2)
    let (fast_cfg, std_cfg) = if config.cond7_spike_filter_enabled {
        (
            SignalFilterConfig::new(
                config.fast_filter_window_secs,
                config.fast_filter_busy_count,
                config.fast_filter_quiet_count,
            ),
            SignalFilterConfig::scaled(
                15,
                config.filter_busy_count,
                config.filter_quiet_count,
                config.spike_filter_secs,
            ),
        )
    } else {
        // With filter disabled, 1/1 raw sample passes straight through
        (
            SignalFilterConfig::new(1, 1, 0),
            SignalFilterConfig::new(1, 1, 0),
        )
    };

    let mut raw_blockers: Vec<PendingBlocker> = Vec::new();
    let mut is_meeting_live = false;
    let mut meeting_app_name = String::new();
    let mut is_active_work = false;
    let mut active_work_labels: Vec<String> = Vec::new();
    let mut is_human_active = false;
    let mut is_explicit_hold = false;
    let mut explicit_hold_reason = String::new();

    // Safety hold on hard sensor failure
    if metrics.sensor_error {
        raw_blockers.push(PendingBlocker::new(
            0,
            "Sensor Failure",
            "System Telemetry",
            "Error",
            None,
            None,
            "Sensor Failure (Safety hold active)".to_string(),
        ));
    }

    // Scheduled Stay-Awake Until
    if let Some(until) = metrics.stay_awake_until {
        if metrics.now_epoch_secs < until {
            let rem = until - metrics.now_epoch_secs;
            is_explicit_hold = true;
            explicit_hold_reason = format!("Scheduled Stay-Awake ({}s)", rem);
            raw_blockers.push(PendingBlocker::new(
                0,
                "Scheduled Stay-Awake",
                "User Schedule",
                &format!("{}s left", rem),
                None,
                None,
                format!("Scheduled Stay-Awake ({}s remaining)", rem),
            ));
        }
    }

    // Condition 1: Human Input Idle (Digital)
    let cond1_pass = if state.unattended_wake_active {
        // Keep unattended return path alive while user hasn't interacted since wake
        metrics.idle_secs >= 10
    } else {
        metrics.idle_secs >= effective_idle_threshold
    };
    let cond1_idle = ConditionStatus {
        enabled: config.cond1_idle_enabled,
        is_passing: cond1_pass,
        value_text: if state.unattended_wake_active {
            format!("{}s (Unattended wake)", metrics.idle_secs)
        } else {
            format!("{}s", metrics.idle_secs)
        },
        threshold_text: format!(">= {}s", effective_idle_threshold),
        blocker_detail: if !cond1_pass && config.cond1_idle_enabled {
            Some(format!(
                "Human Activity (idle {}s < {}s)",
                metrics.idle_secs, effective_idle_threshold
            ))
        } else {
            None
        },
        probe_unavailable: None,
    };
    if config.cond1_idle_enabled && !cond1_pass {
        is_human_active = true;
        raw_blockers.push(PendingBlocker::new(
            1,
            "Human Idle",
            "Mouse / Keyboard",
            &format!("{}s < {}s", metrics.idle_secs, effective_idle_threshold),
            None,
            None,
            format!(
                "Human Idle ({}s < {}s)",
                metrics.idle_secs, effective_idle_threshold
            ),
        ));
    }

    // Condition 2: Watched Agent CPU (Analog -> fast_cfg)
    let cond2_raw_exceeded = metrics.max_agent_cpu >= effective_agent_cpu_threshold;
    let cond2_state = state.agent_filter.update(cond2_raw_exceeded, fast_cfg);
    let cond2_pass = !cond2_state.is_blocking();
    let cond2_agent_cpu = ConditionStatus {
        enabled: config.cond2_agent_cpu_enabled,
        is_passing: cond2_pass,
        value_text: format!(
            "{:.1}% ({}) [{}/{} {}]",
            metrics.max_agent_cpu,
            metrics.max_agent_proc,
            state.agent_filter.above_count(),
            state.agent_filter.window_len(),
            cond2_state.label()
        ),
        threshold_text: format!("< {:.1}%", effective_agent_cpu_threshold),
        blocker_detail: if !cond2_pass && config.cond2_agent_cpu_enabled {
            Some(format!(
                "Agent CPU ({} at {:.1}% >= {:.1}%)",
                metrics.max_agent_proc, metrics.max_agent_cpu, effective_agent_cpu_threshold
            ))
        } else {
            None
        },
        probe_unavailable: None,
    };
    if config.cond2_agent_cpu_enabled && !cond2_pass {
        is_active_work = true;
        let who = if metrics.max_agent_proc.is_empty() || metrics.max_agent_proc == "None" {
            "Agent Worker".to_string()
        } else {
            metrics.max_agent_proc.clone()
        };
        active_work_labels.push(who.clone());
        raw_blockers.push(PendingBlocker::new(
            2,
            "Agent CPU",
            &who,
            &format!("{:.1}%", metrics.max_agent_cpu),
            Some(who.clone()),
            Some("process".to_string()),
            format!(
                "Agent CPU ({} at {:.1}% >= {:.1}%)",
                who, metrics.max_agent_cpu, effective_agent_cpu_threshold
            ),
        ));
    }

    // Condition 3: Active Downloads (Analog -> fast_cfg)
    let cond3_raw_exceeded = metrics.max_dl_kbps >= config.download_threshold_kbps;
    let cond3_state = state.download_filter.update(cond3_raw_exceeded, fast_cfg);
    let cond3_pass = !cond3_state.is_blocking();
    let cond3_download = ConditionStatus {
        enabled: config.cond3_download_enabled,
        is_passing: cond3_pass,
        value_text: format!(
            "{:.0} KB/s ({}) [{}/{}]",
            metrics.max_dl_kbps,
            metrics.max_dl_proc,
            state.download_filter.above_count(),
            state.download_filter.window_len()
        ),
        threshold_text: format!("< {:.0} KB/s", config.download_threshold_kbps),
        blocker_detail: if !cond3_pass && config.cond3_download_enabled {
            Some(format!(
                "Active Download ({} at {:.0} KB/s)",
                metrics.max_dl_proc, metrics.max_dl_kbps
            ))
        } else {
            None
        },
        probe_unavailable: None,
    };
    if config.cond3_download_enabled && !cond3_pass {
        is_active_work = true;
        let who = if metrics.max_dl_proc.is_empty() || metrics.max_dl_proc == "None" {
            "Downloader".to_string()
        } else {
            metrics.max_dl_proc.clone()
        };
        active_work_labels.push(who.clone());
        raw_blockers.push(PendingBlocker::new(
            3,
            "Active Download",
            &who,
            &format!("{:.0} KB/s", metrics.max_dl_kbps),
            Some(who.clone()),
            Some("process".to_string()),
            format!(
                "Active Download ({} at {:.0} KB/s)",
                who, metrics.max_dl_kbps
            ),
        ));
    }

    // Condition 4: Whole-PC CPU (Analog -> std_cfg)
    let cond4_state = state
        .pc_cpu_filter
        .update(metrics.whole_pc_cpu >= config.pc_cpu_threshold, std_cfg);
    let cond4_pass = !cond4_state.is_blocking();
    let cond4_pc_cpu = ConditionStatus {
        enabled: config.cond4_pc_cpu_enabled,
        is_passing: cond4_pass,
        value_text: format!(
            "{:.1}% [{}/{} {}]",
            metrics.whole_pc_cpu,
            state.pc_cpu_filter.above_count(),
            state.pc_cpu_filter.window_len(),
            cond4_state.label()
        ),
        threshold_text: format!("< {:.1}%", config.pc_cpu_threshold),
        blocker_detail: if !cond4_pass && config.cond4_pc_cpu_enabled {
            Some(format!("Whole-PC CPU ({:.1}%)", metrics.whole_pc_cpu))
        } else {
            None
        },
        probe_unavailable: None,
    };
    if config.cond4_pc_cpu_enabled && !cond4_pass {
        is_active_work = true;
        active_work_labels.push("PC CPU".to_string());
        raw_blockers.push(PendingBlocker::new(
            4,
            "Whole-PC CPU",
            "System CPU",
            &format!("{:.1}%", metrics.whole_pc_cpu),
            None,
            None,
            format!("Whole-PC CPU ({:.1}%)", metrics.whole_pc_cpu),
        ));
    }

    // Condition 5: Attributed Disk I/O (Physical-Disk-Bounded + Signal Filter, §5.2)
    let cond5_state = state.disk_filter.update(
        effective_disk_kbps >= config.pc_disk_threshold_kbps,
        std_cfg,
    );
    let cond5_pass = !cond5_state.is_blocking();
    let has_top_disk_proc = !metrics.top_disk_proc.is_empty() && metrics.top_disk_proc != "None";
    let disk_who = if has_top_disk_proc {
        metrics.top_disk_proc.clone()
    } else {
        "Physical Disk".to_string()
    };
    let format_kbps = |kbps: f32| -> String {
        if kbps >= 1024.0 {
            format!("{:.1} MB/s", kbps / 1024.0)
        } else {
            format!("{:.0} KB/s", kbps)
        }
    };
    let disk_rate_str = format_kbps(effective_disk_kbps);
    let top_proc_rate_str = format_kbps(metrics.top_disk_kbps);
    let blocker_value_str = if has_top_disk_proc && metrics.top_disk_kbps > 0.0 {
        format!("{} (Disk {})", top_proc_rate_str, disk_rate_str)
    } else {
        disk_rate_str.clone()
    };
    let cond5_pc_disk = ConditionStatus {
        enabled: config.cond5_pc_disk_enabled,
        is_passing: cond5_pass,
        value_text: if metrics.attributed_disk_kbps.is_some() && has_top_disk_proc {
            format!(
                "{} | Top: {} {} | Raw SSD: {:.0} KB/s [{}/{}]",
                disk_rate_str,
                disk_who,
                top_proc_rate_str,
                metrics.whole_pc_disk_kbps,
                state.disk_filter.above_count(),
                state.disk_filter.window_len()
            )
        } else {
            format!(
                "{} | Raw SSD: {:.0} KB/s [{}/{}]",
                disk_rate_str,
                metrics.whole_pc_disk_kbps,
                state.disk_filter.above_count(),
                state.disk_filter.window_len()
            )
        },
        threshold_text: format!("< {:.0} KB/s", config.pc_disk_threshold_kbps),
        blocker_detail: if !cond5_pass && config.cond5_pc_disk_enabled {
            Some(format!("Disk I/O: {} {}", disk_who, blocker_value_str))
        } else {
            None
        },
        probe_unavailable: None,
    };
    if config.cond5_pc_disk_enabled && !cond5_pass {
        is_active_work = true;
        active_work_labels.push(format!("Disk: {}", disk_who));
        let ignore_target = if has_top_disk_proc {
            Some(metrics.top_disk_proc.clone())
        } else {
            None
        };
        raw_blockers.push(PendingBlocker::new(
            5,
            "Disk I/O",
            &disk_who,
            &blocker_value_str,
            ignore_target,
            Some("disk_noise".to_string()),
            format!("Blocked by disk I/O: {} {}", disk_who, blocker_value_str),
        ));
    }

    // Condition 6: Default-Route Network I/O (Analog -> std_cfg, §5.2)
    let cond6_state = state
        .net_filter
        .update(effective_net_kbps >= config.pc_net_threshold_kbps, std_cfg);
    let cond6_pass = !cond6_state.is_blocking();
    let cond6_pc_net = ConditionStatus {
        enabled: config.cond6_pc_net_enabled,
        is_passing: cond6_pass,
        value_text: format!(
            "{:.0} KB/s [{}/{}]",
            effective_net_kbps,
            state.net_filter.above_count(),
            state.net_filter.window_len()
        ),
        threshold_text: format!("< {:.0} KB/s", config.pc_net_threshold_kbps),
        blocker_detail: if !cond6_pass && config.cond6_pc_net_enabled {
            Some(format!("Network I/O ({:.0} KB/s)", effective_net_kbps))
        } else {
            None
        },
        probe_unavailable: None,
    };
    if config.cond6_pc_net_enabled && !cond6_pass {
        is_active_work = true;
        active_work_labels.push("Network".to_string());
        raw_blockers.push(PendingBlocker::new(
            6,
            "Network I/O",
            "Default-Route NIC",
            &format!("{:.0} KB/s", effective_net_kbps),
            None,
            None,
            format!("Whole-PC Network ({:.0} KB/s)", effective_net_kbps),
        ));
    }

    // Condition 10: Watched GPU % (Baseline-Relative + NVML Power Draw + Filter, §5.1)
    let (gpu_busy_sample, gpu_eff_thresh) = state.gpu_baseline.is_busy_sample(
        metrics.gpu_util,
        metrics.gpu_power_mw,
        config.gpu_threshold_pct,
        config.gpu_baseline_margin_pct,
    );
    let gpu_unavail = metrics.gpu_health.reason().map(str::to_string);
    let cond10_state = if gpu_unavail.is_some() {
        state.gpu_filter.update(false, std_cfg)
    } else {
        state.gpu_filter.update(gpu_busy_sample, std_cfg)
    };
    let cond10_pass = gpu_unavail.is_some() || !cond10_state.is_blocking();
    let gpu_power_suffix = match metrics.gpu_power_mw {
        Some(mw) if mw > 0 => format!(" | {:.0}W", mw as f32 / 1000.0),
        _ => String::new(),
    };
    let cond10_gpu = ConditionStatus {
        enabled: config.cond10_gpu_enabled,
        is_passing: cond10_pass,
        value_text: if let Some(ref reason) = gpu_unavail {
            format!("Unavailable ({})", reason)
        } else {
            format!(
                "{:.1}% (floor {:.1}%{}) [{}/{}]",
                metrics.gpu_util,
                state.gpu_baseline.util_floor(),
                gpu_power_suffix,
                state.gpu_filter.above_count(),
                state.gpu_filter.window_len()
            )
        },
        threshold_text: format!(
            "< {:.1}% (base+{:.0}%)",
            gpu_eff_thresh, config.gpu_baseline_margin_pct
        ),
        blocker_detail: if !cond10_pass && config.cond10_gpu_enabled {
            Some(format!(
                "Watched GPU ({:.1}% > {:.1}% floor+{:.0}%)",
                metrics.gpu_util,
                state.gpu_baseline.util_floor(),
                config.gpu_baseline_margin_pct
            ))
        } else {
            None
        },
        probe_unavailable: gpu_unavail,
    };
    if config.cond10_gpu_enabled && !cond10_pass {
        is_active_work = true;
        active_work_labels.push(format!("GPU {:.1}%", metrics.gpu_util));
        raw_blockers.push(PendingBlocker::new(
            10,
            "GPU Compute",
            "Discrete GPU",
            &format!("{:.1}%{}", metrics.gpu_util, gpu_power_suffix),
            None,
            None,
            format!("Watched GPU ({:.1}%)", metrics.gpu_util),
        ));
    }

    // Condition 7: Signal Conditioning Layer Summary (§2)
    let filter_summary_items = [
        (
            "Agent",
            state.agent_filter.state(),
            state.agent_filter.above_count(),
            state.agent_filter.window_len(),
            config.cond2_agent_cpu_enabled,
        ),
        (
            "DL",
            state.download_filter.state(),
            state.download_filter.above_count(),
            state.download_filter.window_len(),
            config.cond3_download_enabled,
        ),
        (
            "CPU",
            state.pc_cpu_filter.state(),
            state.pc_cpu_filter.above_count(),
            state.pc_cpu_filter.window_len(),
            config.cond4_pc_cpu_enabled,
        ),
        (
            "Disk",
            state.disk_filter.state(),
            state.disk_filter.above_count(),
            state.disk_filter.window_len(),
            config.cond5_pc_disk_enabled,
        ),
        (
            "Net",
            state.net_filter.state(),
            state.net_filter.above_count(),
            state.net_filter.window_len(),
            config.cond6_pc_net_enabled,
        ),
        (
            "GPU",
            state.gpu_filter.state(),
            state.gpu_filter.above_count(),
            state.gpu_filter.window_len(),
            config.cond10_gpu_enabled,
        ),
    ];
    let any_filtered_busy = filter_summary_items
        .iter()
        .any(|&(_, st, _, _, en)| en && st == SignalState::Busy);
    let max_pending = filter_summary_items
        .iter()
        .filter(|&&(_, st, _, _, en)| en && st == SignalState::Pending)
        .max_by_key(|x| x.2)
        .copied();
    let max_any = filter_summary_items
        .iter()
        .filter(|x| x.4)
        .max_by_key(|x| x.2)
        .copied();

    state.pc_spike_sustained_counter = max_any.map(|x| x.2).unwrap_or(0);

    let cond7_value_text = if !config.cond7_spike_filter_enabled {
        "Bypassed (raw 1s)".to_string()
    } else if any_filtered_busy {
        let busy_names: Vec<String> = filter_summary_items
            .iter()
            .filter(|x| x.4 && x.1 == SignalState::Busy)
            .map(|x| format!("{} {}/{}", x.0, x.2, x.3))
            .collect();
        format!("Busy ({})", busy_names.join(", "))
    } else if let Some((name, _, cnt, win, _)) = max_pending {
        format!("Spike filter {}/{} ({}) — Pending", cnt, win, name)
    } else {
        format!("Quiet (0/{})", config.spike_filter_secs)
    };

    let cond7_spike_filter = ConditionStatus {
        enabled: config.cond7_spike_filter_enabled,
        is_passing: !any_filtered_busy,
        value_text: cond7_value_text,
        threshold_text: format!(
            "Busy >={}/{} (Off <={})",
            std_cfg.busy_on, std_cfg.window_secs, std_cfg.quiet_off
        ),
        blocker_detail: if any_filtered_busy && config.cond7_spike_filter_enabled {
            Some("Signal Conditioning Layer Busy".to_string())
        } else {
            None
        },
        probe_unavailable: None,
    };

    // Condition 8: Monitor Timeout Tracking
    let cond8_monitor_timeout = ConditionStatus {
        enabled: config.cond8_monitor_timeout_enabled,
        is_passing: true,
        value_text: format!("{}m", config.monitor_timeout_mins),
        threshold_text: "Standby sync".to_string(),
        blocker_detail: None,
        probe_unavailable: None,
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

    // Condition 9: OS Power Requests (Probe availability + owner visibility + stale browser suppression, §5.7)
    let ignored_owners: Vec<String> = config
        .ignored_power_owners
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let mut active_power_owners: Vec<PowerRequestOwner> = metrics
        .power_requests
        .owners
        .iter()
        .filter(|o| o.category.eq_ignore_ascii_case("DISPLAY"))
        .filter(|o| {
            let nl = o.name.to_lowercase();
            !ignored_owners.iter().any(|ig| nl.contains(ig))
        })
        .cloned()
        .collect();

    // Legacy boolean fallback if caller only set `metrics.display_request_active` in simple tests
    if active_power_owners.is_empty()
        && metrics.display_request_active
        && metrics.power_requests.health.is_available()
        && metrics.power_requests.owners.is_empty()
    {
        active_power_owners.push(PowerRequestOwner {
            category: "DISPLAY".to_string(),
            caller_type: "PROCESS".to_string(),
            name: "Media/Display Lock".to_string(),
        });
    }

    // Check if ONLY browsers hold DISPLAY requests and they are silent/paused > 60s (§5.7)
    let all_browsers = !active_power_owners.is_empty()
        && active_power_owners
            .iter()
            .all(|o| is_browser_process(&o.name));
    let any_media_audible = metrics.media.browser_audio_active || metrics.media.smtc_playing;
    let mut suppressed_browser_request = None;

    if all_browsers && !any_media_audible {
        let start = *state
            .browser_silent_since_epoch
            .get_or_insert(metrics.now_epoch_secs);
        if metrics.now_epoch_secs.saturating_sub(start) >= 60 {
            suppressed_browser_request = Some(
                active_power_owners
                    .iter()
                    .map(|o| o.name.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            active_power_owners.clear();
        }
    } else {
        state.browser_silent_since_epoch = None;
    }

    let power_req_unavail = metrics.power_requests.health.reason().map(str::to_string);
    let cond9_effective_active = config.cond9_power_requests_enabled
        && config.respect_display_requests
        && power_req_unavail.is_none()
        && !active_power_owners.is_empty();
    let cond9_pass = !cond9_effective_active;

    let cond9_power_requests = ConditionStatus {
        enabled: config.cond9_power_requests_enabled && config.respect_display_requests,
        is_passing: cond9_pass,
        value_text: if let Some(ref reason) = power_req_unavail {
            format!("Unavailable ({})", reason)
        } else if let Some(ref sup) = suppressed_browser_request {
            format!("Ignored stale browser ({} paused >60s)", sup)
        } else if !active_power_owners.is_empty() {
            let names: Vec<&str> = active_power_owners
                .iter()
                .map(|o| o.name.as_str())
                .collect();
            format!("Active ({})", names.join(", "))
        } else {
            "None".to_string()
        },
        threshold_text: "Respect DISPLAY".to_string(),
        blocker_detail: if cond9_effective_active {
            let owner = active_power_owners
                .first()
                .map(|o| o.name.as_str())
                .unwrap_or("Display Request");
            Some(format!("Power Request ({})", owner))
        } else {
            None
        },
        probe_unavailable: power_req_unavail,
    };

    if cond9_effective_active {
        let primary_owner = active_power_owners
            .first()
            .map(|o| o.name.clone())
            .unwrap_or_else(|| "Display Lock".to_string());
        raw_blockers.push(PendingBlocker::new(
            9,
            "Power Request",
            &primary_owner,
            "DISPLAY lock",
            Some(primary_owner.clone()),
            Some("power_owner".to_string()),
            format!("Power Request (DISPLAY: {})", primary_owner),
        ));
    }

    // Condition 11: Microphone & Camera with Signal-Presence & App Categories (§5.4)
    let mic_held = metrics.mic_cam.mic_held || metrics.mic_active;
    let cam_held = metrics.mic_cam.cam_held || metrics.cam_active;
    let holder_name = if !metrics.mic_cam.holder_app.is_empty() {
        metrics.mic_cam.holder_app.clone()
    } else {
        metrics.mic_cam_app.clone()
    };
    let mic_unavail = metrics.mic_cam.health.reason().map(str::to_string);

    let (cond11_blocks, cond11_value_text, cond11_is_meeting) =
        if mic_unavail.is_some() && !metrics.mic_active && !metrics.cam_active {
            state.mic_held_since_epoch = None;
            state.unknown_mic_active_since_epoch = None;
            (
                false,
                format!(
                    "Unavailable ({})",
                    mic_unavail.as_deref().unwrap_or("Probe error")
                ),
                false,
            )
        } else if !mic_held && !cam_held {
            state.mic_held_since_epoch = None;
            state.unknown_mic_active_since_epoch = None;
            (false, "Quiet".to_string(), false)
        } else {
            let held_since = *state
                .mic_held_since_epoch
                .get_or_insert(metrics.now_epoch_secs);
            let held_mins = metrics.now_epoch_secs.saturating_sub(held_since) / 60;
            let category = classify_mic_app(
                &holder_name,
                &config.mic_meeting_apps,
                &config.mic_creative_apps,
            );
            let display_holder = if holder_name.is_empty() {
                "Call/App".to_string()
            } else {
                holder_name.clone()
            };
            let sig = metrics.mic_cam.capture_signal;

            match category {
                MicAppCategory::Meeting => {
                    // Strict rule (§5.4): Meeting apps ALWAYS block if mic/cam held OR render audio flows
                    (
                        true,
                        format!(
                            "In Call ({} — {}, {})",
                            display_holder,
                            if cam_held { "Cam/Mic" } else { "Mic/Audio" },
                            sig.label()
                        ),
                        true,
                    )
                }
                MicAppCategory::Creative => {
                    // Premiere / DAW / OBS blocks ONLY when acoustic capture signal is Active (or cam active)
                    if cam_held || sig == MicSignalState::Active {
                        (
                            true,
                            format!("Recording ({} — Signal {})", display_holder, sig.label()),
                            false,
                        )
                    } else {
                        (
                            false,
                            format!(
                                "Mic held by {} — no signal {}m ({}, not blocking)",
                                display_holder,
                                held_mins,
                                sig.label()
                            ),
                            false,
                        )
                    }
                }
                MicAppCategory::Unknown => {
                    // Unknown app blocks ONLY when real acoustic signal is Active for >= 10s
                    if sig == MicSignalState::Active {
                        let active_since = *state
                            .unknown_mic_active_since_epoch
                            .get_or_insert(metrics.now_epoch_secs);
                        if metrics.now_epoch_secs.saturating_sub(active_since) >= 5 {
                            (
                                true,
                                format!("Active ({} — Signal Active)", display_holder),
                                false,
                            )
                        } else {
                            (
                                false,
                                format!("Observing ({} — Signal Active)", display_holder),
                                false,
                            )
                        }
                    } else {
                        state.unknown_mic_active_since_epoch = None;
                        (
                            false,
                            format!(
                                "Mic held by {} — no signal {}m ({}, not blocking)",
                                display_holder,
                                held_mins,
                                sig.label()
                            ),
                            false,
                        )
                    }
                }
            }
        };

    let cond11_pass = !config.cond11_mic_cam_enabled || !cond11_blocks;
    let cond11_mic_cam = ConditionStatus {
        enabled: config.cond11_mic_cam_enabled,
        is_passing: cond11_pass,
        value_text: cond11_value_text.clone(),
        threshold_text: "Category + Signal".to_string(),
        blocker_detail: if !cond11_pass && config.cond11_mic_cam_enabled {
            Some(cond11_value_text.clone())
        } else {
            None
        },
        probe_unavailable: mic_unavail,
    };
    if config.cond11_mic_cam_enabled && !cond11_pass {
        let display_holder = if holder_name.is_empty() {
            "Call/Meeting".to_string()
        } else {
            holder_name.clone()
        };
        if cond11_is_meeting {
            is_meeting_live = true;
            meeting_app_name = display_holder.clone();
        }
        raw_blockers.push(PendingBlocker::new(
            11,
            if cond11_is_meeting {
                "Meeting Live"
            } else {
                "Mic / Camera Active"
            },
            &display_holder,
            metrics.mic_cam.capture_signal.label(),
            if !holder_name.is_empty() {
                Some(holder_name.clone())
            } else {
                None
            },
            Some("process".to_string()),
            format!("Mic/Cam In Use ({})", display_holder),
        ));
    }

    // Also handle meeting app with render audio active even if mic/cam registry handle wasn't held (§5.4 / T7)
    if config.cond11_mic_cam_enabled && cond11_pass && metrics.mic_cam.holder_render_audio_active {
        let holder = if holder_name.is_empty() {
            "Meeting Call".to_string()
        } else {
            holder_name.clone()
        };
        is_meeting_live = true;
        meeting_app_name = holder.clone();
        raw_blockers.push(PendingBlocker::new(
            11,
            "Meeting Live",
            &holder,
            "Audio flowing",
            None,
            None,
            format!("Meeting Audio Active ({})", holder),
        ));
    }

    // Check Agent Lease & Zombie Lease Detector (§5.6)
    let mut trigger_zombie_lease_alert = None;
    if let Some(until) = state.agent_lease_until_epoch_secs {
        if metrics.now_epoch_secs < until {
            let rem = until - metrics.now_epoch_secs;
            let reason = if state.agent_lease_reason.is_empty() {
                "Agent Task".to_string()
            } else {
                state.agent_lease_reason.clone()
            };

            // Zombie lease monitoring for PID-bound leases
            let mut hung_tag = String::new();
            let mut auto_expired = false;
            if state.agent_lease_pid.is_some() {
                let is_idle_tree =
                    metrics.lease_tree_cpu_pct < 0.2 && metrics.lease_tree_disk_kbps < 5.0;
                if is_idle_tree {
                    let start = *state
                        .lease_idle_since_epoch
                        .get_or_insert(metrics.now_epoch_secs);
                    let idle_secs = metrics.now_epoch_secs.saturating_sub(start);
                    let alert_secs = (config.lease_idle_mins.max(1) as u64) * 60;
                    if idle_secs >= alert_secs && !state.lease_hung_alert_sent {
                        state.lease_hung_alert_sent = true;
                        trigger_zombie_lease_alert = Some((reason.clone(), idle_secs));
                    }
                    if config.lease_auto_expire_enabled
                        && idle_secs >= (config.lease_auto_expire_mins.max(1) as u64) * 60
                    {
                        auto_expired = true;
                    } else if idle_secs >= alert_secs {
                        hung_tag = format!(" [IDLE {}m — may be hung]", idle_secs / 60);
                    }
                } else {
                    state.lease_idle_since_epoch = None;
                    state.lease_hung_alert_sent = false;
                }
            }

            if auto_expired {
                state.agent_lease_until_epoch_secs = None;
                state.agent_lease_reason.clear();
                state.agent_lease_pid = None;
                state.lease_idle_since_epoch = None;
                state.lease_hung_alert_sent = false;
            } else {
                is_explicit_hold = true;
                explicit_hold_reason = format!("Lease: {}", reason);
                let pid_info = state
                    .agent_lease_pid
                    .map(|p| format!(" [PID {}]", p))
                    .unwrap_or_default();
                raw_blockers.push(PendingBlocker::new(
                    0,
                    "Agent Lease",
                    &format!("{}{}", reason, pid_info),
                    &format!("{}s left{}", rem, hung_tag),
                    None,
                    None,
                    format!(
                        "Agent Lease: \"{}\"{} ({}s remaining)",
                        reason, pid_info, rem
                    ),
                ));
            }
        } else {
            state.agent_lease_until_epoch_secs = None;
            state.agent_lease_reason.clear();
            state.agent_lease_pid = None;
            state.lease_idle_since_epoch = None;
            state.lease_hung_alert_sent = false;
            if config.sleep_after_lease {
                state.countdown_secs = 0;
            }
        }
    } else {
        state.lease_idle_since_epoch = None;
        state.lease_hung_alert_sent = false;
    }

    // Check Postpone
    if let Some(until) = state.postponed_until_epoch_secs {
        if metrics.now_epoch_secs < until {
            let rem = until - metrics.now_epoch_secs;
            is_explicit_hold = true;
            explicit_hold_reason = format!("Postponed ({}s)", rem);
            raw_blockers.push(PendingBlocker::new(
                0,
                "Postponed",
                "User Action",
                &format!("{}s left", rem),
                None,
                None,
                format!("Postponed ({}s remaining)", rem),
            ));
        } else {
            state.postponed_until_epoch_secs = None;
        }
    }

    // Scenario 5.3: Passive Media State Machine
    let mut passive_media_app = String::new();
    let mut passive_media_locked = false;
    let mut passive_media_sleeping_now = false;

    let stage1_secs = (config.monitor_timeout_mins.max(1)) * 60;
    let prompt_trigger_secs = config
        .media_prompt_after_secs
        .min(stage1_secs.saturating_sub(30).max(30));

    let media_playing = metrics.media.is_playing_or_audible();
    let cur_session_key = if metrics.media.session_key.is_empty() {
        metrics.media.effective_app_name().to_lowercase()
    } else {
        metrics.media.session_key.clone()
    };

    if media_playing {
        state.media_paused_since_epoch = None;

        // If user clicked "Sleep now" for this specific session key, don't re-prompt or block
        if state.media_sleep_now_session_key.as_deref() == Some(cur_session_key.as_str()) {
            passive_media_sleeping_now = true;
        } else {
            let media_app = metrics.media.effective_app_name().to_string();

            // Check optional max lease cap
            let lock_expired = if config.media_max_lease_hours > 0 {
                state
                    .media_lock_since_epoch
                    .map(|st| {
                        metrics.now_epoch_secs.saturating_sub(st)
                            >= (config.media_max_lease_hours as u64) * 3600
                    })
                    .unwrap_or(false)
            } else {
                false
            };
            if lock_expired {
                state.media_lock_session_key = None;
                state.media_lock_since_epoch = None;
                state.media_prompt_shown_for_session = None;
            }

            let same_locked_session =
                state.media_lock_session_key.as_deref() == Some(cur_session_key.as_str());

            if same_locked_session {
                passive_media_locked = true;
                passive_media_app = media_app.clone();
                state.media_lock_app = media_app.clone();
                state.media_prompt_active = false;
            } else if metrics.idle_secs >= prompt_trigger_secs {
                // Entered Passive Media while human idle!
                if state.media_prompt_shown_for_session.as_deref() != Some(cur_session_key.as_str())
                {
                    state.media_prompt_active = true;
                    state.media_prompt_shown_for_session = Some(cur_session_key.clone());
                    state.media_prompt_since_epoch = Some(metrics.now_epoch_secs);
                } else if state.media_prompt_active {
                    // Ignoring the prompt means user is present -> auto-convert to session lock after 60s
                    if let Some(p_since) = state.media_prompt_since_epoch {
                        if metrics.now_epoch_secs.saturating_sub(p_since) >= 60 {
                            state.media_prompt_active = false;
                            state.media_lock_session_key = Some(cur_session_key.clone());
                            state.media_lock_since_epoch = Some(metrics.now_epoch_secs);
                            state.media_lock_app = media_app.clone();
                            passive_media_locked = true;
                        }
                    }
                }
                passive_media_app = media_app;
            } else {
                // User recently active; reset prompt gate until idle returns
                state.media_prompt_active = false;
            }

            if !passive_media_sleeping_now {
                let status_desc = if passive_media_locked {
                    "Session locked awake"
                } else if state.media_prompt_active {
                    "Awaiting prompt"
                } else {
                    "Playback active"
                };
                raw_blockers.push(PendingBlocker::new(
                    0,
                    "Passive Media",
                    &passive_media_app,
                    status_desc,
                    None,
                    None,
                    format!("Passive Media ({})", passive_media_app),
                ));
            }
        }
    } else {
        // Media paused / stopped -> clear after pause_grace_secs (default 60s, §5.3 & T9)
        state.media_idle_since_epoch = None;
        if state.media_lock_session_key.is_some()
            || state.media_prompt_active
            || state.media_sleep_now_session_key.is_some()
        {
            let paused_since = *state
                .media_paused_since_epoch
                .get_or_insert(metrics.now_epoch_secs);
            if metrics.now_epoch_secs.saturating_sub(paused_since)
                >= config.media_pause_grace_secs as u64
            {
                state.media_lock_session_key = None;
                state.media_lock_since_epoch = None;
                state.media_lock_app.clear();
                state.media_prompt_active = false;
                state.media_prompt_since_epoch = None;
                state.media_prompt_shown_for_session = None;
                state.media_sleep_now_session_key = None;
                state.media_paused_since_epoch = None;
            } else if state.media_lock_session_key.is_some() {
                passive_media_locked = true;
                passive_media_app = if state.media_lock_app.is_empty() {
                    "Media Player".to_string()
                } else {
                    state.media_lock_app.clone()
                };
                raw_blockers.push(PendingBlocker::new(
                    0,
                    "Passive Media",
                    &passive_media_app,
                    "Pause grace",
                    None,
                    None,
                    format!("Passive Media ({}, pause grace)", passive_media_app),
                ));
            }
        }
    }

    // Update blocker_first_seen timestamps for exact "since when" reporting (§6)
    let active_keys: Vec<String> = raw_blockers.iter().map(|b| b.tracking_key()).collect();
    state
        .blocker_first_seen
        .retain(|k, _| active_keys.contains(k));
    let mut structured_blockers = Vec::with_capacity(raw_blockers.len());
    let mut blockers = Vec::with_capacity(raw_blockers.len());

    for b in raw_blockers {
        let key = b.tracking_key();
        let first_seen = *state
            .blocker_first_seen
            .entry(key)
            .or_insert(metrics.now_epoch_secs);
        let since_secs = metrics.now_epoch_secs.saturating_sub(first_seen);
        blockers.push(b.legacy_text);
        structured_blockers.push(BlockerInfo {
            condition: b.condition,
            name: b.name,
            who: b.who,
            value: b.value,
            since_secs,
            ignore_target: b.ignore_target,
            ignore_kind: b.ignore_kind,
        });
    }

    // Intent Classification Precedence (§3 Table 1..8)
    let intent = if mode == OperationalMode::BestPerformance {
        UserIntent::BestPerformance
    } else if mode == OperationalMode::GuardOff {
        UserIntent::GuardOff
    } else if is_explicit_hold {
        UserIntent::ExplicitHold {
            reason: explicit_hold_reason,
        }
    } else if is_meeting_live {
        UserIntent::MeetingLive {
            app: meeting_app_name,
        }
    } else if !passive_media_app.is_empty() && !passive_media_sleeping_now {
        UserIntent::PassiveMedia {
            app: passive_media_app.clone(),
            locked: passive_media_locked,
        }
    } else if is_active_work {
        UserIntent::ActiveWork {
            summary: active_work_labels.join(", "),
        }
    } else if is_human_active {
        UserIntent::HumanActive
    } else if !blockers.is_empty() {
        UserIntent::ActiveWork {
            summary: blockers[0].clone(),
        }
    } else {
        UserIntent::IdleReady
    };

    // Stage 1 (Display-off) & OS Wake Lock per §3 & §6 & §5.8
    let is_video_or_browser_media = metrics.media.browser_audio_active
        || crate::intent::is_video_app(metrics.media.effective_app_name());
    let defer_display_off_for_media = (config.protect_monitor_audio && (metrics.media.monitor_speaker_active || metrics.is_laptop))
        || is_video_or_browser_media;
    let defer_display_off_for_monitor_audio = defer_display_off_for_media
        && (metrics.media.render_active || metrics.media.smtc_playing);

    let stage1_display_off_allowed = match mode {
        OperationalMode::BestPerformance | OperationalMode::GuardOff => false,
        OperationalMode::SmartGuard => {
            !is_human_active
                && !is_meeting_live
                && !cond9_effective_active
                && !defer_display_off_for_monitor_audio
        }
    };
    let hold_os_wake_lock = mode == OperationalMode::BestPerformance
        || (mode == OperationalMode::SmartGuard && (!blockers.is_empty() || (media_playing && !passive_media_sleeping_now)));

    // Critical Low Battery Override
    let mut critical_battery_sleep = false;
    if metrics.on_battery && config.low_battery_auto_sleep_enabled {
        if let Some(pct) = metrics.battery_pct {
            if pct <= config.low_battery_percent_threshold {
                critical_battery_sleep = true;
            }
        }
    }

    // Overall Decision & Countdown (§2.3: Pending NEVER resets countdown!)
    let mut trigger_sleep = false;
    let all_conditions_pass;
    let blocker_banner_text;

    if critical_battery_sleep {
        all_conditions_pass = true;
        trigger_sleep = true;
        state.countdown_secs = 0;
        blocker_banner_text = format!(
            "CRITICAL BATTERY: Machine sleeping to protect data ({}% <= {}%)",
            metrics.battery_pct.unwrap_or(0),
            config.low_battery_percent_threshold
        );
    } else if mode == OperationalMode::BestPerformance {
        all_conditions_pass = false;
        state.countdown_secs = countdown_max;
        blocker_banner_text = OperationalMode::BestPerformance.status_note().to_string();
    } else if mode == OperationalMode::GuardOff {
        all_conditions_pass = false;
        state.countdown_secs = countdown_max;
        blocker_banner_text = OperationalMode::GuardOff.status_note().to_string();
    } else if blockers.is_empty() {
        all_conditions_pass = true;
        if state.countdown_secs > 0 {
            state.countdown_secs -= 1;
        }
        blocker_banner_text = if state.unattended_wake_active {
            format!(
                "UNATTENDED WAKE -> Returning to sleep in {}s / {}s",
                state.countdown_secs, countdown_max
            )
        } else {
            format!(
                "ALL CONDITIONS PASS -> PC Sleeping in {}s / {}s",
                state.countdown_secs, countdown_max
            )
        };

        if state.countdown_secs == 0 {
            trigger_sleep = true;
            state.countdown_secs = countdown_max;
            state.unattended_wake_active = false;
        }
    } else {
        all_conditions_pass = false;
        state.countdown_secs = countdown_max;
        let first = &blockers[0];
        let extra = if blockers.len() > 1 {
            format!(" (+{} other)", blockers.len() - 1)
        } else {
            String::new()
        };
        blocker_banner_text = format!("BLOCKED BY: {}{}", first, extra);
    }

    let trigger_grace_banner = mode == OperationalMode::SmartGuard
        && all_conditions_pass
        && state.countdown_secs <= config.grace_period_secs
        && state.countdown_secs > 0;

    Decision {
        operational_mode: mode,
        intent,
        all_conditions_pass,
        blockers,
        structured_blockers,
        blocker_banner_text,
        countdown_secs: state.countdown_secs,
        countdown_max,
        trigger_sleep,
        trigger_grace_banner,
        stage1_display_off_allowed,
        defer_display_off_for_monitor_audio,
        hold_os_wake_lock,
        best_performance_expired,
        log_monitor_off_duration,
        woke_from_sleep_reset,
        detected_wake_classification,
        media_prompt_active: state.media_prompt_active,
        media_prompt_app: if state.media_prompt_active {
            metrics.media.effective_app_name().to_string()
        } else {
            String::new()
        },
        media_prompt_title: if state.media_prompt_active {
            metrics.media.smtc_title.clone()
        } else {
            String::new()
        },
        media_lock_active: state.media_lock_session_key.is_some(),
        media_lock_app: state.media_lock_app.clone(),
        trigger_zombie_lease_alert,
        active_power_owners: active_power_owners,
        suppressed_browser_request,
        gpu_effective_threshold_pct: gpu_eff_thresh,
        gpu_idle_floor_pct: state.gpu_baseline.util_floor(),
        suggested_thresholds,
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

struct PendingBlocker {
    condition: u8,
    name: String,
    who: String,
    value: String,
    ignore_target: Option<String>,
    ignore_kind: Option<String>,
    legacy_text: String,
}

impl PendingBlocker {
    fn new(
        condition: u8,
        name: &str,
        who: &str,
        value: &str,
        ignore_target: Option<String>,
        ignore_kind: Option<String>,
        legacy_text: String,
    ) -> Self {
        Self {
            condition,
            name: name.to_string(),
            who: who.to_string(),
            value: value.to_string(),
            ignore_target,
            ignore_kind,
            legacy_text,
        }
    }

    fn tracking_key(&self) -> String {
        format!("{}:{}:{}", self.condition, self.name, self.who)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> EvalConfig {
        EvalConfig::default()
    }

    fn passing_metrics(now: u64) -> Metrics {
        Metrics {
            idle_secs: 250,
            max_agent_cpu: 0.2,
            max_agent_proc: "None".to_string(),
            max_dl_kbps: 5.0,
            max_dl_proc: "None".to_string(),
            whole_pc_cpu: 4.0,
            whole_pc_disk_kbps: 40.0,
            whole_pc_net_kbps: 15.0,
            gpu_util: 1.0,
            now_epoch_secs: now,
            local_hour: 12,
            ..Default::default()
        }
    }

    fn fresh_state(cfg: &EvalConfig, now: u64) -> EvaluationState {
        EvaluationState {
            countdown_secs: cfg.idle_threshold_secs,
            last_wall_clock_secs: now.saturating_sub(1),
            ..Default::default()
        }
    }

    /// T1: GPU steady 5-12 %, user idle 5 min -> baseline adapts, filter passes, sleeps.
    #[test]
    fn replay_t1_gpu_steady_idle_noise_sleeps() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);
        let mut slept = false;

        for t in 0..300 {
            let mut m = passing_metrics(1000 + t);
            m.idle_secs = 180 + t as u32;
            m.gpu_util = 5.0 + ((t % 8) as f32); // oscillates 5..12%
            m.gpu_power_mw = Some(16_000);
            let d = evaluate(&cfg, &m, &mut st);
            assert!(
                d.cond10_gpu.is_passing,
                "T1 failed: steady GPU noise {}% blocked at t={}",
                m.gpu_util, t
            );
            if d.trigger_sleep {
                slept = true;
                break;
            }
        }
        assert!(slept, "T1 should complete countdown and trigger sleep");
    }

    /// T2: GPU 60 % sustained 20 s -> blocks, names GPU.
    #[test]
    fn replay_t2_gpu_sustained_compute_blocks() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);
        let mut blocked = false;
        let mut blocker_str = String::new();

        for t in 0..25 {
            let mut m = passing_metrics(1000 + t);
            m.gpu_util = 60.0;
            m.gpu_power_mw = Some(150_000);
            let d = evaluate(&cfg, &m, &mut st);
            if !d.cond10_gpu.is_passing {
                blocked = true;
                blocker_str = d.blocker_banner_text.clone();
            }
        }
        assert!(blocked, "T2 should block on sustained 60% GPU load");
        assert!(
            blocker_str.contains("GPU"),
            "Blocker banner should name GPU: {}",
            blocker_str
        );
    }

    /// T3: 1-second CPU spike to 50 % every 10 s -> no countdown reset.
    #[test]
    fn replay_t3_periodic_cpu_spike_no_countdown_reset() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);
        let mut prev_cd = st.countdown_secs;

        for t in 0..45 {
            let mut m = passing_metrics(1000 + t);
            m.whole_pc_cpu = if t % 10 == 5 { 55.0 } else { 4.0 };
            let d = evaluate(&cfg, &m, &mut st);
            assert!(
                d.countdown_secs < prev_cd || prev_cd == 0,
                "T3 failed: countdown reset or stalled at t={}, cd={} vs prev={}",
                t,
                d.countdown_secs,
                prev_cd
            );
            prev_cd = d.countdown_secs;
        }
    }

    /// T4: Defender 40 MB/s (filtered out as noise in metrics/attribution) -> no block;
    /// cargo at 14 MB/s sustained -> blocks and names cargo.
    #[test]
    fn replay_t4_disk_attribution_noise_vs_cargo() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);

        // Defender active on raw global disk, but attributed real disk is ~0 KB/s
        for t in 0..20 {
            let mut m = passing_metrics(1000 + t);
            m.whole_pc_disk_kbps = 40_960.0;
            m.attributed_disk_kbps = Some(100.0);
            m.top_disk_proc = "None".to_string();
            let d = evaluate(&cfg, &m, &mut st);
            assert!(d.cond5_pc_disk.is_passing, "T4 noise should not block");
        }

        // Now `cargo.exe` drives 14 MB/s sustained
        let mut blocked_msg = String::new();
        for t in 20..40 {
            let mut m = passing_metrics(1000 + t);
            m.whole_pc_disk_kbps = 15_000.0;
            m.attributed_disk_kbps = Some(14_336.0);
            m.top_disk_proc = "cargo.exe".to_string();
            m.top_disk_kbps = 14_336.0;
            let d = evaluate(&cfg, &m, &mut st);
            if !d.cond5_pc_disk.is_passing {
                blocked_msg = d.blocker_banner_text.clone();
            }
        }
        assert!(
            blocked_msg.contains("cargo.exe"),
            "T4 should block and name cargo.exe, got: {}",
            blocked_msg
        );
    }

    /// T5: 2 MB/s on loopback/virtual NIC only (default route network ~0) -> ignored.
    #[test]
    fn replay_t5_loopback_virtual_nic_ignored() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);

        for t in 0..25 {
            let mut m = passing_metrics(1000 + t);
            m.whole_pc_net_kbps = 2048.0; // e.g. localhost/Docker traffic
            m.default_route_net_kbps = Some(5.0); // default route internet NIC quiet
            let d = evaluate(&cfg, &m, &mut st);
            assert!(
                d.cond6_pc_net.is_passing,
                "T5 loopback/virtual traffic should be ignored"
            );
            assert!(d.all_conditions_pass);
        }
    }

    /// T6: Premiere holds mic with flat/ambient signal 4 min -> not blocking.
    #[test]
    fn replay_t6_premiere_ambient_mic_not_blocking() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);

        for t in 0..240 {
            let mut m = passing_metrics(1000 + t);
            m.mic_cam = MicCamSnapshot {
                health: ProbeHealth::Available,
                mic_held: true,
                cam_held: false,
                holder_app: "Adobe Premiere Pro.exe".to_string(),
                capture_peak: 0.004,
                capture_signal: MicSignalState::Ambient,
                ..Default::default()
            };
            let d = evaluate(&cfg, &m, &mut st);
            assert!(
                d.cond11_mic_cam.is_passing,
                "T6 Premiere ambient mic must not block"
            );
            assert!(d.all_conditions_pass);
        }
    }

    /// T7: Teams call, mic muted (Dead signal), render audio active -> blocks.
    #[test]
    fn replay_t7_teams_muted_call_with_render_audio_blocks() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);
        let mut m = passing_metrics(1000);
        m.mic_cam = MicCamSnapshot {
            health: ProbeHealth::Available,
            mic_held: true,
            cam_held: false,
            holder_app: "ms-teams.exe".to_string(),
            holder_render_audio_active: true,
            capture_peak: 0.0,
            capture_signal: MicSignalState::Dead,
        };
        let d = evaluate(&cfg, &m, &mut st);
        assert!(
            !d.cond11_mic_cam.is_passing,
            "T7 Teams call must block even when muted"
        );
        assert!(!d.all_conditions_pass);
        assert_eq!(d.intent.code(), "MeetingLive");
    }

    /// T8, T9, T10: Passive media prompt, Hide lock, Display off allowed,
    /// track change does NOT re-prompt, pause >60s clears lock and sleeps!
    #[test]
    fn replay_t8_t9_t10_passive_media_lifecycle() {
        let mut cfg = base_config();
        cfg.media_prompt_after_secs = 60;
        cfg.monitor_timeout_mins = 5;
        let mut st = fresh_state(&cfg, 1000);

        // T8: Media playing, human idle 600s -> triggers prompt ONCE
        let mut m = passing_metrics(1000);
        m.idle_secs = 600;
        m.media = MediaSessionSnapshot {
            health: ProbeHealth::Available,
            render_active: true,
            smtc_playing: true,
            smtc_app: "Spotify.exe".to_string(),
            smtc_title: "Song A".to_string(),
            session_key: "spotify::session-1".to_string(),
            ..Default::default()
        };

        let d1 = evaluate(&cfg, &m, &mut st);
        assert!(d1.media_prompt_active, "T8 should raise media prompt");
        assert!(!d1.all_conditions_pass);
        assert!(
            d1.stage1_display_off_allowed,
            "T8 Stage 1 display off still allowed"
        );

        // User clicks Hide -> creates lock bound to session_key
        st.apply_media_prompt_action(MediaPromptAction::Hide, 1001);
        let d2 = evaluate(&cfg, &m, &mut st);
        assert!(!d2.media_prompt_active);
        assert!(d2.media_lock_active);
        assert!(!d2.all_conditions_pass, "T8 locked playback blocks Stage 2");
        assert!(d2.stage1_display_off_allowed);

        // T10: Playlist changes track title ("Song B") with same session key -> NO re-prompt!
        m.now_epoch_secs = 1180;
        m.idle_secs = 780;
        m.media.smtc_title = "Song B - Another Track".to_string();
        let d3 = evaluate(&cfg, &m, &mut st);
        assert!(
            !d3.media_prompt_active,
            "T10 title change must NOT re-prompt"
        );
        assert!(d3.media_lock_active);
        assert!(!d3.all_conditions_pass);

        // T9: Media pauses for 90s -> clears lock after 60s grace and enters normal countdown
        m.media.render_active = false;
        m.media.smtc_playing = false;
        m.media.smtc_paused_or_stopped = true;
        // t=1181 (paused 0s): still locked within pause grace
        m.now_epoch_secs = 1181;
        let d_grace = evaluate(&cfg, &m, &mut st);
        assert!(d_grace.media_lock_active);
        // t=1271 (paused 90s > 60s): lock cleared, countdown resumes!
        m.now_epoch_secs = 1271; // note gap triggers reset handler cleanly unless stepped; step by 1s from 1182:
        for s in 1182..=1272 {
            m.now_epoch_secs = s;
            m.idle_secs = 780 + (s - 1180) as u32;
            let _ = evaluate(&cfg, &m, &mut st);
        }
        assert!(
            !st.media_lock_session_key.is_some(),
            "T9 lock must clear after 90s pause"
        );
        let d_after = evaluate(&cfg, &m, &mut st);
        assert!(
            d_after.all_conditions_pass,
            "T9 normal countdown resumes after pause grace"
        );
    }

    /// T11 & T12: Best Performance vs Guard Off.
    #[test]
    fn replay_t11_t12_operational_modes() {
        let mut cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);
        let m = passing_metrics(1000);

        // T11: Best Performance
        cfg.operational_mode = OperationalMode::BestPerformance;
        let d_bp = evaluate(&cfg, &m, &mut st);
        assert!(!d_bp.stage1_display_off_allowed);
        assert!(!d_bp.all_conditions_pass);
        assert!(d_bp.hold_os_wake_lock);
        assert_eq!(d_bp.intent.code(), "BestPerformance");

        // Turn off -> lock released
        cfg.operational_mode = OperationalMode::SmartGuard;
        let d_sg = evaluate(&cfg, &m, &mut st);
        assert!(!d_sg.hold_os_wake_lock);
        assert!(d_sg.all_conditions_pass);

        // T12: Guard Off
        cfg.operational_mode = OperationalMode::GuardOff;
        let d_off = evaluate(&cfg, &m, &mut st);
        assert_eq!(d_off.intent.code(), "GuardOff");
        assert!(!d_off.hold_os_wake_lock);
        assert_eq!(
            d_off.blocker_banner_text,
            "Paused: watchdog is off. Your normal OS power settings apply."
        );
    }

    /// T13: PID lease at zero CPU/IO 20 min -> alert at threshold; no expiry unless enabled.
    #[test]
    fn replay_t13_zombie_lease_alerts_without_unwanted_expiry() {
        let mut cfg = base_config();
        cfg.lease_idle_mins = 15;
        cfg.lease_auto_expire_enabled = false;
        let mut st = EvaluationState {
            countdown_secs: 180,
            last_wall_clock_secs: 999,
            agent_lease_until_epoch_secs: Some(5000),
            agent_lease_reason: "Frozen agent".to_string(),
            agent_lease_pid: Some(4242),
            ..Default::default()
        };

        let mut alerted = false;
        for min in 0..=20 {
            let mut m = passing_metrics(1000 + min * 60);
            m.lease_tree_cpu_pct = 0.0;
            m.lease_tree_disk_kbps = 0.0;
            st.last_wall_clock_secs = m.now_epoch_secs - 1;
            let d = evaluate(&cfg, &m, &mut st);
            if d.trigger_zombie_lease_alert.is_some() {
                alerted = true;
            }
            assert!(
                st.agent_lease_until_epoch_secs.is_some(),
                "Should NOT auto-expire when disabled"
            );
        }
        assert!(alerted, "T13 must trigger zombie lease alert at 15m");
    }

    /// T14: Wake by network at 3 a.m. (unattended, no user input) -> short unattended timer.
    #[test]
    fn replay_t14_unattended_network_wake_short_timer() {
        let mut cfg = base_config();
        cfg.idle_threshold_secs = 600; // 10 min normal
        cfg.unattended_wake_secs = 120; // 2 min unattended
        let mut st = EvaluationState {
            countdown_secs: 10,
            last_wall_clock_secs: 1000,
            ..Default::default()
        };

        // Jump 3 hours forward at 3 AM with UnattendedUnknown wake source
        let mut m = passing_metrics(1000 + 3 * 3600);
        m.idle_secs = 3 * 3600;
        m.wake_classification = Some(WakeClassification::UnattendedUnknown);
        let d = evaluate(&cfg, &m, &mut st);
        assert!(d.woke_from_sleep_reset);
        assert_eq!(
            d.countdown_max, 120,
            "T14 must use short 120s unattended timer"
        );
        assert_eq!(d.countdown_secs, 119);
    }

    /// T15: Probe unavailable (NVML missing, no admin for powercfg) -> condition passes cleanly, badge shown.
    #[test]
    fn replay_t15_probe_unavailable_passes_with_badge() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);
        let mut m = passing_metrics(1000);
        m.gpu_health = ProbeHealth::Unavailable("NVML not found".to_string());
        m.power_requests.health = ProbeHealth::Unavailable("Requires Administrator".to_string());
        m.display_request_active = true;

        let d = evaluate(&cfg, &m, &mut st);
        assert!(
            d.cond10_gpu.is_passing,
            "T15 unavailable GPU probe must pass"
        );
        assert!(d.cond10_gpu.probe_unavailable.is_some());
        assert!(
            d.cond9_power_requests.is_passing,
            "T15 unavailable powercfg probe must pass"
        );
        assert!(d.cond9_power_requests.probe_unavailable.is_some());
        assert!(d.all_conditions_pass);
    }

    /// T16: Windows GetProcessIoCounters inflated to 10.7 MB/s by non-disk IPC while real physical disk is ~184 KB/s.
    /// Must NOT block Cond 5 or Cond 7, and Stage 1 display-off must stay allowed.
    #[test]
    fn replay_t16_inflated_non_disk_ipc_passes_with_low_physical_disk() {
        let mut cfg = base_config();
        cfg.idle_threshold_secs = 15;
        cfg.pc_disk_threshold_kbps = 5000.0;
        let mut st = fresh_state(&cfg, 1000);

        for t in 0..30u64 {
            let mut m = passing_metrics(1000 + t);
            m.idle_secs = 60 + t as u32;
            m.raw_physical_disk_available = true;
            m.whole_pc_disk_kbps = 184.0; // real SSD physical activity
            m.attributed_disk_kbps = Some(10864.0); // inflated pipe/IPC logical counters
            m.top_disk_proc = "someproc.exe".to_string();
            m.top_disk_kbps = 2966.0;
            let d = evaluate(&cfg, &m, &mut st);
            assert!(d.cond5_pc_disk.is_passing, "T16 failed at t={}", t);
            assert!(d.cond7_spike_filter.is_passing);
            assert!(d.all_conditions_pass);
            assert!(d.stage1_display_off_allowed);
        }
    }

    /// T17: User idle for 2 minutes with silent browser open (no active SMTC/render audio, low physical disk) -> sleeps cleanly.
    #[test]
    fn replay_t17_browser_open_idle_2min_sleeps_cleanly() {
        let mut cfg = base_config();
        cfg.idle_threshold_secs = 15;
        let mut st = fresh_state(&cfg, 2000);
        let mut slept = false;

        for t in 0..20u64 {
            let mut m = passing_metrics(2000 + t);
            m.idle_secs = 120 + t as u32;
            m.raw_physical_disk_available = true;
            m.whole_pc_disk_kbps = 120.0;
            m.attributed_disk_kbps = Some(0.0); // browser IPC excluded
            m.default_route_net_kbps = Some(if t == 5 { 180.0 } else { 2.0 }); // small telemetry blip
            let d = evaluate(&cfg, &m, &mut st);
            assert!(d.stage1_display_off_allowed);
            if d.trigger_sleep {
                slept = true;
            }
        }
        assert!(
            slept,
            "T17 must trigger sleep when idle with silent browser"
        );
    }

    #[test]
    fn test_stage1_blocked_during_live_meeting() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);
        let mut m = passing_metrics(1000);
        m.mic_cam.mic_held = true;
        m.mic_cam.holder_app = "zoom.exe".to_string();
        m.mic_cam.capture_peak = 0.08;
        let d = evaluate(&cfg, &m, &mut st);
        assert!(matches!(d.intent, UserIntent::MeetingLive { .. }));
        assert!(!d.stage1_display_off_allowed);
    }

    #[test]
    fn test_video_media_blocks_sleep_and_defers_display_off() {
        let cfg = base_config();
        let mut st = fresh_state(&cfg, 1000);
        let mut m = passing_metrics(1000);
        m.idle_secs = 120;
        m.media = MediaSessionSnapshot {
            health: ProbeHealth::Available,
            render_active: true,
            render_peak: 0.25,
            browser_audio_active: true,
            render_app: "chrome.exe".to_string(),
            session_key: "render::chrome.exe::1".to_string(),
            ..Default::default()
        };

        let d = evaluate(&cfg, &m, &mut st);
        assert!(!d.all_conditions_pass, "Active video/movie must block sleep");
        assert!(!d.stage1_display_off_allowed, "Active video/movie must keep display ON");
        assert!(d.hold_os_wake_lock, "Active video/movie must hold OS wake lock");
        assert!(!d.trigger_sleep);
    }
}
