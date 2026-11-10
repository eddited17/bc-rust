# Benchmark data and BCPT reports

Tools for the I16 benchmark of bc-rust against Business Central 29.1. The plan is [docs/benchmark-plan.md](../../docs/benchmark-plan.md).

| file | what |
|---|---|
| `gleif_customers.py` | turns the GLEIF LEI golden copy into API v2.0 `customers` payloads, loads them, and checks what each system stored |
| `test_gleif_customers.py` | self-test on five synthetic records (`python3 -I tools/benchdata/test_gleif_customers.py`) |
| `bcpt_report.py` | compares BC Performance Toolkit log entries of two systems, scenario by scenario |

Everything is Python 3 with the standard library only. Run it with `python3 -I`. The GLEIF file and the generated payloads stay outside git: the repo is public and the file is 500 MB.

## GLEIF customers

Source: the GLEIF "golden copy" of the LEI-CDF (level 1) file, CSV, published three times a day at https://goldencopy.gleif.org/api/v2/golden-copies/publishes/latest. Licence CC0 1.0 (GLEIF LEI Data Terms of Use, section II.3). GLEIF's terms also say not to present derived data as GLEIF's, so call the customers "derived from the GLEIF golden copy". Record the file name and its sha256 with every run; `stats.json` holds both.

```sh
python3 -I tools/benchdata/gleif_customers.py sample \
  --gleif 20261110-0000-gleif-goldencopy-lei2-golden-copy.csv.zip \
  --countries country-region.csv --post-codes post-code.csv \
  --out out-100k --n 100000 --seed 16 --digits 8 --postcode-policy blank
```

`--countries` and `--post-codes` are exports of the company's Country/Region (table 9) and Post Code (table 225) tables: `code,name,iso_code,...` and `code,city,search_city,country,county`. Export them from both systems. If the two exports differ, the systems don't start from the same data, and the run is void.

Outputs:
- `customers.jsonl` has one POST body per line, keys in the API page's field order.
- `customers.expected.jsonl` has what BC should store for each customer (Code fields upper-cased, post code rewrites applied) and flags for every rule that fired.
- `stats.md` and `stats.json` hold the statistics. `unmapped-countries.csv` lists the ISO codes without a Country/Region row.

### Selection

A record is picked when its `SHA-256(seed, 0x00, LEI)` is among the N smallest of the file. The pick doesn't depend on the file's row order, and the same file and seed give the same sample byte for byte. Customers are numbered `GL00000001`... in that hash order. A newer golden copy gives a different sample, because LEIs come and go.

The script keeps every entity status (active, inactive, lapsed registrations). `--status ACTIVE` keeps only active entities. Messy data is the point, so it cleans nothing that BC accepts as is.

### Mapping

| API field | Customer field | GLEIF column | BC length |
|---|---|---|---|
| `number` | No. | generated, `GL` + 8 digits in line order | Code[20] |
| `displayName` | Name | Entity.LegalName | Text[100] |
| `type` | Contact Type | always `Company` | |
| `addressLine1` | Address | LegalAddress.FirstAddressLine, plus " " + AddressNumber when that is set and not already in the line | Text[100] |
| `addressLine2` | Address 2 | LegalAddress.AdditionalAddressLine.1 | Text[50] |
| `city` | City | LegalAddress.City | Text[30] |
| `state` | County | LegalAddress.Region (ISO 3166-2, e.g. `US-MA`, sent as is) | Text[30] |
| `country` | Country/Region Code | LegalAddress.Country mapped through the ISO Code column of Country/Region | Code[10] |
| `postalCode` | Post Code | LegalAddress.PostalCode | Code[20] |

Empty values are left out of the payload. GLEIF level 1 has no phone, e-mail or VAT number.

Why these numbers: the CRONUS customer series `CUST` runs from C00010 to C99990 in steps of 10, so it can't number 100,000 customers, but it allows manual numbers. `GL...` sorts after CRONUS's customers 10000-50000, so `Customer.FindFirst` still returns 10000. It also stays outside the range 10000..40000 that the BCPT sample "Post Sales Invoices" filters on.

### Length rules

BC measures Text[n] and Code[n] in UTF-16 code units (the .NET string length), not in characters. A name with one character outside the Basic Multilingual Plane (a math letter, an emoji, a rare CJK ideograph) counts that character twice. The script cuts a value that is too long to the longest prefix of at most n units and never splits a surrogate pair. BC itself rejects such a value with "The length of the string is ..., but it must be less than or equal to n characters", so without the cut the record would fail on BC.

Code fields (Post Code, and Search Name, which BC derives from Name) are trimmed with .NET's whitespace set and upper-cased the way BC does it. BC 29.1 calls `string.ToUpper(culture)` (Ncl `StringHelper.ToUpperUsingNavCultureInfoSafe`), a one-to-one mapping per character, so `ß` stays `ß` and `ﬁ` stays `ﬁ`. Python's and Rust's `to_uppercase` use the full Unicode mapping (`ß` -> `SS`). The script implements the one-to-one mapping and flags every name where the two differ (`full_vs_simple_uppercase_differs`). One exception was observed on BC itself: it leaves Georgian Mkhedruli letters unchanged, although Unicode 11 and later map them to Mtavruli, and the script does the same.

### Country codes

GLEIF uses ISO 3166 alpha-2. The script maps a code through the Country/Region table's ISO Code column. One alias is built in: CRONUS stores Greece as `EL`, with ISO Code `EL` and numeric 300, so `GR` maps to `EL`. A code without a row is sent blank (BC's "domestic") and counted in `unmapped-countries.csv`. It isn't dropped, because a blank country is valid data for BC.

### Post codes

BC validates the API fields in the page's order (BC: `NavFormODataExtension.SetValuesAsync` walks the page's controls; bc-rust: `odata.rs` `apply_props`), so `postalCode` comes last. Two validations matter:
- City: `Post Code.ValidateCity` returns at once when `GuiAllowed` is false, which it is in API sessions on both systems. The city is never rewritten.
- Post Code: `Post Code.ValidatePostCode` looks the code up in the Post Code table, in any country. On a hit it prefers a row of the customer's country, then a row with the same Search City. It then overwrites Post Code, City, County and Country/Region with that row. A code whose only `*` is the last character is used as a filter.

CRONUS (Contoso) has 371 post codes. Most carry a country prefix (`AT-1100`), but 70 are real UK codes without one (`EC2A 3JL`, `B27 4KT`), and a London legal address can hit them.

`--postcode-policy blank` (the default) sends no post code for those records, so neither system can rewrite anything. The 2026-11-10 sample blanks 81 UK codes this way. `--postcode-policy keep` sends the GLEIF value and writes BC's rewrite into the expected file (`postcode_rewritten_by_bc`). Either way, `verify` shows whether both systems stored the same thing.

## Loading and checking

```sh
# bc-rust (no auth) or BC (basic auth: user and password from environment variables, never on the command line)
python3 -I tools/benchdata/gleif_customers.py load --payloads out-100k/customers.jsonl \
  --api 'http://HOST:PORT/BC/api/v2.0/companies(ID)' --user-env BC_USER --password-env BC_PASSWORD --log load.jsonl

python3 -I tools/benchdata/gleif_customers.py export-sql     # prints the PostgreSQL and SQL Server export queries
python3 -I tools/benchdata/gleif_customers.py verify --expected out-100k/customers.expected.jsonl \
  --stored bcrust.csv --stored-b bc.csv --out verify.md
```

The loader posts one customer at a time on one connection. That is deliberate. Each new customer creates a contact (Marketing Setup, Bus. Rel. Code for Customers = CUST) numbered from the `CONT` series, and parallel posting would give the two systems different contact numbers. Contoso's `CONT` series ends at CT100000 and CRONUS already uses 24 numbers, so before a 100,000-customer load clear that series line's Ending No. on both systems.

`verify` compares each system's stored rows with the expected file and, with `--stored-b`, the two systems with each other, column by column, including the posting groups that the API's customer template sets. It exits non-zero on any difference.

First result, 2026-11-10. Lines 1..99,467 of the seed-16 sample were loaded into BC 29.1 (container `bcside`, its CRONUS) and into bc-rust (b41a391, PostgreSQL in the same VM).
- BC stored exactly the expected values for all 99,467 customers in every compared column.
- bc-rust stored 99,465. The two others failed because of the upper-casing above (Search Name 102 and 101 characters). Of the 99,465, 120 have a different Search Name; every other column is identical on both systems.
- The posting groups are blank on both: CRONUS has no template that the API applies to a new customer.

Collation matters for filters. The container's CRONUS database is Latin1_General_100_CS_AS (case-sensitive), and bc-rust's PostgreSQL text columns use a case-insensitive collation. `city eq 'George Town'` returns 813 rows on BC and 960 on bc-rust. For filter values in a comparison, pick spellings that occur only once, or compare with the same case rule.

## BCPT reports

`bcpt_report.py` reads each system's BCPT Log Entry rows in the shape of BC's own `bcptLogEntries` API (`/api/microsoft/performancToolkit/v1.0/companies(ID)/bcptLogEntries`, JSON or CSV). `bcpt_report.py export-sql` prints the PostgreSQL query that gives the same columns on bc-rust, which doesn't serve that API route yet. Per codeunit and operation it reports iterations, errors, p50/p95/mean duration, the ratio of medians with a bootstrap 95 % interval (fixed seed), and the SQL statement medians. `--warmup-s` and `--drop-first` drop the cold start.

```sh
python3 -I tools/benchdata/bcpt_report.py --a bcrust-log.csv --b bc-log.json --code SALES10 --version -1 \
  --warmup-s 120 --operation Scenario --out bcpt.md
```
