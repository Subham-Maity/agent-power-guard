use crate::signal::{MicSignalState, ProbeHealth};

/// Top-level user operational mode (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OperationalMode {
    /// Normal watchdog evaluation managing Stage 1 (display-off) and Stage 2 (sleep).
    #[default]
    SmartGuard,
    /// Watchdog inactive; the operating system's normal power plan applies.
    GuardOff,
    /// Stage 1 and Stage 2 disabled; an explicit OS wake lock keeps display and PC awake.
    BestPerformance,
}

impl OperationalMode {
    pub fn name(self) -> &'static str {
        match self {
            OperationalMode::SmartGuard => "Smart Guard",
            OperationalMode::GuardOff => "Guard Off",
            OperationalMode::BestPerformance => "Best Performance",
        }
    }

    pub fn api_key(self) -> &'static str {
        match self {
            OperationalMode::SmartGuard => "smart_guard",
            OperationalMode::GuardOff => "guard_off",
            OperationalMode::BestPerformance => "best_performance",
        }
    }

    pub fn status_note(self) -> &'static str {
        match self {
            OperationalMode::SmartGuard => "Active: watchdog manages display-off and sleep.",
            OperationalMode::GuardOff => {
                "Paused: watchdog is off. Your normal OS power settings apply."
            }
            OperationalMode::BestPerformance => {
                "Locked awake: sleep and display-off are disabled until you turn this off."
            }
        }
    }

    pub fn from_api_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "smart" | "smart_guard" | "smartguard" | "on" => Some(Self::SmartGuard),
            "off" | "guard_off" | "guardoff" | "paused" => Some(Self::GuardOff),
            "best" | "best_performance" | "bestperformance" | "never_sleep" => {
                Some(Self::BestPerformance)
            }
            _ => None,
        }
    }
}

/// Unified intent state emitted by the intent classification step (§3) in precedence order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UserIntent {
    BestPerformance,
    GuardOff,
    ExplicitHold {
        reason: String,
    },
    MeetingLive {
        app: String,
    },
    PassiveMedia {
        app: String,
        locked: bool,
    },
    ActiveWork {
        summary: String,
    },
    HumanActive,
    #[default]
    IdleReady,
}

impl UserIntent {
    pub fn code(&self) -> &'static str {
        match self {
            UserIntent::BestPerformance => "BestPerformance",
            UserIntent::GuardOff => "GuardOff",
            UserIntent::ExplicitHold { .. } => "ExplicitHold",
            UserIntent::MeetingLive { .. } => "MeetingLive",
            UserIntent::PassiveMedia { .. } => "PassiveMedia",
            UserIntent::ActiveWork { .. } => "ActiveWork",
            UserIntent::HumanActive => "HumanActive",
            UserIntent::IdleReady => "IdleReady",
        }
    }

    pub fn display_label(&self) -> String {
        match self {
            UserIntent::BestPerformance => "Best Performance (Locked Awake)".to_string(),
            UserIntent::GuardOff => "Guard Off (OS Power Plan)".to_string(),
            UserIntent::ExplicitHold { reason } => format!("Explicit Hold ({})", reason),
            UserIntent::MeetingLive { app } => format!("Meeting / Call Live ({})", app),
            UserIntent::PassiveMedia { app, locked } => {
                if *locked {
                    format!("Passive Media Locked ({})", app)
                } else {
                    format!("Passive Media Detected ({})", app)
                }
            }
            UserIntent::ActiveWork { summary } => format!("Active Work ({})", summary),
            UserIntent::HumanActive => "Human Input Active".to_string(),
            UserIntent::IdleReady => "Idle & Ready".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPromptAction {
    KeepAwake,
    SleepNow,
    Hide,
}

impl MediaPromptAction {
    pub fn from_api_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "keep" | "keep_awake" | "keepawake" => Some(Self::KeepAwake),
            "sleep" | "sleep_now" | "sleepnow" => Some(Self::SleepNow),
            "hide" | "dismiss" => Some(Self::Hide),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicAppCategory {
    Meeting,
    Creative,
    Unknown,
}

pub fn classify_mic_app(
    app_name: &str,
    meeting_keywords_csv: &str,
    creative_keywords_csv: &str,
) -> MicAppCategory {
    let lower = app_name.to_lowercase();
    if lower.is_empty() {
        return MicAppCategory::Unknown;
    }
    if meeting_keywords_csv
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .any(|k| lower.contains(&k))
    {
        return MicAppCategory::Meeting;
    }
    if creative_keywords_csv
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .any(|k| lower.contains(&k))
    {
        return MicAppCategory::Creative;
    }
    MicAppCategory::Unknown
}

pub fn is_browser_process(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "chrome", "msedge", "edge", "firefox", "brave", "opera", "vivaldi", "arc", "chromium",
        "waterfox", "zen",
    ]
    .iter()
    .any(|b| lower.contains(b))
}

pub fn is_video_app(name: &str) -> bool {
    let lower = name.to_lowercase();
    is_browser_process(&lower)
        || [
            "vlc",
            "mpv",
            "mpc-hc",
            "mpc-be",
            "potplayer",
            "wmplayer",
            "microsoft.media.player",
            "movies & tv",
            "netflix",
            "prime",
            "disney",
            "stremio",
            "plex",
            "kodi",
            "iina",
            "quicktime",
        ]
        .iter()
        .any(|v| lower.contains(v))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PowerRequestOwner {
    pub category: String,    // "DISPLAY", "SYSTEM", "AWAYMODE"
    pub caller_type: String, // "PROCESS", "DRIVER", "SRV"
    pub name: String,        // e.g. "chrome.exe" or "Realtek Audio"
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PowerRequestSnapshot {
    pub health: ProbeHealth,
    pub display_active: bool,
    pub system_active: bool,
    pub owners: Vec<PowerRequestOwner>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaSessionSnapshot {
    pub health: ProbeHealth,
    pub render_active: bool,
    pub render_peak: f32,
    pub render_app: String,
    pub monitor_speaker_active: bool,
    pub browser_audio_active: bool,
    pub smtc_playing: bool,
    pub smtc_paused_or_stopped: bool,
    pub smtc_app: String,
    pub smtc_title: String,
    /// Stable lock key built ONLY from source app + session/PID identity (never track title).
    pub session_key: String,
}

impl MediaSessionSnapshot {
    pub fn is_playing_or_audible(&self) -> bool {
        self.render_active || self.smtc_playing
    }

    pub fn effective_app_name(&self) -> &str {
        if !self.smtc_app.is_empty() {
            &self.smtc_app
        } else if !self.render_app.is_empty() {
            &self.render_app
        } else {
            "Media Player"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MicCamSnapshot {
    pub health: ProbeHealth,
    pub mic_held: bool,
    pub cam_held: bool,
    pub holder_app: String,
    pub holder_render_audio_active: bool,
    pub capture_peak: f32,
    pub capture_signal: MicSignalState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WakeClassification {
    #[default]
    UserInput,
    ScheduledTimer,
    UnattendedUnknown,
}

impl WakeClassification {
    pub fn label(self) -> &'static str {
        match self {
            WakeClassification::UserInput => "User Input",
            WakeClassification::ScheduledTimer => "Scheduled Wake Timer",
            WakeClassification::UnattendedUnknown => "Unattended / Network / Background",
        }
    }
}

/// Transparent structured blocker row exposed in GUI, Block History, and `/api/status` (§6 & §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockerInfo {
    pub condition: u8,
    pub name: String,
    pub who: String,
    pub value: String,
    pub since_secs: u64,
    pub ignore_target: Option<String>,
    pub ignore_kind: Option<String>,
}

/// Structured Guide Section shared across in-app `(i)` modal, web dashboard, and documentation (§6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuideSection {
    pub title: &'static str,
    pub body: &'static str,
}

pub fn get_info_guide_sections() -> Vec<GuideSection> {
    vec![
        GuideSection {
            title: "1. Three Power Modes",
            body: "Smart Guard manages Stage 1 (monitor off) and Stage 2 (deep sleep) using all 11 intent-aware checks.\n\
                   Guard Off disables the watchdog completely so your normal Windows/OS power plan controls sleep.\n\
                   Best Performance turns off both monitor-off and sleep and holds a dedicated OS wake lock (optional 1h/2h expiry).",
        },
        GuideSection {
            title: "2. The 11 Guard Conditions",
            body: "1. Human Idle: Resets whenever keyboard or mouse input occurs.\n\
                   2. Watched Agent CPU: Tracks AI agent & developer process trees (shells alone don't block; active workers do).\n\
                   3. Active Downloads: Watches download clients over disk/network throughput.\n\
                   4. Whole-PC CPU: Post-filter fallback protection for heavy background workloads.\n\
                   5. Whole-PC Disk I/O: Attributed per-process after filtering out OS noise (Defender, SearchIndexer, Windows Update).\n\
                   6. Whole-PC Network: Measures only default-route NIC traffic (ignoring loopback, WSL, Hyper-V, Docker bridges).\n\
                   7. Signal Conditioning Filter: Rolling duty-cycle hysteresis across analog conditions 2, 3, 4, 5, 6, and 10.\n\
                   8. Monitor Timeout Sync: Keeps OS display-off standby timer synced to Stage 1.\n\
                   9. Power Requests: Tracks OS DISPLAY locks with per-owner visibility, ignore list, and automatic suppression of stale silent browser tabs (>60s).\n\
                   10. Watched GPU (AI): Uses baseline-relative GPU utilization + NVML board power draw so steady desktop compositor activity (5-12%) never blocks sleep.\n\
                   11. Mic & Camera (Meetings vs Creative): Meeting apps (Zoom, Teams, Discord) always block when active or receiving audio; creative apps (Premiere, DAWs, OBS) block only when real acoustic signal is Active.",
        },
        GuideSection {
            title: "3. How the Signal-Conditioning Filter Works (8/15 Indicator)",
            body: "Instead of resetting the sleep countdown on a single 1-second spike, each analog condition maintains a sliding window (default 15s for system metrics, 10s for agents/downloads).\n\
                   - Pending (e.g. 3/15): Brief blips are visible in the UI but do NOT reset the countdown.\n\
                   - Busy (>= 8/15): Sustained or bursty real work flips the gate to Busy and pauses/resets sleep.\n\
                   - Hysteresis (<= 2/15): Once Busy, it waits until activity clearly drops to unlatch, preventing flapping.",
        },
        GuideSection {
            title: "4. Passive Media Prompts & Session Locks",
            body: "If you are sitting idle (in bed or across the room) watching or listening to media, a non-focus-stealing prompt asks whether playback should keep the PC awake.\n\
                   - Clicking 'Keep awake' or 'Hide' binds a lock strictly to that player session (NOT track title, so playlists never re-prompt on next song).\n\
                   - Stage 1 (display off) still proceeds to save screen energy unless monitor-speaker protection is active.\n\
                   - Pausing/stopping playback for longer than pause_grace (default 60s) clears the lock automatically.",
        },
        GuideSection {
            title: "5. Blocker Transparency & One-Tap Ignore",
            body: "Every active blocker reports WHO holds the PC awake, HOW MUCH resource it uses, and SINCE WHEN.\n\
                   If a background tool, antivirus, or paused app causes a false block, click '+ Ignore' right next to the blocker (or in the Web Dashboard) to ignore that process, disk noise, or power-request owner permanently.",
        },
        GuideSection {
            title: "6. Why Won't My PC Sleep? Troubleshooting Checklist",
            body: "1. Check the Intent badge & Active Blockers list at the top of Watchdog & Presets.\n\
                   2. Look for a 'Probe Unavailable' badge on Cond 9 if running unelevated (run as Admin if you need Windows powercfg /requests inspection).\n\
                   3. Check Block History tab for what held the machine awake overnight (including Unattended Wake entries).\n\
                   4. Verify Best Performance mode or a hung Agent Lease (zombie detector alerts after 15m of 0% CPU/IO) isn't holding the lock.",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operational_mode_notes() {
        assert_eq!(
            OperationalMode::GuardOff.status_note(),
            "Paused: watchdog is off. Your normal OS power settings apply."
        );
        assert_eq!(
            OperationalMode::SmartGuard.status_note(),
            "Active: watchdog manages display-off and sleep."
        );
        assert_eq!(
            OperationalMode::BestPerformance.status_note(),
            "Locked awake: sleep and display-off are disabled until you turn this off."
        );
    }

    #[test]
    fn test_mic_category_classification() {
        let meet = "zoom, teams, discord, slack, whatsapp, meet";
        let creative = "premiere, resolve, audacity, obs, reaper";
        assert_eq!(
            classify_mic_app("ms-teams.exe", meet, creative),
            MicAppCategory::Meeting
        );
        assert_eq!(
            classify_mic_app("Adobe Premiere Pro.exe", meet, creative),
            MicAppCategory::Creative
        );
        assert_eq!(
            classify_mic_app("random-tool.exe", meet, creative),
            MicAppCategory::Unknown
        );
    }
}
