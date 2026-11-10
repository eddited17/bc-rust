# Architecture

How bc-rust is built: the crates, how storage maps to PostgreSQL, and the two execution engines. Moved here unchanged from the README on 2026-11-09; the BC sources under `vendor/` are fetched as the README's "Running it" section shows.

```
crates/
  al-syntax   lexer with preprocessor (#if/#else), recursive-descent parser, AST
  al-model    catalog: tables, fields, keys, FlowField formulas, pages, enums, events, extensions merged
  bc-store    storage engine (replaces SQL Server)
  al-runtime  AL interpreter: values, records, filters, triggers, events, builtins
  bc-server   `bc` binary: HTTP API, demo seeding, benchmarks; `al-test` (Microsoft's test codeunits), `demo-data` (Contoso)
  bc-nst      the service tier's client protocol, so Microsoft's web server and client run on bc-rust
web/          bc-rust's own web client (plain JS, embedded in the binary)
tools/        msweb-linux (Microsoft's web server on Linux), side-by-side benchmarks, platform table generator
platform/     platform system tables (Company, User, Field, AllObj, Integer, ...) written in AL
vendor/       BC sources, not committed (see below)
```

Storage sits behind a `Backend`/`Conn` trait in bc-store (`backend.rs`). Each session holds one connection.

PostgreSQL (`pg.rs`) is the system of record. Each company gets a schema (`c_cronus`), plus `shared` for tables with DataPerCompany = false. Each BC table becomes `t<id>` with one column `f<field id>` per field, and `COMMENT ON TABLE` carries the BC table name. Text and Code use an ICU case-insensitive collation, so key uniqueness and sort order match BC's SQL collation. Dates follow BC's SQL convention: closing dates are 23:59:59 and 0D is 1753-01-01. Tables and indexes are created on first use, and new fields from extensions become `ALTER TABLE ... ADD COLUMN`. The mapping from AL calls to SQL is direct:
- `SetRange`/`SetFilter` become WHERE clauses.
- `Next()` uses keyset pagination (`(k1, k2) > ($1, $2) ORDER BY k1, k2`) and prefetches 50 rows the way BC's server does.
- FlowFields and `CalcSums` become `SUM`/`COUNT`/`EXISTS` queries.
- `LockTable` makes later reads `FOR UPDATE`.
- `if Codeunit.Run(...) then` uses savepoints.
- `NumberSequence` maps to native sequences.

The embedded engine (`store.rs`, `MemBackend`) is kept for tests and benchmarks. It keeps each table in an ordered B-tree keyed by a memcomparable encoding of the primary key, serializes writers, and writes a WAL with fdatasync per commit.

The compiled engine (the default; crates/al-runtime/src/compiled.rs) turns a procedure into a tree of closures on its first call, with identifiers, calls, members of the implicit Rec and loops resolved once. Anything it doesn't compile falls back to the tree-walker, which walks the AST directly; `BC_ENGINE=tree` runs everything on the tree-walker. See docs/engine.md. Records hold a row of store values plus filters, the current key and an optional temporary table. Table globals are per record variable, as in BC. Calls pick overloads by argument type, `var` parameters pass by reference (including record fields), `[TryFunction]` turns errors into `false` when its result is used, and integration events dispatch to subscribers with parameters matched by name.
