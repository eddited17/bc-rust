# Execution engine: measurement, design and first prototype

Status as of 2026-11-08 (P12, rounds 10–12).
- **Landed:**
  - the measurement method (`bc sales` BENCH output and `tools/bench/cpu-ab.py`);
  - the compiled engine behind `BC_ENGINE=compiled`, off by default: calls, members, field access and assignment, loops, case and exit (round 11, -7.3% / -7.6%);
  - cheaper frame setup for both engines (round 12, crates/al-runtime/src/frames.rs).
- **Default since 2026-11-08 06:25 UTC (I8):** the compiled engine. `BC_ENGINE=tree` selects the tree-walker, the escape hatch; `BC_ENGINE=lookup` is a diagnostic (find the compiled body, then run the tree-walker). An empty or unset `BC_ENGINE` means compiled, so scripts that want the tree-walker must say `BC_ENGINE=tree`. Evidence: the full w1demo baseapp comparison and the five-run recheck (data/capture/standard-apps-trial-2026-11-07/round13/notes.md).
- **Evidence:** data/capture/standard-apps-trial-2026-11-07/round5 (profile), round10, round11 and round12.

## Where the time goes

Profile of the in-memory posting loop on `standard-prod` (gdb stack sampler, round 5):

- About 97% of the time is the tree-walker interpreting AL, spread over about 3,000 procedures with no single hot spot. 67% of it is reached through event subscribers.
- Name resolution at run time, on every execution:
  - variables: `find_var` is a case-insensitive linear scan of locals and globals (6.2%; 41 name comparisons per global lookup on average);
  - procedures: `resolve_call` looks the name up and scores overloads (8% inclusive, mostly argument evaluation);
  - builtins and record methods: dispatched by matching lower-cased method names in large `match` blocks;
  - fields: looked up by name in a hash map.
- Values: every variable read clones the `Val` (strings, decimals) out of an `Rc<RefCell<Val>>` slot. Decimal and date literals are re-parsed on every evaluation, and text literals re-allocated.
- Allocation: about 20% of leaf samples are malloc, free or memcpy, from record copies (`copy_record` 9%), creating all globals of an instance (5.5%) and new rows.

Micro-optimizations of single items (event-key prefilter, lazy globals, cached variable lookup) each land at 1–5%, at or below the noise of this host. The structural lever is to resolve the AST once.

## Measuring on a busy host

`BENCH=1 POST=<n> bc sales --memory [--apps <set>]` prints one line per process:
- `cpu_us_*`: thread CPU time per order. It's per-thread (`CLOCK_THREAD_CPUTIME_ID`, Unix), so time spent waiting for a CPU doesn't count.
- `steps_*`: AL steps per order.
- `ns_per_step_min/p10/median`: CPU time per AL step over the steady-state orders. Orders alternate between three customers with different step counts, so per-step cost is the comparable unit. Interference on a shared host (other processes on the same core, caches) only ever adds time, so the 10th percentile is a robust estimate of the code's own cost.

`tools/bench/cpu-ab.py A=<bin>[:ENV=V] B=<bin>[:ENV=V] --blocks N --orders 30 [--apps standard-prod]` runs N rounds:
- each round is one block per side, back to back, in a random order;
- it reports the median of the per-round B/A ratios (drift in load between rounds cancels), a bootstrap 90% interval over rounds, and whether both sides ran the same AL steps.

Validation (2026-11-08): A against an identical copy of A, under a steady load around 18, measured -1.0% (-3.7% .. +1.8%). When the load jumps within seconds, the interval widens to +-10–20% and says so; decide only on intervals that exclude 0, and repeat. Running A and B at the same time was tried and rejected: identical binaries then measured -15%.

The AL step count is the deterministic work measure (identical by design). Allocations per order would be a further proxy, but a counting allocator adds an atomic increment to every allocation of the shipping binary, so it isn't in.

## Design: resolve each procedure once

### What a compiled procedure is

The input is the procedure's AST (`&'static Procedure` from the catalog, which lives for the whole run), the layout of its frame (parameters, locals, return value) and its object's globals. From those it is compiled once per thread, on first execution, into an executable tree:

| AST | compiled form |
|---|---|
| identifier | `Local(i)` / `Global(i)` / `RecField(fi)` / `WithField(depth, fi)` / `Rec` / `xRec`, resolved with exactly the tree-walker's precedence (locals, globals, `with` records, the implicit Rec's fields, special names) |
| call of a procedure | `Proc { code, proc }` when the overload is decided statically (one candidate of that arity, or declared argument types); otherwise the dynamic `resolve_call` |
| builtin, record method, static class method | an id resolved from the name once, then a direct call into the existing implementation |
| `Rec.Field`, `Var."Field"` on a Record variable | the field index, from the variable's declared table |
| literals | constants (decimals and dates parsed once) |
| `+ - * /`, compares | typed fast paths for Integer, Decimal, Boolean and Code (with Code's right-aligned digit comparison), else the generic `binop` |

### Closures or bytecode

- **Closure compilation** (each node becomes a boxed `Fn(&mut Session, &mut Frame) -> R<Val>`, the prototype's form):
  - it can be adopted incrementally, any node can fall back to the tree-walker, and it reuses the interpreter's functions, so semantics stay identical by construction;
  - its costs are one indirect call per node and scattered small allocations.
- **Bytecode** (a register VM with a dense instruction array and one dispatch loop):
  - better cache behaviour and no per-node allocation;
  - but it needs every statement kind and all control flow (Exit, Break, error unwinding, TryFunction, asserterror) before it runs anything real. A fallback means an opcode that calls the tree-walker on a subtree, with variables synchronised in both directions.
- **Recommendation:** closures first, starting with the nodes where the time is: calls, member access, variable access. Revisit bytecode only if per-node dispatch shows up as the next bottleneck.

### Living with the tree-walker

- **Granularity:** anything not compiled is evaluated by the tree-walker for that node (expressions) or statement. A procedure can be all tree-walker.
- **Shared state:** both work on the same `Frame` (locals as `Vec<Var>` with names), so mixing needs no state copying. A slot-only frame without names comes later, once a procedure is compiled completely.
- **Cache:** compiled bodies are keyed by (catalog, code, procedure) per thread and never invalidated (the catalog is immutable).
- **Layout check:** a variable index taken from the first frame is checked against the name on use, and anything unexpected goes back to the tree-walker.

### Keeping AL semantics identical

- **Steps:** each compiled statement counts one step with the same step-limit check as `exec`; statements handed to the tree-walker count their own. Gate: step counts identical in `bc sales`, the flows test and al-test slices.
- **Errors:** the same error constructors, the same line attribution (`at_line`) and the same `Flow` values for Exit, Break, Continue and errors. Collectible errors, TryFunction and asserterror stay on the invoke path.
- **Events:** calls go through `invoke`, which raises integration events and runs subscribers and triggers, so publisher detection and subscriber order don't change.
- **Evaluation order:** operands are evaluated in the tree-walker's order. Only literals are folded; there is no algebraic rewriting.
- **Gates for each step:**
  - `bc sales` steps identical on both app sets;
  - cargo tests, including flows with `BC_ENGINE=compiled`;
  - al-test ERM-Sales + SMB with `--jobs 1` on both datasets: outcomes, steps and messages identical;
  - `tools/bench/cpu-ab.py` for the gain.

## Prototype (BC_ENGINE=compiled; crates/al-runtime/src/compiled.rs)

- **Compiled:**
  - statements: Block, If, and assignment to a local or global variable (through `lvalue` semantics: `read_lval`, `binop`, `write_lval`);
  - expressions: literals, local and global variable reads, `and`/`or` with the tree-walker's short-circuit, other binary operators through `binop`, and the ternary.
- **Left to the tree-walker:** everything else per node, including Code comparisons and `in`.

Measured on the posting loop (default apps, 30 orders per block):

| variant | vs tree-walker (median of paired rounds, 90% interval) |
|---|---|
| compiled, body cache with SipHash and an `Rc` clone per call | +7.5% (+5.0 .. +9.8); standard-prod +7.9% (+5.9 .. +10.1) |
| `BC_ENGINE=lookup` (find the compiled body, then run the tree-walker) | -1.2% (-4.5 .. +4.4): dispatch overhead is negligible |
| compiled, cheap hasher and leaked bodies | +3.2% (+0.2 .. +6.1); repeated with 16 rounds: +4.9% (-0.9 .. +8.3) |

Steps were identical in every run (35,310 per order on the default apps, 36,558 on standard-prod).

Lesson: compiling only the leaves (variables, literals, assignments) while calls and member access go back to the tree-walker adds an indirect call and a closure per node, and that costs about as much as it saves. In the profile the time is in calls, member access and name-based dispatch, so the next step has to compile those.

## Round 11: calls, members, loops (2026-11-08)

Each step was measured against the tree-walker on the same binary (`cpu-ab.py tree=bc compiled=bc:BC_ENGINE=compiled`, ns/step p10, paired rounds), with steps identical in every run:

| step | default apps | standard-prod |
|---|---|---|
| 1. expression statements; calls with one candidate pre-resolved; implicit-Rec method / builtin dispatch decided once; `RecVar.Method(..)` and `RecVar.Field` by index | +1.6% (-1.8 .. +2.6) | +0.2% (-3.0 .. +1.9) |
| 2. + `case`, `while`, `repeat`, `for`, `exit`, `break`, `continue` | +0.5% (-5.8 .. +2.3) | -4.5% (-5.4 .. -2.7) |
| 3. + field assignment (`RecVar.Field := ..`, `Field := ..` on Rec) | -4.6% (-6.6 .. +2.9) | -1.4% (-9.7 .. +5.6) |
| 4. + `Rec.Method(..)` / `Rec.Field` on the implicit Rec | -7.1% (-11.8 .. +1.9) | -1.0% (-8.8 .. +8.9) |
| cumulative 1–4, 20 rounds | **-7.3% (-11.3 .. -3.9)** | **-7.6% (-11.8 .. -1.1)** |

Behaviour: the al-test slice ERM-Sales + SMB (2,461 tests per dataset, `--jobs 4`, the same binary with and without the flag) has identical outcomes on w1demo and contoso for steps 1–2 and step 3. Steps are identical except tests that depend on the clock or on test order: O365 Test Name Nearness (TestPurchaseLine/TestSalesLine) and once O365 Role Center Notifications TestTrialNoNotification (68 → 72). That one reproduced as 68 in both modes, twice each, when run alone with `--jobs 1`.

Tried and not landed:
- `CUVar.Proc(..)` on codeunit variables pre-resolved: -0.3% / -0.0% (intervals ±6%), no measurable gain.
- `Var.name` as `&'static str` (no name copy per local per call): -2.9% (-5.6 .. +2.8) / -0.1%, no measurable gain, and a change to a struct shared with P7.

Where the compiled run spends its time now (gdb samples, standard-prod, step 3):
- 39%: in fallback nodes, mostly member calls on bases that aren't a declared Record variable (step 4 then took the implicit Rec).
- 27%: in `record_method` itself, i.e. the record operations (filters, store, triggers), which compilation doesn't change.
- 14%: in `invoke`'s frame setup (default values for locals, globals of new instances, events).

## Round 12: `and`/`or` semantics, cheaper frames (2026-11-08)

`and` / `or`: AL evaluates both operands, left to right, without short-circuiting. Posting relies on it (Item Jnl.-Post Line: `X := X or SetupTempSplitItemJnlLine(..)`). The tree-walker does this since P11's fix (03:09 UTC). compiled.rs now does the same. crates/al-runtime/tests/p12_engine_and_or.rs runs one AL procedure under `BC_ENGINE=tree` and `BC_ENGINE=compiled`, one child process each, because the engine is chosen once per process. The procedure has side-effecting right operands in assignments, `if` and `while`. With the old compiled.rs the test fails (`1|d|..` instead of `8|abcdefgh|..`).

Cheaper frames (crates/al-runtime/src/frames.rs). These apply to both engines and change no results:
- `invoke` checks event attributes with `eq_ignore_ascii_case`, instead of lower-casing every attribute name into a new String on every call;
- `default_val` and `coerce` lower-case the type name on the stack (`frames::Lower`). This covers every local's default value and every coerce, including parameters and typed assignments;
- a Record type's table id is resolved once per type node, keyed by the node's `&'static` address (`frames::table_of`);
- a new record's initial row (`new_row` + `init_row`) is built once per (catalog, table, work date) and then cloned (`frames::initial_row`). `init_row` depends only on the field definitions, the catalog's enums and the work date, and store values are plain data.

| run (paired rounds, ns/step p10) | result (90% interval) |
|---|---|
| default apps, compiled engine, 16 rounds (binaries from before the `and`/`or` fix) | -9.5% (-34.2 .. -3.0) |
| standard-prod, compiled engine, 16 rounds | -5.3% (-7.7 .. -1.8) |
| default apps, compiled engine, 16 rounds | -1.9% (-8.3 .. -0.3) |
| default apps, tree-walker, 24 rounds | **-6.1% (-11.4 .. -2.2)** |
| default apps, compiled engine, 24 rounds | **-9.5% (-12.6 .. -3.7)** |

Steps were identical in every run (18,910 per order on the default apps, 20,189 on standard-prod), cargo test passed 134/0, and the flows test passed with `BC_ENGINE=compiled`. It landed at 03:32 UTC.

The engine alone, on the tree with the frames change and the `and`/`or` fix (`tree=bc compiled=bc:BC_ENGINE=compiled`):

| run | compiled vs tree (90% interval) |
|---|---|
| default apps, 24 rounds | -0.6% (-11.4 .. +1.9); unpaired -9.4% |
| default apps, 32 rounds | **-8.4% (-12.1 .. -2.7)** |
| standard-prod, 16 rounds | -5.3% (-10.6 .. +1.9) |

Combined, the compiled engine with frames against the tree-walker without them (the state before round 12): -7.0% (-11.3 .. -1.6) on the default apps over 24 rounds, and -6.0% (-14.3 .. +2.1) on standard-prod over 16 rounds. The host load was about 20 with five al-test runs, so single runs varied from -0.6% to -9.5%. Read the longer runs.

Rows filled on first access (round 13, landed at 05:36 UTC):
- `RecHandle` is `Rc<RecCell>` (crates/al-runtime/src/record.rs).
- `RecCell` wraps the `RefCell<RecState>` and keeps the table's shared initial row (`frames::initial_row`, now an `Rc<Row>`) until the first `borrow()` / `borrow_mut()`. That first access copies the row in.
- The state can't be reached any other way, so every reader sees the same values as before.
- A record that is never accessed never copies its row: most of a record's table globals, and many locals. `copy_record` of an untouched record stays untouched.
- Same API: `borrow`, `borrow_mut`, `Rc::downgrade`. A handle is built with `RecCell::new(state)` instead of `RefCell::new(state)`.

| run (paired rounds, ns/step p10) | result (90% interval) |
|---|---|
| default apps, compiled engine, 32 rounds | **-8.3% (-9.1 .. -3.3)** |
| default apps, tree-walker, 24 rounds | **-6.5% (-9.1 .. -4.9)** |
| standard-prod, compiled engine, 24 rounds | **-7.6% (-8.6 .. -4.8)** |

Steps were identical (19,739 per order on the default apps on both engines, 21,018 on standard-prod). cargo test passed 142/0, both on the snapshot and on the tree after landing, and the flows test passed with `BC_ENGINE=compiled`.

Per-variable laziness in slots was not used: about 30 places read a variable's slot directly, plus every `LVal::Slot` and by-reference path.

No name copy per call (round 13, landed at 07:19 UTC):
- `Var.name` is a `&'static str` taken from the catalog, so `invoke` no longer allocates a `Box<str>` per parameter and local on every call. The same applies to globals.
- Under mimalloc (round 11) it measured -1.8% with an interval that includes 0. With the system allocator, the default since the mimalloc crashes, the allocations cost more:
  - default apps: -5.9% (-9.5 .. -4.0) and -0.8% (-2.1 .. +2.7), 32 rounds each;
  - standard-prod: -6.5% (-9.1 .. -2.6), 24 rounds.
- Steps were identical, and cargo test passed 143/0 on the snapshot and 144/0 on the tree after landing.
- A `Var` built with a name that isn't `&'static` no longer compiles: take it from the catalog or use a literal.

Copy-on-write record rows (round 15, landed at 08:05 UTC):
- `RecState.row` is a `CowRow` (crates/al-runtime/src/record.rs), an `Arc<Row>`.
- Reads go through `Deref`. Any mutable access makes the row unique first (`Arc::make_mut`), so a holder never sees another holder's writes.
- These share one row until somebody writes it: record copies (by-value parameters, `A := B`, xRec), the xRec row stored on every load, the prefetch buffer, and a new record's initial row.
- Writes to temporary tables still copy, as before.
- Assign a `Row` to a record with `.row = row.into()` (a move, not a copy). Use `.to_vec()` where an owned `Row` is needed.

| run (paired rounds, ns/step p10) | result (90% interval) |
|---|---|
| default apps, compiled, 32 rounds | **-14.2% (-19.7 .. -9.6)** |
| default apps, tree-walker, 24 rounds | **-21.2% (-25.1 .. -19.0)** |
| standard-prod, compiled, 24 rounds | **-16.3% (-18.7 .. -13.9)** |
| standard-prod, tree-walker, 24 rounds | **-15.3% (-17.0 .. -12.3)** |

Steps were identical, and cargo test passed 144/0 on the snapshot and on the tree after landing. The al-test slice ERM-Sales + SMB + SCM-Warehouse (3,247 enabled tests per dataset) had identical outcomes on w1demo and contoso. The few step differences are in tests that vary between runs of the same binary (data/capture/standard-apps-trial-2026-11-07/round15/notes.md).

Slot-indexed variables and fewer fallbacks (round 16, landed at 09:05 UTC):
- **Variables by slot:** a compiled body records, when it compiles, every variable it resolved. That is the slot (local or global index) plus the address of the declaration's name, which is a catalog `&'static str` and so identifies the declaration. It also records the frame's shape: number of locals and globals, and the first global's declaration.
- **One check per call:** each call checks the layout once with pointer compares. Inside the body, variables are read and written by index with no name check. A frame with another layout runs the whole body on the tree-walker. None is expected, since `invoke` builds locals from the declaration.
- **Fewer fallbacks:**
  - a counting build showed `not x` was 30% and `X::Y` scope expressions 20% of the remaining fallbacks;
  - `not`, unary minus and plus are now compiled exactly like the tree-walker's Unary arm;
  - date and time literals are folded when the body compiles;
  - scope expressions are evaluated once by the tree-walker and then reused while the implicit Rec's table and xRec presence match. `eval_scope` depends on nothing else, given the checked layout. Only plain values (Int, Opt, Bool) are cached.

| run (compiled engine, paired rounds, ns/step p10) | result (90% interval) |
|---|---|
| default apps, 32 rounds | -4.3% (-8.3 .. +2.5); -6.1% (-10.2 .. -0.8); -3.6% (-8.9 .. -0.6) |
| standard-prod | -2.8% (-6.2 .. +0.9) over 24 rounds; -4.2% (-8.8 .. -1.1) over 32 rounds |
| tree-walker, default apps, 24 rounds (code unchanged) | +1.3% (-1.1 .. +4.2) |

- Slot access alone measured +0.8% / -1.5%, intervals including 0: the per-access name compare was not the cost. Reading the slot and cloning the value is.
- Steps were identical, and the al-test slice ERM-Sales + SMB + SCM-Warehouse had identical outcomes on both datasets.
- cargo test: 144/0 on the snapshot; on the tree, 143/0 for lib and integration plus 2/0 for al-test's unit tests.
- Host load during the runs was 30–40.

Compiled call arguments (round 17, landed at 11:07 UTC):
- For calls with one candidate, the compiled closure used to evaluate every argument with the tree-walker's `eval_arg`. For a variable argument that looks the name up twice (`find_var`, then `lvalue`) and clones the value.
- Now an identifier argument that resolved to a variable when the body compiled reads its slot directly, with `eval_arg`'s result.
- The parameter is known, so a `var` or DotNet parameter takes only the slot and a by-value parameter only the value. Other arguments still use `eval_arg`, in the same order.
- Results: default apps -8.8% (-13.0 .. -3.2) and -4.2% (-8.5 .. +2.1); standard-prod -2.0% (-6.3 .. +0.3) and -8.1% (-11.9 .. -4.0).
- Steps were identical, and the slice (ERM-Sales + SMB + SCM-Warehouse) had identical outcomes on both datasets. cargo test passed 147/0.

Shared label slots (round 18, landed at 11:58 UTC):
- `globals_for` used to give every new codeunit/page instance, and every record's table globals, its own copy of every label text. That was 2.6% of the time.
- Labels are constants, so within a session all instances now share one slot per Label/TextConst declaration (`frames::label_slot`, keyed by a real session id, `Session::id`).
- Session's Drop still voids every global slot as before and forgets the session's label slots, so behaviour across sessions is unchanged.
- Results: default apps -6.4% (-8.2 .. -2.0); standard-prod -5.9% (-8.1 .. +0.4) and -5.7% (-10.0 .. -0.4); tree-walker -4.8% (-8.9 .. -1.9). Steps identical, slice outcomes identical on both datasets, cargo test 149/0.
- The +72 steps seen in round 17 were nondeterminism in the baseline: the baseline binary itself reproduces the other value when rerun.

Fewer string compares (round 18, landed at 12:40 UTC):
- **Event publishers:** whether a publisher procedure has subscribers in the catalog is computed once per procedure (`frames::event_has_subscribers`). A publisher without subscribers returns at once, instead of building its argument list and lower-casing the event name for a lookup that finds nothing.
- **Type kinds:** the kind of a Simple type (Integer, Code, Text, ...) is decided once per type node (`frames::simple_kind`). default_val, coerce and coerce_param take a fast path for the common kinds; the rest falls through to the unchanged string match.
- **Layout check:** the compiled layout check compares the frame's shape only. The per-variable check remains as a debug assertion.
- Results: default apps -11.2% (-13.7 .. -4.4); standard-prod -3.5% (-6.1 .. -1.8); tree-walker -2.6% (-6.4 .. +1.5). Steps identical, slice outcomes identical on both datasets, cargo test 151/0.

Not landed in round 17 (data/capture/standard-apps-trial-2026-11-07/round17/notes.md):
- Compiling `in`, member access on codeunit, interface and Record variables, and argless `Is*` cut the fallbacks by 72%, but measured no gain (-1.5% to +5.8%): the removed fallbacks are cheap.
- Shared label slots (each instance copied every label text into its globals, 2.6% of the time) measured -5.4% / -3.3%. But two warehouse tests changed step counts when other tests ran first, with no label write found. Parked until the cause is known.

Frames without per-call allocations (round 14, not landed): reusing slots and the locals Vec from a per-thread pool (patch round2/patches/p12_slotpool.py).
- Results: -0.5% (-2.4 .. +3.7) on the default apps; standard-prod -1.4% (-4.5 .. +3.3) and -1.8% (-4.0 .. +0.6). Steps were identical.
- Why so little: glibc's per-thread cache already serves these small blocks cheaply.
- What frame teardown really costs: dropping the local values (7.3% of samples), mainly record rows (3.7%) and table globals (1.6%). A row of a wide table is about 12 KB, too large for that cache, so glibc frees it through its slow path.
- So a slot-only frame would save at most these 1–2%. The next levers are record copies and row copies (copy-on-write rows) and the allocator (data/capture/standard-apps-trial-2026-11-07/round14/notes.md). Also not done: `Var.name` as `&'static str` (a struct shared with P7; no measured gain alone in round 11).

## Switching the default (round 13, 2026-11-08)

The full w1demo baseapp suite ran on one binary under both engines; 42,098 of 42,146 tests were compared (the 44 left are
timer-based Test Job Queue / Test Background Posting tests).
- Raw differences: 1 outcome, 261 tests with different step counts.
- The 55 codeunits with differences (2,344 tests) were run again under tree, compiled and tree. Over those five runs:
  1,966 identical; 302 vary between tree runs and 65 between compiled runs (nondeterministic); 11 differ only between engines.
- Those 11 come from the tests themselves:
  - 5 CRM/Match tests differ by ±1 step (clock);
  - 6 SCM Supply Planning -II tests differ by 48 steps. `Library - Random::Init` seeds Random from the time of day (`SetSeed(Time - 000000T)`), and a per-call trace shows identical calls and steps until that point.
- The one outcome difference (Workflow E2E TestRejectingApprovalRequestForPurchInvWithLoopbackToEvent) also fails on the tree-walker in earlier runs, and passes on compiled in the recheck. Its test users are keyed by `CreateGuid()`.
- `and` / `or` evaluate both operands in both engines (tests/p12_engine_and_or.rs).

## Plan and estimate

1. **Calls:** `Proc { code, proc }` with static overload choice, builtins and record methods by id, arguments compiled. The addressable time is `resolve_call`'s overhead, string-matched dispatch and the name scans: an estimated 10–15% of posting.
2. **Member access on Record variables** by field index, and `Rec`/`xRec`/with-fields resolved statically: 3–5%.
3. **Remaining statements** (Case, While, Repeat, For, Exit) so loop bodies stay compiled: removes per-node fallbacks inside hot loops.
4. **Typed fast paths and fewer Val clones** (read by reference for comparisons, borrow instead of clone): 3–5%.
5. **Then a slot-only frame**, and bytecode if dispatch dominates.

Revised after round 11: compiling dispatch gave about 7.5%. What is left that compilation can reach is the remaining fallbacks (xRec, member chains, overloaded calls) and typed fast paths: a few percent more, about 10–12% in total. The 15–25% needs work under the call sites: record operations (dispatch by id inside `record_method`, copy-on-write rows), and cheaper frames (lazy local Record variables, globals created on use, no per-call name copies). Each of these has to show a gain with cpu-ab on its own. That is on top of the allocator (round 6) and independent of the database work (docs/deploy-db.md).
