#!/usr/bin/env python3
"""
check-keep-gate.py: apply the harness keep-gate to a trial's run.log.

Compares a trial against the previous best-kept run (typically the
baseline for the first trial, then whatever was last marked `keep=kept`
in results.tsv). Emits PASS/FAIL with reasoning so the agent doesn't
have to interpret the gate by hand and (more importantly) so the gate
is applied consistently across sessions.

The rule, formally:

  1. correctness: trial must have `correctness: pass`.
  2. CI gate: trial CI upper bound on geomean strictly < previous CI
     lower bound on geomean (90% bootstrap CI; non-overlapping).
  3. Worst-combo guard: trial `worst_ns` <= 1.05 * previous `worst_ns`.
     A sanity cap that catches catastrophic single-combo regressions
     regardless of compensation elsewhere.
  4. Aggregate trade-off: sum of absolute improvements across all
     regressed combos must be at least TRADE_OFF_RATIO times the sum
     of absolute regressions. Default 10x. Captures the intent of
     "wins must clearly dominate losses" without forcing per-combo
     uniformity (which would reject legitimate asymmetric trades like
     -2931 ns on Large skip-deep at the cost of +39 ns on Large
     skip-shallow).

The aggregate rule replaces a per-combo "regression < 5% AND < 20 ns"
floor that an earlier draft used. The per-combo rule incorrectly fails
asymmetric trades that are clearly net beneficial, and it produced
inconsistent decisions across re-runs because individual per-combo
values have ~20% run-to-run variance at sub-30 ns scales on M1. The
aggregate rule is more robust to that variance: a +39 ns regression
on one combo only matters if the wins elsewhere don't dominate it 10x.

Usage:
    check-keep-gate.py <previous-best.log> <trial.log>

Exit codes:
    0  trial PASSES the keep gate (advance, commit, log keep)
    1  trial FAILS the keep gate (revert)
    2  trial correctness failed (revert; exit-2 from run_experiment
       should already have caught this, but we re-check here)
    3  parse error (one of the logs doesn't have the expected fields)

Both arguments are paths to the stdout from
`cargo run --release --bin run_experiment -p <target> -- --mode baseline`
or the trial-mode equivalent. The script tolerates either single-pass
or three-pass logs (it reads the same fields from both).
"""

import re
import sys
from pathlib import Path

# Worst-combo guard. Single-number sanity cap.
WORST_COMBO_PCT_MAX = 0.05

# Aggregate trade-off ratio: total absolute improvement must exceed
# total absolute regression by at least this multiple. 10x is the
# empirical threshold where the trial is clearly beneficial even after
# accounting for measurement variance and asymmetric per-combo trade-offs.
# Tunable here; documented in HARNESS.md.
TRADE_OFF_RATIO = 10.0


def parse_log(path: Path) -> dict:
    """Parse a run.log produced by run_experiment.rs. Returns a dict of
    fields; missing fields are None or empty."""
    text = path.read_text()
    out: dict = {}

    m = re.search(r"^correctness:\s+(\w+)", text, re.M)
    out["correctness"] = m.group(1) if m else None

    # Geomean field name varies by target: ns_per_query (pq-l2),
    # ns_per_intersect (posting-intersect), ns_per_seek (posting-seek).
    m = re.search(r"^geomean_ns_per_\w+:\s+(\d+)", text, re.M)
    out["geomean"] = int(m.group(1)) if m else None

    m = re.search(r"^geomean_ns_ci_90pct:\s+\[(\d+),\s*(\d+)\]", text, re.M)
    out["ci"] = (int(m.group(1)), int(m.group(2))) if m else None

    m = re.search(r"^worst_ns_per_\w+:\s+(\d+)\s+\((.+)\)", text, re.M)
    if m:
        out["worst"] = int(m.group(1))
        out["worst_combo"] = m.group(2).strip()
    else:
        out["worst"] = None
        out["worst_combo"] = None

    # Per-combo lines under `per_combo_geomean_ns:` header. Format:
    #   <shape> <dist>     -> <number> ns
    combos: dict[str, int] = {}
    in_combo = False
    for line in text.splitlines():
        if line.startswith("per_combo_geomean_ns:"):
            in_combo = True
            continue
        if not in_combo:
            continue
        m = re.match(r"\s+(\S.*?)\s+->\s+(\d+)\s+ns\s*$", line)
        if m:
            label = re.sub(r"\s+", " ", m.group(1).strip())
            combos[label] = int(m.group(2))
        elif line.strip() and not line.startswith(" "):
            in_combo = False
    out["combos"] = combos

    return out


def main() -> int:
    if len(sys.argv) != 3:
        print(
            "usage: check-keep-gate.py <previous-best.log> <trial.log>",
            file=sys.stderr,
        )
        return 3

    prev_path = Path(sys.argv[1])
    cur_path = Path(sys.argv[2])
    if not prev_path.exists():
        print(f"error: previous log not found: {prev_path}", file=sys.stderr)
        return 3
    if not cur_path.exists():
        print(f"error: trial log not found: {cur_path}", file=sys.stderr)
        return 3

    prev = parse_log(prev_path)
    cur = parse_log(cur_path)

    # Gate 1: correctness (check first; correctness-fail logs omit the
    # speed fields, so the missing-fields check would otherwise mask
    # exit code 2 behind a parse-error exit 3).
    if cur["correctness"] is None:
        print("error: trial log missing correctness field", file=sys.stderr)
        return 3
    if cur["correctness"] != "pass":
        print(f"FAIL: correctness = {cur['correctness']} (must be 'pass')")
        return 2

    # Now require full fields on both sides; a passing trial without speed
    # data is a malformed run.log.
    required = ("correctness", "geomean", "ci", "worst")
    for label, parsed in (("previous", prev), ("trial", cur)):
        missing = [k for k in required if parsed[k] is None]
        if missing:
            print(f"error: {label} log missing fields: {missing}", file=sys.stderr)
            return 3
        if not parsed["combos"]:
            # Gate 4 requires per-combo data on both sides; silently
            # disabling it would let trials pass without the aggregate
            # trade-off check.
            print(
                f"error: {label} log has no per_combo_geomean_ns rows; "
                f"cannot apply Gate 4",
                file=sys.stderr,
            )
            return 3

    failures: list[str] = []

    # Gate 2: CI non-overlap on geomean.
    cur_ci_hi = cur["ci"][1]
    prev_ci_lo = prev["ci"][0]
    if cur_ci_hi >= prev_ci_lo:
        failures.append(
            f"CI overlap on geomean: trial upper {cur_ci_hi} >= "
            f"previous lower {prev_ci_lo} "
            f"(trial CI {cur['ci']}, previous CI {prev['ci']})"
        )

    # Gate 3: worst-combo sanity cap.
    worst_budget = int(prev["worst"] * (1.0 + WORST_COMBO_PCT_MAX))
    if cur["worst"] > worst_budget:
        failures.append(
            f"worst-combo regression: trial {cur['worst']} ns "
            f"(at {cur['worst_combo']}) > 1.05 * previous worst "
            f"{prev['worst']} ns = {worst_budget} ns"
        )

    # Gate 4: aggregate trade-off ratio.
    total_improvement = 0
    total_regression = 0
    per_combo_changes: list[tuple[str, int, int, int]] = []
    if prev["combos"] and cur["combos"]:
        for combo, cur_v in cur["combos"].items():
            prev_v = prev["combos"].get(combo)
            if prev_v is None:
                continue
            delta = cur_v - prev_v
            per_combo_changes.append((combo, prev_v, cur_v, delta))
            if delta < 0:
                total_improvement += -delta
            elif delta > 0:
                total_regression += delta

    if total_regression > 0:
        ratio = total_improvement / total_regression
        if ratio < TRADE_OFF_RATIO:
            # Build a per-combo summary to show where the trade-off failed.
            regressed = [
                (c, p, v, d) for (c, p, v, d) in per_combo_changes if d > 0
            ]
            regressed.sort(key=lambda x: -x[3])
            top_regs = "; ".join(
                f"{c!r} +{d} ns" for (c, _, _, d) in regressed[:3]
            )
            failures.append(
                f"trade-off ratio {ratio:.1f}x below {TRADE_OFF_RATIO:.0f}x "
                f"threshold (improvements {total_improvement} ns, "
                f"regressions {total_regression} ns; top regressions: {top_regs})"
            )

    if failures:
        print(
            f"FAIL: trial does not clear the keep-gate "
            f"(geomean {prev['geomean']} -> {cur['geomean']} ns; "
            f"worst {prev['worst']} -> {cur['worst']} ns)."
        )
        for f in failures:
            print(f"  - {f}")
        return 1

    geo_delta_pct = (cur["geomean"] / prev["geomean"] - 1.0) * 100
    worst_delta_pct = (cur["worst"] / prev["worst"] - 1.0) * 100
    ratio_str = (
        f"{total_improvement / total_regression:.1f}x"
        if total_regression > 0
        else "infinite (no regressions)"
    )
    print(
        f"PASS: trial keeps. geomean {prev['geomean']} -> {cur['geomean']} ns "
        f"({geo_delta_pct:+.1f}%); worst {prev['worst']} -> {cur['worst']} ns "
        f"({worst_delta_pct:+.1f}%); trade-off {ratio_str}."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
