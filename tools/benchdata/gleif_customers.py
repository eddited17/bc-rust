#!/usr/bin/env python3
"""GLEIF LEI-CDF golden copy -> Business Central API v2.0 `customers` payloads (bc-rust benchmark data).

Subcommands (Python 3.10+, standard library only; run with `python3 -I`):

  sample  --gleif FILE.csv.zip|FILE.csv --countries CSV --post-codes CSV --out DIR
          [--n 100000] [--seed 16] [--prefix GL] [--digits 8] [--postcode-policy blank|keep] [--status any|ACTIVE]
      Picks N records by a fixed seed (the N smallest SHA-256(seed, LEI); independent of file order) and writes
        DIR/customers.jsonl           one API v2.0 POST body per line, keys in the API page's field order
        DIR/customers.expected.jsonl  what BC 29.1 should store for each (after its own validation), plus flags
        DIR/stats.json, DIR/stats.md  file and sample statistics; DIR/unmapped-countries.csv
  verify  --expected DIR/customers.expected.jsonl --stored A.csv [--stored-b B.csv] [--label-a bc-rust]
          [--label-b bc] [--out REPORT.md]
      Compares stored customers (CSV exported with the SQL from `export-sql`) with the expected values and, with
      --stored-b, the two sides with each other, field by field.
  export-sql  [--company-schema c_cronus] [--sql-company "CRONUS International Ltd_"] [--prefix GL]
      Prints the export queries for bc-rust (PostgreSQL) and BC (SQL Server).
  load    --payloads DIR/customers.jsonl --api URL/api/v2.0/companies(ID) [--user-env VAR --password-env VAR]
          [--from 0] [--limit N] [--log FILE]
      POSTs the payloads one by one over one keep-alive connection and logs status and time per request.
      Sequential on purpose: the contact each customer creates takes the next number of the CONT series, so
      parallel loads give the two sides different contact numbers.

Rules (details in README.md next to this file):
- Lengths are BC's: a Text[n]/Code[n] holds n UTF-16 code units (.NET string length). Longer values are cut
  to n units (never inside a surrogate pair); Code values are trimmed first, as BC does.
- Search Name (Code[100]) is BC's: Trim + ToUpper(culture) = simple, one-to-one case mapping.
- Country: the LEI record's ISO 3166 alpha-2 code is mapped through the Country/Region table's ISO Code column;
  codes without a row are sent blank and listed.
- Post codes: BC's Post Code.ValidatePostCode runs last (page order) and, when the code exists in the Post Code
  table, overwrites City, County and Country from the table. `blank` (default) drops post codes that hit the table
  or end in a lone `*` (a filter for BC); `keep` sends them and predicts the rewrite in the expected file.
"""
import argparse, csv, hashlib, heapq, http.client, io, json, os, statistics, sys, time, unicodedata, urllib.parse, zipfile
from collections import Counter

# API v2.0 customers page (APIV2Customers.Page.al) field order: BC and bc-rust both validate in this order.
PAGE_ORDER = ["number", "displayName", "type", "addressLine1", "addressLine2", "city", "state", "country", "postalCode"]
LEN = {"number": 20, "displayName": 100, "addressLine1": 100, "addressLine2": 50, "city": 30, "state": 30, "country": 10, "postalCode": 20}
# .NET Char.IsWhiteSpace (what String.Trim removes), which is also Unicode White_Space.
DOTNET_WS = set("\u0009\u000a\u000b\u000c\u000d \u0085                 　")
G = "Entity.LegalAddress."


def u16len(s):
    return len(s.encode("utf-16-le")) // 2


def cut16(s, n):
    """The longest prefix of s with at most n UTF-16 code units (a surrogate pair is never split)."""
    if u16len(s) <= n:
        return s
    out, units = [], 0
    for ch in s:
        w = 2 if ord(ch) > 0xFFFF else 1
        if units + w > n:
            break
        out.append(ch)
        units += w
    return "".join(out)


def bc_trim(s):
    i, j = 0, len(s)
    while i < j and s[i] in DOTNET_WS:
        i += 1
    while j > i and s[j - 1] in DOTNET_WS:
        j -= 1
    return s[i:j]


def simple_upper_char(ch):
    """Unicode simple uppercase mapping (what .NET ToUpper does per character). Python's str.upper() applies the
    full mapping (SpecialCasing: 'ß' -> 'SS'); when that is longer than one character the simple mapping is the
    title-case form if that is a single character (Greek iota-subscript letters), else the character itself.
    Georgian Mkhedruli letters stay as they are: BC 29.1 stored them unchanged in Search Name (2026-11-10, GLEIF
    GL00005871), although Unicode 11+ maps them to Mtavruli."""
    if "\u10d0" <= ch <= "\u10ff":
        return ch
    u = ch.upper()
    if len(u) == 1:
        return u
    t = ch.title()
    return t if len(t) == 1 else ch


def dotnet_upper(s):
    return "".join(simple_upper_char(c) for c in s)


def is_non_ascii(s):
    return any(ord(c) > 0x7F for c in s)


def script_of(s):
    """Rough script of a name: the Unicode name prefix of its first non-ASCII letter."""
    for c in s:
        if ord(c) > 0x7F and c.isalpha():
            n = unicodedata.name(c, "")
            for k in ("LATIN", "CYRILLIC", "GREEK", "ARABIC", "HEBREW", "CJK", "HIRAGANA", "KATAKANA", "HANGUL", "THAI", "DEVANAGARI", "ARMENIAN", "GEORGIAN", "BENGALI", "TAMIL"):
                if n.startswith(k):
                    return k
            return n.split(" ")[0] or "OTHER"
    return "ASCII-only" if not is_non_ascii(s) else "non-letter-only"


def pct(xs, q):
    if not xs:
        return 0
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(q / 100 * (len(xs) - 1))))]


def open_gleif(path):
    if path.endswith(".zip"):
        z = zipfile.ZipFile(path)
        names = [n for n in z.namelist() if n.endswith(".csv")]
        if len(names) != 1:
            sys.exit(f"{path}: expected one .csv in the zip, found {names}")
        return io.TextIOWrapper(z.open(names[0]), encoding="utf-8", newline=""), names[0]
    return open(path, encoding="utf-8", newline=""), os.path.basename(path)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def load_countries(path):
    """ISO alpha-2 -> BC Country/Region code (via the ISO Code column; the row whose code equals the ISO code wins)."""
    m = {}
    for r in csv.DictReader(open(path, encoding="utf-8")):
        iso = (r.get("iso_code") or "").strip().upper()
        code = (r.get("code") or "").strip().upper()
        if not iso or not code:
            continue
        if iso not in m or code == iso or (m[iso] != iso and code < m[iso]):
            m[iso] = code
    # CRONUS stores Greece as EL (the EU's VAT code) with ISO Code EL and ISO Numeric 300, so ISO GR finds no row
    for iso, code in ISO_ALIASES.items():
        if iso not in m and code in {v for v in m.values()}:
            m[iso] = code
    return m


ISO_ALIASES = {"GR": "EL"}


def load_post_codes(path):
    rows = [{k: (v or "") for k, v in r.items()} for r in csv.DictReader(open(path, encoding="utf-8"))]
    # Key1 of table 225 is (Code, City); FindFirst returns the first row in that order (case-insensitive collation)
    rows.sort(key=lambda r: (r["code"].upper(), r["city"].upper()))
    return rows


def predict_post_code(rows, post, city, county, country):
    """Post Code.ValidatePostCode (PostCode.Table.al) as it runs when the API validates postalCode last.
    Returns (post, city, county, country, hit)."""
    if post == "":
        return post, city, county, country, False
    if post.find("*") == len(post) - 1:  # IsFilter: StrPos(PostCode, '*') = StrLen(PostCode)
        prefix = post[:-1]
        cand = [r for r in rows if r["code"].upper().startswith(prefix)]
    else:
        cand = [r for r in rows if r["code"].upper() == post]
    if not cand:
        return post, city, county, country, False
    if country:
        c2 = [r for r in cand if r["country"].upper() == country]
        if c2:
            cand = c2
    if city:
        sc = bc_trim(dotnet_upper(city))
        c3 = [r for r in cand if r["search_city"].upper() == sc]
        if c3:
            cand = c3
    r = cand[0]
    return r["code"], r["city"], r["county"], r["country"], True


def field(row, h, name):
    i = h.get(name)
    return row[i] if i is not None and i < len(row) else ""


def cmd_sample(a):
    csv.field_size_limit(1 << 24)
    countries = load_countries(a.countries)
    post_rows = load_post_codes(a.post_codes)
    f, inner = open_gleif(a.gleif)
    rd = csv.reader(f)
    header = next(rd)
    h = {n: i for i, n in enumerate(header)}
    for need in ("LEI", "Entity.LegalName", G + "FirstAddressLine", G + "City", G + "Country", G + "PostalCode"):
        if need not in h:
            sys.exit(f"column {need} missing")
    seed = str(a.seed).encode()
    heap = []  # (-key, lei, needed fields): the N smallest keys
    full = Counter()
    name_len_all = Counter()
    seen_exact, seen_upper = set(), set()
    t0 = time.time()
    for row in rd:
        if len(row) < len(header):
            full["short_rows"] += 1
            continue
        full["rows"] += 1
        lei = row[h["LEI"]]
        name = row[h["Entity.LegalName"]]
        status = field(row, h, "Entity.EntityStatus")
        full["status_" + (status or "blank")] += 1
        if not name.isascii():
            full["name_non_ascii"] += 1
        L = u16len(name)
        name_len_all[min(L, 999)] += 1
        if L > 100:
            full["name_over_100"] += 1
        d1 = hashlib.blake2b(name.encode(), digest_size=8).digest()
        d2 = hashlib.blake2b(name.strip().upper().encode(), digest_size=8).digest()  # stats only: full upper-casing
        if d1 in seen_exact:
            full["dup_exact"] += 1
        else:
            seen_exact.add(d1)
        if d2 in seen_upper:
            full["dup_upper"] += 1
        else:
            seen_upper.add(d2)
        if a.status != "any" and status != a.status:
            continue
        key = int.from_bytes(hashlib.sha256(seed + b"\0" + lei.encode()).digest()[:16], "big")
        item = (-key, lei, [row[h[c]] if c in h else "" for c in KEEP])
        if len(heap) < a.n:
            heapq.heappush(heap, item)
        elif -key > heap[0][0]:
            heapq.heapreplace(heap, item)
        if full["rows"] % 500000 == 0:
            print(f"  {full['rows']:,} rows, {time.time() - t0:.0f} s", file=sys.stderr)
    full["distinct_names_exact"] = len(seen_exact)
    full["distinct_names_upper"] = len(seen_upper)
    del seen_exact, seen_upper
    picked = sorted(((-k, lei, vals) for k, lei, vals in heap))
    os.makedirs(a.out, exist_ok=True)
    st = Counter()
    unmapped = Counter()
    scripts = Counter()
    lens = {k: [] for k in ("name", "address", "address_2", "city", "county", "post_code")}
    names_exact, names_upper = Counter(), Counter()
    cities, cities_exact, countries_out, order = Counter(), Counter(), Counter(), []
    with open(os.path.join(a.out, "customers.jsonl"), "w", encoding="utf-8") as fp, \
            open(os.path.join(a.out, "customers.expected.jsonl"), "w", encoding="utf-8") as fe:
        for i, (_key, lei, vals) in enumerate(picked, 1):
            v = dict(zip(KEEP, vals))
            flags = []
            number = f"{a.prefix}{i:0{a.digits}d}"
            name_raw = v["Entity.LegalName"]
            addr = v[G + "FirstAddressLine"]
            num = v[G + "AddressNumber"].strip()
            if num and num not in addr:
                addr = f"{addr} {num}".strip() if addr else num
                flags.append("address_number_appended")
            addr2 = v[G + "AdditionalAddressLine.1"]
            city_raw, region_raw, iso, post_raw = v[G + "City"], v[G + "Region"], v[G + "Country"].strip().upper(), v[G + "PostalCode"]
            for k, s in (("name", name_raw), ("address", addr), ("address_2", addr2), ("city", city_raw), ("county", region_raw), ("post_code", bc_trim(post_raw))):
                lens[k].append(u16len(s))
            out = {}
            for key_, raw in (("displayName", name_raw), ("addressLine1", addr), ("addressLine2", addr2), ("city", city_raw), ("state", region_raw)):
                if u16len(raw) > LEN[key_]:
                    flags.append(f"cut_{key_}")
                out[key_] = cut16(raw, LEN[key_])
            post = bc_trim(post_raw)
            if u16len(post) > LEN["postalCode"]:
                flags.append("cut_postalCode")
                post = bc_trim(cut16(post, LEN["postalCode"]))
            country = countries.get(iso, "")
            if iso and not country:
                unmapped[iso] += 1
                flags.append("country_unmapped")
            if iso and country and country != iso:
                flags.append("country_code_differs_from_iso")
            # what BC stores (Code fields upper-cased with the simple mapping)
            exp_post = bc_trim(dotnet_upper(post))
            filt = bool(exp_post) and exp_post.find("*") == len(exp_post) - 1
            if filt:
                flags.append("postcode_is_filter")
            e_post, e_city, e_county, e_country, hit = predict_post_code(post_rows, exp_post, out["city"], out["state"], country)
            if hit:
                st["postcode_hits"] += 1
            if hit or (filt and a.postcode_policy == "blank"):
                if a.postcode_policy == "blank":
                    flags.append("postcode_blanked")
                    post, exp_post = "", ""
                    e_post, e_city, e_county, e_country = "", out["city"], out["state"], country
                else:
                    flags.append("postcode_rewritten_by_bc")
                hit = hit and a.postcode_policy == "keep"
            payload = {"number": number, "displayName": out["displayName"], "type": "Company"}
            for key_, val in (("addressLine1", out["addressLine1"]), ("addressLine2", out["addressLine2"]), ("city", out["city"]),
                              ("state", out["state"]), ("country", country), ("postalCode", post)):
                if val != "":
                    payload[key_] = val
            assert list(payload) == [k for k in PAGE_ORDER if k in payload]
            name = out["displayName"]
            search = bc_trim(dotnet_upper(name))
            rust_upper = bc_trim(name).upper()
            if rust_upper != search:
                flags.append("full_vs_simple_uppercase_differs")
                if len(rust_upper) > 100:
                    flags.append("full_uppercase_over_100")
            if bc_trim(name) == "":
                flags.append("blank_name")
            exp = {"no": number, "lei": lei, "iso_country": iso, "name": name, "search_name": search,
                   "address": out["addressLine1"], "address_2": out["addressLine2"],
                   "city": e_city if hit else out["city"],
                   "county": e_county if hit else out["state"],
                   "country": e_country if hit else country,
                   "post_code": e_post if hit else exp_post,
                   "flags": flags}
            fp.write(json.dumps(payload, ensure_ascii=False) + "\n")
            fe.write(json.dumps(exp, ensure_ascii=False) + "\n")
            for fl in flags:
                st[fl] += 1
            if is_non_ascii(name):
                st["name_non_ascii"] += 1
            scripts[script_of(name)] += 1
            names_exact[name] += 1
            names_upper[search] += 1
            cities[bc_trim(out["city"]).upper()] += 1
            cities_exact[out["city"]] += 1
            countries_out[country or "(blank)"] += 1
            order.append(name)
            if not city_raw.strip():
                st["blank_city"] += 1
            if not post:
                st["blank_post_code"] += 1
            if not iso:
                st["blank_country"] += 1
    n = len(picked)
    st["records"] = n
    target = n / 100
    one_pct = min((c for c in cities.items() if c[0]), key=lambda kv: (abs(kv[1] - target), kv[0]))
    variants = sorted(((k, v) for k, v in cities_exact.items() if bc_trim(k).upper() == one_pct[0]), key=lambda kv: -kv[1])
    unique_name = next((x for x in order if names_upper[bc_trim(dotnet_upper(x))] == 1 and x.isascii() and "'" not in x
                        and '"' not in x and 12 <= len(x) <= 60 and x == x.strip()), None)
    st["dup_name_exact_records"] = sum(c for c in names_exact.values() if c > 1)
    st["dup_name_exact_groups"] = sum(1 for c in names_exact.values() if c > 1)
    st["dup_search_name_records"] = sum(c for c in names_upper.values() if c > 1)
    st["dup_search_name_groups"] = sum(1 for c in names_upper.values() if c > 1)
    with open(os.path.join(a.out, "unmapped-countries.csv"), "w", encoding="utf-8") as fu:
        fu.write("iso_code,records\n")
        for iso, c in unmapped.most_common():
            fu.write(f"{iso},{c}\n")
    src = os.path.abspath(a.gleif)
    stats = {
        "source": {"file": src, "inner": inner, "sha256": sha256_file(a.gleif) if not a.no_hash else None,
                   "countries": a.countries, "post_codes": a.post_codes},
        "params": {"n": a.n, "seed": a.seed, "prefix": a.prefix, "postcode_policy": a.postcode_policy, "status": a.status},
        "full_file": dict(full),
        "full_file_name_len_utf16": {"p50": _hist_pct(name_len_all, 50), "p90": _hist_pct(name_len_all, 90), "p99": _hist_pct(name_len_all, 99), "max": max(name_len_all) if name_len_all else 0},
        "sample": dict(st),
        "sample_lengths_utf16": {k: {"p50": pct(v, 50), "p90": pct(v, 90), "p99": pct(v, 99), "max": max(v) if v else 0,
                                     "over_limit": sum(1 for x in v if x > {"name": 100, "address": 100, "address_2": 50, "city": 30, "county": 30, "post_code": 20}[k])}
                                 for k, v in lens.items()},
        "sample_scripts": dict(scripts.most_common()),
        "sample_top_duplicate_names": [[k, c] for k, c in names_upper.most_common(15) if c > 1],
        "sample_unmapped_countries": dict(unmapped.most_common()),
        "sample_countries": dict(countries_out.most_common()),
        "sample_top_cities_ci": cities.most_common(15),
        "filter_city_1pct": {"city_ci": one_pct[0], "rows_ci": one_pct[1], "exact_variants": variants[:8]},
        "filter_unique_display_name": unique_name,
        "elapsed_s": round(time.time() - t0, 1),
    }
    json.dump(stats, open(os.path.join(a.out, "stats.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    open(os.path.join(a.out, "stats.md"), "w", encoding="utf-8").write(stats_md(stats))
    print(stats_md(stats))


def _hist_pct(hist, q):
    total = sum(hist.values())
    if not total:
        return 0
    want, acc = q / 100 * total, 0
    for k in sorted(hist):
        acc += hist[k]
        if acc >= want:
            return k
    return max(hist)


def stats_md(s):
    f, sm, n = s["full_file"], s["sample"], s["sample"].get("records", 0) or 1
    rows = f.get("rows", 0) or 1
    p = lambda x, d: f"{x:,} ({100 * x / d:.2f} %)"
    lines = [
        "# GLEIF customer sample: statistics", "",
        f"Source: `{os.path.basename(s['source']['file'])}` (sha256 `{s['source']['sha256']}`), seed {s['params']['seed']}, "
        f"N {s['params']['n']:,}, post-code policy `{s['params']['postcode_policy']}`, status filter `{s['params']['status']}`.", "",
        "## Whole file", "",
        f"- records: {f.get('rows', 0):,} (rows with too few columns: {f.get('short_rows', 0):,})",
        f"- legal names with non-ASCII characters: {p(f.get('name_non_ascii', 0), rows)}",
        f"- legal name length (UTF-16 units): p50 {s['full_file_name_len_utf16']['p50']}, p90 {s['full_file_name_len_utf16']['p90']}, "
        f"p99 {s['full_file_name_len_utf16']['p99']}, max {s['full_file_name_len_utf16']['max']}; over 100: {p(f.get('name_over_100', 0), rows)}",
        f"- duplicate legal names: {p(f.get('dup_exact', 0), rows)} records repeat an earlier name exactly, "
        f"{p(f.get('dup_upper', 0), rows)} repeat one after BC's Search Name upper-casing",
        f"- entity status: " + ", ".join(f"{k[7:]} {v:,}" for k, v in sorted(f.items()) if k.startswith("status_")), "",
        "## Sample", "",
        f"- records: {sm.get('records', 0):,}",
        f"- names with non-ASCII characters: {p(sm.get('name_non_ascii', 0), n)}; scripts (first non-ASCII letter): "
        + ", ".join(f"{k} {v:,}" for k, v in list(s["sample_scripts"].items())[:10]),
        f"- names whose full Unicode upper-casing (Rust, Python) differs from BC's simple mapping: {p(sm.get('full_vs_simple_uppercase_differs', 0), n)}; "
        f"of those longer than Code[100] after full upper-casing: {sm.get('full_uppercase_over_100', 0):,}",
        f"- duplicate names: {sm.get('dup_name_exact_records', 0):,} records in {sm.get('dup_name_exact_groups', 0):,} groups (exact); "
        f"{sm.get('dup_search_name_records', 0):,} records in {sm.get('dup_search_name_groups', 0):,} groups (Search Name)",
        f"- cut to BC's length: name {sm.get('cut_displayName', 0):,}, address {sm.get('cut_addressLine1', 0):,}, address 2 {sm.get('cut_addressLine2', 0):,}, "
        f"city {sm.get('cut_city', 0):,}, county {sm.get('cut_state', 0):,}, post code {sm.get('cut_postalCode', 0):,}",
        f"- address number appended to address line 1: {sm.get('address_number_appended', 0):,}",
        f"- country: unmapped {p(sm.get('country_unmapped', 0), n)} (sent blank), blank in GLEIF {sm.get('blank_country', 0):,}, "
        f"BC code differs from ISO {sm.get('country_code_differs_from_iso', 0):,}",
        f"- post codes found in the Post Code table (BC rewrites city/county/country): {sm.get('postcode_hits', 0):,}; "
        f"blank post code {sm.get('blank_post_code', 0):,}; filter-like (trailing *) {sm.get('postcode_is_filter', 0):,}",
        f"- blank city {sm.get('blank_city', 0):,}; blank name {sm.get('blank_name', 0):,}", "",
        "Length percentiles in the sample (UTF-16 units, before cutting):", "",
        "| field | p50 | p90 | p99 | max | over BC limit |", "|---|---:|---:|---:|---:|---:|",
    ]
    for k, v in s["sample_lengths_utf16"].items():
        lines.append(f"| {k} | {v['p50']} | {v['p90']} | {v['p99']} | {v['max']} | {v['over_limit']:,} |")
    if s["sample_top_duplicate_names"]:
        lines += ["", "Most repeated Search Names in the sample: " + "; ".join(f"`{k}` x{c}" for k, c in s["sample_top_duplicate_names"][:10])]
    fc = s["filter_city_1pct"]
    lines += ["", "## Filter values", "",
              f"- 1 % city: `{fc['city_ci']}` matches {fc['rows_ci']:,} rows case-insensitively, as bc-rust's PostgreSQL `ci` collation compares. "
              "A case-sensitive database (the BC 29.1 container's CRONUS is Latin1_General_100_CS_AS) matches only the exact spelling; "
              "exact spellings: " + ", ".join(f"`{k}` {v:,}" for k, v in fc["exact_variants"]),
              f"- one unique displayName (case-insensitive, ASCII, no quotes): `{s['filter_unique_display_name']}`", "",
              "Country spread (top 20): " + ", ".join(f"{k} {v:,}" for k, v in list(s["sample_countries"].items())[:20]),
              "", "Top cities (case-insensitive): " + ", ".join(f"{k or '(blank)'} {v:,}" for k, v in s["sample_top_cities_ci"])]
    if s["sample_unmapped_countries"]:
        lines += ["", "Unmapped ISO codes: " + ", ".join(f"{k} {v}" for k, v in s["sample_unmapped_countries"].items())]
    return "\n".join(lines) + "\n"


KEEP = ["Entity.LegalName", G + "FirstAddressLine", G + "AddressNumber", G + "AdditionalAddressLine.1", G + "City", G + "Region", G + "Country", G + "PostalCode", "Entity.EntityStatus"]

COLS = ["no", "name", "search_name", "address", "address_2", "city", "county", "country", "post_code"]


def read_stored(path):
    out = {}
    for r in csv.DictReader(open(path, encoding="utf-8-sig", newline="")):
        r = {k.strip().lower(): (v if v is not None else "") for k, v in r.items()}
        out[r["no"].strip().upper()] = r
    return out


def cmd_verify(a):
    exp = {}
    for line in open(a.expected, encoding="utf-8"):
        e = json.loads(line)
        exp[e["no"]] = e
    A = read_stored(a.stored)
    B = read_stored(a.stored_b) if a.stored_b else None
    rep = [f"# Stored customers vs expected ({len(exp):,} expected)", ""]

    def compare(side, label):
        missing = [n for n in exp if n not in side]
        diff, ex = Counter(), {}
        for n, e in exp.items():
            r = side.get(n)
            if r is None:
                continue
            for c in COLS[1:]:
                if c in r and r[c] != e[c]:
                    diff[c] += 1
                    ex.setdefault(c, []).append((n, e[c], r[c]))
        rep.append(f"## {label}: {len(side):,} stored rows, {len(missing):,} expected rows missing")
        rep.append("")
        for c in COLS[1:]:
            rep.append(f"- {c}: {diff[c]:,} differ" + ("" if not diff[c] else "; e.g. " + "; ".join(f"{n}: expected `{x}` stored `{y}`" for n, x, y in ex[c][:3])))
        if missing:
            rep.append(f"- missing (first 10): {', '.join(missing[:10])}")
        rep.append("")
        return diff, missing

    da, ma = compare(A, a.label_a)
    ok = not ma and not sum(da.values())
    if B is not None:
        db, mb = compare(B, a.label_b)
        ok = ok and not mb and not sum(db.values())
        cols = sorted(set(next(iter(A.values()), {}).keys()) & set(next(iter(B.values()), {}).keys()) - {"no"})
        d2, ex2 = Counter(), {}
        for n in sorted(set(A) | set(B)):
            ra, rb = A.get(n), B.get(n)
            if ra is None or rb is None:
                d2["(row only on one side)"] += 1
                continue
            for c in cols:
                if ra.get(c, "") != rb.get(c, ""):
                    d2[c] += 1
                    ex2.setdefault(c, []).append((n, ra.get(c), rb.get(c)))
        rep.append(f"## {a.label_a} vs {a.label_b}: {len(cols)} common columns")
        rep.append("")
        for c in ["(row only on one side)"] + cols:
            rep.append(f"- {c}: {d2[c]:,} differ" + ("" if not d2[c] or c not in ex2 else "; e.g. " + "; ".join(f"{n}: `{x}` vs `{y}`" for n, x, y in ex2[c][:3])))
        ok = ok and not sum(d2.values())
    rep.append("")
    rep.append("RESULT: " + ("identical" if ok else "DIFFERENCES"))
    text = "\n".join(rep) + "\n"
    if a.out:
        open(a.out, "w", encoding="utf-8").write(text)
    print(text)
    return 0 if ok else 1


def cmd_export_sql(a):
    p = a.prefix.replace("'", "''")
    print(f"""-- bc-rust (PostgreSQL), e.g. docker exec <container> psql -U bc -d <db> -c "\\copy (...) to stdout with csv header" > bcrust.csv
select f1 as no, f2 as name, f3 as search_name, f5 as address, f6 as address_2, f7 as city, f92 as county,
       f35 as country, f91 as post_code, f88 as gen_bus_posting_group, f110 as vat_bus_posting_group,
       f21 as customer_posting_group, f5050 as contact_type
from {a.company_schema}.t18 where f1 like '{p}%' order by f1;

-- BC 29.1 (SQL Server in the container; Base Application table, company prefix as in tools/oracle/read-sql.ps1).
-- Export with Invoke-Sqlcmd / System.Data.SqlClient to CSV with the same header.
select [No_] as no, [Name] as name, [Search Name] as search_name, [Address] as address, [Address 2] as address_2,
       [City] as city, [County] as county, [Country_Region Code] as country, [Post Code] as post_code,
       [Gen_ Bus_ Posting Group] as gen_bus_posting_group, [VAT Bus_ Posting Group] as vat_bus_posting_group,
       [Customer Posting Group] as customer_posting_group, [Contact Type] as contact_type
from [{a.sql_company}$Customer$437dbf0e-84ff-417a-965d-ed2bb9650972] where [No_] like '{p}%' order by [No_];""")


def cmd_load(a):
    u = urllib.parse.urlsplit(a.api.rstrip("/") + "/customers")
    conn_cls = http.client.HTTPSConnection if u.scheme == "https" else http.client.HTTPConnection
    conn = conn_cls(u.hostname, u.port or (443 if u.scheme == "https" else 80), timeout=120)
    headers = {"Content-Type": "application/json"}
    if a.user_env:
        import base64
        user, pw = os.environ.get(a.user_env, ""), os.environ.get(a.password_env or "", "")
        headers["Authorization"] = "Basic " + base64.b64encode(f"{user}:{pw}".encode()).decode()
    path = u.path + (("?" + u.query) if u.query else "")
    log = open(a.log, "a", encoding="utf-8") if a.log else None
    ok = bad = 0
    times = []
    t_all = time.time()
    with open(a.payloads, encoding="utf-8") as fp:
        for i, line in enumerate(fp):
            if i < a.start:
                continue
            if a.limit and i >= a.start + a.limit:
                break
            body = line.strip().encode()
            t0 = time.perf_counter()
            for attempt in (1, 2):
                try:
                    conn.request("POST", path, body=body, headers=headers)
                    r = conn.getresponse()
                    data = r.read()
                    break
                except (http.client.HTTPException, OSError):
                    conn.close()
                    if attempt == 2:
                        raise
            ms = (time.perf_counter() - t0) * 1000
            times.append(ms)
            if r.status in (200, 201):
                ok += 1
                msg = ""
            else:
                bad += 1
                msg = data.decode(errors="replace")[:300]
            if log:
                log.write(json.dumps({"i": i, "no": json.loads(line)["number"], "status": r.status, "ms": round(ms, 2), "error": msg}, ensure_ascii=False) + "\n")
    wall = time.time() - t_all
    summary = {"ok": ok, "failed": bad, "wall_s": round(wall, 1), "per_s": round((ok + bad) / wall, 1) if wall else 0,
               "ms_p50": round(pct(times, 50), 1), "ms_p95": round(pct(times, 95), 1), "ms_p99": round(pct(times, 99), 1)}
    print(json.dumps(summary))
    if log:
        log.write(json.dumps({"summary": summary}) + "\n")
    return 0 if bad == 0 else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sp = ap.add_subparsers(dest="cmd", required=True)
    s = sp.add_parser("sample")
    s.add_argument("--gleif", required=True)
    s.add_argument("--countries", required=True, help="CSV with columns code,iso_code (BC Country/Region table)")
    s.add_argument("--post-codes", required=True, help="CSV with columns code,city,search_city,country,county (BC Post Code table)")
    s.add_argument("--out", required=True)
    s.add_argument("--n", type=int, default=100000)
    s.add_argument("--seed", type=int, default=16)
    s.add_argument("--prefix", default="GL")
    s.add_argument("--digits", type=int, default=8)
    s.add_argument("--postcode-policy", choices=["keep", "blank"], default="blank")
    s.add_argument("--status", default="any", help="'any' or an Entity.EntityStatus value such as ACTIVE")
    s.add_argument("--no-hash", action="store_true", help="skip the SHA-256 of the input file")
    v = sp.add_parser("verify")
    v.add_argument("--expected", required=True)
    v.add_argument("--stored", required=True)
    v.add_argument("--stored-b")
    v.add_argument("--label-a", default="bc-rust")
    v.add_argument("--label-b", default="BC 29.1")
    v.add_argument("--out")
    e = sp.add_parser("export-sql")
    e.add_argument("--company-schema", default="c_cronus")
    e.add_argument("--sql-company", default="CRONUS International Ltd_")
    e.add_argument("--prefix", default="GL")
    ld = sp.add_parser("load")
    ld.add_argument("--payloads", required=True)
    ld.add_argument("--api", required=True, help="…/api/v2.0/companies(<id>)")
    ld.add_argument("--user-env")
    ld.add_argument("--password-env")
    ld.add_argument("--from", dest="start", type=int, default=0)
    ld.add_argument("--limit", type=int, default=0)
    ld.add_argument("--log")
    a = ap.parse_args()
    return {"sample": cmd_sample, "verify": cmd_verify, "export-sql": cmd_export_sql, "load": cmd_load}[a.cmd](a) or 0


if __name__ == "__main__":
    sys.exit(main())
