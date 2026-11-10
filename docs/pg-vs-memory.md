# PostgreSQL and the in-memory store: what differs

Production runs on PostgreSQL. Microsoft's tests run in al-test on the embedded in-memory store,
because it is fast and every worker can share one copy of the demo company. This page lists where
the two backends behave differently, which differences are fixed, and which are accepted.

Status: 2026-11-10 (I14, stream S3a). Measured with `al-test --pg` (below): a fixed slice of 79 test
codeunits, then the full Base App suites on both demo companies.

## Running the tests on PostgreSQL

```
al-test --suite baseapp --data w1demo --codeunit 134327,134378 -v \
        --pg 'host=/path/to/socket port=5432 user=bc password=... dbname=s3a'
```

- `dbname` is a name prefix, not a database. al-test copies the demo company (built or loaded from
  the demo cache as usual) into a template database `<prefix>_tpl`, then gives every worker its own
  clone `<prefix>_w<N>` (`CREATE DATABASE ... TEMPLATE`, about a second). The template is reused
  while the build and the company stay the same; the worker databases are dropped after the run
  (`--pg-keep` leaves them).
- al-test refuses the live names (`bc`, `bc_nst`) and only drops or replaces databases whose
  comment starts with `al-test scratch`, so it never touches a database it didn't make.
- Isolation is the same as in memory: a test's `Commit()` is a savepoint, and the codeunit (or,
  with `--isolation test`, the test) ends with a rollback, here of the worker's one PostgreSQL
  transaction. Number sequences, which no rollback undoes, are put back from the snapshot before
  every codeunit, as in memory.
- Code: `crates/bc-server/src/pgtest.rs`, the `--pg` option in `crates/bc-server/src/bin/al-test.rs`.
- The memory backend stays the default and nothing changes for runs without `--pg`.

## What the slice showed

The slice (posting of sales and purchase documents, general journals, warehouse, number series,
dimensions, and pages that look up and navigate by Code keys) ran from one binary on both
backends, W1 demo company, 4 jobs, 3,592 tests that Microsoft's CI runs. The codeunits:

```
134378,134385,134386,134387,134391,134763,134891,134893,132501,134384,134108,134398,137062,134000,134327,134328,134329,134330,134331,134337,134762,134892,134099,137061,134001,134851,134920,134932,134226,134227,134420,134760,134921,134935,134250,134251,137033,137030,137047,137048,137051,137055,137056,137064,137151,137152,137153,137154,137161,137504,137831,137081,138100,134374,134370,134530,134531,134380,134382,134474,134475,134476,134483,134485,134486,134822,134828,134234,138047,134559,134835,134837,132537,134393,134394,138000,134825,134824,134645
```

| Run | Memory | PostgreSQL |
|---|---|---|
| First run (before the fixes below) | 3,436 pass | 3,427 pass (9 lost, all in one codeunit) |
| After the fixes | 3,436 pass | 3,436 pass, 0 outcome differences |

Every outcome difference and its cause:

| Tests | Memory | PostgreSQL | Cause | Status |
|---|---|---|---|---|
| ERM Purchase Return Order: 9 tests after BatchPostWithWorkDateBackground | pass | "current transaction is aborted" | A stale savepoint: a `Commit()` inside a trapped `Codeunit.Run` became the test's rollback point, the trapped error rolled back past it, and the next rollback to it failed on PostgreSQL, which aborts the whole transaction. Memory quietly rolled back too little instead. | Fixed (al-runtime `rollback_trapped`) |
| SelectionFilterManagementTest :: GetSelectionFilterForDimensionValueWithSpecialSymbols (and ...ComputingRangesOnLists) | fail | fail, other message | Sort order of `&`, `.`, `@`, `<`, `=` (see "Sort order"). The test expects SQL Server's order, which neither backend has. | Accepted |
| No. Series Batch Tests :: TestDailyNoSeriesWithDifferentOrder | fail | fail, other code in the message | Same error ("The No. Series already exists"); the code comes from the `Any` codeunit, seeded from the clock, and differs between two memory runs too. | Noise |
| ShowLoggedDescriptionChanges* (2 tests) | fail | fail | The message holds a random Guid (CreateGuid is not seeded). | Noise |

Wall time on 4 jobs: memory 18 s, PostgreSQL 63 s (before the savepoint fix 294 s); the tests' own
time adds up to 69 s and 222 s. The memory side was also checked against the binary without the
fixes (same snapshot): 0 outcome differences in the 3,592 tests.

## What the full runs showed

The whole suites ran on PostgreSQL from main at 8df2a2c (code of 7a23b1c), 10 jobs, and were compared
with S5's in-memory runs of 8a1ba68. Every codeunit with an outcome difference then ran again on
memory with the PostgreSQL run's binary (4 jobs), which separates backend differences from code
changes and run-to-run noise.

| Run | Memory | PostgreSQL | Wall time on 10 jobs |
|---|---|---|---|
| W1 (`--suite baseapp --data w1demo`), 30,029 CI tests | 27,698 pass | 27,708 pass | 466 s |
| Contoso (`--suite system,foundation,baseapp --data contoso`), 31,592 CI tests | 28,579 pass | 28,593 pass | 373 s |
| W1 after the fixes below (same code on both sides) | 27,702 pass (27,704 with the final `Get` fix: Test Job Queue's 2 tests, checked on that codeunit) | 27,723 pass | 496 s |

Both PostgreSQL runs completed (complete `data:` lines, exit 0). The tests' own time over the same
tests was 1.28 times memory's on W1 (3,102 s against 2,419 s) and 1.27 times on Contoso.

Differences that the same-binary memory reruns confirmed (PostgreSQL fails, memory passes):

| Tests | Dataset | Cause | Status |
|---|---|---|---|
| SCM Bugfixes: 5 planning tests ("Unhandled UI: ModalPage Planning Error Log") | both | **Next() read-ahead.** On PostgreSQL every `Next()` reads up to 50 rows ahead. Planning's DeleteTracking loop deletes a Reservation Entry's paired entry through a second variable; the loop's `Next()` then returned the deleted row from its read-ahead and `Delete()` failed ("The Reservation Entry does not exist"). | Fixed (al-runtime recops `next`) |
| ERM Cost Accounting - Tables :: TestCostBudgetRegisterOnValidateClosed | both | Same: `ModifyAll(Closed, true)` through a second variable, then `Next(-1)` served the rows as they were before. | Fixed (same) |
| WF Demo General Journal Batch :: DirectApproverRejectsRequest | both | `SalesHeader.Get(RecordLink."Record ID")` with the RecordId of a Gen. Journal Batch copied the batch's key values into the Sales Header's key: PostgreSQL refused a Code value for the Option column ("error serializing parameter 0"); memory found nothing. BC returns false for a RecordId of another table. | Fixed (recops `Get`) |
| Resource Employee :: EmployeeNoIncrement; SCM Costing Reports :: AutoCostPostFalseAndAdjustCost | both / Contoso | al-test's per-codeunit sequence restore. A fresh PostgreSQL sequence reports its start value as `Current()`, memory the value before it, so restored sequences were one increment ahead on PostgreSQL (PeekNextNo said E0020, the employee got E0010). | Fixed (demo.rs `restore_sequences`) |
| ERM Account Schedule Charts (2) and ERM Finance Performance (3): "Unexpected account schedule period" | W1 | The `--pg` template skipped the stored rows of the platform's computed tables, so Feature Key states set while the company was built were missing: "Feature - Fin. Report Default" was off on PostgreSQL. | Fixed (pgtest.rs) |

The read-ahead bug was not only in tests: the same AL pattern on a live PostgreSQL server read stale
rows. In memory it showed only with a custom sort order (SetCurrentKey on fields no key starts with,
where every backend reads the whole set ahead): SCM Reservation Worksheet's two allocation tests
pass in memory since the fix.

Not backend differences:

- 19 tests (ERM Pmt Disc And VAT Cust/Vend 4, ERM Sales Order 7, ERM Invoice and Reminder 3,
  Document Attachment Tests 2, Report Selections Tests, Service Contracts, ERM RS Dimensions as
  Columns) fail in memory and pass on PostgreSQL (full runs, S5's and ours). They pass in memory
  only when run as single tests: with their whole codeunit at `--jobs 1` they fail too (I15:
  ERM Sales Order "Sales Credit Memo 1007 must be approved and released", ERM Pmt Disc And VAT
  Cust/Vend "G/L Register No. must be equal to '164' in G/L Entry 225"), so what reaches them comes
  from the earlier test methods of the same codeunit (codeunit isolation), not from other
  codeunits. A memory worker's copy-on-write overlay is empty after every codeunit's rollback
  (checked after each of the 1,177 codeunits of the W1 quick slice). In ERM Pmt Disc And VAT
  Cust/Vend the payment's VAT entry finds its G/L Entry - VAT Entry Link pointing at G/L entry 225
  of the demo company (register 9). Cause not isolated.
- Noise seen on both backends: Integration Table Synch. Test (10 ms timestamps), Email Retry Test,
  ERM Financial Report Schedules, Test OAuth 2.0 WebService, No. Series Batch Tests (clock-seeded
  `Any`).

## Differences between the backends

### Sort order of text (Code and Text fields)

- **Memory** compares lower-cased Unicode code points (`crates/bc-store/src/value.rs`, `cmp_ci`
  and the key encoding `encode_key_part`).
- **PostgreSQL** stores Code and Text columns with the ICU collation `ci`
  (`und-u-ks-level2`: root order, case ignored, accents count). ICU sorts spaces, then punctuation,
  then symbols, then digits, then letters, and sorts an accented letter next to its base letter.
- **BC on SQL Server** uses the database collation (usually `Latin1_General_100_CI_AS`, a Windows
  collation). Neither backend copies it, by design (no blind SQL Server emulation).

They disagree when a key holds punctuation or symbols, or letters outside a-z. Measured on the W1
demo company (every text column with two or more distinct values, read in PostgreSQL's order from
the template database and checked against the memory comparison):
2,497 columns checked, 43 in a different order, 5 of them primary keys:

- No. Series and No. Series Line codes: `S-CR-P+` before `S-CR+` on PostgreSQL (`-` sorts before `+`
  in ICU, after it in code points). 6 neighbours out of 155 codes.
- Permission set names (shared tables): `SECURITY - BASEAPP` before `SECURITY (SYSTEM)`.
- Not keys: VAT registration formats (`@` and `#`), number formats with `_` (`P_000001` before
  `P00001`), country and vendor names with `Å`, `ø`, `æ`, `ä` (ICU sorts them next to `a`/`o`;
  code points put them after `z`), Data Exchange XPath columns with `[`, `/`, `:`.

The characters behind it are punctuation, symbols and letters outside a-z. ASCII punctuation and
symbols sort like this (checked in the database):

```
PostgreSQL (ICU): _ - , ; : ! ? . ' " ( ) [ ] { } @ * / \ & # % ` ^ + < = > | ~ $  then digits, then letters
memory:           ! " # $ % & ' ( ) * + , - . /  digits  : ; < = > ? @ [ \ ] ^ _ `  letters  { | } ~
```

What this changes for AL code: `FindFirst`/`FindLast`/`Next` over such keys, range filters such as
`'A'..'M'`, and lists built from sorted keys (SelectionFilterManagement builds `A..C` ranges from
neighbours) can come out in another order.

**Decision (proposal): accept the difference and keep PostgreSQL as the definition.** Reasons:

1. The slice shows one test affected, and it fails on both backends because it expects SQL Server's
   order.
2. Matching ICU in memory needs ICU's root collation as a byte-comparable sort key, because the
   in-memory indexes are ordered byte strings. The ICU crates available offline don't include the
   collator, and a hand-made table would have to cover expansions (`æ`, `ß`, `œ`) and ignorable
   characters to be exact. Every in-memory key order would change, so it needs a full A/B on both
   datasets.
3. If ordering ever matters for a test that should pass, the way forward is a sort key in
   `value.rs` derived from PostgreSQL's own ICU (export the level-1/level-2 order of the Latin
   characters from the database), checked by a property test that sorts random strings both ways
   (`BC_PG_TEST`). Until then the list above is the accepted difference.

### Equality of text

Both backends ignore case. ICU also treats as equal what code points don't: a precomposed `é` and
`e` plus a combining accent, `ß` and `ss`, and strings that differ only by ignorable characters
(zero-width space, soft hyphen, C1 control characters such as the `\x8d` in one W1 demo
description). A primary key that is unique in memory can therefore collide on PostgreSQL. Not seen
in the demo data or the slice. Accepted.

### Wildcard filters (`*`, `?`, `@`)

- On real BC a wildcard filter without `@` is case-sensitive, and `@` makes it case-insensitive
  [src:A07.6]. Those tests ran on BC's cloud database; the bcside VM's CRONUS database is
  case-sensitive too.
- Both bc-rust backends lower-case the value and the pattern (`filter.rs` and `pg.rs` `node_sql`),
  so they agree with each other, but not with BC: without `@` their wildcard filters ignore case.
- Whether `@` also ignores accents (`@*cafe*` against `Café`) is an open question [src:A07.7].
  Both backends keep accents.
- Lower-casing differs in a few characters: PostgreSQL's `lower()` follows the database's C library
  locale, the pattern is lower-cased by Rust (e.g. `İ`). Accepted.

### Transactions and failed statements

- A test codeunit runs in one PostgreSQL transaction. Any failed statement aborts the transaction
  until it is rolled back to a savepoint; in memory a statement never fails. Code that ignores a
  database error therefore behaves the same in memory and silently poisons the rest of the
  codeunit on PostgreSQL. The slice found one case (above, fixed).
- Rolling back to a savepoint that an earlier rollback already removed is an error on PostgreSQL
  and a no-op in memory. The runtime now keeps the test's last-Commit savepoint valid after a
  trapped rollback. A remaining case in theory: an `asserterror` inside a trapped
  `Codeunit.Run` rolls back to the test's last Commit(), below the trap's savepoint.
- **Read-ahead.** `Next()` serves rows read ahead (50 on PostgreSQL; the whole set with a custom sort
  order on either backend) only while no other record variable of the session has written the table
  since they were read; the variable that writes keeps its own read-ahead, so a loop that modifies
  its current record still reads in batches.
- Savepoints nest on PostgreSQL. A codeunit's test methods and `Commit()`s used to leave thousands
  of nested savepoints, and every visibility check of the transaction's own rows walked them:
  SCM Warehouse II's tests slowed from 0.2 s to 9 s per test (245 s for the codeunit, 4 s in
  memory). al-test now releases a method's savepoints when the method ends
  (`Conn::release`, a no-op in memory): 33.5 s for that codeunit.
- Number sequences are DDL on PostgreSQL: creating or dropping one inside a test is rolled back
  with the transaction, in memory it is not. `nextval` is never rolled back on either.

### Caches, isolation, locking

PostgreSQL has a read cache per session and per transaction (`cache`, `tx_reads`, `tx_rows` in
`pg.rs`), READ COMMITTED isolation, row locks and advisory locks for the BC locking contract
(docs/job-queue.md). The memory store has none of these. In al-test every worker has one session
on its own database, so caches are exercised but concurrency is not: concurrency and locking are
covered by the soak tests (S3b), not by al-test. No slice or full-run outcome differed because of
a cache.

### Values

Text can't hold NUL characters on PostgreSQL. Dates are timestamps (closing dates 23:59:59,
`0D` = 1753-01-01) and decimals lose the sign and scale of zero; the read caches return exactly
what a database read returns (`pg.rs` test `caches_match_database`).

## Cost

Over the full suites PostgreSQL takes about 1.3 times memory's test time (the posting-heavy slice:
3.5 times). A full W1 run on 10 jobs takes about 8 minutes, Contoso about 6. Copying the demo company
into the template takes 9-16 s once per build; a worker clone about a second.
