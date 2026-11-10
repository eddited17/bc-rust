#!/usr/bin/env python3
"""wirevocab: the names Microsoft's web client and web server exchange with a real BC service tier, and the
bc-nst string literals that are not among them.

Pass 1 reads the capture sets listed in sets.tsv and writes docs/wire-vocab.tsv: one row per name (method,
JSON key, type string, enum value, ...) with the capture record where it was first seen. Names only: no value
of a captured message is written unless it is a name by the rules below.

Pass 2 reads every string literal in crates/bc-nst/src and writes the ones that are not in the vocabulary
(the "emitted but unseen" list), each with a category that separates wire candidates from noise.

Python 3 standard library only. Run with -I. See README.md.
"""

import argparse
import base64
import bisect
import collections
import glob
import gzip
import json
import os
import re
import subprocess
import sys
import zlib
from urllib.parse import parse_qsl, urlsplit

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
# Work files outside the repository live under one base directory: BC_RUST_VOLUME overrides it.
VOLUME = os.environ.get("BC_RUST_VOLUME", "/media/eddited/Volume")
CLEANROOM = os.path.join(VOLUME, "cleanroom", "wirevocab")
CORPUS = os.path.join(VOLUME, "cleanroom", "captures")

# --------------------------------------------------------------------------------------------------------------
# shared patterns

IDENT = re.compile(r"[A-Za-z_$][A-Za-z0-9_$]*\Z")
# A key with words but no digits: 'Data.CurrentBookmark', 'Multitask Window ID'.
WORDY_KEY = re.compile(r"[A-Za-z_$][A-Za-z_$.\- ]{0,58}[A-Za-z_]\Z")
DOTTED = re.compile(r"[A-Za-z_][A-Za-z0-9_`]*(?:\.[A-Za-z_][A-Za-z0-9_`+]*)+(?:\[\])?\Z")
GUID = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
NUMBER = re.compile(r"-?\d+(?:\.\d+)?\Z")
B64 = re.compile(r"[A-Za-z0-9+/]{16,}={0,2}\Z")
# Values under these keys are never read, let alone written.
SENSITIVE = re.compile(r"(?i)token|passw|secret|cookie|authori|credential|signature|nonce|session.?key|csrf|bearer|api.?key")
# Keys whose values are data (captions, names, record values, ids, filters): their values never become enum names.
DATA_KEY = re.compile(
    r"(?i)name|user|company|caption|text|title|descr|value|tooltip|url|uri|link|path|mail|phone|address|city|"
    r"country|filter|bookmark|record|^params|^args|argument|message|synopsis|about|label|instruction|query|search|"
    r"id$|ids$|handle|code$|^no$|number|date|time|culture|locale|currency|key$|keys$|token|expression|formula|"
    r"allowed$")
METHOD_KEYS = {"method", "interactionName", "MethodName", "methodName"}
TYPE_KEYS = {"$type", "$polyType", "handlerType", "ClrType", "typeName", "TypeName", "errorType", "exceptionType",
             "ExceptionType", "$valueType"}
TYPE_PREFIXES = ("Microsoft.Dynamics.", "DN.", "Newtonsoft.")
STATIC_EXT = re.compile(r"(?i)\.(js|css|png|svg|gif|jpe?g|ico|woff2?|ttf|eot|map|html?|txt|xml|resx|json)$")
ENUM_MAX_DISTINCT = 64
SHAPE_MIN_ORIGINALS = 3


def randomish(s):
    """True for generated tokens: GUIDs, hex ids, base64-ish blobs, short random instance ids."""
    if GUID.search(s):
        return True
    if len(s) >= 8 and re.fullmatch(r"[0-9a-fA-F]+", s) and re.search(r"\d", s):
        return True
    turns = len(re.findall(r"(?<=[A-Za-z])(?=\d)|(?<=\d)(?=[A-Za-z])", s))
    if turns >= 6:
        return True
    if re.fullmatch(r"[a-z0-9]{6,12}", s) and turns >= 2:
        return True
    if len(s) >= 24 and not re.search(r"[a-z]{4}", s):
        return True
    return False


def shape_of(s):
    """Digit runs and GUIDs become '#': 'Control123_Format' -> 'Control#_Format'."""
    return re.sub(r"\d+", "#", GUID.sub("#", s))


GENERATED_PREFIX = re.compile(r"([A-Za-z]+)\d+\1\d+")


def generated_id(s):
    """Ids the service tier generates from object and control ids: a run of three or more digits
    (Control1111081075_DynamicCaption) or a prefixed number written twice (p50300p50300IsOdd)."""
    return bool(re.search(r"\d{3,}", s) or GENERATED_PREFIX.match(s))


def gen_shape(s):
    """The shape of a generated id: 'p50300p50300IsOdd' -> 'p#p#*', 'Control123_Format' -> 'Control#_Format'."""
    m = GENERATED_PREFIX.match(s)
    if m:
        return f"{m.group(1)}#{m.group(1)}#*"
    return shape_of(s)


def shape_regex(shape):
    return re.compile("".join(".*" if c == "*" else re.escape(c) for c in shape) + r"\Z")


def unwrap(s):
    """A string that carries JSON: nested JSON text, base64 of JSON, base64 of gzip/zlib JSON. Else None."""
    c = s[:1]
    if c in "{[" and len(s) >= 2 and s[-1:] in "}]":
        try:
            return json.loads(s)
        except ValueError:
            return None
    if len(s) >= 16 and B64.match(s) and len(s) % 4 == 0:
        try:
            raw = base64.b64decode(s, validate=True)
        except ValueError:
            return None
        if raw[:2] == b"\x1f\x8b":
            try:
                raw = gzip.decompress(raw)
            except (OSError, EOFError, zlib.error):
                return None
        elif raw[:1] == b"\x78":
            try:
                raw = zlib.decompress(raw)
            except zlib.error:
                pass
        if raw[:1] in (b"{", b"["):
            try:
                return json.loads(raw.decode("utf-8"))
            except (ValueError, UnicodeDecodeError):
                return None
    return None


def lp_strings(b):
    """Identifier-like strings in a binary buffer, read the way .NET's BinaryWriter writes strings
    (7-bit encoded length, then UTF-8; see BinaryWriter.Write(string) in dotnet/runtime)."""
    out = []
    i, n = 0, len(b)
    while i < n:
        length, shift, j = 0, 0, i
        while j < n and j - i < 3:
            c = b[j]
            length |= (c & 0x7F) << shift
            j += 1
            if c < 0x80:
                break
            shift += 7
        if 2 <= length <= 120 and j + length <= n:
            s = b[j:j + length]
            if re.fullmatch(rb"[A-Za-z_][A-Za-z0-9_]*", s):
                out.append(s.decode())
                i = j + length
                continue
        i += 1
    return out


def inflate(raw):
    for wbits in (31, 15, -15):
        try:
            return zlib.decompress(raw, wbits)
        except zlib.error:
            pass
    return raw


# --------------------------------------------------------------------------------------------------------------
# pass 1: the vocabulary


class Vocab:
    def __init__(self):
        self.order = 0
        self.ctx = None              # (scenario, step, layer) of the record being read
        self.names = {}              # (name, kind) -> [order, scenario, step, layers]
        self.cand = collections.defaultdict(dict)    # key -> {value: [order, scenario, step, layers]}
        self.distinct = collections.defaultdict(set)  # key -> its distinct string values
        self.shapes = {}             # (shape, kind) -> [order, scenario, step, layers, originals]
        self.records = collections.Counter()
        self.guids = set()           # GUID values seen; used by pass 2 only, never written
        self.binstrings = set()      # identifier-like strings inside data set buffers; pass 2 only, never written

    # -- bookkeeping

    def add(self, name, kind):
        scn, step, layer = self.ctx
        e = self.names.get((name, kind))
        if e is None:
            self.names[(name, kind)] = [self.order, scn, step, {layer}]
        else:
            e[3].add(layer)

    def add_type(self, t):
        self.add(t, "type")
        short = t.rsplit(".", 1)[-1].rstrip("[]")
        if short != t and IDENT.match(short):
            self.add(short, "type-short")

    def guids_in(self, s):
        for g in GUID.findall(s):
            self.guids.add(g.lower())

    def add_shape(self, original, kind, shape=None, ctx=None):
        shape = shape or shape_of(original)
        if shape == original or not re.search(r"[A-Za-z]", shape):
            return
        order, scn, step, layers = ctx or (self.order, *self.ctx[:2], {self.ctx[2]})
        e = self.shapes.get((shape, kind))
        if e is None:
            self.shapes[(shape, kind)] = [order, scn, step, set(layers), {original}]
        else:
            e[3] |= set(layers)
            if order < e[0]:
                e[0:3] = [order, scn, step]
            if len(e[4]) < SHAPE_MIN_ORIGINALS:
                e[4].add(original)

    # -- JSON

    def key(self, k):
        self.guids_in(k)
        if IDENT.match(k) and not randomish(k):
            self.add(k, "key")
        elif WORDY_KEY.match(k):
            self.add(k, "key")
        self.add_shape(k, "key-shape")

    def walk(self, o, key=None):
        if isinstance(o, dict):
            if isinstance(o.get("buffers"), list) and "compressed" in o:
                self.dataset(o)
            for k, v in o.items():
                if isinstance(k, str):
                    self.key(k)
                    self.value(k, v)
        elif isinstance(o, list):
            lk = (key or "") + "[]"
            for v in o:
                self.value(lk, v)

    def value(self, k, v):
        if isinstance(v, (dict, list)):
            return self.walk(v, k.rstrip("[]") or None)
        if not isinstance(v, str) or not v:
            return
        base = k[:-2] if k.endswith("[]") else k
        if SENSITIVE.search(base):
            return
        inner = unwrap(v)
        if inner is not None:
            return self.walk(inner, base)
        if base in METHOD_KEYS:
            if IDENT.match(v):
                self.add(v, "method")
            return
        if len(v) > 300:
            self.distinct[k].add(v)
            return
        self.guids_in(v)
        if base in TYPE_KEYS or (DOTTED.match(v.split(",")[0].strip()) and v.startswith(TYPE_PREFIXES)):
            head, _, rest = v.partition(",")
            head = head.strip()
            if DOTTED.match(head) or (base in TYPE_KEYS and IDENT.match(head)):
                self.add_type(head)
                asm = rest.split(",")[0].strip()
                if asm and (DOTTED.match(asm) or IDENT.match(asm)):
                    self.add(asm, "assembly")
                return
        self.distinct[k].add(v)
        if len(v) <= 80:
            scn, step, layer = self.ctx
            e = self.cand[k].get(v)
            if e is None:
                self.cand[k][v] = [self.order, scn, step, {layer}]
            else:
                e[3].add(layer)

    def dataset(self, o):
        for b in o.get("buffers") or []:
            if not isinstance(b, str):
                continue
            try:
                raw = base64.b64decode(b, validate=True)
            except ValueError:
                continue
            if o.get("compressed"):
                raw = inflate(raw)
            for s in lp_strings(raw):
                self.dataset_string(s)

    def dataset_string(self, s):
        """Strings inside a binary data set mix column names with record values, and without the data set
        layout (stream G) nothing tells them apart. They stay in memory: pass 2 marks a bc-nst literal found
        only here as dataset-only. Nothing from a data set is written to the vocabulary."""
        self.binstrings.add(s)
        if re.search(r"\d", s):
            self.add_shape(s, "binary-shape", gen_shape(s) if generated_id(s) else None)

    # -- HTTP

    def url(self, u, static_ok=False):
        p = urlsplit(u)
        path = p.path
        if not path.startswith("/") or (not static_ok and STATIC_EXT.search(path)):
            return
        self.guids_in(path)
        segs = path.split("/")
        shape = []
        for seg in segs:
            if not seg:
                shape.append(seg)
            elif re.fullmatch(r"[A-Za-z][A-Za-z0-9_.\-]*", seg) and not randomish(seg) and not GUID.search(seg):
                shape.append(seg)
                self.add(seg, "path-segment")
            else:
                shape.append("#")
        self.add("/".join(shape), "path")
        for name, val in parse_qsl(p.query, keep_blank_values=True):
            if re.fullmatch(r"[A-Za-z_$][A-Za-z0-9_$.\-]*", name):
                self.add(name, "query")
                if not SENSITIVE.search(name):
                    self.guids_in(val)

    def headers(self, h):
        if isinstance(h, dict):
            for name in h:
                if re.fullmatch(r"[A-Za-z0-9\-_]+", name):
                    self.add(name, "header")

    # -- readers per format

    def read_nst_proxy(self, path, rel, layer):
        step = ""
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                if not line.strip():
                    continue
                r = json.loads(line)
                self.order += 1
                seq = r.get("seq", self.order)
                if r.get("channel") == "marker":
                    lab = str(r.get("label", ""))
                    if lab.startswith("step start: "):
                        step = lab[len("step start: "):]
                    continue
                self.ctx = (f"{rel}#{seq}", step, layer)
                self.records[rel] += 1
                kind = r.get("kind")
                if kind in ("request", "upgrade") and isinstance(r.get("path"), str):
                    self.url(r["path"], static_ok=True)
                if kind in ("request", "response", "upgrade", "upgrade-response"):
                    self.headers(r.get("headers"))
                if r.get("bodyEncoding") == "json":
                    body = r.get("body")
                    if isinstance(body, dict) and isinstance(body.get("method"), str) and IDENT.match(body["method"]):
                        self.add(body["method"], "method")
                    self.walk(body)

    def read_capture(self, path, rel, layer):
        step = ""
        with open(path, encoding="utf-8") as fh:
            for n, line in enumerate(fh, 1):
                if not line.strip():
                    continue
                r = json.loads(line)
                self.order += 1
                d, p = r.get("dir"), r.get("payload")
                if d == "step":
                    if isinstance(p, dict) and p.get("phase") == "start":
                        step = str(p.get("label", ""))
                    continue
                self.ctx = (f"{rel}#{n}", step, layer)
                self.records[rel] += 1
                if d == "http" and isinstance(p, dict):
                    # request line only: request bodies (the sign-in form) are never read
                    self.url(str(p.get("url", "")))
                elif d == "http-body" and isinstance(p, dict):
                    self.url(str(p.get("url", "")))
                    self.headers(p.get("headers"))
                    if "json" in str(p.get("mime", "")) and isinstance(p.get("body"), str):
                        try:
                            self.walk(json.loads(p["body"]))
                        except ValueError:
                            pass
                elif d == "ws-open" and isinstance(p, str):
                    self.url(p, static_ok=True)
                elif d in ("send", "recv") and isinstance(p, str):
                    try:
                        msg = json.loads(p)
                    except ValueError:
                        continue
                    if isinstance(msg, dict) and isinstance(msg.get("method"), str) and IDENT.match(msg["method"]):
                        self.add(msg["method"], "method")
                    self.walk(msg)

    # -- result

    def finish(self):
        """Decide the enum keys and recurring shapes, return the rows."""
        rows = {}

        def put(name, kind, e):
            k = (name, kind)
            if k not in rows or e[0] < rows[k][0]:
                rows[k] = [e[0], e[1], e[2], set(e[3])]
            else:
                rows[k][3] |= e[3]

        for (name, kind), e in self.names.items():
            put(name, kind, e)
        self.enum_keys = []
        for k, vals in self.cand.items():
            base = k[:-2] if k.endswith("[]") else k
            if DATA_KEY.search(base) or SENSITIVE.search(base) or not base:
                continue
            # generated ids (Control123_Format, p21p21StyleTxt) count as shapes under any non-data key
            for v, e in vals.items():
                if IDENT.match(v) and generated_id(v) and not randomish(v):
                    self.add_shape(v, "id-shape", gen_shape(v), ctx=e)
            distinct = len(self.distinct[k])
            if distinct > ENUM_MAX_DISTINCT:
                continue
            wordy = sum(1 for v in vals if IDENT.match(v) or NUMBER.match(v))
            if wordy < 0.8 * distinct:
                continue
            took = 0
            for v, e in vals.items():
                if not IDENT.match(v) or len(v) < 2 or randomish(v):
                    continue
                if not generated_id(v):
                    put(v, "enum", e)
                    took += 1
            if took:
                self.enum_keys.append((base, took))
        self.bin_shapes = set()
        for (shape, kind), e in self.shapes.items():
            if len(e[4]) < SHAPE_MIN_ORIGINALS:
                continue
            if kind == "binary-shape":
                self.bin_shapes.add(shape)
            else:
                put(shape, kind, e)
        out = []
        for (name, kind), e in rows.items():
            out.append((name, kind, e[1], e[2], ",".join(sorted(e[3]))))
        out.sort(key=lambda r: (r[0].lower(), r[0], r[1]))
        return out


def read_sets(path):
    sets = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            if not line.strip() or line.startswith("#"):
                continue
            cols = line.rstrip("\n").split("\t")
            if len(cols) not in (5, 6):
                sys.exit(f"wirevocab: {path}: expected 5 or 6 tab-separated columns: {line.strip()!r}")
            name, layer, fmt, expect, files = cols[:5]
            root = cols[5].strip() if len(cols) == 6 and cols[5].strip() else "captures"
            sets.append({"set": name, "layer": layer, "format": fmt, "expect": expect, "globs": files.split(),
                         "root": root})
    return sets


def check_provenance(s, path):
    """The set must be what sets.tsv says: an nst-proxy log whose upstream is the real NST, or a browser
    capture that only loaded pages from the expected web server."""
    what, _, want = s["expect"].partition("=")
    with open(path, encoding="utf-8") as fh:
        if what == "upstream":
            first = json.loads(fh.readline())
            got = first.get("upstream")
            if first.get("kind") != "start" or got != want:
                return f"{path}: proxy upstream {got!r}, expected {want}"
            return None
        if what == "origin":
            for line in fh:
                r = json.loads(line)
                if r.get("dir") in ("http", "http-body") and isinstance(r.get("payload"), dict):
                    host = urlsplit(str(r["payload"].get("url", ""))).netloc
                    if host and host != want:
                        return f"{path}: request to {host}, expected only {want}"
                elif r.get("dir") == "ws-open" and isinstance(r.get("payload"), str):
                    host = urlsplit(r["payload"]).netloc
                    if host != want:
                        return f"{path}: WebSocket to {host}, expected {want}"
            return None
    return f"{path}: unknown expectation {s['expect']!r}"


def pass1(roots, sets_path, out_path):
    v = Vocab()
    used = []
    for s in read_sets(sets_path):
        if s["root"] not in roots:
            sys.exit(f"wirevocab: capture set {s['set']}: unknown root {s['root']!r} (known: {', '.join(roots)})")
        base = roots[s["root"]]
        files = []
        for g in s["globs"]:
            files.extend(sorted(glob.glob(os.path.join(base, g))))
        if not files:
            sys.exit(f"wirevocab: capture set {s['set']}: no files under {base} for {' '.join(s['globs'])}")
        for f in files:
            err = check_provenance(s, f)
            if err:
                sys.exit(f"wirevocab: capture set {s['set']} fails its provenance check: {err}")
            rel = os.path.relpath(f, base)
            if s["format"] == "nst-proxy":
                v.read_nst_proxy(f, rel, s["layer"])
            elif s["format"] == "capture":
                v.read_capture(f, rel, s["layer"])
            else:
                sys.exit(f"wirevocab: unknown format {s['format']}")
            used.append((s["set"], s["root"], rel, v.records[rel]))
    rows = v.finish()
    tmp = out_path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.write("name\tkind\tscenario\tstep\tlayers\n")
        for r in rows:
            fh.write("\t".join(clean_cell(x) for x in r) + "\n")
    os.replace(tmp, out_path)
    return rows, used, v


def clean_cell(s):
    return str(s).replace("\t", " ").replace("\n", " ").replace("\r", " ")


def load_vocab(path):
    rows = []
    with open(path, encoding="utf-8") as fh:
        next(fh)
        for line in fh:
            name, kind, scn, step, layers = line.rstrip("\n").split("\t")
            rows.append((name, kind, scn, step, layers))
    return rows


# --------------------------------------------------------------------------------------------------------------
# pass 2: bc-nst string literals


def rust_literals(src):
    """String literals outside comments: (offset, end, text, attr) where attr is the attribute the literal sits
    in ('serde(...', 'cfg(...', ...) or None. Also returns the test regions (offset ranges)."""
    out, test_regions = [], []
    i, n = 0, len(src)
    depth = 0
    pending_test = None       # offset where #[cfg(test)] / #[test] was seen
    open_tests = []           # brace depths at which a test region started, with start offsets
    attr_stack = None         # [start, bracket depth] of the attribute being read
    while i < n:
        c = src[i]
        if c == "/" and src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
            continue
        if c == "/" and src.startswith("/*", i):
            lvl, j = 1, i + 2
            while j < n and lvl:
                if src.startswith("/*", j):
                    lvl, j = lvl + 1, j + 2
                elif src.startswith("*/", j):
                    lvl, j = lvl - 1, j + 2
                else:
                    j += 1
            i = j
            continue
        m = re.compile(r'b?r(#*)"').match(src, i) if c in "br" else None
        if m and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            hashes = m.group(1)
            end = src.find('"' + hashes, m.end())
            end = n if end < 0 else end
            out.append((i, end + 1 + len(hashes), src[m.end():end], attr_text(src, attr_stack)))
            i = end + 1 + len(hashes)
            continue
        if c == '"' or (c == "b" and src.startswith('b"', i) and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_"))):
            j = i + (2 if c == "b" else 1)
            buf = []
            while j < n and src[j] != '"':
                if src[j] == "\\":
                    buf.append(src[j:j + 2])
                    j += 2
                else:
                    buf.append(src[j])
                    j += 1
            out.append((i, j + 1, unescape("".join(buf)), attr_text(src, attr_stack)))
            i = j + 1
            continue
        if c == "'":
            m = re.compile(r"'(?:[^'\\\n]|\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.))'").match(src, i)
            i = m.end() if m else i + 1
            continue
        if c == "#" and src.startswith("#[", i) or c == "#" and src.startswith("#![", i):
            if attr_stack is None:
                k = src.index("[", i)
                attr_stack = [k + 1, 1]
                head = src[k + 1:k + 40].replace(" ", "")
                if head.startswith("cfg(test)") or head.startswith("test]") or head.startswith("tokio::test"):
                    pending_test = i
                i = k + 1
                continue
        if attr_stack is not None:
            if c == "[":
                attr_stack[1] += 1
            elif c == "]":
                attr_stack[1] -= 1
                if attr_stack[1] == 0:
                    attr_stack = None
            i += 1
            continue
        if c == "{":
            if pending_test is not None:
                open_tests.append((depth, pending_test))
                pending_test = None
            depth += 1
        elif c == "}":
            depth -= 1
            if open_tests and open_tests[-1][0] == depth:
                _, start = open_tests.pop()
                test_regions.append((start, i + 1))
        elif c == ";" and pending_test is not None:
            pending_test = None  # `#[cfg(test)] mod tests;` or `use`: the file itself is handled by the caller
        i += 1
    return out, test_regions


def attr_text(src, attr_stack):
    if attr_stack is None:
        return None
    return src[attr_stack[0]:attr_stack[0] + 60].split("(")[0].strip() or "attr"


def unescape(s):
    return re.sub(r"\\(u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|\n\s*|.)", lambda m: {
        "n": "\n", "t": "\t", "r": "\r", "0": "\0", '"': '"', "\\": "\\", "'": "'"}.get(m.group(1), (
            chr(int(m.group(1)[2:-1], 16)) if m.group(1).startswith("u{") else
            chr(int(m.group(1)[1:], 16)) if m.group(1).startswith("x") else
            "" if m.group(1).startswith("\n") else m.group(1))), s)


def external_test_files(root):
    """Files that are modules declared under #[cfg(test)] (`#[cfg(test)] mod tests;`)."""
    tests = set()
    for path in glob.glob(os.path.join(root, "**", "*.rs"), recursive=True):
        src = open(path, encoding="utf-8").read()
        for m in re.finditer(r"#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;", src):
            d = os.path.dirname(path)
            stem = os.path.splitext(os.path.basename(path))[0]
            for cand in (os.path.join(d, m.group(1) + ".rs"), os.path.join(d, stem, m.group(1) + ".rs"),
                         os.path.join(d, m.group(1), "mod.rs")):
                if os.path.exists(cand):
                    tests.add(os.path.normpath(cand))
    return tests


LOG_CALL = re.compile(r"\b(?:e?print(?:ln)?|log::\w+|tracing::\w+|trace|debug|info|warn|error|dbg)!\s*\(")
PANIC_CALL = re.compile(r"\b(?:panic|unreachable|todo|unimplemented|assert(?:_eq|_ne)?|debug_assert\w*)!\s*\(|\.expect\(\s*$")
ENV_CALL = re.compile(r"\b(?:env::var(?:_os)?|var_os|var|getenv|option_env!|env!)\s*\(\s*$")
CLI_CALL = re.compile(r"\b(?:arg|flag|opt|has_flag|value_of)\(\s*$")
SQL = re.compile(r"\b(?:SELECT|INSERT INTO|UPDATE|DELETE FROM|CREATE|FROM|WHERE|JOIN|ORDER BY|pg_\w+)\b")
# weak emit contexts: a call argument, an assignment, a return value
EMIT_BEFORE = re.compile(
    r"(?:\bpush\(\s*$|\bset\(\s*$|\bwith\(\s*$|\bentry\(\s*$|\bname:\s*$|\bkind:\s*$|\btype_name:\s*$|\bstr\(\s*$|"
    r"\bput\(\s*$|\bkey\(\s*$|\bnew\(\s*$|\b(?:Some|Ok)\(\s*$|\breturn\s+$|=\s*$|,\s*$|\(\s*$|\[\s*$)")
# strong emit contexts: insert("X"...), Column::new("X"), a match arm's result, json!("X"), an El builder
STRONG_EMIT = re.compile(r"(?:\binsert\(\s*$|\bColumn::new\(\s*$|=>\s*$|json!\s*\(\s*$|\"\$polyType\"\s*:\s*$|"
                         r"\bEl::new\(\s*$|\.(?:put|str|int|bool|arr|obj|child|attr)\(\s*$)")
# read contexts: a JSON lookup by name, a comparison
READ_BEFORE = re.compile(
    r"(?:\.get(?:_mut)?\(\s*$|\[\s*$|\.contains_key\(\s*$|\.remove\(\s*$|\.starts_with\(\s*$|\.ends_with\(\s*$|"
    r"\.contains\(\s*$|==\s*$|!=\s*$|eq_ignore_ascii_case\(\s*$|\.strip_prefix\(\s*$|\.strip_suffix\(\s*$)")
# the web server's positional parameters, read as p.get(<index>, "<name>")
PARAM_BEFORE = re.compile(r"\bget\(\s*\d+\s*,\s*$")
POINTER_BEFORE = re.compile(r"\.pointer(?:_mut)?\(\s*$")
DISPATCH_AFTER = re.compile(r"\s*(?:\)*\s*=>|\|\s*\"|\|\s*$|\)\s*(?:\||=>))")
ROUTE_BEFORE = re.compile(r"\broute\(\s*$")
SERDE_NAME = re.compile(r"\b(?:rename|alias|tag|content|rename_all|serialize|deserialize)\s*=\s*$|\brename\(\s*(?:serialize|deserialize)\s*=\s*$")
AL_CALL_BEFORE = re.compile(r"\b(?:raise|raise_event|trigger|invoke|call|run_method|call_method|run_trigger)\w*\([^()]*,\s*$")
TZ_WORDS = {"UTC", "UCT", "GMT", "ZULU", "UNIVERSAL", "GREENWICH"}
TABLE_ROW = re.compile(r"\(\s*$")
TABLE_ROW_AFTER = re.compile(r"\s*,\s*-?\d+\s*\)")
FMT_PH = re.compile(r"\{[^{}]*\}")
NAME_TOKEN = re.compile(r"\$?[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*")
# AL language keywords and operators (the AL language reference on Microsoft Learn)
AL_KEYWORDS = set("""and or not xor div mod in true false begin end if then else repeat until while do for to downto
case of with exit var procedure trigger local internal protected temporary rec xrec currpage currreport
currfieldno this requestpage dataitem column table page codeunit report query xmlport enum interface
tableextension pageextension enumextension reportextension permissionset entitlement controladdin
dotnet profile label option""".split())

# Categories. A wire candidate is a literal bc-nst can put on the wire (or a name it accepts from the wire) that
# no real capture shows. The rest is noise for the wire claim, kept so nothing is silently dropped.
CATEGORY_DOC = collections.OrderedDict([
    ("wire-emit", "a name bc-nst writes into a message (json!, insert, El::new/put, a match arm's result, a call argument)"),
    ("wire-dispatch", "a match-arm pattern: a method, interaction or value name bc-nst accepts from the wire"),
    ("wire-read", "a name bc-nst looks up in a received message (.get(\"X\"), [\"X\"], comparisons)"),
    ("wire-param", "a positional-parameter label, p.get(i, \"name\"): the web server sends params as arrays in every capture, so the name only travels if named params are ever sent"),
    ("wire-type", "a dotted .NET type name"),
    ("wire-serde", "a name in a #[serde(rename/alias = ...)] attribute"),
    ("wire-format", "a format string that builds a name ({} placeholders) and fits no seen name or shape"),
    ("wire-path", "an HTTP route, URL path or JSON pointer whose segments were not all seen"),
    ("wire-guid", "a GUID constant that no capture shows and BCApps' app.json files don't name"),
    ("wire-other", "a name-like literal in no recognised context"),
    ("case-only", "seen, but only with different letter case"),
    ("dataset-only", "found only inside a binary data set, where column names and record values look alike: stream G's layout decides"),
    ("al-unused", "an entry of a (\"Name\", number) table whose name BCApps never uses in AL: not a wire name, but its origin needs a source"),
    ("al", "an AL word bc-nst reads from or raises in the AL model (property, value, trigger, event, procedure, built-in, keyword), as used in BCApps"),
    ("text", "a sentence: UI and error texts (stream C)"),
    ("test", "inside #[cfg(test)] code"),
    ("log", "in a log or print call"),
    ("panic", "in a panic, assert or expect message"),
    ("env", "an environment variable name"),
    ("cli", "a command-line flag"),
    ("own", "bc-rust's own names: hash seeds (bc-rust/...), template and index markers ($slot, $CommandBar), file names"),
    ("guid-bcapps", "a GUID that BCApps' app.json files name (an app id)"),
    ("attr", "in another attribute (serde default/with/skip functions, cfg features)"),
    ("sql", "SQL"),
    ("other", "anything else (date formats, time zone ids, punctuation-heavy strings)"),
])
WIRE_CATEGORIES = ["wire-emit", "wire-dispatch", "wire-read", "wire-param", "wire-type", "wire-serde", "wire-format",
                   "wire-path", "wire-guid", "wire-other", "case-only", "dataset-only"]
NOISE_CATEGORIES = [c for c in CATEGORY_DOC if c not in WIRE_CATEGORIES]

FEATURES = [
    ("lookup", r"(?i)lookup"),
    ("drill-down", r"(?i)drill"),
    ("assist-edit", r"(?i)assist"),
    ("rename", r"(?i)rename"),
    ("run-modal", r"(?i)runmodal|modal|endclient\w*call"),
    ("dialogs", r"(?i)dialog|confirm|strmenu|message|notif|prompt|wizard"),
    ("delete/insert", r"(?i)delete|insert|newrecord"),
    ("filter/view", r"(?i)filter|tableview|sorting|\bview|evaluate"),
    ("errors", r"(?i)exception|error|(?<!de)fault|fatal|diagnostic"),
    ("callbacks", r"(?i)callback|clientcall|automation|dotnet|addin|extensibility"),
    ("page-background-task", r"(?i)backgroundtask|pbt|pagebackground"),
    ("file-transfer", r"(?i)upload|download|file|stream|media|blob|image"),
    ("report", r"(?i)report|print"),
    ("actions", r"(?i)action|promot|commandbar|category_"),
    ("session", r"(?i)session|connection|company|login|logon|tenant|user|profile|permission|workdate"),
]


def feature_of(token, rel):
    for name, rx in FEATURES:
        if re.search(rx, token):
            return name
    if "metadata" in rel:
        return "metadata"
    if "dataset" in rel:
        return "dataset"
    if rel.endswith(("form.rs", "ui.rs")):
        return "forms"
    if rel.endswith(("http.rs",)):
        return "http"
    return "other"


class Seen:
    """Vocabulary lookups for pass 2."""

    def __init__(self, rows, guids, binstrings, bin_shapes):
        self.exact = {}
        self.lower = {}
        self.shapes = set()
        for name, kind, scn, step, layers in rows:
            self.exact.setdefault(name, (kind, scn, layers))
            self.lower.setdefault(name.lower(), name)
            if kind.endswith("-shape"):
                self.shapes.add(name)
        self.wild = [shape_regex(x) for x in sorted(self.shapes) if "*" in x]
        self.all_names = list(self.exact)
        self.guids = guids
        self.binstrings = binstrings
        self.bin_shapes = bin_shapes
        self.bin_wild = [shape_regex(x) for x in sorted(bin_shapes) if "*" in x]

    def shape_seen(self, shape):
        return shape in self.shapes or any(rx.match(shape) for rx in self.wild)

    def has(self, s):
        return s in self.exact or s in self.shapes or (generated_id(s) and self.shape_seen(gen_shape(s)))

    def in_binary(self, s):
        """Only inside a data set buffer, where a column name and a record value look the same."""
        if len(s) >= 4 and not s.isupper() and s in self.binstrings:
            return True
        shape = FMT_PH.sub("#", s) if FMT_PH.search(s) else (gen_shape(s) if generated_id(s) else None)
        return bool(shape) and (shape in self.bin_shapes or any(rx.match(shape) for rx in self.bin_wild))

    def case_only(self, s):
        return s not in self.exact and s.lower() in self.lower

    def fmt_seen(self, lit):
        """A format string with {} placeholders: seen if its digit shape is a recorded shape or some name fits it."""
        shape = FMT_PH.sub("#", lit)
        if self.shape_seen(shape):
            return True
        parts = FMT_PH.split(lit)
        rx = re.compile(".+?".join(re.escape(p) for p in parts) + r"\Z")
        lead = parts[0]
        for name in self.all_names:
            if name.startswith(lead) and rx.match(name):
                return True
        return False

    def path_seen(self, lit):
        segs = [s for s in FMT_PH.sub("#", lit).split("/") if s and s != "#" and not s.isdigit()]
        return all(self.has(s) or s.lower() in self.lower for s in segs)


def al_words(root):
    """Words AL source uses, from the BCApps checkout (vendor/bcapps) and the System.app AL in platform/ (both MIT):
    strict = property names and values, trigger and event names, data types, area names and the AL keywords;
    broad = strict plus variable and parameter names. Also the app ids in BCApps' app.json files."""
    dirs = [d for d in (os.path.join(root, "vendor", "bcapps"), os.path.join(root, "platform")) if os.path.isdir(d)]
    strict = set(AL_KEYWORDS)
    broad = set()
    procs = set()
    guids = set()
    if not dirs:
        return strict, strict, set(), {w.lower() for w in strict}, guids, dirs
    types = (r"(Record|Codeunit|Page|Report|Enum|Option|Text|Code|Decimal|Integer|BigInteger|Boolean|Date|Time|"
             r"DateTime|Duration|Guid|RecordId|RecordRef|FieldRef|KeyRef|Blob|Media|MediaSet|InStream|OutStream|"
             r"JsonObject|JsonArray|JsonToken|JsonValue|XmlDocument|HttpClient|Dictionary|List|Char|Byte|Label|"
             r"Variant|DateFormula|TestPage|Notification|ErrorInfo|FilterPageBuilder|Query|XmlPort|Interface|DotNet)\b")
    pats = [
        (strict, r"^\s*[A-Za-z][A-Za-z0-9]*\s*=\s*[A-Za-z][A-Za-z0-9]*(\s*,\s*[A-Za-z][A-Za-z0-9]*)*\s*;"),  # Name = Value;
        (strict, r"^\s*[A-Za-z][A-Za-z0-9]*\s*="),                                                    # property names
        (strict, r"\btrigger [A-Za-z][A-Za-z0-9]*\("),
        (strict, r"'On[A-Za-z0-9]+'"),                                                                 # event names
        (strict, r": " + types),
        (strict, r"^\s*area\([A-Za-z][A-Za-z0-9]*\)"),                                                 # area(Processing)
        (broad, r"\b[A-Za-z][A-Za-z0-9]*: " + types),                                                  # variables, parameters
        (procs, r"\bprocedure [A-Za-z][A-Za-z0-9]*\("),
        (procs, r"\b[A-Za-z][A-Za-z0-9]*\("),                                                          # calls, built-ins
    ]
    for target, pat in pats:
        r = subprocess.run(["grep", "-rhoE", "--include=*.al", pat] + dirs, capture_output=True, text=True,
                           errors="replace")
        for line in r.stdout.split("\n"):
            for w in re.findall(r"[A-Za-z][A-Za-z0-9]*", line):
                target.add(w)
    procs.discard("procedure")
    broad |= strict
    r = subprocess.run(["grep", "-rhoiE", "--include=app.json", r'"(id|appId)" *: *"[0-9a-f-]{36}"'] + dirs,
                       capture_output=True, text=True, errors="replace")
    for g in GUID.findall(r.stdout):
        guids.add(g.lower())
    return strict, broad, procs, {w.lower() for w in strict | procs}, guids, dirs


def classify(lit, before, after, attr, in_test, seen, al, line_text=""):
    """-> (category, token) or None when the literal is seen or is no name at all."""
    r = classify1(lit, before, after, attr, in_test, seen, al, line_text)
    if r and r[0] in WIRE_CATEGORIES and r[0] not in ("case-only", "wire-param") and seen.in_binary(lit):
        return ("dataset-only", lit)
    return r


def classify1(lit, before, after, attr, in_test, seen, al, line_text=""):
    al_strict, alw, al_procs, alw_lower, al_guids = al
    if not re.search(r"[A-Za-z]", lit):
        return None
    if in_test:
        cat = "test"
    elif attr is not None:
        cat = "wire-serde" if attr.startswith("serde") and SERDE_NAME.search(before) else "attr"
    elif LOG_CALL.search(before):
        cat = "log"
    elif PANIC_CALL.search(before):
        cat = "panic"
    elif ENV_CALL.search(before) or re.fullmatch(r"(?:BC|NST|RUST|CARGO|PG|DATABASE)_[A-Z0-9_]+", lit):
        cat = "env"
    elif CLI_CALL.search(before) or re.fullmatch(r"--?[a-z][a-z0-9\-]*", lit):
        cat = "cli"
    else:
        cat = None
    g = GUID.fullmatch(lit.strip("{}"))
    if g:
        if lit.strip("{}").lower() in seen.guids:
            return None
        return (cat or ("guid-bcapps" if lit.strip("{}").lower() in al_guids else "wire-guid"), lit)
    if re.search(r"(?i)\bbc-(?:rust|nst)\b|^nst-", lit) or STATIC_EXT.search(lit):
        return (cat or "own", lit)
    has_ph = bool(FMT_PH.search(lit)) and not re.match(r"^\s*[{\[]\s*\"", lit)
    # sentences: UI and error texts
    if re.search(r"[A-Za-z]{2,}\s+[A-Za-z]{2,}", lit) and not re.match(r"^\s*[{\[]\s*\"", lit):
        if has_ph and set(w.lower() for w in re.findall(r"[A-Za-z]+", FMT_PH.sub(" ", lit))) <= AL_KEYWORDS:
            return (cat or "al", lit)
        if cat:
            return (cat, lit)
        if SQL.search(lit):
            return ("sql", lit)
        return None if seen.has(lit) else ("text", lit)
    if has_ph:
        if cat:
            return (cat, lit)
        words = set(w.lower() for w in re.findall(r"[A-Za-z]+", FMT_PH.sub(" ", lit)))
        if words and words <= AL_KEYWORDS:
            return ("al", lit)
        if re.search(r":\s*\{|\{[^{}]*\}\s+\w|\w\s+\{|\(", lit):
            return ("text", lit)
        if re.fullmatch(r"(?:\{[^{}]*\})+(?:DT|D|T)", lit):
            return ("al", lit)  # AL date/time literal syntax (0D, 0T, 0DT)
        if lit.startswith("/"):
            return None if seen.path_seen(lit) else ("wire-path", lit)
        if seen.fmt_seen(lit):
            return None
        if not re.search(r"[A-Za-z]{2,}", FMT_PH.sub("", lit)):
            return None
        return ("wire-format", lit)
    if seen.has(lit):
        return None
    if cat:
        return (cat, lit)
    if POINTER_BEFORE.search(before) or (lit.startswith("/") and "/" in lit[1:]) or re.fullmatch(r"/[A-Za-z][\w.\-]*", lit) \
            or ROUTE_BEFORE.search(before):
        return None if seen.path_seen(lit) else ("wire-path", lit)
    if DOTTED.match(lit) and re.match(r"(?:Microsoft|System|DN|Newtonsoft)\.", lit):
        return ("wire-type", lit)
    if lit.startswith("$") and IDENT.match(lit):
        return ("own", lit)  # bc-rust's own $ markers; the wire's $ names ($polyType, $type) are in the vocabulary
    if lit in TZ_WORDS:
        return ("other", lit)
    if not (IDENT.match(lit) or NAME_TOKEN.fullmatch(lit)):
        if SQL.search(lit):
            return ("sql", lit)
        return ("other", lit)
    lower_al = lit.islower() and lit.lower() in alw_lower
    # ("Name", 3) rows: AL keyword-to-number tables. The name is input; only the number travels.
    if TABLE_ROW.search(before) and TABLE_ROW_AFTER.match(after):
        return ("al" if lit in al_strict or lit.lower() in alw_lower else "al-unused", lit)
    # an AL event or procedure bc-nst raises or calls by name
    if AL_CALL_BEFORE.search(before) and lit in al_procs:
        return ("al", lit)
    if PARAM_BEFORE.search(before):
        return ("case-only" if seen.case_only(lit) else "wire-param", lit)
    strong = (after.lstrip().startswith(":") and not after.lstrip().startswith("::")) or STRONG_EMIT.search(before) \
        or re.match(r"\s*\.(?:into|to_string|to_owned)\(\)", after) \
        or (not lit.islower() and f'"{lit.lower()}"' in line_text)  # "charallowed" read, "CharAllowed" written
    if strong and not lower_al:
        return ("case-only" if seen.case_only(lit) else "wire-emit", lit)
    # a capitalised match-arm pattern is a method or value name bc-nst accepts, even when AL uses the word too
    if DISPATCH_AFTER.match(after) and not lit.islower():
        return ("case-only" if seen.case_only(lit) else "wire-dispatch", lit)
    if lower_al or (lit in alw or lit in al_procs) and not strong and not seen.case_only(lit):
        return ("al", lit)
    if seen.case_only(lit):
        return ("case-only", lit)
    if strong:
        return ("wire-emit", lit)
    if DISPATCH_AFTER.match(after):
        return ("wire-dispatch", lit)
    if READ_BEFORE.search(before):
        return ("wire-read", lit)
    if EMIT_BEFORE.search(before):
        return ("wire-emit", lit)
    return ("wire-other", lit)


def pass2(root, src_dir, seen, al):
    rows = []
    dropped = collections.Counter()
    test_files = external_test_files(src_dir)
    for path in sorted(glob.glob(os.path.join(src_dir, "**", "*.rs"), recursive=True)):
        src = open(path, encoding="utf-8").read()
        rel = os.path.relpath(path, root)
        lines = [0]
        for m in re.finditer("\n", src):
            lines.append(m.end())
        lits, regions = rust_literals(src)
        file_test = os.path.normpath(path) in test_files
        for start, end, lit, attr in lits:
            line = bisect.bisect_right(lines, start)
            ls = lines[line - 1]
            le = src.find("\n", start)
            le = len(src) if le < 0 else le
            # the code before the literal: this line, plus the previous one when this line starts with it
            before = src[ls:start]
            if not before.strip() and line >= 2:
                before = src[lines[line - 2]:start]
            after = src[end:le]
            in_test = file_test or any(a <= start < b for a, b in regions)
            # a literal on a continuation line of a log call
            stmt_start = max(src.rfind(";", 0, start), src.rfind("{\n", 0, start), src.rfind("}\n", 0, start))
            stmt = src[stmt_start + 1:start] if stmt_start >= 0 else before
            if not LOG_CALL.search(before) and LOG_CALL.search(stmt) and not re.search(r"\bjson!|insert\(", stmt):
                before = stmt
            r = classify(lit, before, after, attr, in_test, seen, al, src[ls:le])
            if r is None:
                dropped["seen-or-not-a-name"] += 1
                continue
            cat, token = r
            context = src[ls:le].strip()
            rows.append((rel, line, token, cat, feature_of(token, os.path.relpath(path, src_dir)), context))
    return rows, dropped


def esc(s, limit=None):
    s = s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n").replace("\r", "\\r")
    if limit and len(s) > limit:
        s = s[:limit] + "..."
    return s


# --------------------------------------------------------------------------------------------------------------
# report


def report(vocab_rows, used, v, unseen, dropped, audit_dir, out_md):
    L = []
    kinds = collections.Counter(r[1] for r in vocab_rows)
    layers = collections.Counter(r[4] for r in vocab_rows)
    L.append(f"# wirevocab summary\n")
    L.append(f"Vocabulary: {len(vocab_rows)} names ({len(set(r[0] for r in vocab_rows))} distinct strings).\n")
    L.append("| kind | names |\n|---|---|")
    for k, n in sorted(kinds.items(), key=lambda x: -x[1]):
        L.append(f"| {k} | {n} |")
    L.append("\n| layers | names |\n|---|---|")
    for k, n in sorted(layers.items(), key=lambda x: -x[1]):
        L.append(f"| {k} | {n} |")
    L.append("\nCapture files read (records):\n")
    for s, root, rel, n in used:
        L.append(f"- {s}: `{rel}` in {root} ({n})")
    if v is not None:
        L.append("\nKeys whose values became enum names: " + ", ".join(f"{k} ({n})" for k, n in sorted(v.enum_keys)))
    L.append("\n## Unseen bc-nst literals\n")
    by_cat = collections.defaultdict(list)
    for r in unseen:
        by_cat[r[3]].append(r)
    L.append("| category | occurrences | distinct literals | wire candidate | meaning |\n|---|---|---|---|---|")
    for cat in WIRE_CATEGORIES + NOISE_CATEGORIES:
        rs = by_cat.get(cat, [])
        L.append(f"| {cat} | {len(rs)} | {len(set(r[2] for r in rs))} | {'yes' if cat in WIRE_CATEGORIES else 'no'} "
                 f"| {CATEGORY_DOC[cat]} |")
    L.append(f"\nDropped as seen in the vocabulary or not a name (no letters): {dropped['seen-or-not-a-name']}.")
    wire = [r for r in unseen if r[3] in WIRE_CATEGORIES]
    L.append(f"\nWire candidates: {len(wire)} occurrences, {len(set(r[2] for r in wire))} distinct.\n")
    L.append("## Unseen wire candidates by feature\n")
    feats = collections.defaultdict(lambda: collections.OrderedDict())
    for r in wire:
        d = feats[r[4]]
        if r[2] not in d:
            d[r[2]] = [r, 0]
        d[r[2]][1] += 1
    for feat, d in sorted(feats.items(), key=lambda x: -len(x[1])):
        L.append(f"### {feat} ({len(d)})\n")
        for tok, (r, cnt) in d.items():
            L.append(f"- `{esc(tok, 90)}` {r[3]}, {r[0]}:{r[1]}" + (f" (x{cnt})" if cnt > 1 else ""))
        L.append("")
    if audit_dir and os.path.isdir(audit_dir):
        vocab_names = set(r[0] for r in vocab_rows)
        unseen_tokens = set(r[2] for r in unseen)
        L.append("## Cross-check with the audit's rough token lists\n")
        for fn in sorted(glob.glob(os.path.join(audit_dir, "bcnst_tokens*.txt"))):
            toks = [t.strip() for t in open(fn, encoding="utf-8") if t.strip()]
            inv = [t for t in toks if t in vocab_names]
            inu = [t for t in toks if t in unseen_tokens]
            L.append(f"- `{os.path.basename(fn)}`: {len(toks)} tokens, {len(inv)} in the vocabulary, "
                     f"{len(inu)} in the unseen list, {len(toks) - len(set(inv) | set(inu))} in neither (identifiers or comments only)")
            if fn.endswith("_seen.txt"):
                miss = [t for t in toks if t not in vocab_names]
                if miss:
                    L.append(f"  - audit says seen, not in this vocabulary: {', '.join(miss)}")
    text = "\n".join(L) + "\n"
    if out_md:
        with open(out_md, "w", encoding="utf-8") as fh:
            fh.write(text)
    return text


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--repo", default=REPO)
    ap.add_argument("--captures", default=None, help="root 'captures' (default <repo>/data/capture)")
    ap.add_argument("--corpus", default=CORPUS, help="root 'cleanroom', the clean-room capture corpus "
                                                    "(default $BC_RUST_VOLUME/cleanroom/captures)")
    ap.add_argument("--sets", default=os.path.join(HERE, "sets.tsv"))
    ap.add_argument("--vocab", default=None, help="vocabulary TSV (default <repo>/docs/wire-vocab.tsv)")
    ap.add_argument("--unseen", default=os.path.join(CLEANROOM, "unseen.tsv"))
    ap.add_argument("--summary", default=os.path.join(CLEANROOM, "summary.md"))
    ap.add_argument("--audit", default=os.path.join(VOLUME, "legal", "work", "out"),
                    help="directory with the audit's bcnst_tokens*.txt, for the cross-check only")
    ap.add_argument("--quiet", action="store_true")
    a = ap.parse_args()
    root = os.path.abspath(a.repo)
    captures = a.captures or os.path.join(root, "data", "capture")
    vocab_path = a.vocab or os.path.join(root, "docs", "wire-vocab.tsv")
    _, used, v = pass1({"captures": captures, "cleanroom": os.path.abspath(a.corpus)}, a.sets, vocab_path)
    rows = load_vocab(vocab_path)
    seen = Seen(rows, v.guids, v.binstrings, v.bin_shapes)
    al_strict, alw, al_procs, alw_lower, al_guids, al_dirs = al_words(root)
    if not al_dirs:
        print("wirevocab: no vendor/bcapps or platform/ AL found; AL words are not told apart", file=sys.stderr)
    unseen, dropped = pass2(root, os.path.join(root, "crates", "bc-nst", "src"), seen, (al_strict, alw, al_procs, alw_lower, al_guids))
    os.makedirs(os.path.dirname(a.unseen), exist_ok=True)
    tmp = a.unseen + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.write("file\tline\tliteral\tcategory\tfeature\tcontext\n")
        for rel, line, tok, cat, feat, ctx in unseen:
            fh.write(f"{rel}\t{line}\t{esc(tok)}\t{cat}\t{feat}\t{esc(ctx, 160)}\n")
    os.replace(tmp, a.unseen)
    text = report(rows, used, v, unseen, dropped, a.audit, a.summary)
    if not a.quiet:
        print(text)
    print(f"wirevocab: {len(rows)} names -> {vocab_path}; {len(unseen)} unseen literals -> {a.unseen}; summary {a.summary}",
          file=sys.stderr)


if __name__ == "__main__":
    main()
