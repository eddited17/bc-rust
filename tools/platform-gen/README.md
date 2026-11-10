# platform-gen

Generates bc-rust's system tables and trigger codeunits in `platform/` from Microsoft's own AL source. Every BC platform artifact contains `System.app`, the symbol package of the system objects. Its `src/` folder holds the AL source of all system tables, enums, interfaces, codeunits, pages and reports (MIT license). `gen.py` copies the ones bc-rust needs and leaves out only the `///` doc comments. `docs/platform-tables.md` says why the files look the way they do.

```sh
CARGO_TARGET_DIR=/tmp/cargo-h python3 -I tools/platform-gen/gen.py \
    --app <artifacts>/29.1.57326.57497/platform.zip
```

## Inputs

- `--app` is the BC platform artifact `platform.zip` or a `System.app` file taken from it. In the zip it is `ModernDev/PFiles/Microsoft Dynamics NAV/<nnn>/AL Development Environment/System.app`.
- `--bc-build` is the BC build written into the file headers. It defaults to the `a.b.c.d` folder name in the `--app` path, so you only need it for a bare `System.app`.
- `--repo` is the checkout to write into. It defaults to this one. The AL source that decides what gets included is whatever bc-rust loads, which is `al_model::default_app_dirs` (BCApps under `vendor/bcapps`) without `platform/`.
- `--missing` prints the System.app tables that were left out as markdown table rows, for the list in `docs/platform-tables.md`.

You need `python3` and `cargo`. `gen.py` runs `probe/`, a small Rust program that loads AL with bc-rust's own parser (`crates/al-model`). It is a standalone Cargo package with its own `[workspace]` and lockfile, built with `--offline` into `$CARGO_TARGET_DIR`. Building it in the same target dir as the workspace reuses the workspace's compiled `al-model`.

## What it does

1. Unpacks `System.app` (a 40-byte NAVX header, then a zip) into a temporary directory and indexes its tables, enums, interfaces, codeunits, pages and reports.
2. Selects objects. It starts from the tables listed in `ALWAYS` in `gen.py` (the ones bc-rust's Rust code fills, computes or serves) and every System.app table the loaded AL names without defining: Record types, TableRelation, CalcFormula, page SourceTable, `Database::`, report, query and xmlport dataitems. It adds System.app enums and interfaces used as types, in `extends` or in `implements`, and codeunits, pages and reports used as types or in `Codeunit::`, `Page::` and `Report::` constants. Every `[EventSubscriber]` names its publisher that way. Then it follows the relations, CalcFormulas and enum fields of the selected tables, the interfaces of selected enums, the Record and Enum types in interface signatures, and the types, object constants and SourceTable that selected codeunits, pages and reports name.

   Two kinds of codeunit, page or report stay out, and the run names them under "left out". The first is one with a `[Native]` procedure. The service tier implements those, System.app's AL has empty bodies, and bc-rust's runtime handles a codeunit it has no AL for as a platform object (`al-runtime` `dotnet::platform_codeunit` implements Base64Convert). The second is one whose kind and name the app source already defines. Event-only codeunits get a comment that points to "Platform events" in `docs/platform-tables.md`.
3. Writes `platform/SystemTables.al` (the header plus namespace `System.Reflection`) and one `platform/system/<namespace>.al` per other namespace. AL allows one namespace per file. It deletes every other `.al` file in `platform/system/`. An object that a hand-written `platform/*.al` file defines is skipped, and a message names the file.
4. Checks the result. It loads the full app set with the probe and compares every generated table (fields, types, options, classes, relations, keys) and every generated codeunit, page or report (properties, globals, procedures with attributes, signatures, locals and bodies, without line numbers) with the same dump of System.app. Then it prints the catalog's load statistics, the subscribers to unknown publishers and the table names still unresolved. It exits non-zero if any object differs.

A second run over its own output changes nothing.

## A new BC build

1. Run `gen.py` with the new `platform.zip`.
2. Look at `git diff platform/`. Table ids, field numbers and keys that changed are layout changes. Existing PostgreSQL databases keep the old columns and primary keys, because bc-store only ever adds columns. `docs/platform-tables.md` explains how to handle that.
3. Update `docs/platform-tables.md`: the version under "Source", the migration notes, the output of `--missing` and the event list under "Platform events".
4. Run `UNKNOWN=1 alstats` from the repo root (`cargo build --release -p al-model --bin alstats`) and `cargo test --release -p bc-server -p bc-store`.

If a newer BCApps references a system table that the platform build does not have, the table stays unresolved. The run prints it under "unknown table names left". MCP Configuration Agent is one today.
