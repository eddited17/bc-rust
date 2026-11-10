# wirevocab: the names on the wire, and bc-nst's names that aren't there

bc-nst's knowledge of the protocol between Microsoft's web server and a BC service tier may come only from
watching that protocol run against a real BC service tier. This tool makes that checkable.

Pass 1 reads recordings of Microsoft's unmodified web client against the real BC 29.1 service tier and writes
every name it finds into `docs/wire-vocab.tsv`, each with the record where it first appears. Pass 2 reads every
string literal in `crates/bc-nst/src` and lists the ones the vocabulary doesn't contain: the names bc-nst can
send or accept that no real recording shows.

```sh
python3 -I tools/wirevocab/wirevocab.py          # both passes, about 60 s
python3 -I tools/wirevocab/wirevocab.py --quiet  # same, without printing the summary
```

One run rewrites three files. `$BC_RUST_VOLUME` is the base for work files outside the repository (default
`/media/eddited/Volume`):

| File | What |
|---|---|
| `docs/wire-vocab.tsv` | the vocabulary (in the repository) |
| `$BC_RUST_VOLUME/cleanroom/wirevocab/unseen.tsv` | bc-nst literals outside the vocabulary (`--unseen`) |
| `$BC_RUST_VOLUME/cleanroom/wirevocab/summary.md` | counts, unseen wire candidates grouped by feature (`--summary`) |

Python 3 standard library only. `--captures` (root `captures`, default `data/capture`), `--corpus` (root
`cleanroom`, default `$BC_RUST_VOLUME/cleanroom/captures`, the clean-room capture campaign), `--sets`,
`--vocab` and `--repo` move the inputs and outputs. The output order is stable: the same inputs give
byte-identical files.

## Inputs

**Capture sets** are listed in `sets.tsv` with the layer they record, a provenance rule and a root
(`captures` or `cleanroom`, names only, so the file holds no host paths). A set with no matching files stops
the run. The tool refuses
to run when a set breaks its rule: an nst-proxy log must name the real NST (`127.0.0.1:7085` in the
bc-sidebyside VM) as its upstream, and a browser capture may only load pages from the web server the set
names (`127.0.0.1:8180` is the web server inside the BC container, `127.0.0.1:8190` is the Linux-hosted web
server in front of that container's NST).

| Set | Layer | Files | Records |
|---|---|---|---|
| `nst-real` | web server to NST | `nst-real/nst.jsonl`: sign-in, page 22, customer card 21 (edit, validation error, revert), page 42, page 99999 | 587 |
| `nst-real-prefix` | web server to NST | `nst-real/pre-fix/nst.jsonl`: the same scenario's first recording, which stops at the role center | 82 |
| `real-29.1` | browser to web server | `real-29.1/frames.jsonl`: role center, pages 22, 21, 42 | 210 |
| `real-29.1-error` | browser to web server | `real-29.1-error/frames.jsonl`: page 99999 | 86 |
| `linux-webserver-real-nst` | browser to web server | the first runs, `tz-utc/`, `run2/cold`, `run2/warm-1` to `warm-6` | 1552 |
| `linux-webserver-real-nst-windows` | browser to web server | `run2/windows-8180-1` to `-6`, the container's own web server | 1234 |

bc-nst speaks the web-server-to-NST layer, so `layers` = `nst` is the stronger evidence for a name bc-nst
sends. The browser layer is Microsoft's web server talking to Microsoft's browser client. bc-nst never speaks
it, but many names cross both layers.

Not used, because the other end was bc-nst or no client traffic was recorded:

- `data/capture/bc-nst-webclient`, and the nst-proxy logs in `/media/eddited/Volume/r1/proxy1` and
  `proxy2` (their upstream is a dev bc-nst on `127.0.0.1:7188` and `:7189`), and the e4 frames under
  `cleanup-candidates`: web client against bc-nst.
- `redeploy-*`, `hotfix-*`, `standard-apps-trial-*`, `cronus-live-*`: bc-rust deploy checks.
- `sidebyside-*`, `oracle-*`, `rdlc-excel-bc`: API, posting and report measurements, no client protocol.
- `crates/bc-nst/tests/golden/*.json`: git ignores them and nothing records how they were made.
- `notes.md`, `samples/`, `calls.txt` and the web server logs inside the used sets: our own write-ups and
  Microsoft's server log, not the traffic.

**BCApps AL** in `vendor/bcapps` and `platform/` (MIT) tells pass 2 which literals are AL words: property
names and values, trigger, event and procedure names, data types, `area(...)` names and the AL keywords.

**The audit's lists** `/media/eddited/Volume/legal/work/out/bcnst_tokens*.txt` feed one cross-check section
of the summary and nothing else (`--audit`).

## docs/wire-vocab.tsv

Columns: `name`, `kind`, `scenario`, `step`, `layers`. `scenario` is the first record that shows the name:
the capture file under `data/capture` plus `#` and the record's `seq` (nst-proxy logs) or line number
(browser captures). `step` is the driver step running at that moment. `layers` lists every layer the name
occurs on (`nst`, `browser`).

| kind | What |
|---|---|
| `key` | a JSON object key. Keys with digits that are not identifiers (`1019255337_c27`) only count as shapes |
| `key-shape` | a key with digit runs and GUIDs replaced by `#` (`PP_#`, `#_c#`), when three or more different keys have it |
| `method` | a JSON-RPC `method`, an `interactionName`, a `MethodName` |
| `type` | a `$polyType`, `$type`, `handlerType` or `ClrType` value, or another dotted `Microsoft.Dynamics.`/`DN.` type string |
| `type-short` | the last segment of a type (`ActionDefinition`) |
| `assembly` | the assembly part of an assembly-qualified type string |
| `enum` | an identifier-like value of a key that has at most 64 distinct values, at least 80% of them identifiers or numbers, and whose name doesn't suggest data (name, caption, value, id, filter, allowed, ...) |
| `id-shape` | a generated id as a shape, when three or more ids have it: digit runs of three or more become `#` (`Control#_Format`), a prefixed number written twice becomes `p#p#*` (`p21p21StyleTxt`). Generated ids never appear as `enum` |
| `path`, `path-segment` | an HTTP or WebSocket path with numbers and GUIDs as `#`, and its name segments |
| `query` | a query parameter name |
| `header` | an HTTP header name |

Nested JSON is unpacked wherever it appears: JSON in a string, base64 of JSON, gzip or zlib inside base64
(`compressedResult`, `namedParameters`, `exceptionData`). Binary data sets are read for strings with the
length prefix .NET's `BinaryWriter` writes (dotnet/runtime), with no other knowledge of their layout. Without
the layout a column name and a record value look the same, so these strings stay in memory for pass 2 and
none of them reaches the vocabulary. Stream G decodes the layout.

What the file never holds: header values, request bodies (the sign-in form is never parsed), any value under
a key that looks like a credential (token, password, cookie, csrf, authorization, ...), GUIDs, record data,
captions and texts, anything from a binary data set. GUIDs seen in the captures stay in memory so pass 2
can check GUID constants.

## unseen.tsv

Columns: `file`, `line`, `literal`, `category`, `feature`, `context` (the source line, cut at 160 characters).
One row per occurrence. Comments are skipped. A literal that is in the vocabulary, or a format string whose
shape or some name fits it, is dropped. So is a literal without letters.

Wire candidates, the list stream F tries to provoke on real BC:

| category | What |
|---|---|
| `wire-emit` | a name bc-nst writes into a message: `json!`, `insert`, `El::new`/`put`, a match arm's result, a call argument |
| `wire-dispatch` | a match-arm pattern: a method, interaction or value name bc-nst accepts |
| `wire-read` | a name bc-nst looks up in a received message |
| `wire-param` | a label in `p.get(i, "name")`. Every captured call sends its params as an array, so the name only travels if a client sends named params |
| `wire-type` | a dotted .NET type name |
| `wire-serde` | a `#[serde(rename/alias = ...)]` name |
| `wire-format` | a format string that builds a name and fits no seen name or shape |
| `wire-path` | an HTTP route, URL path or JSON pointer with an unseen segment |
| `wire-guid` | a GUID constant that no capture shows and no BCApps `app.json` names |
| `wire-other` | a name-like literal in no recognised context |
| `case-only` | seen, but only with other letter case |
| `dataset-only` | found only as a string inside a binary data set (four letters or more), where names and record values can't be told apart yet |

Noise: `al-unused` (a `("Name", number)` table entry that BCApps never uses in AL; not a wire name, but it
needs a source), `al`, `text` (sentences: UI and error texts), `test`, `log`, `panic`, `env`, `cli`, `own`
(bc-rust's hash seeds, `$slot`-style markers, file names), `guid-bcapps`, `attr`, `sql`, `other`.

`feature` groups names by keyword (lookup, drill-down, rename, run-modal, dialogs, filter/view, errors,
callbacks, ...) and falls back to the source area (metadata, dataset, forms).

## Limits

The categories come from regular expressions over one source line, not from type information. Expect a few
wrong ones: an AL word that bc-nst also sends ends up in `al`, a default value (`"admin"`) in `wire-emit`.
The enum rule trades recall for keeping data out of a public file, so some real enum values stay out of the
vocabulary and show up as unseen. The vocabulary only covers what the recorded scenarios exercised. Nobody
has recorded a lookup, drill-down, assist-edit, rename, delete, insert, confirm or message dialog, run-modal
page or upload yet, so their names are missing. New capture sets go into `sets.tsv`, and one run regenerates
everything.
