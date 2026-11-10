# Open questions

Behaviour questions that no allowed source settles yet (see [SOURCES.md](../SOURCES.md)). Until one does, the behaviour stays unimplemented or is flagged as a guess in [assumptions.md](assumptions.md).

| ID | Question | Area (docs/sources.md) | What was tried | Date |
|---|---|---|---|---|
| OQ-K1 | Does SetCurrentKey(F1, F2) take a key (F1) whose fields followed by the primary key fields (F2) start with the given fields, or sort without a key? | A06.7 | Learn SetCurrentKey (prefix rule only); aldevtools has no case. Probe `k-setcurrentkey-pk-tail` | 2026-11-10 |
| OQ-K2 | Which filter groups does GetView write (only the current one, or all)? Does SetView accept `FILTERGROUP(n: ..)` sections? Is a caption that holds `,` (SORTING) or `=` (WHERE) quoted? | A06.10 | Learn GetView and SetView pages say nothing; aldevtools tests set filters in group 0 or read GetView in the group they set. Probe `k-view-groups-captions` | 2026-11-10 |
| OQ-K3 | When is a printed non-text filter value quoted (a leading quote, `( ) & | < > =`, `..`)? When is a text value quoted for leading or trailing blanks, `*` or `?`? When does `=` stay in front of an equality? | A07.3, A07.4 | aldevtools covers `@`, `>`, blanks and inner spaces only. Probe `k-filter-print` | 2026-11-10 |
| OQ-K4 | Inside a quoted filter value (`'A*B'`), do `*` and `?` match as wildcards or literally? | A07.5 | aldevtools 68210 is consistent with both readings. Probe `k-filter-quoted-wildcard` | 2026-11-10 |
| OQ-K5 | Format: abbreviated field names (`<D>`, `<Mo>`, `<H>`), `<C,..>` and `<S,..>`, Format(Value, Length) of numbers, Decimal standard formats 3 and 4 on a Decimal variable (Learn lists them; the aldevtools matrices on a Variant raise InvalidStandardFormat) | A08.4 | Learn Format pages, aldevtools format-matrix. Probe `k-format-names-std` | 2026-11-10 |
| OQ-K6 | A TableRelation to a table whose primary key has two or more fields, written without a field: which field does validation compare with? | A10.4 | Learn TableRelation property says nothing; no aldevtools case. Probe `k-relation-no-field` | 2026-11-10 |
| OQ-K7 | FieldError on a blank Time or DateFormula field: "You must specify" or "must not be"? | A10.5 | aldevtools shows Text, Code, Date and DateTime only. Probe `k-fielderror-blank-types` | 2026-11-10 |
| OQ-K8 | A Record passed by value: does the callee see the caller's filters, sorting and marks? | A11b.4 | Learn says nothing; aldevtools covers temporary records only. Probe `k-record-byvalue` | 2026-11-10 |
| OQ-K9 | DeleteAll(true): when the OnDelete trigger of one record deletes a later record of the set, is that record skipped or is there an error? | A11b.5 | No source. Probe `k-deleteall-cascade` | 2026-11-10 |
| OQ-K10 | Call stack object type words for Report, ReportExtension and PageExtension frames | A29.2 | aldevtools shows CodeUnit, Page, Query, Table, TableExtension, XmlPort. Probe `k-callstack-types` | 2026-11-10 |
| OQ-K11 | Decimal rounding at the 19th significant digit: ties (half away from zero or half to even) and negative values | A31.3 | aldevtools 70540 shows rounding above half only. Probe `k-decimal-ties` | 2026-11-10 |
| OQ-K12 | Does `@` in a filter also ignore accents (`@*cafe*` against `Café`)? | A07.7 | Learn "Entering criteria in filters" says `@` ignores case and says nothing about accents. Probe `k-filter-at-accents` | 2026-11-10 |
| Q-N1 | Does a .NET write through an AL InStream (`Bitmap.Save(InStr)`, `XmlDocument.Save(InStr)`) land in the Blob at the stream's position without truncating it? | A20 (A20.3) | Learn says nothing; the aldevtools Y51BlobStreamPosition cases cover Position only, not .NET writes | 2026-11-10 |
| Q-N2 | What do EnableSaasExtensionInstall, the environment type and the environment name read on a production on-premises server? | A28 (A28.3) | The probe system is a sandbox artifact: EnableSaasExtensionInstall true, environment Sandbox (tests/vectors/a28/n-env.tsv) | 2026-11-10 |
| Q-N3 | What do ApplicationIdentifier() and the server settings read on Business Central online? bc-rust's online mode answers 'FIN' | A28 (A28.3) | No online system to probe | 2026-11-10 |
| Q-N4 | Which platform version number does AL see on BC 29.1 (file version of the service tier)? | A28 (A28.3) | Not probed yet | 2026-11-10 |
| Q-N5 | Texts and codes of CreateEncryptionKey, DeleteEncryptionKey, ExportEncryptionKey and ImportEncryptionKey errors | A24 (A24.2) | The probe system has no key, and changing the shared container's key was not done; aldevtools key lifecycle cases give part of it | 2026-11-10 |
| Q-N6 | Do Break, BreakUnbound and Quit in an XMLport end the running trigger also from a procedure it calls? | A21 (A21.3) | Only Skip has an aldevtools case | 2026-11-10 |
