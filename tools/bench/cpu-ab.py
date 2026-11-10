#!/usr/bin/env python3
"""A/B the in-memory posting loop by per-thread CPU time, robust to a loaded host.

    tools/bench/cpu-ab.py A=/path/to/bc[:ENV=V,...] B=/path/to/bc[:ENV=V,...] [--blocks 12] [--orders 30]
                          [--apps standard-prod] [--seed 1]

Each block is one `bc sales --memory` process with BENCH=1 POST=<orders>. It measures the CPU time
of its own main thread per order (CLOCK_THREAD_CPUTIME_ID: time spent running, not waiting for a
CPU) over the steady-state orders, divided by the order's AL steps. The default metric is the 10th
percentile of that per block (--metric): interference from other work on the host (shared cores,
caches) only makes code slower, so the low quantile is the robust estimate of its own cost. Each
round runs one block of A and one of B, back to back in a random order. (Running A and B at the same time was tried and
rejected: identical binaries then measured -15%, from unequal interference between the two.)

The result is the median of the block values per side with their spread (p25–p75, min–max), and
the difference as the median of the per-round B/A ratios (A and B of a round ran back to back, so
shifts in host load cancel), with a bootstrap 90% interval over rounds. It also checks that both
sides executed the same number of AL steps (steps_min/steps_max per block). Run from the repository root.

Validated on 2026-11-08: under a steady load around 18, A against an identical copy measured -1.0%
(-3.7% .. +1.8%). When the load jumps within seconds the interval widens to +-10-20%, and the
script's interval shows that. Decide only on intervals that exclude 0, and repeat the run.
"""
import argparse, os, random, re, statistics, subprocess, sys

METRIC = 'nsps_p10'


def parse_side(spec):
    name, _, rest = spec.partition('=')
    path, _, envs = rest.partition(':')
    env = dict(kv.split('=', 1) for kv in envs.split(',') if kv)
    return name, path, env


def start(path, env, orders, apps):
    e = {**os.environ, 'BENCH': '1', 'POST': str(orders), 'BC_APPS': '', **env}
    args = [path, 'sales', '--memory'] + (['--apps', apps] if apps else [])
    return subprocess.Popen(args, env=e, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)


def block(path, env, orders, apps, proc=None):
    proc = proc or start(path, env, orders, apps)
    out = proc.communicate()[0]
    m = re.search(r'BENCH orders=(\d+) cpu_us_median=([0-9.]+) cpu_us_p25=([0-9.]+) cpu_us_p75=([0-9.]+) steps_min=(\d+) steps_max=(\d+) ns_per_step_min=([0-9.]+) ns_per_step_p10=([0-9.]+) ns_per_step_median=([0-9.]+)', out)
    if not m:
        sys.exit(f'no BENCH line from {path} {env}:\n{out[-2000:]}')
    metric = {'cpu': 2, 'nsps_min': 7, 'nsps_p10': 8, 'nsps_median': 9}[METRIC]
    return float(m.group(metric)), (int(m.group(5)), int(m.group(6)))


def q(v, p):
    v = sorted(v)
    return v[round((len(v) - 1) * p)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('sides', nargs=2)
    ap.add_argument('--blocks', type=int, default=12)
    ap.add_argument('--orders', type=int, default=30)
    ap.add_argument('--apps', default='')
    ap.add_argument('--seed', type=int, default=1)
    ap.add_argument('--metric', default='nsps_p10', choices=['cpu', 'nsps_min', 'nsps_p10', 'nsps_median'],
                    help='per block: median thread CPU per order (cpu), or CPU per AL step over the orders: min, p10 (default), median')
    a = ap.parse_args()
    global METRIC
    METRIC = a.metric
    random.seed(a.seed)
    sides = [parse_side(s) for s in a.sides]
    res = {n: [] for n, _, _ in sides}
    steps = {n: set() for n, _, _ in sides}
    for _ in range(a.blocks):
        order = sides[:]
        random.shuffle(order)
        for n, p, env in order:
            v, st = block(p, env, a.orders, a.apps)
            res[n].append(v)
            steps[n].add(st)
    (na, va), (nb, vb) = list(res.items())
    for n, v in res.items():
        print(f'{n}: median {statistics.median(v):9.1f} {"us/order" if METRIC == "cpu" else "ns/step"}  p25-p75 {q(v, .25):.1f}-{q(v, .75):.1f}  min-max {min(v):.1f}-{max(v):.1f}  (n={len(v)})  steps {sorted(steps[n])}')
    # paired: A and B of one round ran back to back, so their ratio cancels drift in host load
    ratios = [100 * (b / a - 1) for a, b in zip(va, vb)]
    d = statistics.median(ratios)
    boots = [statistics.median([random.choice(ratios) for _ in ratios]) for _ in range(2000)]
    print(f'{nb} vs {na}: {d:+.1f}% (median of {len(ratios)} paired rounds; bootstrap 90% interval {q(boots, .05):+.1f}% .. {q(boots, .95):+.1f}%)')
    du = 100 * (statistics.median(vb) / statistics.median(va) - 1)
    print(f'  (unpaired, medians of the blocks: {du:+.1f}%)')
    print('steps identical' if steps[na] == steps[nb] else f'STEPS DIFFER: {steps[na]} vs {steps[nb]}')


if __name__ == '__main__':
    main()
