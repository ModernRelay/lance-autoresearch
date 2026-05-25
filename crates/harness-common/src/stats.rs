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

/// Paired bootstrap CI on the ratio `geomean(agent) / geomean(ref)`.
///
/// The paired resampler picks an index, then draws the (agent, ref) pair at
/// that index together. This preserves the within-query correlation between
/// agent and reference timings, which is exactly what an interleaved
/// measurement captures and what makes the test much tighter than two
/// independent bootstraps. Ratio < 1.0 means agent is faster.
///
/// Returns (lo, hi) where the central 90% of bootstrap ratios fall.
/// `agent` and `ref` must be the same length. Returns (1.0, 1.0) on empty.
pub fn bootstrap_ci_paired_ratio(
    agent: &[u64],
    reference: &[u64],
    n_resamples: usize,
    seed: u64,
) -> (f64, f64) {
    debug_assert_eq!(agent.len(), reference.len());
    if agent.is_empty() {
        return (1.0, 1.0);
    }
    let n = agent.len();
    let mut rng = SplitMix64::new(seed);
    let mut ratios: Vec<f64> = Vec::with_capacity(n_resamples);
    for _ in 0..n_resamples {
        let mut a_sum_ln = 0.0f64;
        let mut r_sum_ln = 0.0f64;
        for _ in 0..n {
            let idx = (rng.next_u64() as usize) % n;
            a_sum_ln += (agent[idx].max(1) as f64).ln();
            r_sum_ln += (reference[idx].max(1) as f64).ln();
        }
        let geomean_a = (a_sum_ln / n as f64).exp();
        let geomean_r = (r_sum_ln / n as f64).exp();
        ratios.push(geomean_a / geomean_r);
    }
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo_idx = (n_resamples * 5) / 100;
    let hi_idx = ((n_resamples * 95) / 100).min(n_resamples - 1);
    (ratios[lo_idx], ratios[hi_idx])
}

/// Paired keep-gate: agent is statistically faster than reference when the
/// 90% CI on the ratio `agent/ref` lies strictly below 1.0.
pub fn is_statistically_faster_paired(ratio_ci: (f64, f64)) -> bool {
    ratio_ci.1 < 1.0
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

    #[test]
    fn paired_ratio_catches_small_consistent_win() {
        // Agent is 10% faster than reference on every sample, plus shared
        // multiplicative noise (e.g. thermal). Independent bootstrap would
        // wash out the signal because the noise dominates either marginal;
        // paired bootstrap sees the consistent 0.9 ratio per sample.
        let mut rng = SplitMix64::new(0xCAFE);
        let n = 200;
        let mut agent: Vec<u64> = Vec::with_capacity(n);
        let mut reference: Vec<u64> = Vec::with_capacity(n);
        for _ in 0..n {
            // shared noise factor in [0.5, 1.5]: same query, different runs
            let shared_noise = 0.5 + rng.next_f32() as f64;
            let r_ns = (1000.0 * shared_noise) as u64;
            let a_ns = (900.0 * shared_noise) as u64; // 10% faster
            agent.push(a_ns);
            reference.push(r_ns);
        }
        let (lo, hi) = bootstrap_ci_paired_ratio(&agent, &reference, 1000, 0xABCD);
        // Paired CI should hug 0.9 tightly even with noise.
        assert!(lo < 0.92, "lo={lo}");
        assert!(hi < 0.92, "hi={hi}");
        assert!(is_statistically_faster_paired((lo, hi)));
    }

    #[test]
    fn paired_ratio_no_signal_brackets_1() {
        // Agent and reference are independent samples from the same
        // distribution. Paired ratio CI should bracket 1.0.
        let mut rng = SplitMix64::new(0xDEAD);
        let n = 200;
        let agent: Vec<u64> = (0..n).map(|_| 800 + (rng.next_u64() % 400)).collect();
        let reference: Vec<u64> = (0..n).map(|_| 800 + (rng.next_u64() % 400)).collect();
        let (lo, hi) = bootstrap_ci_paired_ratio(&agent, &reference, 1000, 0xABCD);
        assert!(lo < 1.0 && hi > 1.0, "CI should bracket 1.0: [{lo}, {hi}]");
        assert!(!is_statistically_faster_paired((lo, hi)));
    }
}
