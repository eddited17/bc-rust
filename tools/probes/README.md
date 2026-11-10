# tools/probes: black-box probes on a real Business Central

We learn how Business Central behaves by running our own AL code on a licensed BC and recording what comes back.
We do not read Microsoft's binaries. A probe is a grid: it sweeps inputs wide enough to find limits (numbers 0, 1, 9,
10, 99 ... 99999, text lengths 1-40, every option combination, several dates, several languages) instead of
confirming expected values. For each cell it records the input, the result as text, the error text
(`GetLastErrorText`) and the error code (`GetLastErrorCode`).

`probes.py` generates an AL extension ("bc-rust probes") from the probe sources, compiles it with the container's AL
compiler, publishes it with a new version, runs the probes and archives the output. Python 3.11+, standard library
only. Compile, publish and a short run take 15-20 s; a rerun with an unchanged app takes about 8 s.

```
probes.py run selftest demo-format demo-evaluate          # run grid specs from probes/
probes.py run u1=/path/U1Probes.Codeunit.al --area dateformula --ctx 'locale=en-US&lang=1033'
probes.py run /path/report-probe-dir --background         # report probes (Report.SaveAs) in a background session
probes.py run /path/report-probe-dir --steps 'P1-*,P4'    # only some steps (cases) of a hand-written probe
probes.py vectors demo-format demo-evaluate               # compile archived runs into tests/vectors/
probes.py extract u2-xlsx-numfmt 20261110T113804Z         # rerun a probe's extract script on an archived run
probes.py gen selftest                                    # generate the AL app only, no VM
probes.py list                                            # specs and archived runs
python3 -B -m unittest test_probes                        # generator and vectors tests, no VM
```

## Layout

| Path | What |
|---|---|
| `probes.py` | generator, runner, vectors compiler |
| `al/` | fixed part of the AL app: result tables, `BCR Probe Log`, `BCR Probe Runner`, background runs, install |
| `vm/ct-*.ps1` | scripts that run inside the BC container: build (compile + publish), run, wait, fetch |
| `probes/*.toml` | grid probe specs: `selftest` (the harness itself), `demo-format`, `demo-evaluate` |
| `test_probes.py` | unit tests |

The app uses object ids 50400-50499: 50400-50409 for the fixed part and 50410 up for probe objects. Every run
renumbers probe objects, so a hand-written probe's own ids are placeholders.

## Setup

The runner reads `~/.config/bc-rust/probes.env` (or `$PROBES_CONFIG`) and then the environment:

| Key | Default | Meaning |
|---|---|---|
| `PROBES_EVIDENCE` | `~/.local/share/bc-rust-probes/evidence` | raw run output (never committed) |
| `PROBES_WORK` | `~/.local/share/bc-rust-probes/work` | generated apps, run logs, `published.json` |
| `PROBES_LOCK` | `$PROBES_WORK/probes.lock` | one run at a time (flock) |
| `PROBES_SSH_KEY`, `PROBES_SSH_TARGET`, `PROBES_SSH_PORT` | `data/vm/bc-vm_ed25519`, `bcadmin@127.0.0.1`, `2222` | ssh into the Windows VM |
| `PROBES_CREDENTIALS` | `data/vm/credentials.txt` | `bc_user=` and `bc_password=` lines; sent on stdin only, never printed |
| `PROBES_CONTAINER` | `bcside` | BcContainerHelper container with BC (NavUserPassword) |
| `PROBES_COMPANY` | `CRONUS International Ltd.` | company the probes run in (`--company` overrides) |
| `PROBES_VM_DIR` | `C:/ProgramData/BcContainerHelper/bcrprobes` | working folder; the container shares this path |
| `PROBES_SYMBOLS_FROM` | `.../o15probe/.alpackages` | where the Microsoft symbol packages are copied from once |
| `PROBES_WAIT_SEC` | `7200` | how long a background run may take |

The VM needs Docker with the BC container running, BcContainerHelper's shared folder, and ssh. The container needs
nothing else: the scripts use its AL compiler (`C:\build\vsix\extension\bin\alc.exe`), the service tier's admin
module and its SQL Server.

## How a run works

1. Generate the app (`app.json`, `src/`, `res/`) under `$PROBES_WORK/builds/<version>`. The version is
   `1.0.<yyyymmdd>.<hhmmss>`. If the source SHA-256 equals the last published one, compile and publish are skipped.
2. Copy it to the VM, compile with `alc.exe` inside the container, `Publish-NAVApp`, `Sync-NAVApp -Mode ForceSync`,
   then `Start-NAVAppDataUpgrade` or `Install-NAVApp`, and unpublish older versions of the probe app.
3. Inside the container, call `POST /ODataV4/BCRProbes_RunProbes` (codeunit 50401 as web service `BCRProbes`) with
   `'<run id>|<probe ids>|<contexts>'`. The response is one short line.
4. Read the output with SQL inside the container (`BCR Probe Line`, `BCR Probe File`) into files in the shared folder,
   then `scp` them to the host.

Data never travels as an HTTP response. Through the VM's port forward, BC responses above about 1.3 KB stall, and
stalled requests wedged the service tier.

Contexts: `--ctx` (repeatable) or a spec's `contexts`. A context is `&`-separated pairs:
- `locale=de-DE` is sent as `Accept-Language`, which sets the new session's language and format region.
- `lang=1031` sets `GlobalLanguage` before the probe runs.
- Any other pair is only recorded.

Each probe runs once per context, and every line carries its context.

`--background` (or `// background: true` in a hand-written probe) runs the probes in a background session started
with `StartSession` and polls table `BCR Probe Request`. Report rendering (`Report.SaveAs`) belongs there, never in a
web service request. A background session takes language and region from the user's settings, so `locale=` is not
applied there; set them in the probe (`GlobalLanguage`, report `FormatRegion`).

## Grid specs (`probes/*.toml`)

```toml
id = "demo-format"                     # [a-z0-9-]; evidence/<id>/, tests/vectors/<area>/<id>.tsv
area = "demo"
title = "Format(Integer or Decimal, 0, format number)"
contexts = ["locale=en-US&lang=1033", "locale=de-DE&lang=1031"]
vars = """
I: Integer;
"""
# body: set Result (Text); Out (JsonObject) for extra outputs; Skip := true drops a cell that does not apply
body = """
Result := Format(v, 0, fmt);
"""
[[dim]]                                # dimensions: the last varies fastest
name = "v"
type = "Decimal"
values = ["0.5", "-1234567.891"]
gen = [{ kind = "nines", max_digits = 7, signs = true, fractions = [".5", ".125"] }]
[[dim]]
name = "fmt"
type = "Integer"
values = [0, 1, 2, 9]
```

Top-level keys come before the first `[[dim]]` (TOML). Keys:
- `vars`: the cell's local variables.
- `body`: the cell's statements.
- `procs`: extra procedures.
- `keep_writes = true`: commits each cell's writes. By default every cell is rolled back.

Dimension types and how values are written:

| Type | Value |
|---|---|
| `Integer` | integers |
| `BigInteger` | integers |
| `Decimal` | strings, so the digits are kept |
| `Boolean` | `true` / `false` |
| `Text`, `Text[n]`, `Code[n]` | strings; control characters are allowed |
| `Date` | `"2024-02-29"`, `"C2024-12-31"` (closing date), `"0D"` |
| `Time` | `"13:45:01.123"` |
| `DateTime` | `"2024-02-29T13:45:01"` |
| `Char` | a code point or a one-character string |
| `Guid` | a GUID string |
| anything else | `raw = true`, with AL expressions as values |

Generators (`gen`):

| Kind | Produces |
|---|---|
| `nines` | 0, 1, 9, 10, 99, 100 ... up to `max_digits`; optional `signs` and `fractions` |
| `range` | `from`..`to`, `step` |
| `text_lengths` | `from`..`to` characters of `fill` |
| `chars` | single characters from code point `from` to `to` |
| `month_ends` | the last day of each month of `years` |
| `dates` | `from`..`to`, `step_days` |
| `int_limits` | the Integer limits |

Each cell runs through `Codeunit.Run` on a fresh instance of the generated codeunit, so errors are trapped and
the cell's database writes are rolled back. The result and the last error left by a cell that succeeded are kept.
`GlobalLanguage` and `WorkDate` are restored after each cell. The `selftest` spec checks all of this; run it after
changing the harness.

## Hand-written probes

A single `.al` file or a directory of `.al` files plus layouts (`.rdlc`, `.docx`, `.xlsx`). Give the probe id
as `ID=PATH` or with a `// probe-id:` header. Other headers:
- `// area:`
- `// title:`
- `// contexts: a=1; b=2`
- `// background: true`
- `// extract: script.py`

All objects (tables, reports, codeunits, ...) are renumbered into the app's range. Layout paths are rewritten to
`res/<probe id>/`. Do not refer to objects by number. The entry codeunit has one of these contracts:

| Contract | Signature | How it runs |
|---|---|---|
| drive | `procedure Drive(var Log: Codeunit "BCR Probe Log")` | called once; the probe logs cells itself and can honour `--steps` with `Log.Selected(StepName)` |
| take | `procedure RunAll(): Text` plus `Take()` and public parameterless `P*` procedures | each `P*` is one step: its own `Codeunit.Run`, rolled back, output from `Take()` logged and committed; `--steps P03_X,P04_Y` runs only those |
| text | `procedure RunAll(): Text` | one JSON object per line |
| array | `procedure RunAll(var Results: JsonArray)` | one element per cell |

`BCR Probe Log` API for drive probes:
- `Cell(Input: JsonObject; Ok; Result)` and `CellOut(...; Out: JsonObject)`: take the error text and code from
  `GetLastErrorText`/`GetLastErrorCode`, then clear them.
- `CellFull(...)`: takes them explicitly.
- `Note(Text)`.
- `Flush()`: commits.
- `Selected(StepName)`: false when `--steps` was given and does not name this step. A name ending in `*` selects by
  prefix (`P1-*`).
- `AddFile(Name; var InStream)`: stores a binary output, such as a workbook, for the next logged cell.

Call `Flush` and `AddFile` outside TryFunctions and cells.

Lines of take, text and array probes become cells: the probe's keys other than `result`, `error`, `code`, `ok` and
`out` form `in`. `ok` is the line's own `ok`, or else "error is empty".

Inputs: files in a probe directory's `inputs/` folder (for example workbooks for a reader probe) are packaged as
app resources. Read them in AL with `NavApp.GetResource('<probe id>/<path under inputs>', InStream)`. Their
SHA-256 values go into the run's `.meta.json`.

`extract`: when a probe directory holds `extract.py` (or names a script), the runner calls
`python3 -I <script> <run.jsonl> <files dir>` after the run. It archives stdout (one JSON object per row) as
`<ts>.rows.jsonl`, and `vectors` then uses those rows. Report probes use this to turn each workbook into one row per
cell.

## Output

Raw output stays outside the repository, in `$PROBES_EVIDENCE/<probe id>/<UTC timestamp>.*`:

| File | Content |
|---|---|
| `.jsonl` | the lines (see below) |
| `.al` | the probe's AL source as compiled (all files of a hand-written probe) |
| `.toml` | the grid spec |
| `.res.*` | layouts |
| `.files/` | binary outputs |
| `.rows.jsonl` | extract output |
| `.meta.json` | BC version, app version and source SHA-256, probe source SHA-256, contexts, steps, timings, notes |
| `.sha256` | `sha256sum -c` format for all of the above |

Line shapes (one JSON object per line):

```
{"probe","ctx","meta":{run, cells, user, company, globalLanguage, windowsLanguage, clientType, formatDecimal, formatDate, ...}}
{"probe","ctx","cell","in":{...},"ok","result","error","code"[,"out":{...}][,"files":[...]]}
{"probe","ctx","note"}                       step timings, failed steps with call stack, unknown probe ids
```

## Vectors (`tests/vectors/<area>/<probe id>.tsv`)

`probes.py vectors` compiles the archived runs of a probe into one file that can be committed and that bc-rust tests
load as test vectors. Raw runs are never committed.

- Runs: every run with the newest run's source: the probe's AL source, its layout and input files (SHA-256 each)
  and the BC version. Runs of an older source or another BC version are ignored, so a changed probe replaces its
  vectors. Partial runs (`--steps`) of the same source are merged.
- Probes with an extract script: rows from the runs whose rows came from the newest extract script.
- Rows: one per (context, input). Identical repeats are merged. If an input gives different results in different
  runs, `vectors` stops and lists them (`--allow-conflicts` keeps the newest).
- Order: by context, then by the cell's grid index. The order is stable for a given probe source.
- Header lines start with `#`: probe, area, title, BC version, probe source SHA-256, run dates, each context's
  session facts (language, region formats), row count.
- Columns: `ctx.<key>...`, `in.<key>...`, `ok`, `result`, `code`, `error`, `out.<key>...`. Out keys named `ms`
  or ending in `_ms` are timings: they stay in the raw runs and are left out of vectors and conflict checks. Probes with an extract
  script get one column per row key instead.
- Escapes: `\\`, `\t`, `\n`, `\r`, `\xNN`. An empty field is an empty text.

Error texts make up most of the size. A file of several MB packs to about 4% in git, and stays diffable as text.
