use prometheus_client::encoding::text::{EncodeMetric, Encoder};
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::{MetricType, TypedMetric};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Prometheus metric based on a gauge, but additionally records the minimum and maximum values of
/// that gauge since the last recorded value was taken.
///
/// This allows a user of the metric to see the full range of values within a smaller timespan with
/// greater precision and less overhead than a histogram. If the details of the intermediate values
/// are required, the histogram remains a more appropriate choice.
#[derive(Debug, Clone, Default)]
pub struct RangeGauge {
    gauge: Gauge<u64, AtomicU64>,
    min: Arc<AtomicU64>,
    max: Arc<AtomicU64>,
}

impl RangeGauge {
    fn update_max(&self, new_max: u64) {
        let mut current_max = self.max.load(Ordering::Relaxed);

        // If the current max value is less than the new value, update it
        while current_max < new_max {
            match self.max.compare_exchange(
                current_max,
                new_max,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(e) => {
                    // Max value changed in the meantime. Try to update again.
                    // This will eventually converge to the correct value; either another thread updated max to a value below ours,
                    // and thus we'll try again with a yet higher value; or the max is above ours, and we can terminate.
                    current_max = e;
                }
            }
        }
    }

    fn update_min(&self, new_min: u64) {
        let mut current_min = self.min.load(Ordering::Relaxed);

        // If the current min value is greater than the new value, update it
        while current_min > new_min {
            match self.min.compare_exchange(
                current_min,
                new_min,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(e) => {
                    // Min value changed in the meantime. Try to update again.
                    // This will eventually converge to the correct value; either another thread updated min to a value above ours,
                    // and thus we'll try again with a yet smaller value; or the min is below ours, and we can terminate.
                    current_min = e;
                }
            }
        }
    }

    /// Increase the [`RangeGauge`] by 1, returning the previous value.
    pub fn inc(&self) -> u64 {
        self.inc_by(1)
    }

    /// Increase the [`RangeGauge`] by `v`, returning the previous value.
    pub fn inc_by(&self, v: u64) -> u64 {
        let prev = self.gauge.inc_by(v);
        self.update_max(prev + v);
        prev
    }

    /// Decrease the [`RangeGauge`] by 1, returning the previous value.
    pub fn dec(&self) -> u64 {
        self.dec_by(1)
    }

    /// Decrease the [`RangeGauge`] by `v`, returning the previous value.
    pub fn dec_by(&self, v: u64) -> u64 {
        let prev = self.gauge.dec_by(v);
        self.update_min(prev - v);
        prev
    }

    /// Sets the [`RangeGauge`] to `v`, returning the previous value.
    pub fn set(&self, v: u64) -> u64 {
        let prev = self.gauge.set(v);
        self.update_max(v);
        self.update_min(v);
        prev
    }

    /// Get the current value of the [`RangeGauge`].
    pub fn get(&self) -> u64 {
        self.gauge.get()
    }

    /// Exposes the inner atomic type of the [`RangeGauge`].
    ///
    /// This should only be used for advanced use-cases which are not directly
    /// supported by the library.
    pub fn inner(&self) -> &AtomicU64 {
        &self.gauge.inner()
    }
}

impl TypedMetric for RangeGauge {
    const TYPE: MetricType = MetricType::Gauge;
}

impl EncodeMetric for RangeGauge {
    fn encode(&self, mut encoder: Encoder) -> Result<(), std::io::Error> {
        let current = self.get();
        // Getting the current values of the metric resets the min/max
        let min = self.min.swap(current, Ordering::Relaxed);
        let max = self.max.swap(current, Ordering::Relaxed);

        encoder
            .no_suffix()?
            .no_bucket()?
            .encode_value(self.get())?
            .no_exemplar()?;

        encoder
            .encode_suffix("min")?
            .no_bucket()?
            .encode_value(min)?
            .no_exemplar()?;

        encoder
            .encode_suffix("max")?
            .no_bucket()?
            .encode_value(max)?
            .no_exemplar()?;

        Ok(())
    }

    fn metric_type(&self) -> MetricType {
        Self::TYPE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prometheus_client::encoding::text::encode;
    use prometheus_client::registry::Registry;

    struct MetricValueHelper(Registry<RangeGauge>);

    impl MetricValueHelper {
        fn new(metric: &RangeGauge) -> Self {
            let mut reg = Registry::default();
            reg.register("mygauge", "", metric.clone());
            Self(reg)
        }

        #[track_caller]
        fn assert_values(&self, val: u64, min: u64, max: u64) {
            let mut encoded = vec![];
            encode(&mut encoded, &self.0).unwrap();
            assert_eq!(
                std::str::from_utf8(&encoded).unwrap(),
                format!(
                    "\
# HELP mygauge .
# TYPE mygauge gauge
mygauge {val}
mygauge_min {min}
mygauge_max {max}
# EOF
"
                ),
            );
        }
    }

    #[test]
    fn test_rangegauge_values() {
        let rg = RangeGauge::default();
        let helper = MetricValueHelper::new(&rg);

        helper.assert_values(0, 0, 0);
        rg.inc();
        helper.assert_values(1, 0, 1);
        // the act of observing the value should reset the min/max history
        helper.assert_values(1, 1, 1);
        rg.dec();
        helper.assert_values(0, 0, 1);
        // the act of observing the value should reset the min/max history
        helper.assert_values(0, 0, 0);
        // check that max continues to observe the highest seen value after the value goes down
        rg.inc_by(3);
        rg.dec_by(2);
        helper.assert_values(1, 0, 3);
        // change both min and max in one sample period
        rg.inc_by(1);
        rg.dec_by(2);
        helper.assert_values(0, 0, 2);
    }
}
