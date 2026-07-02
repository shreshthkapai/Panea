//! Diagnostics, capability reporting, and performance reporting boundaries.

pub const LAYER: &str = "diagnostics";

use std::{collections::VecDeque, time::Duration};

use render_core::{FeatureCostSample, OptionalFeatureCostMode, RenderInstrumentation};
use semantics::{CommandBlockConfidence, IntegrationMode, SemanticDiagnostics, SemanticEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerformanceBudget {
    pub max_frame_time: Duration,
    pub max_idle_wakeups_per_second: u64,
    pub max_damage_regions: usize,
}

impl Default for PerformanceBudget {
    fn default() -> Self {
        Self {
            max_frame_time: Duration::from_millis(16),
            max_idle_wakeups_per_second: 2,
            max_damage_regions: 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualBudget {
    pub max_animation_fps: u16,
    pub max_cursor_asset_size_kb: u32,
    pub max_active_animations: u16,
    pub max_animated_region_pixels: u32,
}

impl Default for VisualBudget {
    fn default() -> Self {
        Self {
            max_animation_fps: 60,
            max_cursor_asset_size_kb: 256,
            max_active_animations: 8,
            max_animated_region_pixels: 250_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VisualRuntimeStats {
    pub requested_animation_fps: u16,
    pub cursor_asset_size_kb: u32,
    pub active_animations: u16,
    pub animated_region_pixels: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualWarningKind {
    AnimationFpsOverBudget,
    CursorAssetTooLarge,
    TooManyAnimations,
    AnimatedRegionTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualWarning {
    pub kind: VisualWarningKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBudgetReport {
    pub passed: bool,
    pub warnings: Vec<VisualWarning>,
}

pub fn evaluate_visual_budget(
    stats: VisualRuntimeStats,
    budget: VisualBudget,
) -> VisualBudgetReport {
    let mut warnings = Vec::new();

    if stats.requested_animation_fps > budget.max_animation_fps {
        warnings.push(VisualWarning {
            kind: VisualWarningKind::AnimationFpsOverBudget,
            message: format!(
                "animation FPS {} exceeded cap {}",
                stats.requested_animation_fps, budget.max_animation_fps
            ),
        });
    }
    if stats.cursor_asset_size_kb > budget.max_cursor_asset_size_kb {
        warnings.push(VisualWarning {
            kind: VisualWarningKind::CursorAssetTooLarge,
            message: format!(
                "cursor asset {} KiB exceeded cap {} KiB",
                stats.cursor_asset_size_kb, budget.max_cursor_asset_size_kb
            ),
        });
    }
    if stats.active_animations > budget.max_active_animations {
        warnings.push(VisualWarning {
            kind: VisualWarningKind::TooManyAnimations,
            message: format!(
                "active animations {} exceeded cap {}",
                stats.active_animations, budget.max_active_animations
            ),
        });
    }
    if stats.animated_region_pixels > budget.max_animated_region_pixels {
        warnings.push(VisualWarning {
            kind: VisualWarningKind::AnimatedRegionTooLarge,
            message: format!(
                "animated region {} px exceeded cap {} px",
                stats.animated_region_pixels, budget.max_animated_region_pixels
            ),
        });
    }

    VisualBudgetReport {
        passed: warnings.is_empty(),
        warnings,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSessionSecurityState {
    NotConnected,
    HostKeyUnknown,
    HostKeyTrusted,
    HostKeyMismatch,
    Authenticated,
    AuthenticationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSessionDiagnostics {
    pub profile_name: String,
    pub host: String,
    pub port: u16,
    pub security_state: RemoteSessionSecurityState,
    pub remote_pty_requested: bool,
    pub bytes_received: usize,
    pub disconnected: bool,
    pub last_error: Option<String>,
}

impl RemoteSessionDiagnostics {
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "ssh profile={} target={}:{} state={:?}",
            self.profile_name, self.host, self.port, self.security_state
        )];
        if self.remote_pty_requested {
            parts.push("remote_pty=requested".to_owned());
        }
        parts.push(format!("bytes_received={}", self.bytes_received));
        if self.disconnected {
            parts.push("disconnected=true".to_owned());
        }
        if let Some(error) = &self.last_error {
            parts.push(format!("error={error}"));
        }
        parts.join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerformanceWarningKind {
    FrameOverBudget,
    ExcessiveIdleWakeups,
    ExcessiveDamageRegions,
    DisabledFeatureHasCost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceWarning {
    pub kind: PerformanceWarningKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformanceGateReport {
    pub passed: bool,
    pub warnings: Vec<PerformanceWarning>,
}

impl PerformanceGateReport {
    #[must_use]
    pub fn pass() -> Self {
        Self {
            passed: true,
            warnings: Vec::new(),
        }
    }
}

pub fn evaluate_performance_gate(
    sample: RenderInstrumentation,
    budget: PerformanceBudget,
) -> PerformanceGateReport {
    let mut warnings = Vec::new();

    if sample.frame_time > budget.max_frame_time {
        warnings.push(PerformanceWarning {
            kind: PerformanceWarningKind::FrameOverBudget,
            message: format!(
                "frame time {:?} exceeded budget {:?}",
                sample.frame_time, budget.max_frame_time
            ),
        });
    }

    if sample.idle_wakeups > budget.max_idle_wakeups_per_second {
        warnings.push(PerformanceWarning {
            kind: PerformanceWarningKind::ExcessiveIdleWakeups,
            message: format!(
                "idle wakeups {} exceeded budget {}",
                sample.idle_wakeups, budget.max_idle_wakeups_per_second
            ),
        });
    }

    if sample.damage_region_count > budget.max_damage_regions {
        warnings.push(PerformanceWarning {
            kind: PerformanceWarningKind::ExcessiveDamageRegions,
            message: format!(
                "damage regions {} exceeded budget {}",
                sample.damage_region_count, budget.max_damage_regions
            ),
        });
    }

    PerformanceGateReport {
        passed: warnings.is_empty(),
        warnings,
    }
}

pub fn evaluate_feature_cost(sample: &FeatureCostSample) -> PerformanceGateReport {
    if sample.mode != OptionalFeatureCostMode::Disabled {
        return PerformanceGateReport::pass();
    }

    let has_cost = sample.instrumentation.animated_region_count > 0
        || !sample.instrumentation.frame_time.is_zero()
        || sample.instrumentation.draw_call_count > 0;

    if has_cost {
        PerformanceGateReport {
            passed: false,
            warnings: vec![PerformanceWarning {
                kind: PerformanceWarningKind::DisabledFeatureHasCost,
                message: format!(
                    "{:?} recorded work while disabled: frame={:?}, draw_calls={}, animations={}",
                    sample.feature,
                    sample.instrumentation.frame_time,
                    sample.instrumentation.draw_call_count,
                    sample.instrumentation.animated_region_count
                ),
            }],
        }
    } else {
        PerformanceGateReport::pass()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellIntegrationWarningKind {
    Disabled,
    Inactive,
    HeuristicMode,
    RemoteInactive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegrationWarning {
    pub kind: ShellIntegrationWarningKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegrationReport {
    pub shell_detected: Option<String>,
    pub integration_active: bool,
    pub last_event: Option<SemanticEventKind>,
    pub last_event_age: Option<Duration>,
    pub command_block_confidence: CommandBlockConfidence,
    pub remote_integration_active: bool,
    pub warnings: Vec<ShellIntegrationWarning>,
}

impl ShellIntegrationReport {
    #[must_use]
    pub fn from_semantic_diagnostics(diagnostics: &SemanticDiagnostics) -> Self {
        let mut warnings = Vec::new();

        if diagnostics.mode == IntegrationMode::Disabled {
            warnings.push(ShellIntegrationWarning {
                kind: ShellIntegrationWarningKind::Disabled,
                message: "shell integration is disabled; semantic command features are unavailable"
                    .to_owned(),
            });
        } else if !diagnostics.integration_active {
            warnings.push(ShellIntegrationWarning {
                kind: ShellIntegrationWarningKind::Inactive,
                message: "shell integration has not emitted semantic events for this session"
                    .to_owned(),
            });
        }

        if diagnostics.heuristic_mode {
            warnings.push(ShellIntegrationWarning {
                kind: ShellIntegrationWarningKind::HeuristicMode,
                message: "command regions are heuristic because shell integration is inactive"
                    .to_owned(),
            });
        }

        if diagnostics.shell_detected.is_some() && !diagnostics.remote_integration_active {
            warnings.push(ShellIntegrationWarning {
                kind: ShellIntegrationWarningKind::RemoteInactive,
                message: "remote shell integration status is unknown".to_owned(),
            });
        }

        Self {
            shell_detected: diagnostics.shell_detected.clone(),
            integration_active: diagnostics.integration_active,
            last_event: diagnostics.last_event,
            last_event_age: diagnostics.last_event_age,
            command_block_confidence: diagnostics.command_block_confidence,
            remote_integration_active: diagnostics.remote_integration_active,
            warnings,
        }
    }

    #[must_use]
    pub fn render_text(&self) -> String {
        let shell = self.shell_detected.as_deref().unwrap_or("unknown");
        let last_event = self
            .last_event
            .map_or_else(|| "none".to_owned(), |event| format!("{event:?}"));
        let warning = self
            .warnings
            .first()
            .map_or("ok", |warning| warning.message.as_str());

        format!(
            "shell={shell} active={} last_event={} confidence={:?} remote_active={} status={warning}",
            self.integration_active,
            last_event,
            self.command_block_confidence,
            self.remote_integration_active
        )
    }
}

#[derive(Debug, Clone)]
pub struct PerformanceOverlay {
    enabled: bool,
    samples: VecDeque<RenderInstrumentation>,
    capacity: usize,
    backend: String,
}

impl PerformanceOverlay {
    #[must_use]
    pub fn new(enabled: bool, backend: impl Into<String>) -> Self {
        Self {
            enabled,
            samples: VecDeque::new(),
            capacity: 120,
            backend: backend.into(),
        }
    }

    pub fn record(&mut self, sample: RenderInstrumentation) {
        if !self.enabled {
            return;
        }

        while self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    #[must_use]
    pub fn latest(&self) -> Option<RenderInstrumentation> {
        self.samples.back().copied()
    }

    #[must_use]
    pub fn render_text(&self, budget: PerformanceBudget) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let latest = self.latest()?;
        let fps = if latest.frame_time.is_zero() {
            0.0
        } else {
            1.0 / latest.frame_time.as_secs_f64()
        };
        let gpu = latest
            .gpu_submit_time
            .map_or_else(|| "n/a".to_owned(), |duration| format!("{duration:?}"));
        let warning = evaluate_performance_gate(latest, budget)
            .warnings
            .first()
            .map_or("ok".to_owned(), |warning| warning.message.clone());

        Some(format!(
            "fps={fps:.1} frame={:?} cpu={:?} gpu={} backend={} glyph_hits={} glyph_misses={} atlas_uploads={} damage_regions={} draw_calls={} animations={} idle_wakeups={} status={warning}",
            latest.frame_time,
            latest.cpu_prepare_time,
            gpu,
            self.backend,
            latest.glyphs.cache_hits,
            latest.glyphs.cache_misses,
            latest.glyphs.atlas_uploads,
            latest.damage_region_count,
            latest.draw_call_count,
            latest.animated_region_count,
            latest.idle_wakeups,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use render_core::{FeatureCostSample, OptionalFeature, OptionalFeatureCostMode};

    #[test]
    fn gate_reports_over_budget_frame() {
        let report = evaluate_performance_gate(
            RenderInstrumentation {
                frame_time: Duration::from_millis(25),
                ..RenderInstrumentation::default()
            },
            PerformanceBudget::default(),
        );

        assert!(!report.passed);
        assert_eq!(
            report.warnings[0].kind,
            PerformanceWarningKind::FrameOverBudget
        );
    }

    #[test]
    fn overlay_formats_latest_sample() {
        let mut overlay = PerformanceOverlay::new(true, "test-backend");
        overlay.record(RenderInstrumentation {
            frame_time: Duration::from_millis(10),
            cpu_prepare_time: Duration::from_millis(7),
            draw_call_count: 3,
            ..RenderInstrumentation::default()
        });

        let text = overlay
            .render_text(PerformanceBudget::default())
            .expect("overlay text");
        assert!(text.contains("backend=test-backend"));
        assert!(text.contains("draw_calls=3"));
    }

    #[test]
    fn disabled_feature_cost_fails_gate() {
        let report = evaluate_feature_cost(&FeatureCostSample {
            feature: OptionalFeature::CursorAnimation,
            mode: OptionalFeatureCostMode::Disabled,
            instrumentation: RenderInstrumentation {
                draw_call_count: 1,
                ..RenderInstrumentation::default()
            },
        });

        assert!(!report.passed);
    }

    #[test]
    fn visual_budget_reports_expensive_animation_regions() {
        let report = evaluate_visual_budget(
            VisualRuntimeStats {
                requested_animation_fps: 120,
                active_animations: 12,
                animated_region_pixels: 300_000,
                ..VisualRuntimeStats::default()
            },
            VisualBudget::default(),
        );

        assert!(!report.passed);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.kind == VisualWarningKind::AnimationFpsOverBudget)
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.kind == VisualWarningKind::AnimatedRegionTooLarge)
        );
    }

    #[test]
    fn remote_session_diagnostics_summarize_without_secrets() {
        let report = RemoteSessionDiagnostics {
            profile_name: "prod".to_owned(),
            host: "example.com".to_owned(),
            port: 22,
            security_state: RemoteSessionSecurityState::HostKeyMismatch,
            remote_pty_requested: true,
            bytes_received: 128,
            disconnected: true,
            last_error: Some("host key mismatch".to_owned()),
        };

        let summary = report.summary();

        assert!(summary.contains("profile=prod"));
        assert!(summary.contains("state=HostKeyMismatch"));
        assert!(!summary.contains("password"));
    }

    #[test]
    fn shell_integration_report_explains_inactive_state() {
        let report = ShellIntegrationReport::from_semantic_diagnostics(&SemanticDiagnostics {
            mode: IntegrationMode::EscapeSequences,
            shell_detected: Some("bash".to_owned()),
            integration_active: false,
            last_event: None,
            last_event_age: None,
            command_block_confidence: CommandBlockConfidence::None,
            remote_integration_active: false,
            heuristic_mode: false,
        });

        assert_eq!(report.shell_detected.as_deref(), Some("bash"));
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.kind == ShellIntegrationWarningKind::Inactive)
        );
        assert!(report.render_text().contains("shell=bash"));
    }
}
