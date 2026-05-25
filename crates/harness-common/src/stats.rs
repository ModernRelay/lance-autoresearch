//! Aggregators and noise-bound estimators for u64 timing samples.
//!
//! `geomean` is the central tendency we report (insensitive to halve-one /
//! double-other outliers that arithmetic mean would mishandle). `median` and
//! `iqr` give a stable spread summary. `bootstrap_ci_geomean` returns a 90%
//! confidence interval on the geomean via resampling, used by the keep-gate
//! to test whether a trial's measurement is statistically distinguishable
//! from the current-best baseline, rather than relying on a fragile fixed
//! "1% noise band" assumption that doesn't hold on Apple Silicon.

use crate::SplitMix64;

pub fn geomean(xs: &[u64]) -> u64 {
    if xs.is_empty() {
        return 0;
    }
    let mut sum_ln = 0.0f64;
    for &x in xs {
        sum_ln += (x.max(1) as f64).ln();
    }
    (sum_ln / xs.len() as f64).exp() as u64
}

/// Sample median. For even-length samples, returns the mean of the two middle
/// values. Returns 0 for empty input.
pub fn median(xs: &[u64]) -> u64 {
    if xs.is_empty() {
        return 0;
    }
    let mut sorted: Vec<u64> = xs.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2
    } else {
        sorted[n / 2]
    }
}

/// (Q1, Q3), the 25th and 75th percentile. Returns (0, 0) for empty input.
/// Uses nearest-rank selection rather than linear interpolation; differences
/// matter very little at our sample sizes (288+ per measurement).
pub fn iqr(xs: &[u64]) -> (u64, u64) {
    if xs.is_empty() {
        return (0, 0);
    }
    let mut sorted: Vec<u64> = xs.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let q1 = sorted[n / 4];
    let q3 = sorted[(3 * n) / 4];
    (q1, q3)
}

/// 90% confidence interval on the geomean of `xs`, computed via bootstrap
/// resampling with `n_resamples` draws. Returns (lo, hi).
///
/// Use a fixed `seed` so the CI is itself reproducible across trial replays.
/// Cost: O(n_resamples × xs.len()) `ln()` calls; ~40ms for 1000×864 on M1.
pub fn bootstrap_ci_geomean(xs: &[u64], n_resamples: usize, seed: u64) -> (u64, u64) {
    if xs.is_empty() {
        return (0, 0);
    }
    let mut rng = SplitMix64::new(seed);
    let n = xs.len();
    let mut resampled: Vec<u64> = Vec::with_capacity(n_resamples);
    let mut buf: Vec<u64> = vec![0; n];
    for _ in 0..n_resamples {
        for slot in buf.iter_mut() {
            let idx = (rng.next_u64() as usize) % n;
            *slot = xs[idx];
        }
        resampled.push(geomean(&buf));
    }
    resampled.sort_unstable();
    let lo_idx = (n_resamples * 5) / 100;
    let hi_idx = ((n_resamples * 95) / 100).min(n_resamples - 1);
    (resampled[lo_idx], resampled[hi_idx])
}

/// Returns true when A's 90% CI is strictly below B's 90% CI (non-overlapping
/// in the favorable direction for A being faster). The keep-gate primitive:
/// `is_statistically_faster(trial_ci, baseline_ci)` answers "is the trial
/// faster than the baseline at >= 90% confidence?".
pub fn is_statistically_faster(a_ci: (u64, u64), b_ci: (u64, u64)) -> bool {
    a_ci.1 < b_ci.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_yields_zero() {
        assert_eq!(geomean(&[]), 0);
    }

    #[test]
    fn single_value_round_trips() {
        assert_eq!(geomean(&[100]), 100);
    }

    #[test]
    fn geomean_is_below_arithmetic_mean() {
        let xs = [1, 10, 100, 1000];
        let g = geomean(&xs);
        let am: u64 = xs.iter().sum::<u64>() / xs.len() as u64;
        assert!(g < am);
    }

    #[test]
    fn median_odd_and_even() {
        assert_eq!(median(&[5, 3, 1, 4, 2]), 3);
        assert_eq!(median(&[1, 2, 3, 4]), 2); // (2 + 3) / 2 = 2 with u64 floor
        assert_eq!(median(&[]), 0);
    }

    #[test]
    fn iqr_basic() {
        let (q1, q3) = iqr(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!((q1, q3), (3, 7));
    }

    #[test]
    fn bootstrap_ci_brackets_true_geomean() {
        let xs: Vec<u64> = (100..200).collect();
        let true_geo = geomean(&xs);
        let (lo, hi) = bootstrap_ci_geomean(&xs, 1000, 0xABCD);
        assert!(lo <= true_geo && true_geo <= hi, "{lo} <= {true_geo} <= {hi}");
        // CI should be tight relative to the spread.
        assert!(hi - lo < true_geo / 5);
    }

    #[test]
    fn statistically_faster_when_non_overlapping() {
        assert!(is_statistically_faster((100, 200), (250, 300)));
        assert!(!is_statistically_faster((100, 200), (150, 300)));
        assert!(!is_statistically_faster((100, 200), (200, 300)));
    }
}
