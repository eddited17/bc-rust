#!/usr/bin/env python3
"""Compare two al-test results files (JSONL) test by test.

    tools/parity/diff-runs.py A-results.jsonl B-results.jsonl [--top N] [--movers N] [--app SUBSTR]

A is the baseline, B the new run. A test is keyed by (app, codeunit id, test). Tests that
Microsoft's CI disables (outcome "disabled", or "disabled": true with al-test --include-disabled)
are kept apart: the CI-comparable figures count the tests enabled in both runs. It prints:
- totals: passed in A and B, net, gained (not pass in A, pass in B), lost (the other way round);
- movers by app;
- top causes of losses (B's message) and of gains (A's message), with messages normalized (GUIDs,
  timestamps, dates, long hex, quoted literals and numbers blanked) so that equal causes group;
- message-only changes: same outcome, different message after masking only GUIDs, timestamps and
  dates (with deterministic runs, a non-zero count points at remaining nondeterminism);
- tests only in A or only in B;
- the first N movers with both messages.

Standard library only; run it with `python3 -I`.
"""

import argparse
import collections
import json
import re
import sys

GUID = re.compile(r"\{?[0-9a-fA-F]{8}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{12}\}?")
LONG_HEX = re.compile(r"[0-9a-fA-F]{24,}")
DATETIME = re.compile(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?")
DATE = re.compile(r"\b\d{1,4}[/.-]\d{1,2}[/.-]\d{1,4}\b")
TIME = re.compile(r"\b\d{1,2}:\d{2}(:\d{2}(\.\d+)?)?( ?[AP]M)?\b")


def mask(msg: str) -> str:
    """Blank the parts of a message that differ between identical runs."""
    # long hex runs first (several GUIDs written together); only runs with a letter a-f, so long
    # decimal numbers stay
    m = LONG_HEX.sub(lambda h: "<hex>" if re.search("[a-fA-F]", h.group()) else h.group(), msg)
    m = GUID.sub("<guid>", m)
    m = DATETIME.sub("<datetime>", m)
    m = DATE.sub("<date>", m)
    return TIME.sub("<time>", m)


def cause(msg: str) -> str:
    """A message's first line with its literals blanked (for grouping)."""
    m = mask(msg.split("\n")[0])
    m = re.sub(r"'[^']*'", "'…'", m)
    m = re.sub(r"<(?!guid>|hex>|datetime>|date>|time>)[^>]*>", "<…>", m)
    m = re.sub(r"\d+", "#", m)
    return m[:160]


def load(path):
    out = {}
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                print(f"warning: {path}:{n}: not JSON, skipped", file=sys.stderr)
                continue
            off = r.get("outcome") == "disabled" or bool(r.get("disabled"))
            out[(r.get("app", ""), r.get("codeunit_id", 0), r.get("test", ""))] = (r.get("outcome", "?"), r.get("message", ""), r.get("codeunit", ""), off)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("a", help="baseline results JSONL")
    ap.add_argument("b", help="new results JSONL")
    ap.add_argument("--top", type=int, default=15, help="causes to show (default 15)")
    ap.add_argument("--movers", type=int, default=10, help="movers to list with messages (default 10)")
    ap.add_argument("--app", default="", help="only tests of apps whose name contains this")
    o = ap.parse_args()
    A, B = load(o.a), load(o.b)
    if o.app:
        A = {k: v for k, v in A.items() if o.app.lower() in k[0].lower()}
        B = {k: v for k, v in B.items() if o.app.lower() in k[0].lower()}
    # disabled in Microsoft's CI (in either run): apart from the CI-comparable figures
    disabled_a = sum(1 for v in A.values() if v[3])
    disabled_b = sum(1 for v in B.values() if v[3])
    off = {k for k, v in A.items() if v[3]} | {k for k, v in B.items() if v[3]}
    off_one = [k for k in off if k in A and k in B and A[k][3] != B[k][3]]
    A = {k: v for k, v in A.items() if k not in off}
    B = {k: v for k, v in B.items() if k not in off}
    both = [k for k in A if k in B]
    pa = sum(1 for v in A.values() if v[0] == "pass")
    pb = sum(1 for v in B.values() if v[0] == "pass")
    gained = [k for k in both if A[k][0] != "pass" and B[k][0] == "pass"]
    lost = [k for k in both if A[k][0] == "pass" and B[k][0] != "pass"]
    other = [k for k in both if A[k][0] != B[k][0] and k not in gained and k not in lost]
    msg_only = [k for k in both if A[k][0] == B[k][0] and mask(A[k][1]) != mask(B[k][1])]
    only_a = [k for k in A if k not in B]
    only_b = [k for k in B if k not in A]

    print(f"A: {o.a}\nB: {o.b}")
    if disabled_a or disabled_b:
        print(f"disabled in Microsoft's CI (left out below): A {disabled_a}, B {disabled_b}" + (f"; disabled in only one run: {len(off_one)}" if off_one else ""))
    print(f"tests: A {len(A)}, B {len(B)}, in both {len(both)}")
    print(f"passed: A {pa} ({100 * pa / max(len(A), 1):.1f}%), B {pb} ({100 * pb / max(len(B), 1):.1f}%), net {pb - pa:+}")
    print(f"gained {len(gained)}, lost {len(lost)}, other outcome changes (fail <-> error) {len(other)}")
    print(f"same outcome, different message after masking GUIDs/timestamps: {len(msg_only)}")
    if only_a or only_b:
        print(f"only in A: {len(only_a)}, only in B: {len(only_b)}")

    per = collections.defaultdict(lambda: [0, 0, 0, 0])
    for k in both:
        x = per[k[0]]
        x[0] += A[k][0] == "pass"
        x[1] += B[k][0] == "pass"
    for k in gained:
        per[k[0]][2] += 1
    for k in lost:
        per[k[0]][3] += 1
    movers = sorted(((a, v) for a, v in per.items() if v[2] or v[3]), key=lambda t: -(t[1][2] + t[1][3]))
    if movers:
        print("\nmovers by app (passed A -> B, gained, lost):")
        for a, (x, y, g, l) in movers:
            print(f"  {a:45} {x:6} -> {y:6}  +{g:<5} -{l}")

    for title, ks, src in [("top loss causes (B's message)", lost, B), ("top gain causes (A's message)", gained, A)]:
        if not ks:
            continue
        print(f"\n{title}:")
        for m, n in collections.Counter(cause(src[k][1]) for k in ks).most_common(o.top):
            print(f"  {n:6}  {m}")
    if msg_only:
        print("\nmessage-only changes (first 5; with deterministic runs these are remaining nondeterminism):")
        for k in msg_only[:5]:
            print(f"  {A[k][2]} :: {k[2]}\n     A: {mask(A[k][1])[:150]}\n     B: {mask(B[k][1])[:150]}")
    if o.movers and (gained or lost):
        print(f"\nfirst {o.movers} movers:")
        for k in (lost + gained)[: o.movers]:
            print(f"  {'LOST  ' if k in lost else 'GAINED'} {A[k][2]} :: {k[2]}\n     A: {A[k][0]} {A[k][1].splitlines()[0][:140] if A[k][1] else ''}\n     B: {B[k][0]} {B[k][1].splitlines()[0][:140] if B[k][1] else ''}")


if __name__ == "__main__":
    main()
