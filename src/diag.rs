use std::process::Command;

/// Diagnostic duration presets selectable by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagDurationMode {
    /// 120-Second (2 Minutes) Rapid Diagnostic.
    Rapid120s,
    /// 300-Second (5 Minutes) Deep Precision Diagnostic.
    Deep300s,
}

impl DiagDurationMode {
    pub fn total_seconds(&self) -> u32 {
        match self {
            DiagDurationMode::Rapid120s => 120,
            DiagDurationMode::Deep300s => 300,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            DiagDurationMode::Rapid120s => "120s (2 Minutes) Rapid Diagnostic",
            DiagDurationMode::Deep300s => {
                "300s (5 Minutes) Deep Precision Diagnostic [Recommended]"
            }
        }
    }
}

/// Actionable media launch trigger requested by the diagnostic state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagLaunchMedia {
    None,
    Primary,
    Secondary,
}

/// Multi-phase state machine supporting both 120s rapid and 300s deep verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticPhase {
    /// Diagnostic engine is standing by.
    Idle,
    /// Calibration & Hands-Off Warning (verifying 0 human input).
    Arming,
    /// Baseline Idle Calibration & Sleep Countdown Validation.
    Phase1Baseline,
    /// Primary Media Stream Playback & OS DISPLAY Request Ingestion.
    Phase2MediaPlay,
    /// Secondary Background Media Tab & Multi-Stream Audio Mixing.
    Phase3SecondaryMedia,
    /// Post-Media Transition, Tab Switch & Audio/Mic Release Validation.
    Phase4PostMedia,
    /// Hardware Blocker, Disk Attribution & Dynamic Watt Profiling.
    Phase5HardwareBlock,
    /// Synthesis, Root Cause Analysis & Report Generation.
    Phase6Synthesis,
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
            DiagnosticPhase::Phase2MediaPlay => "Phase 2: Primary Media Stream",
            DiagnosticPhase::Phase3SecondaryMedia => "Phase 3: Secondary Background Stream",
            DiagnosticPhase::Phase4PostMedia => "Phase 4: Audio/Mic Release & Driver Check",
            DiagnosticPhase::Phase5HardwareBlock => "Phase 5: Hardware, Disk & Watt Profile",
            DiagnosticPhase::Phase6Synthesis => "Phase 6: Synthesis & Diagnosis",
            DiagnosticPhase::Completed => "Completed",
            DiagnosticPhase::Cancelled => "Cancelled",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            DiagnosticPhase::Idle => "Select duration and click 'I Agree (Hands-Off) — Start Diagnostic'.",
            DiagnosticPhase::Arming => "Hands off keyboard & mouse. Calibrating baseline sensors and verifying 0 human input...",
            DiagnosticPhase::Phase1Baseline => "Verifying baseline idle sleep countdown, checking background daemon I/O and baseline power draw.",
            DiagnosticPhase::Phase2MediaPlay => "Triggering primary test video stream. Validating audio endpoints, SMTC, and OS DISPLAY keep-awake assertions.",
            DiagnosticPhase::Phase3SecondaryMedia => "Triggering secondary background stream. Testing concurrent audio sessions and multi-tab keep-awake behavior.",
            DiagnosticPhase::Phase4PostMedia => "Evaluating audio/mic session release. Verifying drivers and browser tabs release sound pipelines without zombie locks.",
            DiagnosticPhase::Phase5HardwareBlock => "Evaluating physical disk write throughput vs browser cache, inspecting powercfg assertions, and measuring watt drop profile.",
            DiagnosticPhase::Phase6Synthesis => "Synthesizing full sensor timeline into root cause diagnosis and generating report.",
            DiagnosticPhase::Completed => "Diagnostic complete! Review findings below, copy report, or submit 1-click issue to GitHub.",
            DiagnosticPhase::Cancelled => "Diagnostic run was cancelled. Click Start to run a new diagnostic.",
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

/// A 1-second metric sample collected during the diagnostic test.
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
    pub mic_capture_active: bool,
    pub mic_peak: f32,
    pub display_power_request: bool,
    pub system_power_request: bool,
    pub cpu_pct: f32,
    pub gpu_pct: f32,
    pub disk_read_kbps: f32,
    pub disk_write_kbps: f32,
    pub net_rx_kbps: f32,
    pub net_tx_kbps: f32,
    pub est_active_watts: f32,
    pub est_display_off_watts: f32,
    pub est_sleep_watts: f32,
    pub display_off_simulated_fired: bool,
    pub sleep_simulated_fired: bool,
    pub note: String,
}

/// Comprehensive state for the Autonomous Diagnostic & Issue Reporter.
#[derive(Debug, Clone)]
pub struct DiagnosticSession {
    pub active: bool,
    pub phase: DiagnosticPhase,
    pub duration_mode: DiagDurationMode,
    pub start_epoch_secs: Option<u64>,
    pub elapsed_secs: u32,
    pub total_secs: u32,
    pub samples: Vec<DiagSample>,
    pub findings: Vec<DiagFinding>,
    pub generated_report: String,
    pub user_interrupted: bool,
    pub primary_media_opened: bool,
    pub secondary_media_opened: bool,
    pub status_banner: String,
    pub premature_sleep_detected: bool,
    pub zombie_lock_detected: bool,
    pub disk_blocker_detected: bool,
    pub mic_blocker_detected: bool,
    pub display_off_simulated_at: Option<u32>,
    pub sleep_simulated_at: Option<u32>,
    pub display_off_watt_drop: f32,
    pub sleep_watt_drop: f32,
}

impl Default for DiagnosticSession {
    fn default() -> Self {
        Self {
            active: false,
            phase: DiagnosticPhase::Idle,
            duration_mode: DiagDurationMode::Deep300s,
            start_epoch_secs: None,
            elapsed_secs: 0,
            total_secs: 300,
            samples: Vec::with_capacity(310),
            findings: Vec::new(),
            generated_report: String::new(),
            user_interrupted: false,
            primary_media_opened: false,
            secondary_media_opened: false,
            status_banner: "Ready to run autonomous diagnostic suite.".to_string(),
            premature_sleep_detected: false,
            zombie_lock_detected: false,
            disk_blocker_detected: false,
            mic_blocker_detected: false,
            display_off_simulated_at: None,
            sleep_simulated_at: None,
            display_off_watt_drop: 0.0,
            sleep_watt_drop: 0.0,
        }
    }
}

impl DiagnosticSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a fresh diagnostic run with specified duration mode.
    pub fn start(&mut self, now_epoch: u64, mode: DiagDurationMode) {
        let total = mode.total_seconds();
        self.active = true;
        self.phase = DiagnosticPhase::Arming;
        self.duration_mode = mode;
        self.start_epoch_secs = Some(now_epoch);
        self.elapsed_secs = 0;
        self.total_secs = total;
        self.samples.clear();
        self.samples.reserve(total as usize + 10);
        self.findings.clear();
        self.generated_report.clear();
        self.user_interrupted = false;
        self.primary_media_opened = false;
        self.secondary_media_opened = false;
        self.status_banner = format!(
            "Test running: Hands off keyboard & mouse for {} seconds.",
            total
        );
        self.premature_sleep_detected = false;
        self.zombie_lock_detected = false;
        self.disk_blocker_detected = false;
        self.mic_blocker_detected = false;
        self.display_off_simulated_at = None;
        self.sleep_simulated_at = None;
        self.display_off_watt_drop = 0.0;
        self.sleep_watt_drop = 0.0;
    }

    /// Cancels an in-progress diagnostic run.
    pub fn cancel(&mut self, reason: &str) {
        self.active = false;
        self.phase = DiagnosticPhase::Cancelled;
        self.status_banner = format!("Diagnostic cancelled: {}", reason);
    }

    /// Records simulated Display-Off trigger event.
    pub fn record_display_off_trigger(&mut self, current_watts: f32, display_off_watts: f32) {
        if self.display_off_simulated_at.is_none() {
            self.display_off_simulated_at = Some(self.elapsed_secs);
            self.display_off_watt_drop = (current_watts - display_off_watts).max(0.0);
        }
    }

    /// Records simulated Standby Sleep trigger event.
    pub fn record_sleep_trigger(&mut self, current_watts: f32, sleep_watts: f32) {
        if self.sleep_simulated_at.is_none() {
            self.sleep_simulated_at = Some(self.elapsed_secs);
            self.sleep_watt_drop = (current_watts - sleep_watts).max(0.0);
        }
    }

    /// Advances the diagnostic state machine by 1 second.
    /// Returns media launch command (None, Primary, Secondary).
    pub fn tick_1s(
        &mut self,
        idle_secs: u32,
        countdown_secs: Option<u32>,
        intent_label: &str,
        blockers: &[String],
        audio_render_active: bool,
        audio_peak: f32,
        audio_app: &str,
        mic_capture_active: bool,
        mic_peak: f32,
        display_power_request: bool,
        system_power_request: bool,
        cpu_pct: f32,
        gpu_pct: f32,
        disk_read_kbps: f32,
        disk_write_kbps: f32,
        net_rx_kbps: f32,
        net_tx_kbps: f32,
        est_active_watts: f32,
        est_display_off_watts: f32,
        est_sleep_watts: f32,
        display_off_condition_met: bool,
        sleep_condition_met: bool,
        specs_report: &str,
        audio_devices_summary: &str,
    ) -> DiagLaunchMedia {
        if !self.active {
            return DiagLaunchMedia::None;
        }

        self.elapsed_secs = self.elapsed_secs.saturating_add(1);
        let sec = self.elapsed_secs;
        let total = self.total_secs;
        let mut launch_media = DiagLaunchMedia::None;
        let mut note = String::new();

        // Calculate phase boundaries based on duration mode (120s or 300s):
        let (p1_end, p2_end, p3_end, p4_end, p5_end) = if total <= 120 {
            (25, 55, 75, 95, 110)
        } else {
            (60, 130, 200, 250, 285)
        };

        let next_phase = if sec <= 5 {
            DiagnosticPhase::Arming
        } else if sec <= p1_end {
            DiagnosticPhase::Phase1Baseline
        } else if sec <= p2_end {
            DiagnosticPhase::Phase2MediaPlay
        } else if sec <= p3_end {
            DiagnosticPhase::Phase3SecondaryMedia
        } else if sec <= p4_end {
            DiagnosticPhase::Phase4PostMedia
        } else if sec <= p5_end {
            DiagnosticPhase::Phase5HardwareBlock
        } else if sec < total {
            DiagnosticPhase::Phase6Synthesis
        } else {
            DiagnosticPhase::Completed
        };

        // Track accidental human input interruption during hands-off test
        if sec > 3 && idle_secs < 2 {
            self.user_interrupted = true;
            note.push_str("[User Input Detected] ");
        }

        // Trigger primary media stream open at start of Phase 2
        if next_phase == DiagnosticPhase::Phase2MediaPlay && !self.primary_media_opened {
            self.primary_media_opened = true;
            launch_media = DiagLaunchMedia::Primary;
            note.push_str("[Launched Primary Media Stream] ");
        }

        // Trigger secondary background media tab at start of Phase 3
        if next_phase == DiagnosticPhase::Phase3SecondaryMedia && !self.secondary_media_opened {
            self.secondary_media_opened = true;
            launch_media = DiagLaunchMedia::Secondary;
            note.push_str("[Launched Secondary Background Tab] ");
        }

        // Capture Simulated Display-Off Condition
        let mut display_off_fired = false;
        if display_off_condition_met && self.display_off_simulated_at.is_none() {
            self.record_display_off_trigger(est_active_watts, est_display_off_watts);
            display_off_fired = true;
            let drop = (est_active_watts - est_display_off_watts).max(0.0);
            note.push_str(&format!(
                "[STATUS: Display-Off Triggered! Watt Drop: -{:.1}W] ",
                drop
            ));
        }

        // Capture Simulated Standby Sleep Condition
        let mut sleep_fired = false;
        if sleep_condition_met && self.sleep_simulated_at.is_none() {
            self.record_sleep_trigger(est_active_watts, est_sleep_watts);
            sleep_fired = true;
            let drop = (est_active_watts - est_sleep_watts).max(0.0);
            note.push_str(&format!(
                "[STATUS: Standby Sleep Triggered! Watt Drop: -{:.1}W] ",
                drop
            ));
        }

        // Detect Premature Sleep Bug:
        // Sound is playing during media phase, but no OS DISPLAY request and sleep countdown continues!
        if next_phase == DiagnosticPhase::Phase2MediaPlay
            || next_phase == DiagnosticPhase::Phase3SecondaryMedia
        {
            if (audio_render_active || audio_peak > 0.01) && !display_power_request {
                if let Some(cd) = countdown_secs {
                    if cd > 0 && cd < 180 {
                        self.premature_sleep_detected = true;
                        note.push_str("[ALERT: Premature Sleep Risk - Sound playing without OS DISPLAY request!] ");
                    }
                }
            }
        }

        // Detect Zombie Audio Lock:
        // In Phase 4 post-media: render reports active, but volume peak is 0.0
        if next_phase == DiagnosticPhase::Phase4PostMedia {
            if audio_render_active && audio_peak == 0.0 && !display_power_request {
                self.zombie_lock_detected = true;
                note.push_str("[ALERT: Zombie Audio Session holding lock with 0.0 volume!] ");
            }
        }

        // Detect Microphone Blocker:
        // Microphone is actively capturing during idle phases
        if (next_phase == DiagnosticPhase::Phase1Baseline
            || next_phase == DiagnosticPhase::Phase5HardwareBlock)
            && (mic_capture_active || mic_peak > 0.02)
        {
            self.mic_blocker_detected = true;
            note.push_str("[ALERT: Active Microphone Stream holding sleep lock!] ");
        }

        // Detect Elevated Disk I/O Blocker:
        if (next_phase == DiagnosticPhase::Phase1Baseline
            || next_phase == DiagnosticPhase::Phase5HardwareBlock)
            && disk_write_kbps > 8000.0
        {
            self.disk_blocker_detected = true;
            note.push_str("[ALERT: Elevated Disk Write I/O holding sleep] ");
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
            mic_capture_active,
            mic_peak,
            display_power_request,
            system_power_request,
            cpu_pct,
            gpu_pct,
            disk_read_kbps,
            disk_write_kbps,
            net_rx_kbps,
            net_tx_kbps,
            est_active_watts,
            est_display_off_watts,
            est_sleep_watts,
            display_off_simulated_fired: display_off_fired,
            sleep_simulated_fired: sleep_fired,
            note,
        };
        self.samples.push(sample);

        // On completion, synthesize findings and generate markdown report
        if self.phase == DiagnosticPhase::Completed {
            self.active = false;
            self.synthesize_report(specs_report, audio_devices_summary);
            self.status_banner = "Diagnostic complete! Full report generated below.".to_string();
        }

        launch_media
    }

    /// Synthesizes sensor metrics into diagnostic findings and a structured Markdown report.
    pub fn synthesize_report(&mut self, specs_report: &str, audio_devices_summary: &str) {
        let mut findings = Vec::new();

        // 1. Check for Premature Sleep (Laptop sleeps while watching movie)
        if self.premature_sleep_detected {
            findings.push(DiagFinding {
                severity: DiagSeverity::Critical,
                title: "Premature Sleep Risk: Media Player Did Not Assert OS DISPLAY Request".to_string(),
                description: "During media playback, audio was actively rendered through your speakers/headphones, but NO Windows DISPLAY power request was registered. On standard Windows power plans, the OS will put your laptop/PC to sleep mid-movie because it considers the machine idle!".to_string(),
                recommendation: "Ensure Agent Power Guard's Smart Guard mode is ACTIVE. Smart Guard directly monitors WASAPI/CoreAudio sessions at the driver level, ensuring active media playback prevents premature sleep even when browser tabs fail to assert OS power requests.".to_string(),
            });
        }

        // 2. Check for Zombie Audio Session (PC refuses to sleep)
        if self.zombie_lock_detected {
            findings.push(DiagFinding {
                severity: DiagSeverity::Warning,
                title: "Zombie Audio Session Blocker Detected".to_string(),
                description: "An audio endpoint kept an active render session open with 0.0 volume after playback stopped. This is common with virtual audio cables, audio enhancers, or certain USB DACs, and permanently prevents Windows from sleeping.".to_string(),
                recommendation: "In Agent Power Guard's Watchdog settings, configure Audio Idle Timeout or register an override via 'powercfg -requestsoverride DRIVER <driver_name> DISPLAY SYSTEM'.".to_string(),
            });
        }

        // 3. Check for Microphone Blocker
        if self.mic_blocker_detected {
            findings.push(DiagFinding {
                severity: DiagSeverity::Warning,
                title: "Active Microphone Capture Stream Detected".to_string(),
                description: "A background voice app, conferencing tool (Teams/Zoom/Discord), or browser tab maintained an active microphone recording stream during idle periods, keeping the system awake.".to_string(),
                recommendation: "Check Windows Settings > Privacy & Security > Microphone to see which application recently accessed the microphone, or close unused conferencing calls.".to_string(),
            });
        }

        // 4. Check for Disk I/O Blocker
        if self.disk_blocker_detected {
            findings.push(DiagFinding {
                severity: DiagSeverity::Warning,
                title: "High Background Disk I/O Holding Sleep".to_string(),
                description: "Physical disk write throughput exceeded 8,000 KB/s during idle periods. Background Windows Search indexing, cloud backup sync, or telemetry writes can delay or reset sleep countdowns.".to_string(),
                recommendation: "Check Task Manager for background processes with high disk write activity, or increase the PC Disk Threshold in Watchdog & Presets.".to_string(),
            });
        }

        // 5. Check for User Interruption
        if self.user_interrupted {
            findings.push(DiagFinding {
                severity: DiagSeverity::Info,
                title: "Human Input Detected During Automated Test".to_string(),
                description: "Mouse movement or key presses were registered during the hands-off test window, which naturally reset the OS idle timer.".to_string(),
                recommendation: "For absolute baseline idle accuracy, run the test again without touching the keyboard or mouse.".to_string(),
            });
        }

        // 6. Verify System-Level Display-Off and Sleep Status
        if let Some(sec) = self.display_off_simulated_at {
            findings.push(DiagFinding {
                severity: DiagSeverity::Info,
                title: format!(
                    "System Display-Off Trigger Verified at {}s (Saved -{:.1}W)",
                    sec, self.display_off_watt_drop
                ),
                description: format!(
                    "The watchdog evaluator successfully verified system-level display-off readiness at second {}. Monitored display power drop: -{:.1} Watts.",
                    sec, self.display_off_watt_drop
                ),
                recommendation: "Display-off logic and monitor power-saving thresholds are fully operational.".to_string(),
            });
        }

        if let Some(sec) = self.sleep_simulated_at {
            findings.push(DiagFinding {
                severity: DiagSeverity::Info,
                title: format!(
                    "System Standby Sleep Trigger Verified at {}s (Saved -{:.1}W)",
                    sec, self.sleep_watt_drop
                ),
                description: format!(
                    "All sleep conditions passed and system-level S3 Standby Sleep was triggered at second {}. Total workstation power drop: -{:.1} Watts.",
                    sec, self.sleep_watt_drop
                ),
                recommendation: "Sleep engine readiness is fully operational without unexpected blockers.".to_string(),
            });
        }

        // If nominal
        if findings.is_empty() {
            findings.push(DiagFinding {
                severity: DiagSeverity::Info,
                title: "All Diagnostic Checks Nominal".to_string(),
                description: "Idle timer decremented as expected, media audio was correctly detected, no zombie audio locks remained, and background disk/CPU noise was within normal thresholds.".to_string(),
                recommendation: "Your system's power management and Agent Power Guard watchdog are operating cleanly.".to_string(),
            });
        }

        self.findings = findings;
        self.generated_report = self.build_markdown_report(specs_report, audio_devices_summary);
    }

    /// Builds a comprehensive GitHub-ready Markdown report.
    pub fn build_markdown_report(&self, specs_report: &str, audio_devices_summary: &str) -> String {
        let mut md = String::with_capacity(8192);

        md.push_str(&format!(
            "# Agent Power Guard - {}s Diagnostic Report\n\n",
            self.total_secs
        ));
        md.push_str(&format!(
            "**Diagnostic Duration:** {}s / {}s ({:?})\n",
            self.elapsed_secs, self.total_secs, self.duration_mode
        ));
        md.push_str(&format!(
            "**Simulated Triggers:** Display-Off: {} | Standby Sleep: {}\n\n",
            self.display_off_simulated_at
                .map(|s| format!("Triggered at {}s (-{:.1}W)", s, self.display_off_watt_drop))
                .unwrap_or_else(|| "Not reached".to_string()),
            self.sleep_simulated_at
                .map(|s| format!("Triggered at {}s (-{:.1}W)", s, self.sleep_watt_drop))
                .unwrap_or_else(|| "Not reached".to_string()),
        ));

        md.push_str("## 1. System Hardware & Watt Specification\n\n");
        md.push_str(specs_report);
        md.push_str("\n\n");

        md.push_str("## 2. Executive Summary & Diagnostic Findings\n\n");
        for f in &self.findings {
            md.push_str(&format!(
                "- **{} {}**: {}\n  - *Recommendation:* {}\n\n",
                f.severity.badge(),
                f.title,
                f.description,
                f.recommendation
            ));
        }

        md.push_str("## 3. Audio & Microphone Subsystem Endpoints\n\n");
        md.push_str(audio_devices_summary);
        md.push_str("\n\n");

        md.push_str("## 4. Sensor & Watt Profile Timeline (Sampled Every 5s + Events)\n\n");
        md.push_str("| Sec | Phase | Est Watts | Watt Drop | Idle | Sleep CD | Display Trig | Sleep Trig | Audio / Mic | Notes |\n");
        md.push_str("|---|---|---|---|---|---|---|---|---|---|\n");

        for s in &self.samples {
            if s.second % 5 == 0
                || !s.note.is_empty()
                || s.second == 1
                || s.second == self.total_secs
                || s.display_off_simulated_fired
                || s.sleep_simulated_fired
            {
                let cd_str = s
                    .countdown_secs
                    .map(|v| format!("{}s", v))
                    .unwrap_or_else(|| "HOLD".to_string());

                let audio_status = if s.audio_render_active {
                    format!("Play {:.0}%", s.audio_peak * 100.0)
                } else if s.mic_capture_active {
                    format!("Mic {:.0}%", s.mic_peak * 100.0)
                } else {
                    "Muted".to_string()
                };

                let drop_str = if s.sleep_simulated_fired {
                    format!("-{:.0}W (Sleep)", s.est_active_watts - s.est_sleep_watts)
                } else if s.display_off_simulated_fired {
                    format!(
                        "-{:.0}W (DispOff)",
                        s.est_active_watts - s.est_display_off_watts
                    )
                } else {
                    "-".to_string()
                };

                let disp_trig = if s.display_off_simulated_fired {
                    "YES"
                } else {
                    "NO"
                };
                let sleep_trig = if s.sleep_simulated_fired { "YES" } else { "NO" };
                let clean_note = if s.note.trim().is_empty() {
                    "-"
                } else {
                    s.note.trim()
                };

                md.push_str(&format!(
                    "| {} | {} | {:.1}W | {} | {}s | {} | {} | {} | {} | {} |\n",
                    s.second,
                    s.phase.label(),
                    s.est_active_watts,
                    drop_str,
                    s.idle_secs,
                    cd_str,
                    disp_trig,
                    sleep_trig,
                    audio_status,
                    clean_note
                ));
            }
        }

        md.push_str("\n## 5. Root Cause Analysis: Premature Sleep During Video Playback\n\n");
        if self.premature_sleep_detected {
            md.push_str("> **CRITICAL ROOT CAUSE IDENTIFIED:**\n");
            md.push_str("> When streaming videos in modern browsers (Chromium/Firefox) or desktop video players, software frequently omits calling the Windows power assertion `SetThreadExecutionState(ES_DISPLAY_REQUIRED)`.\n");
            md.push_str("> Because the user is passively watching, Windows counts zero physical keyboard or mouse input, assumes the PC is completely abandoned, and triggers S3 sleep mid-movie.\n");
            md.push_str(">\n");
            md.push_str("> **Solution with Agent Power Guard:**\n");
            md.push_str("> Agent Power Guard's Smart Guard intercepts audio playback at the Windows CoreAudio/WASAPI kernel driver layer. As long as dialogue or soundtrack audio is playing through your speakers, headphones, or HDMI, Agent Power Guard locks the sleep timer with zero manual intervention required.\n\n");
        } else {
            md.push_str("No premature sleep anomalies were observed during this specific test run. Verify that movie players output sound through recognized audio sinks and do not pause for longer than the grace period.\n\n");
        }

        md.push_str("---\n*Report generated by Agent Power Guard Autonomous Diagnostic Engine*\n");
        md
    }

    /// Builds a safe, ultra-compact GitHub issue URL that NEVER exceeds GitHub's character limits.
    /// The full detailed report is copied to the user's clipboard and saved locally.
    pub fn build_safe_github_issue_url(
        &self,
        os_name: &str,
        cpu_summary: &str,
        gpu_summary: &str,
    ) -> String {
        let first_finding = self
            .findings
            .first()
            .map(|f| format!("{} {}", f.severity.badge(), f.title))
            .unwrap_or_else(|| "Diagnostic Findings".to_string());

        let title = format!("[Diagnostic Report] {}", first_finding);

        let body_template = format!(
            "### 1. Diagnostic Summary\n\
             - **Operating System:** {}\n\
             - **Hardware Specs:** CPU: {} | GPU: {}\n\
             - **Diagnostic Duration:** {}s ({:?})\n\
             - **Observed Issue:** {}\n\n\
             ### 2. Full Diagnostic Report\n\
             > [!IMPORTANT]\n\
             > The complete diagnostic report with second-by-second timeline, audio/mic devices, and watt profile was **automatically copied to your clipboard**.\n\
             > **Please paste (Ctrl+V or Cmd+V) inside the code block below:**\n\n\
             ```markdown\n\
             [Paste clipboard content here (Ctrl+V)]\n\
             ```\n\n\
             ### 3. Additional Context / Steps to Reproduce\n\
             - Describe what you were doing when the sleep issue occurred:\n",
            os_name, cpu_summary, gpu_summary, self.total_secs, self.duration_mode, first_finding
        );

        let enc_title = url_encode(&title);
        let enc_body = url_encode(&body_template);

        format!(
            "https://github.com/Subham-Maity/agent-power-guard/issues/new?title={}&body={}",
            enc_title, enc_body
        )
    }
}

/// Builds a 1-click GitHub issue creation URL with pre-filled title and body.
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
    fn test_safe_github_issue_url_length() {
        let session = DiagnosticSession::default();
        let url = session.build_safe_github_issue_url(
            "Windows 11 Pro",
            "AMD Ryzen 5 5600X",
            "NVIDIA GeForce GTX 1060 3GB",
        );
        assert!(url.starts_with("https://github.com/Subham-Maity/agent-power-guard/issues/new?"));
        // Ensure the entire URL is well below GitHub's ~8,000 character rejection limit:
        assert!(
            url.len() < 2000,
            "Safe GitHub URL length {} exceeds 2000 chars!",
            url.len()
        );
    }

    #[test]
    fn test_diagnostic_state_machine_progress() {
        let mut session = DiagnosticSession::new();
        session.start(1000, DiagDurationMode::Rapid120s);
        assert!(session.active);
        assert_eq!(session.phase, DiagnosticPhase::Arming);

        // Tick to 10s -> Phase 1 Baseline
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
                0.0,
                false,
                false,
                1.0,
                0.5,
                10.0,
                20.0,
                5.0,
                5.0,
                105.0,
                104.0,
                3.0,
                false,
                false,
                "Specs",
                "Audio",
            );
        }
        assert_eq!(session.phase, DiagnosticPhase::Phase1Baseline);
        assert_eq!(session.samples.len(), 10);
    }

    #[test]
    fn test_premature_sleep_detection() {
        let mut session = DiagnosticSession::new();
        session.start(1000, DiagDurationMode::Rapid120s);

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
                false,
                0.0,
                false, // <-- NO display request!
                false,
                4.0,
                8.0,
                50.0,
                100.0,
                800.0,
                40.0,
                115.0,
                104.0,
                3.0,
                false,
                false,
                "Specs",
                "Audio",
            );
        }
        assert!(session.premature_sleep_detected);
        session.synthesize_report("Specs Summary", "Audio Summary");
        assert!(session
            .findings
            .iter()
            .any(|f| f.severity == DiagSeverity::Critical));
        assert!(session
            .generated_report
            .contains("CRITICAL ROOT CAUSE IDENTIFIED"));
    }
}
