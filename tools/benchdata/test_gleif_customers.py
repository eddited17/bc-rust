#!/usr/bin/env python3
"""Self-test for gleif_customers.py on a tiny synthetic GLEIF file: `python3 -I test_gleif_customers.py`."""
import csv, io, json, os, subprocess, sys, tempfile, zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.dont_write_bytecode = True  # no __pycache__ next to the tool (the repo is public)
sys.path.insert(0, HERE)
import gleif_customers as g  # noqa: E402

G = "Entity.LegalAddress."
HEADER = ["LEI", "Entity.LegalName", G + "FirstAddressLine", G + "AddressNumber", G + "AdditionalAddressLine.1",
          G + "City", G + "Region", G + "Country", G + "PostalCode", "Entity.EntityStatus"]
ROWS = [
    ["LEI00000000000000001", "Großhandel Müller GmbH", "Hauptstraße", "5", "", "Berlin", "DE-BE", "DE", "10115", "ACTIVE"],
    ["LEI00000000000000002", "X" * 120, "Y" * 130, "", "Z" * 60, "C" * 40, "US-NY", "US", " 12345-6789-0000-1234-99 ", "ACTIVE"],
    ["LEI00000000000000003", "ﬁnance 𝔘nicode", "1 Road", "", "", "London", "GB-LND", "GB", "EC2A 3JL", "INACTIVE"],
    ["LEI00000000000000004", "Kosovo Co", "Rr. 1", "", "", "Prishtina", "", "XK", "10000", "ACTIVE"],
    ["LEI00000000000000005", "Athens SA", "Odos 1", "", "", "Athina", "", "GR", "105 57", "ACTIVE"],
]


def run(*args):
    return subprocess.run([sys.executable, "-I", os.path.join(HERE, "gleif_customers.py"), *args], capture_output=True, text=True)


def main():
    assert g.u16len("a𝔘") == 3
    assert g.cut16("ab𝔘c", 3) == "ab"
    assert g.dotnet_upper("straße ﬁ ᾳ") == "STRAßE ﬁ ᾼ"
    assert g.dotnet_upper("შპს") == "შპს"
    assert g.bc_trim("  x 　") == "x"
    with tempfile.TemporaryDirectory() as d:
        src = os.path.join(d, "g.csv")
        with open(src, "w", encoding="utf-8", newline="") as f:
            w = csv.writer(f)
            w.writerow(HEADER)
            w.writerows(ROWS)
        z = os.path.join(d, "g.csv.zip")
        with zipfile.ZipFile(z, "w") as zf:
            zf.write(src, "g.csv")
        cc = os.path.join(d, "c.csv")
        open(cc, "w").write("code,name,iso_code,iso_numeric\nDE,Germany,DE,276\nUS,USA,US,840\nGB,UK,GB,826\nEL,Greece,GR,300\n")
        pc = os.path.join(d, "p.csv")
        open(pc, "w").write("code,city,search_city,country,county\nEC2A 3JL,London,LONDON,GB,\nAT-1100,Wien,WIEN,AT,\n")
        outs = []
        for k in (1, 2):
            out = os.path.join(d, f"o{k}")
            r = run("sample", "--gleif", z, "--countries", cc, "--post-codes", pc, "--out", out, "--n", "4", "--seed", "16", "--postcode-policy", "keep")
            assert r.returncode == 0, r.stderr
            outs.append(open(os.path.join(out, "customers.jsonl"), encoding="utf-8").read())
        assert outs[0] == outs[1], "not deterministic"
        pay = [json.loads(x) for x in outs[0].splitlines()]
        exp = [json.loads(x) for x in open(os.path.join(d, "o1", "customers.expected.jsonl"), encoding="utf-8")]
        assert len(pay) == 4 and [p["number"] for p in pay] == ["GL00000001", "GL00000002", "GL00000003", "GL00000004"]
        for p in pay:
            assert list(p) == [k for k in g.PAGE_ORDER if k in p]
            for k, n in g.LEN.items():
                assert g.u16len(p.get(k, "")) <= n, (k, p)
        by_lei = {e["lei"]: (p, e) for p, e in zip(pay, exp)}
        if "LEI00000000000000001" in by_lei:
            p, e = by_lei["LEI00000000000000001"]
            assert p["addressLine1"] == "Hauptstraße 5" and e["search_name"] == "GROßHANDEL MÜLLER GMBH"
            assert "full_vs_simple_uppercase_differs" in e["flags"]
        if "LEI00000000000000003" in by_lei:
            p, e = by_lei["LEI00000000000000003"]
            assert "postcode_rewritten_by_bc" in e["flags"] and e["city"] == "London" and e["country"] == "GB"
        if "LEI00000000000000004" in by_lei:
            p, e = by_lei["LEI00000000000000004"]
            assert "country" not in p and "country_unmapped" in e["flags"]
        if "LEI00000000000000005" in by_lei:
            p, _ = by_lei["LEI00000000000000005"]
            assert p["country"] == "EL"
        # blank policy: no post code that the table knows is sent
        r = run("sample", "--gleif", z, "--countries", cc, "--post-codes", pc, "--out", os.path.join(d, "ob"), "--n", "5", "--seed", "16")
        assert r.returncode == 0, r.stderr
        for line in open(os.path.join(d, "ob", "customers.jsonl"), encoding="utf-8"):
            assert json.loads(line).get("postalCode") != "EC2A 3JL"
        # verify: the expected values as a stored CSV must come out identical
        st = os.path.join(d, "stored.csv")
        with open(st, "w", encoding="utf-8", newline="") as f:
            w = csv.DictWriter(f, fieldnames=g.COLS)
            w.writeheader()
            for e in exp:
                w.writerow({c: e[c] for c in g.COLS})
        r = run("verify", "--expected", os.path.join(d, "o1", "customers.expected.jsonl"), "--stored", st, "--stored-b", st)
        assert r.returncode == 0 and "RESULT: identical" in r.stdout, r.stdout + r.stderr
    print("ok")


if __name__ == "__main__":
    main()
