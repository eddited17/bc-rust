#!/usr/bin/env python3
"""Compare BC Performance Toolkit (BCPT) log entries of two systems, scenario by scenario.

  bcpt_report.py --a bcrust.json --b bc.json [--label-a bc-rust] [--label-b "BC 29.1"]
                 [--code SUITE] [--version N] [--tag T] [--warmup-s 120] [--drop-first]
                 [--operation Scenario] [--out report.md]

Input per side: the JSON of BC's own log entry API (GET .../api/microsoft/performancToolkit/v1.0/
companies(<id>)/bcptLogEntries, either {"value": [...]} or a list) or a CSV with the same column names:
bcptCode, lineNumber, tag, version, entryNumber, startTime, endTime, codeunitID, codeunitName, sessionId,
operation, message, durationMin (BC's API calls the milliseconds "durationMin"), numberOfSQLStmts, status.
`export-sql` of this script prints the PostgreSQL query that produces that CSV on bc-rust.

Per (codeunit, operation): iterations, errors, p50/p95/p99/mean duration in ms for each side, the ratio of
medians B/A with a bootstrap 95 % interval (fixed seed), and the SQL statement medians. The BCPT "Scenario"
operation is the whole iteration without the user waits (BCPT Role Wrapper).
"""
import argparse, csv, json, random, statistics, sys
from collections import defaultdict
from datetime import datetime


def parse_time(s):
    s = (s or "").strip().replace("Z", "+00:00")
    for fmt in (None, "%Y-%m-%d %H:%M:%S.%f", "%Y-%m-%d %H:%M:%S"):
        try:
            return datetime.fromisoformat(s) if fmt is None else datetime.strptime(s, fmt)
        except ValueError:
            continue
    return None


def load(path):
    if path.endswith(".json"):
        d = json.load(open(path, encoding="utf-8"))
        rows = d["value"] if isinstance(d, dict) else d
    else:
        rows = list(csv.DictReader(open(path, encoding="utf-8-sig", newline="")))
    out = []
    for r in rows:
        out.append({
            "code": str(r.get("bcptCode", "")), "line": int(r.get("lineNumber") or 0), "tag": str(r.get("tag", "")),
            "version": int(r.get("version") or 0), "cu": int(r.get("codeunitID") or 0), "name": str(r.get("codeunitName", "")),
            "session": int(r.get("sessionId") or 0), "op": str(r.get("operation", "")), "msg": str(r.get("message", "")),
            "ms": float(r.get("durationMin") or 0), "sql": float(r.get("numberOfSQLStmts") or 0),
            "ok": str(r.get("status", "")).lower() in ("success", "0"), "start": parse_time(str(r.get("startTime", ""))),
        })
    return out


def select(rows, a):
    rows = [r for r in rows if (not a.code or r["code"] == a.code) and (not a.tag or r["tag"] == a.tag)]
    if a.version == -1 and rows:
        v = max(r["version"] for r in rows)
        rows = [r for r in rows if r["version"] == v]
    elif a.version:
        rows = [r for r in rows if r["version"] == a.version]
    if a.warmup_s and rows:
        t0 = min(r["start"] for r in rows if r["start"])
        rows = [r for r in rows if r["start"] and (r["start"] - t0).total_seconds() >= a.warmup_s]
    if a.drop_first:
        seen, kept = set(), []
        for r in sorted(rows, key=lambda r: (r["start"] or datetime.min)):
            k = (r["session"], r["cu"], r["op"])
            if k in seen:
                kept.append(r)
            seen.add(k)
        rows = kept
    if a.operation:
        rows = [r for r in rows if r["op"] == a.operation]
    return rows


def q(xs, p):
    if p == 50 and xs:
        return statistics.median(xs)
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))] if xs else float("nan")


def boot_ratio(a, b, n=2000, seed=16):
    if len(a) < 3 or len(b) < 3:
        return float("nan"), float("nan")
    rnd = random.Random(seed)
    rs = []
    for _ in range(n):
        ma = statistics.median(rnd.choices(a, k=len(a)))
        mb = statistics.median(rnd.choices(b, k=len(b)))
        if ma > 0:
            rs.append(mb / ma)
    rs.sort()
    return rs[int(0.025 * len(rs))], rs[int(0.975 * len(rs)) - 1]


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd")
    e = sub.add_parser("export-sql")
    e.add_argument("--company-schema", default="c_cronus")
    ap.add_argument("--a")
    ap.add_argument("--b")
    ap.add_argument("--label-a", default="bc-rust")
    ap.add_argument("--label-b", default="BC 29.1")
    ap.add_argument("--code")
    ap.add_argument("--tag")
    ap.add_argument("--version", type=int, default=0, help="0 = all, -1 = the highest per side")
    ap.add_argument("--warmup-s", type=float, default=0)
    ap.add_argument("--drop-first", action="store_true", help="drop each session's first iteration per operation")
    ap.add_argument("--operation")
    ap.add_argument("--out")
    a = ap.parse_args()
    if a.cmd == "export-sql":
        print(f"""-- bc-rust: BCPT Log Entry (table 149002) in the API's column names
select f2 as "bcptCode", f3 as "lineNumber", f15 as "tag", f13 as "version", f1 as "entryNumber",
       to_char(f4, 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') as "startTime", to_char(f5, 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') as "endTime",
       f7 as "codeunitID", '' as "codeunitName", f14 as "sessionId", f11 as "operation", f6 as "message",
       f9 as "durationMin", f12 as "numberOfSQLStmts", case f10 when 0 then 'Success' else 'Error' end as "status"
from {a.company_schema}.t149002 order by f1;""")
        return 0
    if not a.a or not a.b:
        ap.error("--a and --b are required")
    A, B = select(load(a.a), a), select(load(a.b), a)
    groups = defaultdict(lambda: ([], []))
    names = {}
    for side, rows in ((0, A), (1, B)):
        for r in rows:
            groups[(r["cu"], r["op"])][side].append(r)
            if r["name"]:
                names[r["cu"]] = r["name"]
    lines = [f"# BCPT comparison: {a.label_a} (A) vs {a.label_b} (B)", "",
             f"Filters: code `{a.code or 'any'}`, tag `{a.tag or 'any'}`, version `{a.version or 'all'}`, warm-up {a.warmup_s:.0f} s dropped, "
             f"first iteration per session {'dropped' if a.drop_first else 'kept'}. Durations in ms, user waits excluded (BCPT).", "",
             "| codeunit | operation | n A | err A | p50 A | p95 A | n B | err B | p50 B | p95 B | B/A p50 [95 % CI] | mean A | mean B | SQL p50 A | SQL p50 B |",
             "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|"]
    for (cu, op), (ra, rb) in sorted(groups.items()):
        da = [r["ms"] for r in ra if r["ok"]]
        db = [r["ms"] for r in rb if r["ok"]]
        lo, hi = boot_ratio(da, db)
        ratio = (statistics.median(db) / statistics.median(da)) if da and db and statistics.median(da) > 0 else float("nan")
        f = lambda x: "" if x != x else (f"{x:.0f}" if x >= 10 else f"{x:.1f}")
        lines.append(f"| {cu} {names.get(cu, '')} | {op} | {len(ra)} | {sum(1 for r in ra if not r['ok'])} | {f(q(da, 50))} | {f(q(da, 95))} | "
                     f"{len(rb)} | {sum(1 for r in rb if not r['ok'])} | {f(q(db, 50))} | {f(q(db, 95))} | "
                     f"{'' if ratio != ratio else f'{ratio:.2f} [{lo:.2f}, {hi:.2f}]'} | "
                     f"{f(statistics.mean(da)) if da else ''} | {f(statistics.mean(db)) if db else ''} | "
                     f"{f(q([r['sql'] for r in ra if r['ok']], 50))} | {f(q([r['sql'] for r in rb if r['ok']], 50))} |")
    errs = defaultdict(int)
    for side, rows in (("A", A), ("B", B)):
        for r in rows:
            if not r["ok"]:
                errs[(side, r["cu"], r["msg"][:120])] += 1
    if errs:
        lines += ["", "Errors (side, codeunit, message, count):", ""]
        for (side, cu, msg), n in sorted(errs.items(), key=lambda x: -x[1])[:30]:
            lines.append(f"- {side} {cu}: {msg} ({n})")
    text = "\n".join(lines) + "\n"
    if a.out:
        open(a.out, "w", encoding="utf-8").write(text)
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
