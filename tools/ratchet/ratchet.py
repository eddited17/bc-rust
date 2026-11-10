#!/usr/bin/env python3
"""Ratchet: the tests that passed on a baseline must keep passing.

The lists live outside git, in data/parity/ratchet/ (the repository has no vendor/ or data/), or in
$RATCHET_DIR:

  must-pass-<dataset>.txt   one test per line, "<codeunit id>\t<test>", sorted; from a baseline run
  flaky-<dataset>.txt       known flakes, never counted as losses: "<codeunit id>" (whole codeunit) or
                            "<codeunit id>\t<test>", then optionally "\t# reason"; "#" lines are comments
  slice-<dataset>.txt       the quick slice: codeunit ids, one per line ("# ..." comments)

Commands:

  ratchet.py make RESULTS.jsonl [--flaky FLAKY.txt] [--dir DIR]
      write must-pass-<dataset>.txt from a complete al-test run: every test whose outcome is
      "pass", minus the flakes. The dataset is the results' "data" field.
  ratchet.py check RESULTS.jsonl [--dir DIR] [--full]
      compare a run with the must-pass list of its dataset. A must-pass test that ran and did not
      pass is a LOSS: the losses are printed and the exit status is 1. Tests the run did not
      include are counted as "not in this run" (a slice), unless --full, where they are losses
      too. A run whose summary (<stem>-summary.txt next to the results) has no complete data:
      line is void: exit 2.
  ratchet.py slice DATASET [--dir DIR]
      print the quick slice's codeunit ids comma-separated, for al-test --codeunit.

Outcome classes (al-test's "class" field) are reported when present: flagged passes and
untrusted passes among the must-pass tests (docs/assumptions.md). They are not losses.
"""
import json
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
# RATCHET_DIR overrides it (a snapshot copy of the tree has no data/)
DEFAULT_DIR = os.environ.get("RATCHET_DIR") or os.path.join(REPO, "data", "parity", "ratchet")


def load(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def key(r):
    return (int(r["codeunit_id"]), r["test"])


def read_keys(path):
    out = set()
    if not os.path.exists(path):
        return out
    with open(path, encoding="utf-8") as f:
        for line in f:
            # AL names have no "#": everything after one is a comment
            line = line.split("#", 1)[0].rstrip()
            if not line.strip():
                continue
            parts = [p.strip() for p in line.split("\t") if p.strip()]
            if len(parts) == 1:
                out.add((int(parts[0]), None))
            else:
                out.add((int(parts[0]), parts[1]))
    return out


def is_flaky(k, flaky):
    return (k[0], None) in flaky or k in flaky


def dataset_of(rows):
    ds = {r.get("data", "?") for r in rows}
    if len(ds) != 1:
        sys.exit(f"ratchet: results mix datasets {sorted(ds)}")
    return ds.pop()


def summary_of(results):
    if results.endswith("-results.jsonl"):
        return results[: -len("-results.jsonl")] + "-summary.txt"
    return None


def data_line_ok(results):
    """(ok, text): the summary's data: lines are complete (al-test marks a half-built company INCOMPLETE)."""
    s = summary_of(results)
    if not s or not os.path.exists(s):
        return None, "no summary next to the results (data: line not checked)"
    lines = [l.rstrip("\n") for l in open(s, encoding="utf-8") if l.startswith("data:")]
    if not lines:
        return False, f"{s}: no data: line"
    bad = [l for l in lines if "INCOMPLETE" in l]
    if bad:
        return False, f"{s}: {bad[0][:200]}"
    return True, lines[0][:200]


def opt(args, name, default=None):
    if name in args:
        i = args.index(name)
        v = args[i + 1]
        del args[i : i + 2]
        return v
    return default


def cmd_make(args):
    d = opt(args, "--dir", DEFAULT_DIR)
    flaky_path = opt(args, "--flaky")
    (results,) = args
    ok, text = data_line_ok(results)
    if ok is False:
        sys.exit(f"ratchet make: the run is void: {text}")
    rows = load(results)
    ds = dataset_of(rows)
    flaky = read_keys(flaky_path or os.path.join(d, f"flaky-{ds}.txt"))
    keep = sorted(k for k in map(key, (r for r in rows if r["outcome"] == "pass")) if not is_flaky(k, flaky))
    os.makedirs(d, exist_ok=True)
    out = os.path.join(d, f"must-pass-{ds}.txt")
    with open(out, "w", encoding="utf-8") as f:
        for cu, t in keep:
            f.write(f"{cu}\t{t}\n")
    print(f"{out}: {len(keep):,} must-pass tests ({ds}; {len(rows):,} in the run, flakes left out: "
          f"{sum(1 for r in rows if r['outcome'] == 'pass' and is_flaky(key(r), flaky)):,})")


def cmd_check(args):
    d = opt(args, "--dir", DEFAULT_DIR)
    full = "--full" in args
    args = [a for a in args if a != "--full"]
    (results,) = args
    ok, text = data_line_ok(results)
    print(f"data: {text}")
    if ok is False:
        print("RATCHET VOID: the run has no complete data: line")
        return 2
    rows = load(results)
    ds = dataset_of(rows)
    must = read_keys(os.path.join(d, f"must-pass-{ds}.txt"))
    if not must:
        sys.exit(f"ratchet: no must-pass list for {ds} in {d}")
    flaky = read_keys(os.path.join(d, f"flaky-{ds}.txt"))
    by = {key(r): r for r in rows}
    lost, absent, flaky_flips, gained = [], [], [], 0
    classes = {}
    for k in sorted(must):
        r = by.get(k)
        if r is None or r["outcome"] == "disabled":
            absent.append(k)
            continue
        c = r.get("class")
        if c:
            classes[c] = classes.get(c, 0) + 1
        if r["outcome"] != "pass":
            (flaky_flips if is_flaky(k, flaky) else lost).append(r)
    for k, r in by.items():
        if r["outcome"] == "pass" and k not in must and not is_flaky(k, flaky):
            gained += 1
    ran = len(must) - len(absent)
    print(f"ratchet {ds}: {len(must):,} must-pass tests, {ran:,} in this run, {len(lost)} lost, "
          f"{len(flaky_flips)} known-flaky failures, {gained} passing tests not on the list (gains)")
    if classes:
        print("  must-pass tests by class: " + ", ".join(f"{c} {n:,}" for c, n in sorted(classes.items())))
    if absent and full:
        print(f"  --full: {len(absent):,} must-pass tests did not run, e.g. {absent[:3]}")
    for r in flaky_flips[:20]:
        print(f"  flaky  {r['codeunit_id']} {r['codeunit']} :: {r['test']}: {r['message'].splitlines()[0][:120] if r['message'] else r['outcome']}")
    for r in lost:
        msg = r["message"].splitlines()[0][:160] if r.get("message") else ""
        print(f"  LOST   {r['codeunit_id']} {r['codeunit']} :: {r['test']} [{r['outcome']}] {msg}")
    if lost or (full and absent):
        print(f"RATCHET FAIL: {len(lost)} lost" + (f", {len(absent)} not run" if full and absent else ""))
        return 1
    print("RATCHET OK")
    return 0


def cmd_slice(args):
    d = opt(args, "--dir", DEFAULT_DIR)
    (ds,) = args
    ids = sorted(k[0] for k in read_keys(os.path.join(d, f"slice-{ds}.txt")))
    if not ids:
        sys.exit(f"ratchet: no slice-{ds}.txt in {d}")
    print(",".join(map(str, ids)))
    return 0


def main():
    if len(sys.argv) < 3 or sys.argv[1] not in ("make", "check", "slice"):
        print(__doc__)
        return 2
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "make":
        cmd_make(args)
        return 0
    return cmd_check(args) if cmd == "check" else cmd_slice(args)


if __name__ == "__main__":
    sys.exit(main())
