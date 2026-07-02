//! Diagnostics, capability reporting, and performance reporting boundaries.

pub const LAYER: &str = "diagnostics";

use std::{collections::VecDeque, time::Duration};

use render_core::{FeatureCostSample, OptionalFeatureCostMode, RenderInstrumentation};

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
}
