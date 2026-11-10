# Sources

bc-rust reimplements the behaviour of Business Central. This file says where that behaviour may come from. [docs/sources.md](docs/sources.md) is the ledger: one row per behaviour area, with its evidence.

## Evidence tiers

Use the first tier that settles the question.

| Tier | Source |
|---|---|
| T1 | Microsoft's BCApps and System.app AL (MIT), tests included |
| T2 | Microsoft Learn, ECMA-376, and open-source code read as source: dotnet/runtime, vs-streamjsonrpc, Newtonsoft.Json, OData |
| T3 | A test in aldevtools' AL Language.Tests that its CI runs on a real BC. Cite it, copy nothing until its MIT licence is in place |
| T4 | Our own probe run on a licensed BC. The probe runner compiles the runs into a test-vector file under `tests/vectors/` (format in `tools/probes/README.md`). Raw probe output is not committed |
| T5 | A capture scenario: traffic between Microsoft's web client and a licensed BC |

When bc-rust chooses its own behaviour, the ledger row says `own`. The choice has to be made from public material alone, or be forced by a BCApps test that fails without it (then that test is the T1 evidence). Giving a new label to something learned from a banned source does not make it an own choice.

## Not allowed

- Decompiling, disassembling or byte-searching Microsoft binaries, for any purpose, interoperability included. That covers the service tier, the web server, the AL compiler and every DLL they ship.
- Copying text or code from material produced that way, even if someone else produced it.

Reading files that ship in plain form is fine: configuration files, `deps.json`, JavaScript, CSS and HTML.

## Text, names and trademarks

- Microsoft's wording appears verbatim only when a T1, T2, T4 or T5 reference shows it. Record the reference in the ledger.
- Business Central, Dynamics 365 and Microsoft are used only to describe compatibility. bc-rust's own UI carries no Microsoft branding.

## Contributors

If you have seen decompiled Business Central code, say so in your pull request. You may write probes, tests, docs and reviews, but not code that implements behaviour.

## A new behaviour question

1. Search T1, then T2.
2. Still open: look for an aldevtools test (T3).
3. Still open: write a probe that sweeps a grid of inputs (ranges, lengths, every sign and unit) rather than the value you expect, run it on a licensed BC, and add the resulting vector file.
4. Add or update the row in `docs/sources.md`: ID, area, tier, reference, status, date.
5. Implement it, and put `[src:<ID>]` in a comment next to the code.

If no allowed source settles the question, write it down in [docs/open-questions.md](docs/open-questions.md). The behaviour then stays unimplemented, or it is flagged as a guess through the assume registry ([docs/assumptions.md](docs/assumptions.md)).

## Checks

`tools/legal/guard.sh` runs in CI. It looks for decompiler names and local tool paths, internal Microsoft class names that no public source declares, binaries and resource files in the tree, and `[src:...]` tags without a ledger row. For now it only reports. Run it before you push: `tools/legal/guard.sh`.

Only `main` is pushed. `tools/legal/pre-push` is a hook that refuses every other ref; install it with `cp tools/legal/pre-push .git/hooks/pre-push`.
