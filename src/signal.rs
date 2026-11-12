use std::collections::VecDeque;

/// Health state of a hardware or OS probe.
/// Rule 3: Probe failures never silently block or silently allow without visibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeHealth {
    Available,
    Unavailable(String),
}

impl Default for ProbeHealth {
    fn default() -> Self {
        Self::Available
    }
}

impl ProbeHealth {
    pub fn is_available(&self) -> bool {
        matches!(self, ProbeHealth::Available)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            ProbeHealth::Available => None,
            ProbeHealth::Unavailable(r) => Some(r.as_str()),
        }
    }
}

/// Signal state after rolling-window duty-cycle hysteresis (§2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignalState {
    #[default]
    Quiet,
    Pending,
    Busy,
}

impl SignalState {
    pub fn is_blocking(self) -> bool {
        matches!(self, SignalState::Busy)
    }

    pub fn label(self) -> &'static str {
        match self {
            SignalState::Quiet => "Quiet",
            SignalState::Pending => "Pending",
            SignalState::Busy => "Busy",
        }
    }
}

/// Configuration for one analog signal filter (§2.2 & §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalFilterConfig {
    pub window_secs: u32,
    pub busy_on: u32,
    pub quiet_off: u32,
}

impl SignalFilterConfig {
    pub fn new(window_secs: u32, busy_on: u32, quiet_off: u32) -> Self {
        let w = window_secs.max(1);
        let on = busy_on.clamp(1, w);
        let off = quiet_off.min(on.saturating_sub(1));
        Self {
            window_secs: w,
            busy_on: on,
            quiet_off: off,
        }
    }

    /// Scales a reference (window, busy_on, quiet_off) rule to a user-configured window length.
    pub fn scaled(ref_window: u32, ref_busy: u32, ref_quiet: u32, user_window: u32) -> Self {
        let w = user_window.max(1);
        let rw = ref_window.max(1);
        if w == rw {
            return Self::new(w, ref_busy, ref_quiet);
        }
        let scaled_on = ((ref_busy as f32 / rw as f32) * w as f32).round() as u32;
        let on = scaled_on.clamp(1, w);
        let scaled_off = ((ref_quiet as f32 / rw as f32) * w as f32).floor() as u32;
        let off = scaled_off.min(on.saturating_sub(1));
        Self::new(w, on, off)
    }
}

/// Rolling window duty-cycle + hysteresis filter for noisy 1 Hz analog metrics (§2).
///
/// Unlike an EMA, one brief huge spike counts as only 1 sample in the window and
/// cannot artificially extend blocking once the burst passes, while bursty real work
/// that alternates active/idle still exceeds `busy_on / window_secs`.
#[derive(Debug, Clone, Default)]
pub struct SignalFilter {
    window: VecDeque<bool>,
    busy: bool,
    last_state: SignalState,
    above_count: u32,
    window_len: u32,
}

impl SignalFilter {
    pub fn update(&mut self, exceeded_threshold: bool, cfg: SignalFilterConfig) -> SignalState {
        let w = cfg.window_secs.max(1) as usize;
        let busy_on = cfg.busy_on.clamp(1, w as u32);
        let quiet_off = cfg.quiet_off.min(busy_on.saturating_sub(1));

        self.window.push_back(exceeded_threshold);
        while self.window.len() > w {
            self.window.pop_front();
        }

        let above = self.window.iter().filter(|&&b| b).count() as u32;
        self.above_count = above;
        self.window_len = w as u32;

        if self.busy {
            if above <= quiet_off {
                self.busy = false;
            }
        } else if above >= busy_on {
            self.busy = true;
        }

        self.last_state = if self.busy {
            SignalState::Busy
        } else if above > 0 {
            SignalState::Pending
        } else {
            SignalState::Quiet
        };
        self.last_state
    }

    pub fn reset(&mut self) {
        self.window.clear();
        self.busy = false;
        self.last_state = SignalState::Quiet;
        self.above_count = 0;
    }

    pub fn state(&self) -> SignalState {
        self.last_state
    }

    pub fn above_count(&self) -> u32 {
        self.above_count
    }

    pub fn window_len(&self) -> u32 {
        self.window_len
    }

    pub fn is_blocking(&self) -> bool {
        self.busy
    }
}

/// Tracks the machine's own GPU idle floor (§5.1) so steady compositor / browser /
/// idle-resident-model noise (typically 1–12%) does not block sleep forever.
#[derive(Debug, Clone, Default)]
pub struct GpuBaselineTracker {
    util_samples: VecDeque<f32>,
    power_mw_samples: VecDeque<u32>,
    cached_util_floor: f32,
    cached_power_floor_mw: Option<u32>,
}

impl GpuBaselineTracker {
    /// Maximum utilization (%) considered plausible for passive desktop idle noise.
    const MAX_IDLE_CANDIDATE_PCT: f32 = 22.0;
    const WINDOW_CAP: usize = 120;

    /// Records a sample. When utilization is within passive idle candidates (and optionally
    /// human is idle or baseline hasn't converged yet), updates the low-percentile floor.
    pub fn observe(&mut self, gpu_util_pct: f32, power_mw: Option<u32>, user_idle_secs: u32) {
        let should_learn = gpu_util_pct <= Self::MAX_IDLE_CANDIDATE_PCT
            && (user_idle_secs >= 5 || self.util_samples.len() < 20);
        if should_learn {
            self.util_samples.push_back(gpu_util_pct.clamp(0.0, 100.0));
            while self.util_samples.len() > Self::WINDOW_CAP {
                self.util_samples.pop_front();
            }
            self.cached_util_floor = low_percentile_f32(&self.util_samples, 0.25);

            if let Some(mw) = power_mw {
                if mw > 0 {
                    self.power_mw_samples.push_back(mw);
                    while self.power_mw_samples.len() > Self::WINDOW_CAP {
                        self.power_mw_samples.pop_front();
                    }
                    self.cached_power_floor_mw =
                        Some(low_percentile_u32(&self.power_mw_samples, 0.25));
                }
            }
        }
    }

    pub fn util_floor(&self) -> f32 {
        self.cached_util_floor
    }

    pub fn power_floor_mw(&self) -> Option<u32> {
        self.cached_power_floor_mw
    }

    /// Computes effective threshold (%) = max(configured_fixed, idle_floor + margin).
    /// Also checks if NVML board power surged well above learned idle power draw.
    pub fn is_busy_sample(
        &self,
        gpu_util_pct: f32,
        power_mw: Option<u32>,
        configured_threshold_pct: f32,
        margin_pct: f32,
    ) -> (bool, f32) {
        let baseline_thresh = self.cached_util_floor + margin_pct.max(2.0);
        // Only let adaptive baseline raise the threshold up to passive desktop ceiling (~25%),
        // OR match a higher explicitly configured user threshold.
        let effective_threshold = baseline_thresh.min(28.0).max(configured_threshold_pct);

        let util_busy = gpu_util_pct >= effective_threshold;
        let power_busy = match (power_mw, self.cached_power_floor_mw) {
            // Board power surging >= +18W above idle floor corroborates compute even at moderate util.
            (Some(mw), Some(floor_mw)) if self.power_mw_samples.len() >= 10 => {
                mw >= floor_mw.saturating_add(18_000) && gpu_util_pct >= configured_threshold_pct
            }
            _ => false,
        };

        (util_busy || power_busy, effective_threshold)
    }
}

/// Acoustic classification of audio capture signal over ~60s (§5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MicSignalState {
    /// Digital silence or flat constant handle left open (muted).
    #[default]
    Dead,
    /// Low steady noise floor with minimal variance (quiet room, mic open).
    Ambient,
    /// Peaks clearly above ambient floor with real dynamic variation (speech / recording).
    Active,
}

impl MicSignalState {
    pub fn label(self) -> &'static str {
        match self {
            MicSignalState::Dead => "Dead/Muted",
            MicSignalState::Ambient => "Ambient",
            MicSignalState::Active => "Active",
        }
    }
}

/// Classifies microphone capture peaks over a rolling window (~60s) into Dead, Ambient, or Active.
#[derive(Debug, Clone, Default)]
pub struct MicSignalClassifier {
    peaks: VecDeque<f32>,
    active_hold_remaining: u32,
}

impl MicSignalClassifier {
    const WINDOW_CAP: usize = 60;

    pub fn observe(&mut self, peak: f32) -> MicSignalState {
        let p = peak.clamp(0.0, 1.0);
        self.peaks.push_back(p);
        while self.peaks.len() > Self::WINDOW_CAP {
            self.peaks.pop_front();
        }
        let state = self.classify_instant();
        if state == MicSignalState::Active {
            // Hold Active for a short speech pause window (8s) so natural sentences don't flap
            self.active_hold_remaining = 8;
            MicSignalState::Active
        } else if self.active_hold_remaining > 0 {
            self.active_hold_remaining -= 1;
            MicSignalState::Active
        } else {
            state
        }
    }

    pub fn reset(&mut self) {
        self.peaks.clear();
        self.active_hold_remaining = 0;
    }

    pub fn classify_instant(&self) -> MicSignalState {
        if self.peaks.is_empty() {
            return MicSignalState::Dead;
        }
        let mut sorted: Vec<f32> = self.peaks.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let min_v = sorted[0];
        let max_v = *sorted.last().unwrap_or(&0.0);
        let range = max_v - min_v;

        // Digital silence (< 0.001) or stuck flat handle over >= 5s
        if max_v < 0.001 || (self.peaks.len() >= 5 && range < 0.0003 && max_v < 0.015) {
            return MicSignalState::Dead;
        }

        let floor = sorted[sorted.len() / 5]; // ~20th percentile ambient noise floor
        let dynamic_thresh = (floor * 2.5).max(floor + 0.025).max(0.03);

        // Look at recent 15 seconds so speech triggers quickly and stops decaying after ~15s
        let recent_start = self.peaks.len().saturating_sub(15);
        let active_samples = self
            .peaks
            .iter()
            .skip(recent_start)
            .filter(|&&v| v >= dynamic_thresh && (v - floor) >= 0.018)
            .count();

        // Real voice/music has variation and clears the ambient floor multiple times
        if active_samples >= 2 && range >= 0.02 {
            MicSignalState::Active
        } else if max_v >= 0.001 {
            MicSignalState::Ambient
        } else {
            MicSignalState::Dead
        }
    }
}

/// Named adaptive threshold suggestions (eliminates positional tuple mismatches).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SuggestedThresholds {
    pub pc_cpu_pct: f32,
    pub disk_kbps: f32,
    pub net_kbps: f32,
}

/// Passive adaptive baseline suggester (§8): observes idle background levels during quiet
/// user-idle periods and surfaces gentle threshold suggestions without silently editing config.
#[derive(Debug, Clone, Default)]
pub struct AdaptiveBaselineSuggester {
    cpu_samples: VecDeque<f32>,
    disk_samples: VecDeque<f32>,
    net_samples: VecDeque<f32>,
}

impl AdaptiveBaselineSuggester {
    pub fn observe_idle(&mut self, cpu: f32, disk_kbps: f32, net_kbps: f32, idle_secs: u32) {
        if idle_secs < 60 {
            return;
        }
        push_cap(&mut self.cpu_samples, cpu, 180);
        push_cap(&mut self.disk_samples, disk_kbps, 180);
        push_cap(&mut self.net_samples, net_kbps, 180);
    }

    pub fn suggested_thresholds(&self) -> Option<SuggestedThresholds> {
        if self.cpu_samples.len() < 60 {
            return None;
        }
        let cpu_p90 = low_percentile_f32(&self.cpu_samples, 0.90);
        let disk_p90 = low_percentile_f32(&self.disk_samples, 0.90);
        let net_p90 = low_percentile_f32(&self.net_samples, 0.90);
        Some(SuggestedThresholds {
            pc_cpu_pct: (cpu_p90 + 10.0).clamp(15.0, 60.0).round(),
            disk_kbps: (disk_p90 + 2000.0).clamp(3000.0, 25000.0).round(),
            net_kbps: (net_p90 + 150.0).clamp(150.0, 2000.0).round(),
        })
    }
}

fn push_cap(q: &mut VecDeque<f32>, v: f32, cap: usize) {
    q.push_back(v);
    while q.len() > cap {
        q.pop_front();
    }
}

fn low_percentile_f32(samples: &VecDeque<f32>, q: f32) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut v: Vec<f32> = samples.iter().copied().collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((v.len().saturating_sub(1) as f32) * q.clamp(0.0, 1.0)).round() as usize;
    v[idx]
}

fn low_percentile_u32(samples: &VecDeque<u32>, q: f32) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    let mut v: Vec<u32> = samples.iter().copied().collect();
    v.sort_unstable();
    let idx = ((v.len().saturating_sub(1) as f32) * q.clamp(0.0, 1.0)).round() as usize;
    v[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_filter_ignores_single_and_periodic_spikes() {
        let mut f = SignalFilter::default();
        let cfg = SignalFilterConfig::new(15, 8, 2);

        // Single huge spike
        assert_eq!(f.update(true, cfg), SignalState::Pending);
        assert!(!f.is_blocking());

        // Periodic 1s spike every 10s for 40 ticks -> max 2/15 -> never Busy
        for tick in 0..40 {
            let spike = tick % 10 == 0;
            let st = f.update(spike, cfg);
            assert_ne!(st, SignalState::Busy);
            assert!(!f.is_blocking());
        }
    }

    #[test]
    fn test_signal_filter_catches_bursty_work_and_hysteresis() {
        let mut f = SignalFilter::default();
        let cfg = SignalFilterConfig::new(15, 8, 2);

        // Alternating busy/idle hits 8/15 by tick 15
        for tick in 0..16 {
            f.update(tick % 2 == 0, cfg);
        }
        assert_eq!(f.state(), SignalState::Busy);
        assert!(f.is_blocking());

        // Drop: stays Busy while > 2 samples remain in 15s window (hysteresis)
        let mut unlatched_at = None;
        for i in 0..20 {
            f.update(false, cfg);
            if !f.is_blocking() {
                unlatched_at = Some(i);
                break;
            }
        }
        assert!(unlatched_at.is_some());
        assert_eq!(f.state(), SignalState::Pending); // 2/15 remain -> Pending, not blocking
        for _ in 0..15 {
            f.update(false, cfg);
        }
        assert_eq!(f.state(), SignalState::Quiet);
    }

    #[test]
    fn test_gpu_baseline_absorbs_steady_compositor_noise() {
        let mut tracker = GpuBaselineTracker::default();
        for i in 0..60 {
            let steady = 6.0 + ((i % 6) as f32); // 6..11%
            tracker.observe(steady, Some(15_000), 120);
            let (busy, _) = tracker.is_busy_sample(steady, Some(15_000), 5.0, 12.0);
            assert!(
                !busy,
                "steady compositor noise {}% at tick {} should not be busy",
                steady, i
            );
        }

        // Now real CUDA workload hits 60% & 120W
        let (busy, _) = tracker.is_busy_sample(60.0, Some(120_000), 5.0, 12.0);
        assert!(busy);
    }

    #[test]
    fn test_mic_classifier_distinguishes_dead_ambient_active() {
        let mut dead = MicSignalClassifier::default();
        for _ in 0..30 {
            assert_eq!(dead.observe(0.0001), MicSignalState::Dead);
        }

        let mut ambient = MicSignalClassifier::default();
        for i in 0..30 {
            let v = 0.005 + (i % 3) as f32 * 0.001; // quiet fan / AC hum 0.005..0.007
            assert_eq!(ambient.observe(v), MicSignalState::Ambient);
        }

        let mut active = MicSignalClassifier::default();
        for i in 0..20 {
            let v = if i % 4 == 0 { 0.18 } else { 0.008 }; // speaking voice bursts
            active.observe(v);
        }
        assert_eq!(active.classify_instant(), MicSignalState::Active);
    }

    #[test]
    fn test_adaptive_baseline_suggester_named_fields() {
        let mut suggester = AdaptiveBaselineSuggester::default();
        for _ in 0..60 {
            suggester.observe_idle(19.0, 11302.0, 23.0, 90);
        }
        let sug = suggester
            .suggested_thresholds()
            .expect("expected idle floor suggestions after 60 samples");
        assert!((sug.pc_cpu_pct - 29.0).abs() < 0.1);
        assert!((sug.disk_kbps - 13302.0).abs() < 0.1);
        assert!((sug.net_kbps - 173.0).abs() < 0.1);
    }
}
