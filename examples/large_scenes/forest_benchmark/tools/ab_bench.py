"""Interleaved A/B benchmark runner for forest_benchmark.

Runs several configurations round-robin (order rotated every repetition so drift and thermal state
cancel), repeats each N times, and prints mean / stdev / p50 / p95 / p99 / GPU-raster span per
configuration plus the relative difference to the first configuration with Welch's t statistic.
Include the same binary twice (an A/A control) to see the noise floor of your machine.

Usage:
  python3 ab_bench.py --reps 5 --common "--meshlet --no-shadows --trees 100000 --frames 300" \
      --config head=/path/forest_head \
      --config rebuild=/path/forest_new \
      --config retained=/path/forest_new:--retained \
      --config head2=/path/forest_head
A config is name=binary[:extra args].
"""
import argparse, math, re, statistics as st, subprocess, sys

ROW = re.compile(r"^\| (\S+) \| ([\d.]+) \| ([\d.]+) \| ([\d.]+) \| ([\d.]+) \| ([\d.]+) \| ([\d.]+) \|", re.M)
SPAN = re.compile(r"render/meshlet_visibility_buffer_raster/elapsed_gpu: ([\d.]+)")


def run(binary, args, label):
    out = subprocess.run([binary, "--label", label, "--out", "/dev/null", "--top-spans", "60", *args],
                         capture_output=True, text=True, timeout=1800)
    text = out.stdout
    m = ROW.search(text)
    if not m:
        sys.stderr.write(f"run failed ({label}):\n{(out.stdout + out.stderr)[-1500:]}\n")
        return None
    s = SPAN.search(text)
    errors = len(re.findall(r"retained meshlet instances", out.stdout + out.stderr))
    return dict(mean=float(m[3]), p50=float(m[4]), p95=float(m[5]), p99=float(m[6]), max=float(m[7]),
                raster=float(s[1]) if s else float("nan"), mismatches=errors)


def welch(a, b):
    if len(a) < 2 or len(b) < 2:
        return float("nan")
    va, vb = st.variance(a) / len(a), st.variance(b) / len(b)
    d = math.sqrt(va + vb)
    return (st.mean(b) - st.mean(a)) / d if d > 0 else float("inf")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reps", type=int, default=5)
    ap.add_argument("--common", default="")
    ap.add_argument("--config", action="append", required=True)
    a = ap.parse_args()
    cfgs = []
    for c in a.config:
        name, rest = c.split("=", 1)
        binary, _, extra = rest.partition(":")
        cfgs.append((name, binary, a.common.split() + extra.split()))
    res = {n: [] for n, _, _ in cfgs}
    for rep in range(a.reps):
        order = cfgs[rep % len(cfgs):] + cfgs[:rep % len(cfgs)]
        for name, binary, args in order:
            r = run(binary, args, f"{name}-{rep}")
            if r:
                res[name].append(r)
                print(f"rep {rep} {name:10s} mean {r['mean']:7.2f}  p99 {r['p99']:7.2f}  raster {r['raster']:6.2f}", flush=True)
    base = cfgs[0][0]
    bm = [r["mean"] for r in res[base]]
    print(f"\n{'config':12s} {'n':>2s} {'mean ms':>9s} {'stdev':>6s} {'min-max':>13s} {'p50':>7s} {'p95':>7s} {'p99':>7s} {'raster':>7s} {'vs ' + base:>10s} {'welch t':>8s}")
    for name, _, _ in cfgs:
        rs = res[name]
        if not rs:
            continue
        m = [r["mean"] for r in rs]
        mean = st.mean(m)
        print(f"{name:12s} {len(rs):2d} {mean:9.2f} {st.pstdev(m):6.2f} {min(m):6.2f}-{max(m):6.2f} "
              f"{st.mean(r['p50'] for r in rs):7.2f} {st.mean(r['p95'] for r in rs):7.2f} {st.mean(r['p99'] for r in rs):7.2f} "
              f"{st.mean(r['raster'] for r in rs):7.2f} {100 * (mean / st.mean(bm) - 1):+9.1f}% {welch(bm, m):8.1f}")
    bad = sum(r["mismatches"] for rs in res.values() for r in rs)
    if bad:
        print(f"WARNING: {bad} validation mismatches were logged")


if __name__ == "__main__":
    main()
