#![allow(dead_code)]

use crate::storage::delta_manifest::DeltaManifest;

const SOFT_DELTA_SEGMENT_COUNT: u64 = 1_024;
const HARD_DELTA_SEGMENT_COUNT: u64 = 4_096;
const SOFT_DELTA_PAGE_GROWTH: u64 = 16_384;
const HARD_DELTA_PAGE_GROWTH: u64 = 65_536;
const SOFT_RATIO_PERCENT: u64 = 10;
const HARD_RATIO_PERCENT: u64 = 25;
const MIN_RATIO_DELTA_PAGES: u64 = 1_024;
const MIN_RATIO_DELTA_FACTS: u64 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeltaMaintenanceDecision {
    ContinueDeltaAppend,
    ScheduleBackgroundRecompact,
    MaintenanceBackpressure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeltaGrowthMetrics {
    visible_delta_segment_count: u64,
    visible_delta_page_growth: u64,
    base_page_count: u64,
    visible_delta_fact_count: Option<u64>,
    base_fact_count: Option<u64>,
}

impl DeltaGrowthMetrics {
    pub(crate) fn no_delta(base_page_count: u64) -> Self {
        Self {
            visible_delta_segment_count: 0,
            visible_delta_page_growth: 0,
            base_page_count,
            visible_delta_fact_count: None,
            base_fact_count: None,
        }
    }

    pub(crate) fn from_manifest(manifest: &DeltaManifest) -> Self {
        let visible_delta_segment_count =
            u64::try_from(manifest.segments().len()).unwrap_or(u64::MAX);
        let visible_delta_page_growth = manifest.segments().iter().fold(0u64, |total, segment| {
            total.saturating_add(segment.segment_page_count())
        });

        Self {
            visible_delta_segment_count,
            visible_delta_page_growth,
            base_page_count: manifest.base_identity().page_count(),
            visible_delta_fact_count: None,
            base_fact_count: None,
        }
    }

    pub(crate) fn with_exact_fact_counts(
        mut self,
        visible_delta_fact_count: u64,
        base_fact_count: u64,
    ) -> Self {
        self.visible_delta_fact_count = Some(visible_delta_fact_count);
        self.base_fact_count = Some(base_fact_count);
        self
    }

    pub(crate) fn decide(self) -> DeltaMaintenanceDecision {
        if self.crosses_hard_threshold() {
            DeltaMaintenanceDecision::MaintenanceBackpressure
        } else if self.crosses_soft_threshold() {
            DeltaMaintenanceDecision::ScheduleBackgroundRecompact
        } else {
            DeltaMaintenanceDecision::ContinueDeltaAppend
        }
    }

    pub(crate) fn visible_delta_segment_count(&self) -> u64 {
        self.visible_delta_segment_count
    }

    pub(crate) fn visible_delta_page_growth(&self) -> u64 {
        self.visible_delta_page_growth
    }

    pub(crate) fn base_page_count(&self) -> u64 {
        self.base_page_count
    }

    fn crosses_hard_threshold(&self) -> bool {
        self.visible_delta_segment_count >= HARD_DELTA_SEGMENT_COUNT
            || self.visible_delta_page_growth >= HARD_DELTA_PAGE_GROWTH
            || self.page_ratio_at_least(HARD_RATIO_PERCENT)
            || self.fact_ratio_at_least(HARD_RATIO_PERCENT)
    }

    fn crosses_soft_threshold(&self) -> bool {
        self.visible_delta_segment_count >= SOFT_DELTA_SEGMENT_COUNT
            || self.visible_delta_page_growth >= SOFT_DELTA_PAGE_GROWTH
            || self.page_ratio_at_least(SOFT_RATIO_PERCENT)
            || self.fact_ratio_at_least(SOFT_RATIO_PERCENT)
    }

    fn page_ratio_at_least(&self, percent: u64) -> bool {
        self.visible_delta_page_growth >= MIN_RATIO_DELTA_PAGES
            && ratio_at_least(
                self.visible_delta_page_growth,
                self.base_page_count,
                percent,
            )
    }

    fn fact_ratio_at_least(&self, percent: u64) -> bool {
        let (Some(visible_delta_fact_count), Some(base_fact_count)) =
            (self.visible_delta_fact_count, self.base_fact_count)
        else {
            return false;
        };

        visible_delta_fact_count >= MIN_RATIO_DELTA_FACTS
            && ratio_at_least(visible_delta_fact_count, base_fact_count, percent)
    }
}

fn ratio_at_least(value: u64, base: u64, percent: u64) -> bool {
    base > 0 && u128::from(value) * 100 >= u128::from(base) * u128::from(percent)
}

#[cfg(test)]
mod tests {
    use super::{
        DeltaGrowthMetrics, DeltaMaintenanceDecision, HARD_DELTA_PAGE_GROWTH,
        HARD_DELTA_SEGMENT_COUNT, MIN_RATIO_DELTA_FACTS, MIN_RATIO_DELTA_PAGES,
        SOFT_DELTA_PAGE_GROWTH, SOFT_DELTA_SEGMENT_COUNT,
    };
    use crate::storage::delta_manifest::{DeltaBaseIdentity, DeltaManifest, DeltaManifestSegment};
    use anyhow::Result;

    impl DeltaGrowthMetrics {
        fn from_parts(
            visible_delta_segment_count: u64,
            visible_delta_page_growth: u64,
            base_page_count: u64,
            visible_delta_fact_count: Option<u64>,
            base_fact_count: Option<u64>,
        ) -> Self {
            Self {
                visible_delta_segment_count,
                visible_delta_page_growth,
                base_page_count,
                visible_delta_fact_count,
                base_fact_count,
            }
        }
    }

    fn manifest_with_segments(
        segment_count: u64,
        segment_page_count: u64,
        base_page_count: u64,
    ) -> Result<DeltaManifest> {
        let capacity = usize::try_from(segment_count)?;
        let mut segments = Vec::with_capacity(capacity);
        let mut segment_page_start = base_page_count;

        for index in 0..segment_count {
            let tx_count = index.saturating_add(1);
            segments.push(DeltaManifestSegment::fixture(
                segment_page_start,
                segment_page_count,
                tx_count,
                1,
                tx_count,
                tx_count,
            ));
            segment_page_start = segment_page_start.saturating_add(segment_page_count);
        }

        DeltaManifest::new(1, DeltaBaseIdentity::fixture(base_page_count, 0), segments)
    }

    #[test]
    fn no_manifest_returns_continue() {
        let metrics = DeltaGrowthMetrics::no_delta(1_000_000);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ContinueDeltaAppend
        );
    }

    #[test]
    fn healthy_tiny_segment_manifest_returns_continue() -> Result<()> {
        let manifest = manifest_with_segments(1_000, 3, 1_000_000)?;
        let metrics = DeltaGrowthMetrics::from_manifest(&manifest);

        assert_eq!(metrics.visible_delta_segment_count(), 1_000);
        assert_eq!(metrics.visible_delta_page_growth(), 3_000);
        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ContinueDeltaAppend
        );
        Ok(())
    }

    #[test]
    fn soft_segment_threshold_schedules_background_recompact() -> Result<()> {
        let manifest = manifest_with_segments(SOFT_DELTA_SEGMENT_COUNT, 1, 1_000_000)?;
        let metrics = DeltaGrowthMetrics::from_manifest(&manifest);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ScheduleBackgroundRecompact
        );
        Ok(())
    }

    #[test]
    fn hard_segment_threshold_returns_backpressure() -> Result<()> {
        let manifest = manifest_with_segments(HARD_DELTA_SEGMENT_COUNT, 1, 1_000_000)?;
        let metrics = DeltaGrowthMetrics::from_manifest(&manifest);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::MaintenanceBackpressure
        );
        Ok(())
    }

    #[test]
    fn soft_page_growth_schedules_background_recompact() -> Result<()> {
        let manifest = manifest_with_segments(1, SOFT_DELTA_PAGE_GROWTH, 1_000_000)?;
        let metrics = DeltaGrowthMetrics::from_manifest(&manifest);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ScheduleBackgroundRecompact
        );
        Ok(())
    }

    #[test]
    fn hard_page_growth_returns_backpressure() -> Result<()> {
        let manifest = manifest_with_segments(1, HARD_DELTA_PAGE_GROWTH, 1_000_000)?;
        let metrics = DeltaGrowthMetrics::from_manifest(&manifest);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::MaintenanceBackpressure
        );
        Ok(())
    }

    #[test]
    fn small_base_page_ratio_below_floor_does_not_trigger() {
        let metrics = DeltaGrowthMetrics::from_parts(
            1,
            MIN_RATIO_DELTA_PAGES.saturating_sub(1),
            1,
            None,
            None,
        );

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ContinueDeltaAppend
        );
    }

    #[test]
    fn soft_page_ratio_schedules_after_absolute_floor() {
        let metrics = DeltaGrowthMetrics::from_parts(1, MIN_RATIO_DELTA_PAGES, 10_240, None, None);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ScheduleBackgroundRecompact
        );
    }

    #[test]
    fn hard_page_ratio_returns_backpressure_after_absolute_floor() {
        let metrics = DeltaGrowthMetrics::from_parts(1, MIN_RATIO_DELTA_PAGES, 4_096, None, None);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::MaintenanceBackpressure
        );
    }

    #[test]
    fn fact_ratio_is_ignored_when_exact_counts_are_missing() {
        let metrics = DeltaGrowthMetrics::from_parts(1, 1, 1_000_000, None, None);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ContinueDeltaAppend
        );
    }

    #[test]
    fn fact_ratio_below_absolute_floor_does_not_trigger() {
        let metrics = DeltaGrowthMetrics::no_delta(1_000_000)
            .with_exact_fact_counts(MIN_RATIO_DELTA_FACTS.saturating_sub(1), 1);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ContinueDeltaAppend
        );
    }

    #[test]
    fn soft_fact_ratio_schedules_when_exact_counts_are_available() {
        let metrics = DeltaGrowthMetrics::no_delta(1_000_000)
            .with_exact_fact_counts(MIN_RATIO_DELTA_FACTS, 10_000);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ScheduleBackgroundRecompact
        );
    }

    #[test]
    fn hard_fact_ratio_returns_backpressure_when_exact_counts_are_available() {
        let metrics = DeltaGrowthMetrics::no_delta(1_000_000).with_exact_fact_counts(2_500, 10_000);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::MaintenanceBackpressure
        );
    }

    #[test]
    fn hard_page_threshold_masks_soft_fact_ratio() {
        let metrics =
            DeltaGrowthMetrics::from_parts(1, HARD_DELTA_PAGE_GROWTH, 1_000_000, None, None)
                .with_exact_fact_counts(MIN_RATIO_DELTA_FACTS, 10_000);

        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::MaintenanceBackpressure
        );
    }

    #[test]
    fn manifest_metrics_use_selected_segment_pages_not_trailing_file_pages() -> Result<()> {
        let manifest = manifest_with_segments(2, 3, 500)?;
        let metrics = DeltaGrowthMetrics::from_manifest(&manifest);

        assert_eq!(metrics.base_page_count(), 500);
        assert_eq!(metrics.visible_delta_segment_count(), 2);
        assert_eq!(metrics.visible_delta_page_growth(), 6);
        assert_eq!(
            metrics.decide(),
            DeltaMaintenanceDecision::ContinueDeltaAppend
        );
        Ok(())
    }
}
