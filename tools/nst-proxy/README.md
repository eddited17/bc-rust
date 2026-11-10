# nst-proxy: record web server ↔ service tier traffic

A logging proxy for the BC client services port. Put it between Microsoft's web server and a service tier
(the real NST in the bc-sidebyside VM, or bc-nst) and it writes every message in both directions to JSONL.
Node 22, no dependencies.

```sh
# 1. proxy: 127.0.0.1:7285 -> 127.0.0.1:7085, marker port 7286
REDACT_FILE=/tmp/nstproxy/secrets node tools/nst-proxy/proxy.mjs data/capture/nst-real

# 2. Microsoft's web server (a copy whose ClientServicesPort is 7285) on 127.0.0.1:8192
tools/nst-proxy/run-webserver.sh --copy   # once: copy webserver-linux to webserver-proxy, set the port
tools/nst-proxy/run-webserver.sh

# 3. drive a browser session: sign in, page 22, open the first customer, edit and revert Phone No.,
#    an invalid Country/Region Code and back, close the card, page 42
PASSWORD_FILE=/tmp/nstproxy/secrets NST_LOG=data/capture/nst-real/nst.jsonl \
  node tools/nst-proxy/session.mjs http://127.0.0.1:8192/BC/ admin data/capture/nst-real/screens

# 4. read it
node tools/nst-proxy/summarize.mjs data/capture/nst-real/nst.jsonl data/capture/nst-real/calls.txt \
  --calls-jsonl data/capture/nst-real/calls.jsonl
node tools/nst-proxy/samples.mjs data/capture/nst-real/nst.jsonl data/capture/nst-real/samples 01-open-connection=16 ...
```

`REDACT_FILE` and `PASSWORD_FILE` hold the password on one line (e.g. the `bc_password` value from
`data/vm/credentials.txt`). Keep that file outside the repo.

## Files

| File | What |
|---|---|
| `proxy.mjs` | the proxy. Env: `LISTEN` (127.0.0.1:7285), `UPSTREAM` (127.0.0.1:7085), `CONTROL` (127.0.0.1:7286, `off` to disable), `KEEP_HOST=1`, `REDACT_FILE` |
| `run-webserver.sh` | runs the web server copy in `PROXY_WEB_DIR` with the environment of `tools/msweb-linux/run.sh`; `--copy` recreates the copy from `MSWEB_DIR`. Both can live in `~/.config/bc-rust/msweb.env` (see `tools/msweb-linux/README.md`). It never builds anything. |
| `session.mjs` | headless Chromium driver (same CDP plumbing as `tools/capture/capture.mjs`) with in-page clicks and typing; posts step markers to the proxy. Steps are listed at the top of the file |
| `summarize.mjs` | `nst.jsonl` to one line per call in request order (`calls.txt`); `--calls-jsonl` rewrites `calls.jsonl` in request order |
| `samples.mjs` | `nst.jsonl` to pretty-printed request/response pairs, NavDataSets and `exceptionData` decoded |
| `navdataset.mjs` | NavDataSet decoder (`{"compressed","count","buffers"}` to tables, columns, rows) |

## What the proxy does

- **HTTP** (`/BC/client/metadata/...`, `health`, ...): forwarded with Node's http client. The request
  headers are passed on in their original order and case, including `Authorization`. `Host` is rewritten to
  the upstream address unless `KEEP_HOST=1`. Response headers are passed back unchanged; Node may add
  `Connection`/`Keep-Alive`. The body is re-framed (Content-Length or chunked as the upstream sent it).
- **WebSocket** (`/BC/client/ws/connect`): the upgrade request is written to the upstream verbatim (apart
  from `Host`), and from then on both directions are piped byte for byte. The proxy parses a copy of the
  frames for the log: masking, fragmentation, control frames, and permessage-deflate with or without
  context takeover. It never re-encodes a frame, so it can't change what either side negotiated.
- All sockets run with `TCP_NODELAY`. Both BC peers send large messages as several frames. With Nagle on,
  the proxy held each later frame until the previous one was ACKed and added about 40 ms to most calls.

## nst.jsonl

One JSON object per line, in the order the proxy saw them:

| Field | |
|---|---|
| `seq`, `ts`, `t` | record number, ISO time, ms since the proxy started |
| `conn` | `h<n>` per HTTP request, `ws<n>` per WebSocket |
| `channel` | `http`, `ws`, `marker` (driver steps), `proxy` |
| `dir` | `to-nst` (web server to service tier) or `from-nst` |
| `kind` | `request`, `response`, `upgrade`, `upgrade-response`, `message`, `close`, `ping`, `pong`, `socket-end`, `error`, `marker` |
| `method`, `path`, `status`, `headers`, `ms` | HTTP and upgrade records. `ms` is the time to the response's end. |
| `opcode`, `frames`, `wireBytes`, `compressed`, `tFrame` | WebSocket messages. `wireBytes` counts frame headers. `tFrame` is the arrival time of the first frame. |
| `body`, `bodyEncoding`, `bodyBytes`, `decoded` | the body: `json` (parsed), `text`, `base64` or `none`. `decoded` names the inflation that was applied (`gzip`, `deflate`, `br`, `gzip(sniffed)`). |
| `rpc` | for JSON-RPC messages: `{kind: request/notification/result/error, id, method}` |

Redaction: `Authorization` keeps the scheme and the Basic user name (`Basic <redacted; user=ADMIN>`).
Cookie values are dropped. JSON keys ending in `password`/`passwd` are replaced, and every literal from
`REDACT_FILE` is replaced anywhere in a line.

`calls.jsonl` has one line per finished JSON-RPC call (`seq` of the request, `id`, `method`, `caller`,
`paramsBytes`, `reqBytes`, `respBytes`, `ms`, `outcome`, error code and message) and per HTTP call. The
proxy writes it when each response arrives, so its order is completion order. `summarize.mjs --calls-jsonl`
rewrites it in request order. Calls are matched by socket, direction and id: the NST numbers its own
requests (callbacks) separately from the web server, so the same id can be open in both directions.

## Recordings

- `data/capture/nst-real/`: Microsoft's web server on Linux against the real BC 29.1 service tier (see its
  notes.md).
