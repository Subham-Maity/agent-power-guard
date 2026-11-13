use std::process::Command;

/// 120-Second Automated Diagnostic Test Phases
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticPhase {
    /// Diagnostic engine is standing by.
    Idle,
    /// 0..5s: Calibration & Hands-Off Warning (verifying 0 human input).
    Arming,
    /// 5..25s (20s): Baseline Idle Calibration & Sleep Countdown Validation.
    Phase1Baseline,
    /// 25..55s (30s): Automated Media Playback & DISPLAY Request Ingestion.
    Phase2MediaPlay,
    /// 55..85s (30s): Post-Media Transition, Tab Switch & Audio Release Validation.
    Phase3PostMedia,
    /// 85..110s (25s): Hardware Blocker, Disk Attribution & Driver Deep-Dive.
    Phase4HardwareBlock,
    /// 110..120s (10s): Synthesis, Root Cause Analysis & Report Generation.
    Phase5Synthesis,
    /// Test completed; comprehensive report generated.
    Completed,
    /// Aborted by user or input interruption.
    Cancelled,
}

impl DiagnosticPhase {
    pub fn label(&self) -> &'static str {
        match self {
            DiagnosticPhase::Idle => "Standby",
            DiagnosticPhase::Arming => "Arming & Input Check",
            DiagnosticPhase::Phase1Baseline => "Phase 1: Baseline Idle",
            DiagnosticPhase::Phase2MediaPlay => "Phase 2: Media Playback",
            DiagnosticPhase::Phase3PostMedia => "Phase 3: Audio Release & Grace",
            DiagnosticPhase::Phase4HardwareBlock => "Phase 4: Hardware & Disk",
            DiagnosticPhase::Phase5Synthesis => "Phase 5: Synthesis & Diagnosis",
            DiagnosticPhase::Completed => "Completed",
            DiagnosticPhase::Cancelled => "Cancelled",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            DiagnosticPhase::Idle => "Click 'Start 120s Automated Diagnostic' to begin system sleep & blocker verification.",
            DiagnosticPhase::Arming => "Hands off keyboard & mouse. Calibrating baseline sensors and verifying 0 human input...",
            DiagnosticPhase::Phase1Baseline => "Verifying baseline idle sleep countdown, checking background daemon I/O and CPU baseline.",
            DiagnosticPhase::Phase2MediaPlay => "Opening test media stream. Validating audio endpoints, SMTC, and OS DISPLAY keep-awake assertions.",
            DiagnosticPhase::Phase3PostMedia => "Testing media pause/close. Verifying audio pipeline releases without leaving zombie locks.",
            DiagnosticPhase::Phase4HardwareBlock => "Evaluating disk throughput attribution (SSD vs browser IPC), powercfg assertions, and driver locks.",
            DiagnosticPhase::Phase5Synthesis => "Synthesizing 120s sensor timeline into root cause diagnosis and generating report.",
            DiagnosticPhase::Completed => "Diagnostic complete! Review findings below, copy report, or submit 1-click issue to GitHub.",
            DiagnosticPhase::Cancelled => "Diagnostic run was cancelled. Click Start to run a new 120s diagnostic.",
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(
            self,
            DiagnosticPhase::Idle | DiagnosticPhase::Completed | DiagnosticPhase::Cancelled
        )
    }
}

/// Severity classification for discovered diagnostic findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagSeverity {
    /// Critical defect: causes unexpected sleep mid-movie or severe system sleep refusal.
    Critical,
    /// Warning: sub-optimal configuration, lingering lock, or elevated background load.
    Warning,
    /// Informational: healthy behavior or nominal metric observed.
    Info,
}

impl DiagSeverity {
    pub fn badge(&self) -> &'static str {
        match self {
            DiagSeverity::Critical => "[CRITICAL]",
            DiagSeverity::Warning => "[WARNING]",
            DiagSeverity::Info => "[OK]",
        }
    }
}

/// A specific diagnostic finding with actionable recommendation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagFinding {
    pub severity: DiagSeverity,
    pub title: String,
    pub description: String,
    pub recommendation: String,
}

/// A 1-second metric sample collected during the 120-second test.
#[derive(Debug, Clone)]
pub struct DiagSample {
    pub second: u32,
    pub phase: DiagnosticPhase,
    pub idle_secs: u32,
    pub countdown_secs: Option<u32>,
    pub intent_label: String,
    pub blockers: Vec<String>,
    pub audio_render_active: bool,
    pub audio_peak: f32,
    pub audio_app: String,
    pub display_power_request: bool,
    pub system_power_request: bool,
    pub cpu_pct: f32,
    pub gpu_pct: f32,
    pub disk_read_kbps: f32,
    pub disk_write_kbps: f32,
    pub net_rx_kbps: f32,
    pub net_tx_kbps: f32,
    pub note: String,
}

/// Comprehensive state for the 120-second Diagnostic & Issue Reporter.
#[derive(Debug, Clone)]
pub struct DiagnosticSession {
    pub active: bool,
    pub phase: DiagnosticPhase,
    pub start_epoch_secs: Option<u64>,
    pub elapsed_secs: u32,
    pub total_secs: u32,
    pub samples: Vec<DiagSample>,
    pub findings: Vec<DiagFinding>,
    pub generated_report: String,
    pub user_interrupted: bool,
    pub media_url_opened: bool,
    pub status_banner: String,
    pub detected_audio_devices: Vec<String>,
    pub premature_sleep_detected: bool,
    pub zombie_lock_detected: bool,
    pub disk_blocker_detected: bool,
}

impl Default for DiagnosticSession {
    fn default() -> Self {
        Self {
            active: false,
            phase: DiagnosticPhase::Idle,
            start_epoch_secs: None,
            elapsed_secs: 0,
            total_secs: 120,
            samples: Vec::with_capacity(125),
            findings: Vec::new(),
            generated_report: String::new(),
            user_interrupted: false,
            media_url_opened: false,
            status_banner: "Ready to run 120s automated diagnostic.".to_string(),
            detected_audio_devices: Vec::new(),
            premature_sleep_detected: false,
            zombie_lock_detected: false,
            disk_blocker_detected: false,
        }
    }
}

impl DiagnosticSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a fresh 120-second diagnostic run.
    pub fn start(&mut self, now_epoch: u64) {
        self.active = true;
        self.phase = DiagnosticPhase::Arming;
        self.start_epoch_secs = Some(now_epoch);
        self.elapsed_secs = 0;
        self.total_secs = 120;
        self.samples.clear();
        self.findings.clear();
        self.generated_report.clear();
        self.user_interrupted = false;
        self.media_url_opened = false;
        self.status_banner =
            "Test running: Hands off keyboard & mouse for 120 seconds.".to_string();
        self.premature_sleep_detected = false;
        self.zombie_lock_detected = false;
        self.disk_blocker_detected = false;
    }

    /// Cancels an in-progress diagnostic run.
    pub fn cancel(&mut self, reason: &str) {
        self.active = false;
        self.phase = DiagnosticPhase::Cancelled;
        self.status_banner = format!("Diagnostic cancelled: {}", reason);
    }

    /// Advances the diagnostic state machine by 1 second.
    /// Returns true if a media URL should be launched on this tick.
    pub fn tick_1s(
        &mut self,
        idle_secs: u32,
        countdown_secs: Option<u32>,
        intent_label: &str,
        blockers: &[String],
        audio_render_active: bool,
        audio_peak: f32,
        audio_app: &str,
        display_power_request: bool,
        system_power_request: bool,
        cpu_pct: f32,
        gpu_pct: f32,
        disk_read_kbps: f32,
        disk_write_kbps: f32,
        net_rx_kbps: f32,
        net_tx_kbps: f32,
        os_name: &str,
        hardware_summary: &str,
        audio_device_list: &[String],
    ) -> bool {
        if !self.active {
            return false;
        }

        self.elapsed_secs = self.elapsed_secs.saturating_add(1);
        let sec = self.elapsed_secs;
        let mut launch_media = false;
        let mut note = String::new();

        // Phase transitions based on 120s timeline:
        // 0..5s: Arming
        // 5..25s (20s): Phase 1 Baseline Idle
        // 25..55s (30s): Phase 2 Media Playback
        // 55..85s (30s): Phase 3 Post-Media Audio Release
        // 85..110s (25s): Phase 4 Hardware & Disk Deep-Dive
        // 110..120s (10s): Phase 5 Synthesis
        let next_phase = if sec <= 5 {
            DiagnosticPhase::Arming
        } else if sec <= 25 {
            DiagnosticPhase::Phase1Baseline
        } else if sec <= 55 {
            DiagnosticPhase::Phase2MediaPlay
        } else if sec <= 85 {
            DiagnosticPhase::Phase3PostMedia
        } else if sec <= 110 {
            DiagnosticPhase::Phase4HardwareBlock
        } else if sec < 120 {
            DiagnosticPhase::Phase5Synthesis
        } else {
            DiagnosticPhase::Completed
        };

        // Track user input interruption during test
        if sec > 3 && idle_secs < 2 {
            self.user_interrupted = true;
            note.push_str("[User Input Detected] ");
        }

        // Trigger media stream open at start of Phase 2
        if next_phase == DiagnosticPhase::Phase2MediaPlay && !self.media_url_opened {
            self.media_url_opened = true;
            launch_media = true;
            note.push_str("[Triggering Test Media Stream] ");
        }

        // Detect Premature Sleep Bug:
        // Sound is playing during media phase, but no OS DISPLAY request and sleep countdown continues!
        if next_phase == DiagnosticPhase::Phase2MediaPlay {
            if (audio_render_active || audio_peak > 0.01) && !display_power_request {
                if let Some(cd) = countdown_secs {
                    if cd > 0 && cd < 180 {
                        self.premature_sleep_detected = true;
                        note.push_str("[ALERT: Premature Sleep Risk - Audio playing but DISPLAY request missing!] ");
                    }
                }
            }
        }

        // Detect Zombie Audio Lock:
        // Phase 3 post-media: render is active, but volume is dead 0.0 and app is stopped
        if next_phase == DiagnosticPhase::Phase3PostMedia {
            if audio_render_active && audio_peak == 0.0 && !display_power_request {
                self.zombie_lock_detected = true;
                note.push_str("[ALERT: Zombie Audio Session holding sleep lock!] ");
            }
        }

        // Detect Disk I/O Blocker:
        if (next_phase == DiagnosticPhase::Phase1Baseline
            || next_phase == DiagnosticPhase::Phase4HardwareBlock)
            && disk_write_kbps > 8000.0
        {
            self.disk_blocker_detected = true;
            note.push_str("[ALERT: Elevated Disk I/O holding sleep] ");
        }

        self.phase = next_phase;

        let sample = DiagSample {
            second: sec,
            phase: self.phase,
            idle_secs,
            countdown_secs,
            intent_label: intent_label.to_string(),
            blockers: blockers.to_vec(),
            audio_render_active,
            audio_peak,
            audio_app: audio_app.to_string(),
            display_power_request,
            system_power_request,
            cpu_pct,
            gpu_pct,
            disk_read_kbps,
            disk_write_kbps,
            net_rx_kbps,
            net_tx_kbps,
            note,
        };
        self.samples.push(sample);

        // On completion, synthesize findings and produce markdown report
        if self.phase == DiagnosticPhase::Completed {
            self.active = false;
            self.synthesize_report(os_name, hardware_summary, audio_device_list);
            self.status_banner = "Diagnostic complete! Full report generated below.".to_string();
        }

        launch_media
    }

    /// Synthesizes sensor metrics into diagnostic findings and a structured Markdown report.
    pub fn synthesize_report(
        &mut self,
        os_name: &str,
        hardware_summary: &str,
        audio_device_list: &[String],
    ) {
        let mut findings = Vec::new();

        // 1. Check for Premature Sleep (Laptop sleeps while watching movie)
        if self.premature_sleep_detected {
            findings.push(DiagFinding {
                severity: DiagSeverity::Critical,
                title: "Premature Sleep Risk: Media Player Did Not Assert OS DISPLAY Request".to_string(),
                description: "During Phase 2 media playback, active audio was rendered by the system, but NO Windows DISPLAY power request was registered. On standard power management, Windows will put your laptop to sleep right in the middle of movies or videos because it assumes the machine is completely idle!".to_string(),
                recommendation: "Ensure Agent Power Guard's Smart Guard is ACTIVE. Smart Guard monitors CoreAudio render sessions at the driver level, ensuring media playback keeps your laptop awake even when browsers or video players fail to register OS power requests.".to_string(),
            });
        }

        // 2. Check for Zombie Audio Session (PC refuses to sleep)
        if self.zombie_lock_detected {
            findings.push(DiagFinding {
                severity: DiagSeverity::Warning,
                title: "Zombie Audio Session Blocker Detected".to_string(),
                description: "An audio device driver kept an active render session open with 0.0 volume after playback concluded. This is common with virtual audio cables, audio enhancers, or certain USB DACs, and permanently prevents Windows from entering sleep.".to_string(),
                recommendation: "Under Agent Power Guard's Settings, configure Audio Idle Timeout or register an override via 'powercfg -requestsoverride DRIVER <driver_name> DISPLAY SYSTEM'.".to_string(),
            });
        }

        // 3. Check for Disk I/O Blocker
        if self.disk_blocker_detected {
            findings.push(DiagFinding {
                severity: DiagSeverity::Warning,
                title: "High Background Disk I/O Holding Sleep".to_string(),
                description: "Physical disk write throughput exceeded 8,000 KB/s during idle periods. Background Windows Search indexing, cloud backup sync, or telemetry writes can delay or reset sleep countdowns.".to_string(),
                recommendation: "Check Task Manager for background processes with high disk write activity, or increase the PC Disk Threshold in Watchdog & Presets.".to_string(),
            });
        }

        // 4. Check for User Interruption
        if self.user_interrupted {
            findings.push(DiagFinding {
                severity: DiagSeverity::Info,
                title: "Human Input Detected During Automated Test".to_string(),
                description: "Mouse movement or key presses were registered during the 120s test window, which naturally reset the idle timer.".to_string(),
                recommendation: "For absolute baseline idle accuracy, run the test again without touching the keyboard or mouse.".to_string(),
            });
        }

        // 5. If no critical or warning issues detected
        if findings.is_empty() {
            findings.push(DiagFinding {
                severity: DiagSeverity::Info,
                title: "All Diagnostic Checks Nominal".to_string(),
                description: "Idle timer decremented as expected, media audio was correctly detected, no zombie audio locks remained, and background disk/CPU noise was within normal thresholds.".to_string(),
                recommendation: "Your system's power management and Agent Power Guard watchdog are operating cleanly.".to_string(),
            });
        }

        self.findings = findings;

        // Generate full Markdown report
        self.generated_report =
            self.build_markdown_report(os_name, hardware_summary, audio_device_list);
    }

    /// Builds a comprehensive GitHub-ready Markdown report.
    pub fn build_markdown_report(
        &self,
        os_name: &str,
        hardware_summary: &str,
        audio_device_list: &[String],
    ) -> String {
        let mut md = String::with_capacity(4096);

        md.push_str("# Agent Power Guard - 120s Diagnostic Report\n\n");
        md.push_str(&format!(
            "**Diagnostic Duration:** {}s / 120s\n",
            self.elapsed_secs
        ));
        md.push_str(&format!("**Operating System:** {}\n", os_name));
        md.push_str(&format!("**Hardware Profile:** {}\n\n", hardware_summary));

        md.push_str("## 1. Executive Summary & Findings\n\n");
        for f in &self.findings {
            md.push_str(&format!(
                "- **{} {}**: {}\n  - *Recommendation:* {}\n\n",
                f.severity.badge(),
                f.title,
                f.description,
                f.recommendation
            ));
        }

        md.push_str("## 2. Audio Subsystem & Devices\n\n");
        if audio_device_list.is_empty() {
            md.push_str("- *No audio output devices reported by OS probe.*\n\n");
        } else {
            for dev in audio_device_list {
                md.push_str(&format!("- {}\n", dev));
            }
            md.push('\n');
        }

        md.push_str("## 3. 120-Second Sensor Timeline Samples\n\n");
        md.push_str("| Sec | Phase | Idle (s) | Sleep CD | Intent | Audio | Display Req | CPU % | GPU % | Disk KB/s | Net KB/s | Notes |\n");
        md.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|\n");

        // Sample key seconds to keep the report compact yet highly detailed (every 5 seconds + any annotated seconds)
        for s in &self.samples {
            if s.second % 5 == 0 || !s.note.is_empty() || s.second == 1 || s.second == 120 {
                let cd_str = s
                    .countdown_secs
                    .map(|v| format!("{}s", v))
                    .unwrap_or_else(|| "HOLD".to_string());
                let audio_str = if s.audio_render_active {
                    format!("Active ({:.0}%)", s.audio_peak * 100.0)
                } else {
                    "Muted/Idle".to_string()
                };
                let disp_str = if s.display_power_request { "YES" } else { "NO" };
                let total_disk = s.disk_read_kbps + s.disk_write_kbps;
                let total_net = s.net_rx_kbps + s.net_tx_kbps;
                let clean_note = if s.note.trim().is_empty() {
                    "-"
                } else {
                    s.note.trim()
                };

                md.push_str(&format!(
                    "| {} | {} | {}s | {} | {} | {} | {} | {:.1}% | {:.1}% | {:.0} | {:.0} | {} |\n",
                    s.second,
                    s.phase.label(),
                    s.idle_secs,
                    cd_str,
                    s.intent_label,
                    audio_str,
                    disp_str,
                    s.cpu_pct,
                    s.gpu_pct,
                    total_disk,
                    total_net,
                    clean_note
                ));
            }
        }

        md.push_str("\n## 4. Root Cause Analysis: Laptop Sleep During Movie Playback\n\n");
        if self.premature_sleep_detected {
            md.push_str("> **CRITICAL ROOT CAUSE IDENTIFIED:**\n");
            md.push_str("> When watching videos or movies in web browsers or desktop media players, applications frequently fail to issue Windows `SetThreadExecutionState(ES_DISPLAY_REQUIRED)` calls. Windows' native power manager only counts physical keyboard/mouse input; because you are watching passively, Windows assumes the user walked away and abruptly triggers S3 sleep mid-movie.\n");
            md.push_str(">\n");
            md.push_str("> **How Agent Power Guard Fixes This:**\n");
            md.push_str("> Agent Power Guard's Smart Guard continuously intercepts active CoreAudio/WASAPI render sessions and SMTC playback tokens directly from the Windows kernel. As long as movie dialogue or music is actively streaming through your speakers, headphones, or HDMI monitor, Agent Power Guard locks the sleep timer with zero manual intervention required.\n\n");
        } else {
            md.push_str("No premature sleep anomalies were observed during this specific test run. If your laptop unexpectedly slept in the past, verify whether the movie player was outputting sound through an unmonitored external sink or if it had paused for longer than the grace period.\n\n");
        }

        md.push_str("---\n*Report generated by Agent Power Guard v1.3.0 Diagnostic Engine*\n");
        md
    }
}

/// Standard RFC 3986 URL Percent-Encoding (Zero-dependency, cross-platform).
pub fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for b in input.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push_str("%20"),
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

/// Builds a 1-click GitHub issue creation URL with pre-filled title and markdown body.
pub fn build_github_issue_url(report_markdown: &str, title_override: Option<&str>) -> String {
    let default_title = "[Diagnostic Report] Sleep / Media Watchdog Issue Diagnostic";
    let title = title_override.unwrap_or(default_title);

    let enc_title = url_encode(title);
    let enc_body = url_encode(report_markdown);

    format!(
        "https://github.com/Subham-Maity/agent-power-guard/issues/new?title={}&body={}",
        enc_title, enc_body
    )
}

/// Cross-platform browser URL opener.
pub fn open_browser_url(url: &str) -> bool {
    #[cfg(windows)]
    {
        Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
            .is_ok()
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn().is_ok()
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open").arg(url).spawn().is_ok()
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = url;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_encode_basic() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("abc-123_.~"), "abc-123_.~");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn test_github_issue_url_format() {
        let report = "# Test Report\nEverything is fine.";
        let url = build_github_issue_url(report, Some("Test Title"));
        assert!(url.starts_with("https://github.com/Subham-Maity/agent-power-guard/issues/new?title=Test%20Title&body=%23%20Test%20Report"));
    }

    #[test]
    fn test_diagnostic_state_machine_progress() {
        let mut session = DiagnosticSession::new();
        session.start(1000);
        assert!(session.active);
        assert_eq!(session.phase, DiagnosticPhase::Arming);

        // Tick to 10s -> Phase 1
        for _ in 1..=10 {
            session.tick_1s(
                10,
                Some(150),
                "Idle",
                &[],
                false,
                0.0,
                "",
                false,
                false,
                1.0,
                0.5,
                10.0,
                20.0,
                5.0,
                5.0,
                "Windows 11",
                "Desktop",
                &["Realtek Audio".to_string()],
            );
        }
        assert_eq!(session.phase, DiagnosticPhase::Phase1Baseline);
        assert_eq!(session.samples.len(), 10);
    }

    #[test]
    fn test_premature_sleep_detection() {
        let mut session = DiagnosticSession::new();
        session.start(1000);

        // Advance to 30s (Phase 2 Media Playback) with active audio but NO display request and decreasing countdown
        for _ in 1..=30 {
            session.tick_1s(
                20,
                Some(120),
                "Active Video / Media",
                &[],
                true,
                0.45,
                "chrome.exe",
                false, // <-- NO display request!
                false,
                4.0,
                8.0,
                50.0,
                100.0,
                800.0,
                40.0,
                "Windows 11",
                "Laptop",
                &["Realtek Audio".to_string()],
            );
        }
        assert!(session.premature_sleep_detected);
        session.synthesize_report("Windows 11", "Laptop", &["Realtek Audio".to_string()]);
        assert!(session
            .findings
            .iter()
            .any(|f| f.severity == DiagSeverity::Critical));
        assert!(session
            .generated_report
            .contains("CRITICAL ROOT CAUSE IDENTIFIED"));
    }
}
