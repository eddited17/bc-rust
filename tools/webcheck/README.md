# webcheck: click through Microsoft's web client on bc-nst

A headless browser signs in to Microsoft's Business Central web client (served against bc-nst) and does
what a user does on a complete demo company. It then prints a pass/fail table, one row per step.

1. **Role center.** Scroll it from top to bottom, waiting for every part to load. Then open each cue
   tile's drill-down and go back.
2. **Lists.** Open each list, scroll it, open its first card or document, scroll that (including its
   lines), and close it: Customers, Vendors, Items, Sales Quotes/Orders/Invoices/Credit Memos, Posted
   Sales Invoices, Purchase Orders, Posted Purchase Invoices, Chart of Accounts, G/L Entries,
   Customer/Vendor/Item Ledger Entries, Bank Accounts, General Journals, Dimension Set Entries (479)
   and the Dimension Set Entries FactBox (699).
3. **Sales order.** Create one for the first customer, with one line for the first item, and post it
   with Ship and Invoice. The step passes only if the client confirms "posted as number …" or closes
   the order.
4. **Deep scrolling (`WEBCHECK_DEEP=1`).** On G/L Entries, Item Ledger Entries, Customer Ledger
   Entries and Value Entries, scroll the list's own virtualized grid to its last row, then back to the
   top. Then do page-down bursts on a focused cell, Ctrl+End and Ctrl+Home, and fast mouse-wheel
   bursts down and up. This only reads data, so it is safe on a live system. The details record the
   grid's `aria-rowcount` at the end, which is the number of rows plus one for the header.

A step **fails** on any of these:

- a `fail:` line in the web server log during the step;
- a `<method> fault` line in the bc-nst log, meaning an exception was sent to the client (needs `WEBCHECK_NSTLOG`);
- a client error dialog, or any dialog the step did not expect;
- the "Something went wrong" page;
- a session drop or a credential re-prompt;
- the step's own check, such as a page that does not open.

After a session drop, the next step signs in again in a fresh browser context, so one crash does not
hide the rest.

## Run

You need Node 22 and `playwright-core` with a Chromium (`npx playwright install chromium`). Credentials
come from the environment only.

```sh
WEBCHECK_URL=http://127.0.0.1:8191/BC/ \
WEBCHECK_USER=admin WEBCHECK_PASSWORD=... \
WEBCHECK_WEBLOG=file:/path/to/webserver.log \
WEBCHECK_NSTLOG=file:/path/to/bc-nst.log \
node tools/webcheck/webcheck.mjs
```

For a server that runs as a systemd user unit, read the log from the journal instead:
`WEBCHECK_WEBLOG=journal:msweb-bcnst-8193.service WEBCHECK_NSTLOG=journal:bc-nst.service`.

| Variable | Meaning |
|---|---|
| `WEBCHECK_URL` | web client base URL (default `http://127.0.0.1:8191/BC/`) |
| `WEBCHECK_USER`, `WEBCHECK_PASSWORD` | sign-in. bc-nst accepts any password unless authentication is on. |
| `WEBCHECK_WEBLOG` | web server log, `file:<path>` or `journal:<user unit>`. Without it, `fail:` lines are not checked. |
| `WEBCHECK_NSTLOG` | bc-nst log, same forms. A `<method> fault` line fails the step. Other error lines are listed under the table without failing it. |
| `WEBCHECK_OUT` | output directory: `report.txt`, `results.json`, and a screenshot of each failed step (default `/tmp/webcheck-<time>`) |
| `WEBCHECK_ONLY`, `WEBCHECK_SKIP` | regex on step names to run or skip, e.g. `WEBCHECK_ONLY='sign in\|role center'` |
| `WEBCHECK_KNOWN` | `regex=note;...` for known failures owned elsewhere. A failed step whose name matches, or whose every problem line matches, is reported `KNOWN`, not `FAIL`. |
| `WEBCHECK_POST` | `0` skips the sales order (it posts, so it changes the company's data) |
| `WEBCHECK_CUSTOMER`, `WEBCHECK_ITEM` | numbers for the sales order (default: the first row of Customers / Items) |
| `WEBCHECK_MAX_CUES` | open at most this many cue tiles |
| `WEBCHECK_DEEP` | `1` adds the deep-scroll steps (about 6 minutes on CRONUS) |
| `WEBCHECK_DEEP_STEPS` | maximum number of half-screen scroll steps per list before giving up on reaching the end (default 600) |
| `PLAYWRIGHT_CORE` | path to a `playwright-core` module, if it is not resolvable from here |
| `WEBCHECK_CHROME` | Chromium executable, if not Playwright's default |

The exit code is 1 when a step failed, and 0 otherwise (KNOWN steps don't count as failures).

Each step loads the client once by URL (`?page=<id>`). Reloads reuse the server session, so a long run
leaves forms open on the service tier. This is how Microsoft's web server behaves, and it is the same
against the real NST.
