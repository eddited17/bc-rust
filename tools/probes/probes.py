#!/usr/bin/env python3
"""tools/probes: run grid probes (our own AL code) on a real Business Central and keep the results.

  probes.py gen     SPEC...             generate the AL app into the work directory (no VM needed)
  probes.py run     SPEC... [--ctx C]   generate, compile, publish, run, fetch JSONL into the evidence directory
  probes.py vectors PROBE_ID...|--all   compile the archived runs of probes into tests/vectors/<area>/<id>.tsv
  probes.py list                        list probe specs and archived runs

SPEC is a probe spec (.toml), a hand-written probe codeunit (.al), or the id of a spec in tools/probes/probes/.
See README.md for the spec format, the line format and the vectors format. Python 3.11+, standard library only.
"""
import argparse
import datetime as dt
import fcntl
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
SPEC_DIR = HERE / 'probes'
AL_DIR = HERE / 'al'
VM_SCRIPTS = HERE / 'vm'

APP_ID = '6d9c6cd1-1f06-40da-b858-c6681e78dd2e'
APP_NAME = 'bc-rust probes'
APP_PUBLISHER = 'bc-rust'
ID_FROM, ID_TO = 50400, 50499
REGISTRY_ID = 50402
FIRST_PROBE_OBJ = 50410
RUNNER_CODEUNIT = 50401
MAX_CELLS = 200_000

ID_RE = re.compile(r'^[a-z0-9][a-z0-9-]{0,62}$')
AREA_RE = re.compile(r'^[a-z0-9][a-z0-9_-]{0,62}$')
DIM_RE = re.compile(r'^[A-Za-z][A-Za-z0-9_]{0,29}$')
RESERVED_DIMS = {'result', 'out', 'skip', 'log'}


class ProbeError(Exception):
    pass


# ---------------------------------------------------------------------------------------------- configuration

def load_config():
    """Defaults, then ~/.config/bc-rust/probes.env (or $PROBES_CONFIG), then the environment."""
    home = Path.home()
    cfg = {
        'PROBES_EVIDENCE': str(home / '.local/share/bc-rust-probes/evidence'),
        'PROBES_WORK': str(home / '.local/share/bc-rust-probes/work'),
        'PROBES_LOCK': '',
        'PROBES_SSH_KEY': str(REPO / 'data/vm/bc-vm_ed25519'),
        'PROBES_SSH_TARGET': 'bcadmin@127.0.0.1',
        'PROBES_SSH_PORT': '2222',
        'PROBES_CREDENTIALS': str(REPO / 'data/vm/credentials.txt'),
        'PROBES_CONTAINER': 'bcside',
        'PROBES_COMPANY': 'CRONUS International Ltd.',
        'PROBES_VM_DIR': 'C:/ProgramData/BcContainerHelper/bcrprobes',
        'PROBES_TARGET': 'Cloud',
    }
    f = Path(os.environ.get('PROBES_CONFIG', home / '.config/bc-rust/probes.env'))
    if f.is_file():
        for line in f.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith('#') and '=' in line:
                k, v = line.split('=', 1)
                cfg[k.strip()] = v.strip().strip('"')
    for k in list(cfg):
        if k in os.environ:
            cfg[k] = os.environ[k]
    if not cfg['PROBES_LOCK']:
        cfg['PROBES_LOCK'] = str(Path(cfg['PROBES_WORK']) / 'probes.lock')
    return cfg


def read_credentials(path):
    """bc_user=... and bc_password=... lines (repo data/vm/credentials.txt). Never printed."""
    user = pw = None
    for line in Path(path).read_text().splitlines():
        if line.startswith('bc_user='):
            user = line.split('=', 1)[1].strip()
        elif line.startswith('bc_password='):
            pw = line.split('=', 1)[1].strip()
    if not user or not pw:
        raise ProbeError(f'no bc_user/bc_password in {path}')
    return user, pw


# ---------------------------------------------------------------------------------------------- probe specs

class Dim:
    def __init__(self, name, al_type, values, raw=False):
        self.name, self.al_type, self.values, self.raw = name, al_type, values, raw


class Probe:
    def __init__(self, **kw):
        self.id = kw['id']
        self.area = kw.get('area', 'misc')
        self.title = kw.get('title', '')
        self.path = kw['path']
        self.kind = kw['kind']                  # 'grid' or 'al'
        self.dims = kw.get('dims', [])
        self.vars = kw.get('vars', '')
        self.body = kw.get('body', '')
        self.procs = kw.get('procs', '')
        self.keep_writes = kw.get('keep_writes', False)
        self.contexts = kw.get('contexts', [])
        self.files = {}                         # hand-written: {file name: AL text}
        self.resources = []                     # hand-written: layout files
        self.entry = None                       # hand-written: entry codeunit name
        self.contract = 'grid' if self.kind == 'grid' else None
        self.steps = []
        self.background = bool(kw.get('background', False))
        self.extract = None                     # hand-written: script that turns the run's JSONL into rows
        self.inputs = []                        # hand-written: (file, name) packaged as app resources
        self.obj_id = None
        self.obj_name = None
        self.al = None                          # generated AL text of this probe's codeunit

    @property
    def cells(self):
        if self.kind != 'grid':
            return 0
        return math.prod(len(d.values) for d in self.dims) if self.dims else 1


def resolve_spec(arg):
    """'PATH', 'ID' (a spec in tools/probes/probes/) or 'ID=PATH' (sets the probe id of a hand-written probe).
    PATH: a .toml grid spec, a hand-written .al file, or a directory of .al files plus layouts."""
    pid = None
    if '=' in arg and not Path(arg).exists():
        pid, arg = arg.split('=', 1)
    p = Path(arg)
    if (p.suffix in ('.toml', '.al') and p.is_file()) or p.is_dir():
        return p.resolve(), pid
    for ext in ('.toml', '.al'):
        q = SPEC_DIR / f'{arg}{ext}'
        if q.is_file():
            return q, pid
    raise ProbeError(f'no probe spec {arg!r} (a .toml/.al file, a directory, or an id in {SPEC_DIR})')


def load_spec(arg, area=None):
    path, pid = resolve_spec(arg)
    return load_probe(path, pid, area)


def load_probe(path, pid=None, area=None):
    path = Path(path)
    if path.suffix == '.al' or path.is_dir():
        return load_raw_al(path, pid, area)
    with open(path, 'rb') as f:
        s = tomllib.load(f)
    pid = s.get('id') or path.stem
    if not ID_RE.match(pid):
        raise ProbeError(f'{path}: id {pid!r} must match {ID_RE.pattern}')
    area = s.get('area', 'misc')
    if not AREA_RE.match(area):
        raise ProbeError(f'{path}: area {area!r} must match {AREA_RE.pattern}')
    dims = []
    for d in s.get('dim', []):
        name = d['name']
        if not DIM_RE.match(name) or name.lower().startswith('bcr') or name.lower() in RESERVED_DIMS:
            raise ProbeError(f'{path}: dimension name {name!r} is not allowed')
        values = list(d.get('values', []))
        for g in d.get('gen', []):
            values += gen_values(g)
        values = dedup(values)
        if not values:
            raise ProbeError(f'{path}: dimension {name} has no values')
        dims.append(Dim(name, d.get('type', 'Text'), values, raw=d.get('raw', False)))
    names = [d.name.lower() for d in dims]
    if len(set(names)) != len(names):
        raise ProbeError(f'{path}: duplicate dimension names')
    if 'body' not in s:
        raise ProbeError(f'{path}: no body')
    p = Probe(id=pid, area=area, title=s.get('title', ''), path=path, kind='grid', dims=dims,
              vars=s.get('vars', ''), body=s['body'], procs=s.get('procs', ''),
              keep_writes=bool(s.get('keep_writes', False)), contexts=list(s.get('contexts', [])))
    if p.cells > MAX_CELLS:
        raise ProbeError(f'{path}: {p.cells} cells (limit {MAX_CELLS})')
    return p


OBJ_RE = re.compile(r'^(\s*)(table|tableextension|page|pageextension|report|reportextension|codeunit|query|xmlport|'
                    r'enum|enumextension|permissionset|permissionsetextension)(\s+)(\d+)(\s+)("[^"]+"|[A-Za-z_]\w*)',
                    re.M | re.I)
RES_EXT = ('.rdlc', '.rdl', '.docx', '.xlsx', '.xlf')


def load_raw_al(path, pid=None, area=None):
    """A hand-written probe: one .al file or a directory of .al files (plus layout files such as .rdlc).
    Object ids are placeholders; build_app renumbers every object into the app's range. Entry contracts:
      drive: a codeunit with procedure Drive(var Log: Codeunit "BCR Probe Log")
      take:  a codeunit with procedure RunAll(): Text and Take(): Text; each public parameterless P* procedure
             runs as its own step (then Take()), so one step's failure or slowness does not lose the others
      text:  procedure RunAll(): Text, one JSON object per line
      array: procedure RunAll(var Results: JsonArray)
    Header comments (any file, first 30 lines): '// probe-id: x', '// area: y', '// title: z',
    '// contexts: lang=1033; locale=de-DE&lang=1031', '// background: true' (run in a background session, needed
    for Report.SaveAs), '// extract: extract.py' (default: extract.py in the probe directory; it gets the archived
    JSONL path and prints one JSON object per row, archived as <ts>.rows.jsonl and used by 'vectors')."""
    files = sorted(path.glob('*.al')) if path.is_dir() else [path]
    if not files:
        raise ProbeError(f'{path}: no .al files')
    texts = {f.name: f.read_text(encoding='utf-8-sig') for f in files}
    head = {}
    for t in texts.values():
        for line in t.splitlines()[:30]:
            m = re.match(r'\s*//\s*(probe-id|area|title|contexts|background|extract)\s*:\s*(.*?)\s*$', line)
            if m and m.group(1) not in head:
                head[m.group(1)] = m.group(2)
    if not pid:
        pid = head.get('probe-id') or re.sub(r'[^a-z0-9-]+', '-', (path.name if path.is_dir() else
                                                                 path.name.split('.')[0]).lower()).strip('-')
    if not ID_RE.match(pid):
        raise ProbeError(f'{path}: probe id {pid!r} must match {ID_RE.pattern} (use ID=PATH or "// probe-id: ...")')
    resources = sorted(f for f in path.iterdir() if f.suffix.lower() in RES_EXT) if path.is_dir() else []
    inputs = sorted(f for f in (path / 'inputs').rglob('*') if f.is_file()) if (path / 'inputs').is_dir() else []
    entry = contract = None
    steps = []
    for fn, t in texts.items():
        for m in re.finditer(r'^\s*codeunit\s+\d+\s+("[^"]+"|[A-Za-z_]\w*)', t, re.M | re.I):
            name = m.group(1).strip('"')
            nxt = re.search(r'^\s*(table|codeunit|page|report|query|xmlport|enum)\s+\d+', t[m.end():], re.M | re.I)
            body = t[m.end(): m.end() + nxt.start()] if nxt else t[m.end():]
            if re.search(r'procedure\s+Drive\s*\(\s*var\s+\w+\s*:\s*Codeunit\s+"BCR Probe Log"\s*\)', body, re.I):
                entry, contract = name, 'drive'
                break
            if re.search(r'^\s*procedure\s+RunAll\s*\(\s*\)\s*(\w+\s*)?:\s*Text', body, re.M | re.I):
                cand = 'text'
                if re.search(r'^\s*procedure\s+Take\s*\(\s*\)', body, re.M | re.I):
                    cand = 'take'
                    pub = re.findall(r'^    procedure\s+(P\w*)\s*\(\s*\)\s*;?\s*$', body, re.M)
                    run_all = re.search(r'procedure\s+RunAll[^\n]*\n(.*?)\n    end;', body, re.S)
                    order = re.findall(r'^\s*(P\w*)\s*\(\s*\)\s*;', run_all.group(1), re.M) if run_all else []
                    steps = [x for x in order if x in pub] + [x for x in pub if x not in order]
                if contract not in ('drive',):
                    entry, contract = name, cand
            elif re.search(r'procedure\s+RunAll\s*\(\s*var\s+\w+\s*:\s*JsonArray\s*\)', body, re.I) and not contract:
                entry, contract = name, 'array'
        if contract == 'drive':
            break
    if not contract:
        raise ProbeError(f'{path}: no entry codeunit (Drive(var Log: Codeunit "BCR Probe Log"), RunAll(): Text or '
                         'RunAll(var Results: JsonArray))')
    ctx = [c.strip() for c in head.get('contexts', '').split(';') if c.strip()]
    pr = Probe(id=pid, area=area or head.get('area', pid), title=head.get('title', ''), path=path, kind='al',
               contexts=ctx)
    pr.files, pr.resources, pr.entry, pr.contract, pr.steps = texts, resources, entry, contract, steps
    pr.inputs = [(f, f.relative_to(path / 'inputs').as_posix()) for f in inputs]
    pr.background = head.get('background', '').lower() in ('1', 'true', 'yes')
    ex = head.get('extract') or ('extract.py' if path.is_dir() and (path / 'extract.py').is_file() else None)
    pr.extract = (path if path.is_dir() else path.parent) / ex if ex else None
    return pr


def dedup(values):
    seen, out = set(), []
    for v in values:
        k = (type(v).__name__, v if not isinstance(v, (dt.date, dt.time)) else v.isoformat())
        if k not in seen:
            seen.add(k)
            out.append(v)
    return out


# ---------------------------------------------------------------------------------------------- value generators

def gen_values(g):
    kind = g.get('kind')
    if kind == 'nines':
        # 0, 1, 9, 10, 99, 100, ... up to 10^max_digits; optional negatives and decimal fractions
        maxd = int(g.get('max_digits', 5))
        ints = [0, 1]
        for k in range(1, maxd + 1):
            ints += [10 ** k - 1, 10 ** k]
        if g.get('signs'):
            ints += [-x for x in ints if x]
        fr = g.get('fractions')
        if not fr:
            return ints
        out = []
        for x in ints:
            out.append(str(x))
            for f in fr:
                out.append(f'{x}{f}' if not f.startswith('.') or x >= 0 else f'-{abs(x)}{f}')
        return out
    if kind == 'range':
        return list(range(int(g['from']), int(g['to']) + 1, int(g.get('step', 1))))
    if kind == 'text_lengths':
        fill = g.get('fill', 'A')
        lo, hi = int(g.get('from', 1)), int(g.get('to', 40))
        return [(fill * (n // len(fill) + 1))[:n] for n in range(lo, hi + 1)]
    if kind == 'chars':
        return [chr(c) for c in range(int(g['from']), int(g['to']) + 1)]
    if kind == 'month_ends':
        out = []
        for y in g.get('years', [g.get('year', 2024)]):
            for m in range(1, 13):
                nxt = dt.date(y + (m == 12), m % 12 + 1, 1)
                out.append((nxt - dt.timedelta(days=1)).isoformat())
        return out
    if kind == 'dates':
        a = dt.date.fromisoformat(str(g['from']))
        b = dt.date.fromisoformat(str(g['to']))
        step = int(g.get('step_days', 1))
        out = []
        while a <= b:
            out.append(a.isoformat())
            a += dt.timedelta(days=step)
        return out
    if kind == 'int_limits':
        return [2147483647, -2147483647, 2147483646, -2147483646]
    raise ProbeError(f'unknown generator kind {kind!r}')


# ---------------------------------------------------------------------------------------------- AL rendering

def al_quote(s):
    return "'" + s.replace("'", "''") + "'"


def al_text_expr(s):
    """AL expression for a text value: literal chunks plus BcrCh(n) for control characters."""
    parts, buf = [], ''
    for ch in s:
        o = ord(ch)
        if o < 32 or o == 127 or 0x80 <= o <= 0x9f:
            if buf:
                parts.append(al_quote(buf))
                buf = ''
            parts.append(f'BcrCh({o})')
        else:
            buf += ch
            if len(buf) >= 200:
                parts.append(al_quote(buf))
                buf = ''
    if buf or not parts:
        parts.append(al_quote(buf))
    return ' + '.join(parts)


def al_date(s):
    s = str(s)
    if s in ('', '0D'):
        return '0D'
    closing = s.startswith('C')
    d = dt.date.fromisoformat(s[1:] if closing else s)
    lit = f'{d.year:04d}{d.month:02d}{d.day:02d}D'
    return f'ClosingDate({lit})' if closing else lit


def al_time(s):
    s = str(s)
    if s in ('', '0T'):
        return '0T'
    m = re.match(r'^(\d{2}):(\d{2}):(\d{2})(\.\d{1,3})?$', s)
    if not m:
        raise ProbeError(f'time {s!r}: use HH:MM:SS[.fff]')
    return f'{m.group(1)}{m.group(2)}{m.group(3)}{m.group(4) or ""}T'


def al_bigint_stmts(v):
    if -2147483647 <= v <= 2147483647:
        return [f'R := {v};']
    a, chunks = abs(v), []
    while a:
        a, r = divmod(a, 1000000)
        chunks.insert(0, r)
    st = [f'R := {chunks[0]};'] + [f'R := R * 1000000 + {c};' for c in chunks[1:]]
    if v < 0:
        st.append('R := -R;')
    return st


def value_stmts(dim, v):
    """AL statements that set R (the value function's return) to value v."""
    t = dim.al_type.strip()
    tl = t.lower()
    if dim.raw:
        return [f'R := {v};']
    if tl == 'integer':
        if not isinstance(v, int) or isinstance(v, bool) or not -2147483648 < v <= 2147483647:
            raise ProbeError(f'{dim.name}: {v!r} is not an Integer')
        return [f'R := {v};']
    if tl == 'biginteger':
        return al_bigint_stmts(int(v))
    if tl == 'decimal':
        if isinstance(v, float):
            raise ProbeError(f'{dim.name}: give Decimal values as strings or integers, not floats ({v!r})')
        s = str(v)
        if not re.match(r'^-?\d+(\.\d+)?$', s):
            raise ProbeError(f'{dim.name}: {s!r} is not a decimal literal')
        return [f'R := {s};']
    if tl == 'boolean':
        if not isinstance(v, bool):
            raise ProbeError(f'{dim.name}: {v!r} is not a Boolean')
        return [f'R := {"true" if v else "false"};']
    if tl == 'date':
        return [f'R := {al_date(v)};']
    if tl == 'time':
        return [f'R := {al_time(v)};']
    if tl == 'datetime':
        s = str(v)
        if s in ('', '0DT'):
            return ['R := 0DT;']
        d, _, tm = s.partition('T')
        return [f'R := CreateDateTime({al_date(d)}, {al_time(tm)});']
    if tl == 'char':
        code = v if isinstance(v, int) else ord(str(v))
        return [f'R := {code};']
    if tl == 'guid':
        return [f'Evaluate(R, {al_quote(str(v))});']
    if tl.startswith('text') or tl.startswith('code'):
        return [f'R := {al_text_expr(str(v))};']
    raise ProbeError(f'{dim.name}: type {t!r} needs raw = true (values are AL expressions)')


def json_fragment(dim, v):
    """JSON text of an input value as recorded in the lines: integers and booleans native, the rest strings."""
    tl = dim.al_type.strip().lower()
    if not dim.raw and isinstance(v, bool):
        return 'true' if v else 'false'
    if not dim.raw and tl == 'integer':
        return str(int(v))
    if isinstance(v, (dt.date, dt.datetime, dt.time)):
        v = v.isoformat()
    return json.dumps(str(v), ensure_ascii=True)


def ind(text, n):
    pad = ' ' * n
    return '\n'.join((pad + line if line.strip() else '') for line in text.strip('\n').splitlines())


def obj_name(pid, used):
    name = f'BCRP {pid}'
    if len(name) > 30:
        name = name[:26]
        i = 1
        while f'{name}~{i}' in used:
            i += 1
        name = f'{name}~{i}'
    used.add(name)
    return name


def gen_grid_codeunit(p):
    n = p.cells
    dims = p.dims
    lines = []
    w = lines.append
    w(f'// Generated by tools/probes from {p.path.name} (probe {p.id}); do not edit.')
    w(f'codeunit {p.obj_id} "{p.obj_name}"')
    w('{')
    w('    var')
    w('        BcrCellNo: Integer;')
    w('        BcrI: array[32] of Integer;')
    w('        BcrDone: Boolean;')
    w('        BcrErr: Text;')
    w('        BcrErrCode: Text;')
    w('        Result: Text;')
    w('        Out: JsonObject;')
    w('        Skip: Boolean;')
    w("        BcrRollbackTok: Label 'BCR-PROBE-ROLLBACK', Locked = true;")
    for d in dims:
        w(f'        {d.name}: {d.al_type};')
    w('')
    w('    trigger OnRun()')
    w('    begin')
    w('        BcrIndex(BcrCellNo);')
    w('        BcrLoad();')
    w('        ClearLastError();')
    w('        BcrCell();')
    w('        BcrErr := GetLastErrorText();')
    w('        BcrErrCode := GetLastErrorCode();')
    w('        BcrDone := true;')
    if not p.keep_writes:
        w('        Error(BcrRollbackTok);')
    w('    end;')
    w('')
    w('    local procedure BcrCell()')
    if p.vars.strip():
        w('    var')
        w(ind(p.vars, 8))
    w('    begin')
    w(ind(p.body, 8))
    w('    end;')
    w('')
    if p.procs.strip():
        w(ind(p.procs, 4))
        w('')
    w('    procedure CellCount(): Integer')
    w('    begin')
    w(f'        exit({n});')
    w('    end;')
    w('')
    w('    procedure Drive(var Log: Codeunit "BCR Probe Log")')
    w('    var')
    w(f'        BcrRunner: Codeunit "{p.obj_name}";')
    w('        BcrInput: JsonObject;')
    w('        BcrC: Integer;')
    w('        BcrOk: Boolean;')
    w('        BcrLang: Integer;')
    w('        BcrWDate: Date;')
    w('        BcrErrTxt: Text;')
    w('        BcrErrCd: Text;')
    w('    begin')
    w('        BcrLang := GlobalLanguage();')
    w('        BcrWDate := WorkDate();')
    w(f'        for BcrC := 1 to {n} do begin')
    w('            Clear(BcrRunner);')
    w('            BcrRunner.BcrSetCell(BcrC);')
    w('            ClearLastError();')
    w('            BcrOk := BcrRunner.Run();')
    w('            BcrErrTxt := GetLastErrorText();')
    w('            BcrErrCd := GetLastErrorCode();')
    w('            if BcrRunner.BcrIsDone() then begin')
    w('                // the body ran to its end; an error here is the rollback marker')
    w('                BcrOk := true;')
    w('                BcrErrTxt := BcrRunner.BcrErrText();')
    w('                BcrErrCd := BcrRunner.BcrErrCodeText();')
    w('            end else')
    w('                if BcrErrTxt = BcrRollbackTok then')
    w("                    BcrErrTxt := 'BCR: cell state lost after rollback';")
    if p.keep_writes:
        w('            Commit();')
    w('            if not BcrRunner.BcrIsSkipped() then begin')
    w('                Clear(BcrInput);')
    w('                BcrIndex(BcrC);')
    w('                BcrInput.ReadFrom(BcrInputJson());')
    w('                Log.CellFull(BcrC, BcrInput, BcrOk, BcrRunner.BcrResult(), BcrErrTxt, BcrErrCd, BcrRunner.BcrOut());')
    w('            end;')
    w('            ClearLastError();')
    w('            GlobalLanguage(BcrLang);')
    w('            WorkDate(BcrWDate);')
    w('        end;')
    w('    end;')
    w('')
    w('    procedure BcrSetCell(C: Integer)')
    w('    begin')
    w('        BcrCellNo := C;')
    w('    end;')
    w('')
    for name, typ, expr in (('BcrIsDone', 'Boolean', 'BcrDone'), ('BcrIsSkipped', 'Boolean', 'Skip'),
                            ('BcrResult', 'Text', 'Result'),
                            ('BcrErrText', 'Text', 'BcrErr'), ('BcrErrCodeText', 'Text', 'BcrErrCode'),
                            ('BcrOut', 'JsonObject', 'Out')):
        w(f'    procedure {name}(): {typ}')
        w('    begin')
        w(f'        exit({expr});')
        w('    end;')
        w('')
    # index decomposition: the last dimension varies fastest
    w('    local procedure BcrIndex(C: Integer)')
    w('    var')
    w('        R: Integer;')
    w('    begin')
    w('        R := C - 1;')
    for k in range(len(dims) - 1, -1, -1):
        sz = len(dims[k].values)
        w(f'        BcrI[{k + 1}] := R mod {sz} + 1;')
        w(f'        R := R div {sz};')
    w('    end;')
    w('')
    w('    local procedure BcrLoad()')
    w('    begin')
    for k, d in enumerate(dims):
        w(f'        {d.name} := BcrV{k + 1}(BcrI[{k + 1}]);')
    w('    end;')
    w('')
    w('    local procedure BcrInputJson(): Text')
    w('    begin')
    if dims:
        parts = []
        for k, d in enumerate(dims):
            sep = '{' if k == 0 else ','
            parts.append(al_quote(f'{sep}"{d.name}":') + f' + BcrJ{k + 1}(BcrI[{k + 1}])')
        w('        exit(' + ' + '.join(parts) + " + '}');")
    else:
        w("        exit('{}');")
    w('    end;')
    w('')
    for k, d in enumerate(dims):
        w(f'    local procedure BcrV{k + 1}(I: Integer) R: {d.al_type}')
        w('    begin')
        w('        case I of')
        for i, v in enumerate(d.values, 1):
            st = value_stmts(d, v)
            if len(st) == 1:
                w(f'            {i}:')
                w(f'                {st[0]}')
            else:
                w(f'            {i}:')
                w('                begin')
                for s in st:
                    w(f'                    {s}')
                w('                end;')
        w('        end;')
        w('    end;')
        w('')
        w(f'    local procedure BcrJ{k + 1}(I: Integer): Text')
        w('    begin')
        w('        case I of')
        for i, v in enumerate(d.values, 1):
            w(f'            {i}:')
            w(f'                exit({al_quote(json_fragment(d, v))});')
        w('        end;')
        w('    end;')
        w('')
    w('    local procedure BcrCh(N: Integer) T: Text')
    w('    var')
    w('        Ch: Char;')
    w('    begin')
    w('        Ch := N;')
    w("        T := 'x';")
    w('        T[1] := Ch;')
    w('    end;')
    w('}')
    return '\n'.join(lines) + '\n'


def renumber_raw(p, alloc):
    """Renumber every object of a hand-written probe; point layout paths at res/<probe id>/. Returns
    {file name: text}."""
    out = {}
    res_names = {r.name for r in p.resources}
    old_ids = []
    for fn, t in p.files.items():
        def sub(m):
            typ = m.group(2).lower()
            alloc[typ] = alloc.get(typ, FIRST_PROBE_OBJ - 1) + 1
            if alloc[typ] > ID_TO:
                raise ProbeError(f'object ids for {typ} exhausted')
            old_ids.append(m.group(4))
            return f'{m.group(1)}{m.group(2)}{m.group(3)}{alloc[typ]}{m.group(5)}{m.group(6)}'
        t = OBJ_RE.sub(sub, t)

        def lay(m):
            base = Path(m.group(2)).name
            return f"{m.group(1)}'./res/{p.id}/{base}'" if base in res_names else m.group(0)
        t = re.sub(r"(\b(?:LayoutFile|RDLCLayout|WordLayout|ExcelLayout)\s*=\s*)'([^']+)'", lay, t, flags=re.I)
        out[fn] = t
    joined = '\n'.join(re.sub(r'//[^\n]*', '', t) for t in out.values())
    for oid in old_ids:
        if re.search(rf'(?<![\w.]){oid}(?![\w.])', joined):
            print(f'warning: {p.id}: placeholder id {oid} is still referenced by number after renumbering',
                  file=sys.stderr)
    return out


def gen_wrapper(p, wid, wname):
    """Codeunit that runs a RunAll-style probe step by step: each step in its own Codeunit.Run (errors trapped,
    writes rolled back), its output logged and committed before the next step."""
    if p.contract == 'take':
        calls = [f'BcrP.{x}();' for x in p.steps]
        names = p.steps
    elif p.contract == 'text':
        calls, names = ['BcrText := BcrP.RunAll();'], ['RunAll']
    else:
        calls, names = ['BcrP.RunAll(BcrArr);'], ['RunAll']
    w = []
    w.append(f'// Generated by tools/probes: runs hand-written probe {p.id} ({p.contract} contract); do not edit.')
    w.append(f'codeunit {wid} "{wname}"')
    w.append('{')
    w.append('    var')
    w.append(f'        BcrP: Codeunit "{p.entry}";')
    w.append('        BcrStep: Integer;')
    w.append('        BcrText: Text;')
    w.append('        BcrArr: JsonArray;')
    w.append('        BcrDone: Boolean;')
    w.append("        BcrRollbackTok: Label 'BCR-PROBE-ROLLBACK', Locked = true;")
    w.append('')
    w.append('    trigger OnRun()')
    w.append('    begin')
    w.append('        case BcrStep of')
    for i, c in enumerate(calls, 1):
        w.append(f'            {i}:')
        w.append(f'                {c}')
    w.append('        end;')
    if p.contract == 'take':
        w.append('        BcrText := BcrP.Take();')
    w.append('        BcrDone := true;')
    w.append('        Error(BcrRollbackTok);')
    w.append('    end;')
    w.append('')
    w.append('    procedure Drive(var Log: Codeunit "BCR Probe Log")')
    w.append('    var')
    w.append(f'        BcrW: Codeunit "{wname}";')
    w.append('        BcrS: Integer;')
    w.append('        BcrLang: Integer;')
    w.append('        BcrOk: Boolean;')
    w.append('        BcrT0: DateTime;')
    w.append('    begin')
    w.append('        BcrLang := GlobalLanguage();')
    w.append(f'        for BcrS := 1 to {len(calls)} do begin')
    w.append('            if Log.Selected(BcrName(BcrS)) then begin')
    w.append('                Clear(BcrW);')
    w.append('                BcrW.BcrSet(BcrS);')
    w.append('                ClearLastError();')
    w.append('                BcrT0 := CurrentDateTime();')
    w.append('                BcrOk := BcrW.Run();')
    w.append('                if BcrW.BcrIsDone() then begin')
    w.append("                    Log.Note(StrSubstNo('step %1 done in %2', BcrName(BcrS), CurrentDateTime() - BcrT0));")
    w.append('                    BcrW.BcrEmit(Log);')
    w.append('                end else')
    w.append("                    Log.Note(StrSubstNo('step %1 failed after %2: %3: %4 | call stack: %5', BcrName(BcrS), "
             "CurrentDateTime() - BcrT0, GetLastErrorCode(), GetLastErrorText(), GetLastErrorCallStack()));")
    w.append('                Log.Flush();')
    w.append('                GlobalLanguage(BcrLang);')
    w.append('            end;')
    w.append('        end;')
    w.append('    end;')
    w.append('')
    w.append('    procedure BcrSet(S: Integer)')
    w.append('    begin')
    w.append('        BcrStep := S;')
    w.append('    end;')
    w.append('')
    w.append('    procedure BcrIsDone(): Boolean')
    w.append('    begin')
    w.append('        exit(BcrDone);')
    w.append('    end;')
    w.append('')
    w.append('    procedure BcrEmit(var Log: Codeunit "BCR Probe Log")')
    w.append('    begin')
    w.append('        Log.RawArray(BcrArr);' if p.contract == 'array' else '        Log.RawText(BcrText);')
    w.append('    end;')
    w.append('')
    w.append('    local procedure BcrName(S: Integer): Text')
    w.append('    begin')
    w.append('        case S of')
    for i, n in enumerate(names, 1):
        w.append(f'            {i}:')
        w.append(f'                exit({al_quote(n)});')
    w.append('        end;')
    w.append('    end;')
    w.append('}')
    return '\n'.join(w) + '\n'


def gen_registry(probes):
    lines = ['// Generated by tools/probes; do not edit.',
             f'codeunit {REGISTRY_ID} "BCR Probe Registry"', '{',
             '    procedure Run(ProbeId: Text; var Log: Codeunit "BCR Probe Log"): Boolean']
    if probes:
        lines.append('    var')
        for i, p in enumerate(probes, 1):
            lines.append(f'        P{i}: Codeunit "{p.obj_name}";')
    lines.append('    begin')
    if probes:
        lines.append('        case ProbeId of')
        for i, p in enumerate(probes, 1):
            cells = f'P{i}.CellCount()' if p.kind == 'grid' else str(len(p.steps) if p.contract == 'take' else 0)
            lines += [f'            {al_quote(p.id)}:', '                begin',
                      f'                    Log.BeginProbe({al_quote(p.id)}, {cells});',
                      f'                    P{i}.Drive(Log);', '                end;']
        lines += ['            else', '                exit(false);', '        end;', '        exit(true);']
    else:
        lines.append('        exit(false);')
    lines += ['    end;', '}']
    return '\n'.join(lines) + '\n'


def sha256_bytes(b):
    return hashlib.sha256(b).hexdigest()


def build_app(probes, version, out_dir, target='Cloud'):
    """Write the app (app.json, src/, res/) to out_dir; returns (app_source_sha, {probe id: [src file names]}).
    Sets p.al to the probe's AL source as archived with its runs (all files of a hand-written probe)."""
    probes = sorted(probes, key=lambda p: p.id)
    if len({p.id for p in probes}) != len(probes):
        raise ProbeError('duplicate probe ids')
    alloc = {}
    used = set()
    src = {f.name: f.read_text() for f in sorted(AL_DIR.glob('*.al'))}
    res = {}
    files = {}
    for p in probes:
        if p.kind == 'grid':
            alloc['codeunit'] = alloc.get('codeunit', FIRST_PROBE_OBJ - 1) + 1
            p.obj_id = alloc['codeunit']
            p.obj_name = obj_name(p.id, used)
            p.al = gen_grid_codeunit(p)
            fn = f'Probe{p.obj_id}.Codeunit.al'
            src[fn] = p.al
            files[p.id] = [fn]
            continue
        texts = renumber_raw(p, alloc)
        names = []
        for fn, t in texts.items():
            name = f'Raw_{p.id}_{fn}'
            src[name] = t
            names.append(name)
        if p.contract == 'drive':
            p.obj_name = p.entry
        else:
            alloc['codeunit'] = alloc.get('codeunit', FIRST_PROBE_OBJ - 1) + 1
            p.obj_id = alloc['codeunit']
            p.obj_name = obj_name(f'w{p.id}', used).replace('BCRP ', 'BCRW ', 1)
            name = f'Wrap{p.obj_id}.Codeunit.al'
            src[name] = gen_wrapper(p, p.obj_id, p.obj_name)
            names.append(name)
        for r in p.resources:
            res[f'res/{p.id}/{r.name}'] = r.read_bytes()
        for f, name in p.inputs:
            res[f'res/{p.id}/{name}'] = f.read_bytes()
        files[p.id] = names
        p.al = ''.join(f'// ==== {n}\n{src[n]}' for n in names)
    if alloc.get('codeunit', 0) > ID_TO:
        raise ProbeError('too many probe objects in one run')
    src['ProbeRegistry.Codeunit.al'] = gen_registry(probes)
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / 'src').mkdir(exist_ok=True)
    h = hashlib.sha256()
    for fn in sorted(src):
        (out_dir / 'src' / fn).write_text(src[fn], encoding='utf-8')
        h.update(fn.encode() + b'\0' + src[fn].encode('utf-8') + b'\0')
    for rn in sorted(res):
        (out_dir / rn).parent.mkdir(parents=True, exist_ok=True)
        (out_dir / rn).write_bytes(res[rn])
        h.update(rn.encode() + b'\0' + res[rn] + b'\0')
    h.update(target.encode())
    app = {
        'id': APP_ID, 'name': APP_NAME, 'publisher': APP_PUBLISHER, 'version': version,
        'brief': 'Black-box probes for bc-rust (tools/probes)',
        'description': 'Generated by tools/probes in the bc-rust repository. For a test container only.',
        'dependencies': [], 'platform': '29.0.0.0', 'application': '29.0.0.0',
        'idRanges': [{'from': ID_FROM, 'to': ID_TO}], 'runtime': '16.0', 'target': target,
        'features': ['NoImplicitWith'],
    }
    if any(p.inputs for p in probes):
        app['resourceFolders'] = ['res']   # inputs: NavApp.GetResource('<probe id>/<name>', InStream)
    (out_dir / 'app.json').write_text(json.dumps(app, indent=2) + '\n')
    return h.hexdigest(), files


def new_version(now):
    return f'1.0.{now:%Y%m%d}.{now:%H%M%S}'


# ---------------------------------------------------------------------------------------------- VM access

class Vm:
    def __init__(self, cfg):
        self.cfg = cfg
        self.ssh_base = ['-i', cfg['PROBES_SSH_KEY'], '-o', 'StrictHostKeyChecking=no', '-o',
                         'UserKnownHostsFile=/dev/null', '-o', 'BatchMode=yes', '-o', 'LogLevel=ERROR',
                         '-o', 'ServerAliveInterval=30']
        self.target = cfg['PROBES_SSH_TARGET']
        self.port = cfg['PROBES_SSH_PORT']
        self.vmdir = cfg['PROBES_VM_DIR'].rstrip('/')

    def ssh(self, command, stdin=None, timeout=600):
        r = subprocess.run(['ssh', *self.ssh_base, '-p', self.port, self.target, command], input=stdin,
                           capture_output=True, text=True, timeout=timeout)
        return r.returncode, (r.stdout + r.stderr).replace('\r', '')

    def win(self, path):
        return f'{self.vmdir}/{path}'.replace('/', '\\')

    def container_ps(self, script, *args, stdin=None, timeout=600):
        """Run vm/<script> inside the BC container with PowerShell 7 (the folder is shared with the container)."""
        a = ' '.join(ps_arg(x) for x in args)
        i = '-i ' if stdin is not None else ''
        return self.ssh(f'docker exec {i}{self.cfg["PROBES_CONTAINER"]} pwsh -NoProfile -NonInteractive -File '
                        f'{self.win("vm/" + script)} {a}', stdin, timeout)

    def scp_to(self, local, remote, recursive=False):
        cmd = ['scp', *self.ssh_base, '-P', self.port] + (['-r'] if recursive else []) + \
              [str(local), f'{self.target}:{remote}']
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
        if r.returncode:
            raise ProbeError(f'scp to VM failed: {r.stderr.strip()}')

    def scp_from(self, remote, local, recursive=False):
        r = subprocess.run(['scp', *self.ssh_base, '-P', self.port] + (['-r'] if recursive else []) +
                           [f'{self.target}:{remote}', str(local)],
                           capture_output=True, text=True, timeout=300)
        if r.returncode:
            raise ProbeError(f'scp from VM failed: {r.stderr.strip()}')


def ps_arg(x):
    x = str(x)
    if re.match(r'^[A-Za-z0-9_.:\\/-]+$', x):
        return x
    return "'" + x.replace("'", "''") + "'"


def ensure_vm_dirs(vm):
    """Create the VM folders, seed the symbol packages once, copy the scripts, return the container's BC version."""
    sym = vm.win('.alpackages')
    seed = vm.cfg.get('PROBES_SYMBOLS_FROM', 'C:/ProgramData/BcContainerHelper/o15probe/.alpackages').replace('/', '\\')
    code, out = vm.ssh(
        f"New-Item -ItemType Directory -Force -Path {vm.win('vm')},{vm.win('builds')},{vm.win('out')},{sym} | Out-Null; "
        f"if (-not (Test-Path '{sym}\\*.app') -and (Test-Path '{seed}')) {{ Copy-Item '{seed}\\*.app' '{sym}' }}; "
        f"'SYMBOLS ' + @(Get-ChildItem '{sym}\\*.app').Count; "
        f"'BCVERSION ' + (docker inspect -f '{{{{.Config.Labels.version}}}}-{{{{.Config.Labels.country}}}}' "
        f"{vm.cfg['PROBES_CONTAINER']})")
    if 'BCVERSION' not in out:
        raise ProbeError(f'VM not reachable over ssh: {out.strip()[:300]}')
    if re.search(r'^SYMBOLS 0$', out, re.M):
        raise ProbeError(f'no symbol packages in {sym}; copy the Microsoft .app symbols there (see README)')
    for f in sorted(VM_SCRIPTS.glob('*.ps1')):
        vm.scp_to(f, f'{vm.vmdir}/vm/{f.name}')
    return re.search(r'^BCVERSION (\S*)', out, re.M).group(1).strip() or 'unknown'


# ---------------------------------------------------------------------------------------------- run

def utc_now():
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0)


def lock(cfg):
    path = Path(cfg['PROBES_LOCK'])
    path.parent.mkdir(parents=True, exist_ok=True)
    f = open(path, 'a+')
    try:
        fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        print(f'waiting for {path} (another probe run is active)', flush=True)
        fcntl.flock(f, fcntl.LOCK_EX)
    return f


def log(msg):
    print(f'{utc_now():%H:%M:%S} {msg}', flush=True)


def cmd_gen(args, cfg):
    probes = [load_spec(s, args.area) for s in args.spec]
    out = Path(args.out) if args.out else Path(cfg['PROBES_WORK']) / 'gen' / f'{utc_now():%Y%m%dT%H%M%SZ}'
    sha, files = build_app(probes, new_version(utc_now()), out, args.target or cfg['PROBES_TARGET'])
    print(f'{out} (source sha256 {sha})')
    for p in sorted(probes, key=lambda p: p.id):
        print(f'  {p.id}: {files[p.id]}, {p.cells} cells' if p.kind == 'grid' else
              f'  {p.id}: {files[p.id]} ({p.contract} contract, entry "{p.entry}", {len(p.steps)} steps, '
              f'{len(p.resources)} layout files)')


def cmd_run(args, cfg):
    t0 = time.monotonic()
    if args.company:
        cfg['PROBES_COMPANY'] = args.company
    probes = [load_spec(s, args.area) for s in args.spec]
    work = Path(cfg['PROBES_WORK'])
    evid = Path(cfg['PROBES_EVIDENCE'])
    user, pw = read_credentials(cfg['PROBES_CREDENTIALS'])
    lk = lock(cfg)
    try:
        vm = Vm(cfg)
        now = utc_now()
        version = new_version(now)
        build = work / 'builds' / version
        target = args.target or cfg['PROBES_TARGET']
        app_sha, files = build_app(probes, version, build, target)
        state_f = work / 'published.json'
        state = json.loads(state_f.read_text()) if state_f.is_file() else {}
        bc_version = ensure_vm_dirs(vm)
        timings = {}
        if state.get('source_sha') == app_sha and not args.republish:
            version = state['version']
            log(f'app unchanged, using published {version}')
        else:
            t = time.monotonic()
            vm.scp_to(build, f'{vm.vmdir}/builds/', recursive=True)
            log(f'compiling and publishing {APP_NAME} {version} ({len(probes)} probes)')
            code, out = vm.container_ps('ct-build.ps1', '-Proj', vm.win(f'builds/{version}'),
                                        '-Symbols', vm.win('.alpackages'), timeout=900)
            (build / 'build.log').write_text(out)
            if code or f'PUBLISHED {version}' not in out:
                sys.stdout.write('\n'.join(x for x in out.splitlines() if 'error' in x.lower() or 'FAILED' in x)[-6000:]
                                 + '\n')
                raise ProbeError(f'compile/publish failed (log {build / "build.log"})')
            for line in out.splitlines():
                if line.startswith(('COMPILED', 'PUBLISHED', 'UNPUBLISHED')):
                    log(line)
            state = {'source_sha': app_sha, 'version': version, 'published_utc': f'{utc_now():%FT%TZ}'}
            state_f.write_text(json.dumps(state, indent=2) + '\n')
            timings['publish_s'] = round(time.monotonic() - t, 1)
        # group probes by their contexts (command line wins)
        groups = {}
        for p in sorted(probes, key=lambda p: p.id):
            ctx = tuple(args.ctx) if args.ctx else tuple(p.contexts)
            groups.setdefault((ctx, bool(args.background or p.background)), []).append(p)
        last = None
        for (ctx, bg), ps_ in groups.items():
            if bg and any(c.startswith('locale=') or '&locale=' in c for c in ctx):
                log('note: a background session takes language and region from the user settings; '
                    'locale= in contexts is recorded but not applied')
            last = run_probe_group(vm, cfg, ps_, ctx, version, app_sha, build, files, evid, work, timings,
                                   bc_version, user, pw, last,
                                   args.steps.split(',') if args.steps else None, bg)
        log(f'done in {time.monotonic() - t0:.1f} s')
    finally:
        lk.close()


def normalize_line(x):
    """Turn a raw line of a RunAll-style probe into the cell shape: the probe's own keys other than result,
    error, code and ok become "in"; ok = the line's ok if it has one, else (error is empty)."""
    if 'raw' not in x and 'rawText' not in x:
        return x
    raw = x.get('raw')
    base = {'probe': x['probe'], 'ctx': x.get('ctx', {}), 'cell': x['cell']}
    if not isinstance(raw, dict):
        return {**base, 'in': {}, 'ok': True, 'result': raw if 'raw' in x else x['rawText'], 'error': '', 'code': ''}
    err = raw.get('error') or ''
    ok = raw['ok'] if isinstance(raw.get('ok'), bool) else err == ''
    rest = {k: v for k, v in raw.items() if k not in ('result', 'error', 'code', 'ok', 'out')}
    line = {**base, 'in': rest, 'ok': ok, 'result': raw.get('result', ''), 'error': err, 'code': raw.get('code') or ''}
    if 'out' in raw:
        line['out'] = raw['out']
    return line


def run_probe_group(vm, cfg, probes, ctx, version, app_sha, build, files, evid, work, timings, bc_version,
                    user, pw, last_ts, steps=None, background=False):
    stamp = utc_now()
    ts = f'{stamp:%Y%m%dT%H%M%SZ}'
    while ts == last_ts:                      # one evidence file name per second
        time.sleep(0.3)
        stamp = utc_now()
        ts = f'{stamp:%Y%m%dT%H%M%SZ}'
    run_id = f'R{ts}'
    ids = ','.join(f'{p.id}:{"+".join(steps)}' if steps and p.contract in ('take', 'drive') else p.id for p in probes)
    t = time.monotonic()
    log(f'run {run_id}: {ids} | contexts {";".join(ctx) or "(session default)"}'
        f'{" | background session" if background else ""}')
    remote_out = vm.win(f'out/{run_id}.jsonl')
    bgargs = ['-Background', '-WaitSec', str(int(cfg.get('PROBES_WAIT_SEC', 7200)))] if background else []
    ctxargs = ['-Contexts', ';'.join(ctx)] if ctx else []   # Windows drops empty arguments
    code, out = vm.container_ps('ct-run.ps1', '-RunId', run_id, '-Probes', ids, *ctxargs,
                                '-Company', cfg['PROBES_COMPANY'], '-Out', remote_out, *bgargs,
                                stdin=f'{user}\n{pw}\n', timeout=int(cfg.get('PROBES_WAIT_SEC', 7200)) + 600)
    for line in out.splitlines():
        if line.startswith(('STATUS', 'WAIT-TIMEOUT', 'INVOKE-FAILED')):
            log(line[:2000])
    runs_dir = work / 'runs'
    runs_dir.mkdir(parents=True, exist_ok=True)
    (runs_dir / f'{run_id}.log').write_text(out)
    m = re.search(r'^OUT (\S+)', out, re.M)
    if code or not m:
        sys.stdout.write(out[-6000:])
        raise ProbeError(f'run failed (log {runs_dir / (run_id + ".log")})')
    run_s = [float(x) for x in re.findall(r'^INVOKED ([\d.]+)s', out, re.M)]
    raw = runs_dir / f'{run_id}.jsonl'
    vm.scp_from(f'{vm.vmdir}/out/{run_id}.jsonl', raw)
    ff = re.search(r'^FILES-FAILED.*$', out, re.M)
    if ff:
        raise ProbeError(f'fetching binary outputs failed: {ff.group(0)} (log {runs_dir / (run_id + ".log")})')
    fm = re.search(r'^FILES (\d+)', out, re.M)
    files_dir = runs_dir / f'{run_id}.files'
    if fm:
        vm.scp_from(f'{vm.vmdir}/out/{run_id}.files', runs_dir, recursive=True)
        log(f'{fm.group(1)} binary outputs -> {files_dir}')
    timings['run_s'] = round(time.monotonic() - t, 1)
    if run_s:
        timings['codeunit_s'] = round(sum(run_s), 1)
    lines = [json.loads(x) for x in raw.read_text(encoding='utf-8-sig').split('\n') if x.strip()]
    groups = re.search(r'^GROUPS (\d+)', out, re.M)
    done = sum(1 for x in lines if x.get('done'))
    if not groups or done != int(groups.group(1)):
        sys.stdout.write('\n'.join(x for x in out.splitlines() if x.startswith(('INVOKE', 'GROUPS', 'OUT'))) + '\n')
        raise ProbeError(f'{raw}: {done} of {groups.group(1) if groups else "?"} sub-runs finished')
    lines = [x for x in lines if not x.get('done')]
    for p in probes:
        mine = [normalize_line(x) for x in lines if x.get('probe') == p.id]
        d = evid / p.id
        d.mkdir(parents=True, exist_ok=True)
        jl = d / f'{ts}.jsonl'
        body = ''.join(json.dumps(x, ensure_ascii=False, separators=(',', ':')) + '\n' for x in mine)
        jl.write_text(body, encoding='utf-8')
        al_src = p.al.encode('utf-8')
        (d / f'{ts}.al').write_bytes(al_src)
        extra = []
        if p.kind == 'grid':
            spec_copy = d / f'{ts}.toml'
            shutil.copyfile(p.path, spec_copy)
            extra.append(spec_copy)
        for r in p.resources:
            rc = d / f'{ts}.res.{r.name}'
            shutil.copyfile(r, rc)
            extra.append(rc)
        pf = files_dir / re.sub(r'[\\/:*?"<>|]', '_', p.id)
        bins = sorted(pf.iterdir()) if pf.is_dir() else []
        if bins:
            (d / f'{ts}.files').mkdir(exist_ok=True)
            for b in bins:
                shutil.copyfile(b, d / f'{ts}.files' / b.name)
                extra.append(d / f'{ts}.files' / b.name)
        cells = [x for x in mine if 'cell' in x]
        notes = [x['note'] for x in mine if 'note' in x]
        meta = {
            'probe': p.id, 'area': p.area, 'title': p.title, 'kind': p.kind, 'run_id': run_id,
            'utc': f'{stamp:%FT%TZ}', 'bc_version': bc_version, 'container': cfg['PROBES_CONTAINER'],
            'company': cfg['PROBES_COMPANY'], 'app_version': version, 'app_source_sha256': app_sha,
            'probe_source_sha256': sha256_bytes(al_src), 'spec': str(p.path),
            'spec_sha256': sha256_bytes(p.path.read_bytes()) if p.path.is_file() else None,
            'source_files': {fn: sha256_bytes(t.encode('utf-8')) for fn, t in p.files.items()},
            'resources': {r.name: sha256_bytes(r.read_bytes()) for r in p.resources},
            'inputs': {n: sha256_bytes(f.read_bytes()) for f, n in p.inputs},
            'contract': p.contract, 'entry': p.entry, 'steps': p.steps,
            'steps_run': [x for x in p.steps if not steps or x in steps] if p.steps else (steps or 'all'),
            'contexts': list(ctx), 'cells': len(cells),
            'errors': sum(1 for x in cells if not x.get('ok')), 'notes': notes, 'timings': timings,
            'dims': [d_.name for d_ in p.dims],
        }
        (d / f'{ts}.meta.json').write_text(json.dumps(meta, indent=2, ensure_ascii=False) + '\n')
        sums = ''.join(f'{sha256_bytes(f.read_bytes())}  {f.relative_to(d)}\n'
                       for f in [d / f'{ts}.al', jl, *extra, d / f'{ts}.meta.json'])
        if p.extract:
            rows = d / f'{ts}.rows.jsonl'
            r = subprocess.run(['python3', '-I', str(p.extract), str(jl), str(d / f'{ts}.files')],
                               capture_output=True, text=True, timeout=1800)
            if r.returncode:
                log(f'{p.id}: {p.extract.name} failed: {r.stderr.strip()[-2000:]}')
            else:
                rows.write_text(r.stdout, encoding='utf-8')
                meta['extract'] = {'script': str(p.extract), 'script_sha256': sha256_bytes(p.extract.read_bytes()),
                                   'rows': sum(1 for x in r.stdout.split('\n') if x.strip())}
                (d / f'{ts}.meta.json').write_text(json.dumps(meta, indent=2, ensure_ascii=False) + '\n')
                sums = ''.join(f'{sha256_bytes(f.read_bytes())}  {f.relative_to(d)}\n'
                               for f in [d / f'{ts}.al', jl, *extra, rows, d / f'{ts}.meta.json'])
                log(f'{p.id}: {meta["extract"]["rows"]} rows from {p.extract.name} -> {rows}')
        (d / f'{ts}.sha256').write_text(sums)
        log(f'{p.id}: {len(cells)} cells ({meta["errors"]} with ok=false){"; notes: " + "; ".join(notes) if notes else ""}'
            f' -> {jl}')
    return ts


# ---------------------------------------------------------------------------------------------- vectors

def tsv_escape(v):
    if v is None:
        return ''
    if isinstance(v, bool):
        v = 'true' if v else 'false'
    elif isinstance(v, (dict, list)):
        v = json.dumps(v, ensure_ascii=False, separators=(',', ':'))
    else:
        v = str(v)
    out = []
    for ch in v:
        if ch == '\\':
            out.append('\\\\')
        elif ch == '\t':
            out.append('\\t')
        elif ch == '\n':
            out.append('\\n')
        elif ch == '\r':
            out.append('\\r')
        elif ord(ch) < 32 or ord(ch) == 127:
            out.append(f'\\x{ord(ch):02x}')
        else:
            out.append(ch)
    return ''.join(out)


def sort_key(v):
    if isinstance(v, bool):
        return (0, int(v), '')
    if isinstance(v, (int, float)):
        return (0, v, '')
    return (1, 0, str(v))


def compile_vectors(probe_dir, out_root, allow_conflicts=False):
    """Compile all archived runs of one probe (same probe source and BC version as the newest run) into
    out_root/<area>/<probe>.tsv. Returns (path, rows, conflicts)."""
    metas = []
    for mf in sorted(probe_dir.glob('*.meta.json')):
        m = json.loads(mf.read_text())
        m['_jsonl'] = mf.with_name(mf.name[:-len('.meta.json')] + '.jsonl')
        metas.append(m)
    if not metas:
        raise ProbeError(f'{probe_dir}: no runs')
    def source_key(m):     # the AL source plus layouts and input files
        return (m['probe_source_sha256'], json.dumps(m.get('resources') or {}, sort_keys=True),
                json.dumps(m.get('inputs') or {}, sort_keys=True), m['bc_version'])

    newest = max(metas, key=lambda m: m['utc'])
    use = sorted((m for m in metas if source_key(m) == source_key(newest)), key=lambda m: m['utc'])
    for m in use:
        m['_rows'] = m['_jsonl'].with_name(m['_jsonl'].name[:-len('.jsonl')] + '.rows.jsonl')
    with_rows = [m for m in use if m['_rows'].is_file() and m.get('extract')]
    if with_rows:
        # rows from the newest run's extract script only
        ex = max(with_rows, key=lambda m: m['utc'])['extract']['script_sha256']
        sel = [m for m in with_rows if m['extract']['script_sha256'] == ex]
        return compile_table(max(sel, key=lambda m: m['utc']), sel, [m['_rows'] for m in sel], out_root)
    ctx_keys, in_keys, out_keys = [], [], []
    rows = {}
    conflicts = []
    sessions = {}
    for m in use:
        for line in m['_jsonl'].read_text(encoding='utf-8').split('\n'):
            if not line.strip():
                continue
            x = json.loads(line)
            ctx = x.get('ctx') or {}
            for k in ctx:
                if k not in ctx_keys:
                    ctx_keys.append(k)
                    ctx_keys.sort()
            if 'meta' in x:
                sessions[json.dumps(ctx, sort_keys=True)] = (ctx, x['meta'])
                continue
            if 'cell' not in x:
                continue
            if x.get('out'):       # timings (out keys 'ms' or '*_ms') stay in the raw runs only
                x['out'] = {k: v for k, v in x['out'].items() if not re.search(r'(^|_)ms$', k)}
            for k in x['in']:
                if k not in in_keys:
                    in_keys.append(k)
            for k in (x.get('out') or {}):
                if k not in out_keys:
                    out_keys.append(k)
            key = (json.dumps(ctx, sort_keys=True), json.dumps(x['in'], sort_keys=True, ensure_ascii=False))
            val = (x['ok'], x['result'], x['code'], x['error'],
                   json.dumps(x.get('out') or {}, sort_keys=True, ensure_ascii=False))
            if key in rows and rows[key][1] != val:
                conflicts.append((key, rows[key][1], val, m['utc']))
                if allow_conflicts:
                    rows[key] = (rows[key][0], val, x)
                continue
            if key not in rows:
                rows[key] = ((ctx, x['cell']), val, x)
    if conflicts and not allow_conflicts:
        msg = '\n'.join(f'  {k[0]} {k[1]}: {a} vs {b} (run {u})' for k, a, b, u in conflicts[:20])
        raise ProbeError(f'{probe_dir.name}: {len(conflicts)} inputs give different results across runs:\n{msg}')

    def order(item):
        (ctx, cell), _, _ = item
        return (tuple(sort_key(ctx.get(k, '')) for k in ctx_keys), cell)

    ordered = sorted(rows.values(), key=order)
    area = newest.get('area', 'misc')
    out = out_root / area / f'{newest["probe"]}.tsv'
    out.parent.mkdir(parents=True, exist_ok=True)
    head = vector_header(newest, use, [])
    for _, (ctx, meta) in sorted(sessions.items()):
        facts = ', '.join(f'{k}={meta[k]}' for k in ('globalLanguage', 'windowsLanguage', 'formatDecimal',
                                                     'formatDate', 'formatTime') if k in meta)
        cname = '&'.join(f'{k}={v}' for k, v in ctx.items()) or '(default)'
        head.append(f'# session {cname}: {facts}')
    head += [
        f'# rows: {len(ordered)}{f" ({len(conflicts)} conflicts, newest kept)" if conflicts else ""}',
        '# format: tab-separated; escapes \\\\ \\t \\n \\r \\xNN; ok = cell ran without error;'
        ' error/code = GetLastErrorText/GetLastErrorCode after the cell',
    ]
    cols = [f'ctx.{k}' for k in ctx_keys] + [f'in.{k}' for k in in_keys] + ['ok', 'result', 'code', 'error'] + \
           [f'out.{k}' for k in out_keys]
    body = ['\t'.join(cols)]
    for (ctx, _), val, x in ordered:
        okv, res, code, err, _ = val
        outs = x.get('out') or {}
        rec = [ctx.get(k) for k in ctx_keys] + [x['in'].get(k) for k in in_keys] + [okv, res, code, err] + \
              [outs.get(k) for k in out_keys]
        body.append('\t'.join(tsv_escape(v) for v in rec))
    out.write_text('\n'.join(head + body) + '\n', encoding='utf-8')
    return out, len(ordered), len(conflicts)


def vector_header(newest, use, extra):
    head = [
        '# bc-rust probe vectors, generated by tools/probes (probes.py vectors); do not edit by hand.',
        f'# probe: {newest["probe"]}',
        f'# area: {newest.get("area", "misc")}',
    ]
    if newest.get('title'):
        head.append(f'# title: {newest["title"]}')
    head += [
        f'# bc-version: {newest["bc_version"]} (container '
        f'{", ".join(sorted({m.get("container", "?") for m in use}))}, company '
        f'{", ".join(sorted({m.get("company", "?") for m in use}))})',
        f'# probe-source-sha256: {newest["probe_source_sha256"]}',
        f'# runs: {len(use)}, {use[0]["utc"]} .. {use[-1]["utc"]}',
    ]
    return head + extra


def compile_table(newest, use, rows_files, out_root):
    """Vectors of a probe with an extract script: the rows it printed, one TSV line each, columns in first-seen
    order, exact duplicates dropped, in run then row order."""
    cols, seen, rows = [], set(), []
    for f in rows_files:
        for line in f.read_text(encoding='utf-8').split('\n'):
            if not line.strip():
                continue
            x = json.loads(line)
            for k in x:
                if k not in cols:
                    cols.append(k)
            key = json.dumps(x, sort_keys=True, ensure_ascii=False)
            if key not in seen:
                seen.add(key)
                rows.append(x)
    area = newest.get('area', 'misc')
    out = out_root / area / f'{newest["probe"]}.tsv'
    out.parent.mkdir(parents=True, exist_ok=True)
    ex = newest.get('extract', {})
    head = vector_header(newest, use, [
        f'# rows: {len(rows)}, from {Path(ex.get("script", "extract.py")).name} '
        f'(sha256 {ex.get("script_sha256", "?")[:16]}) over the archived run JSONL',
        '# format: tab-separated; escapes \\\\ \\t \\n \\r \\xNN; one line per extracted row',
    ])
    body = ['\t'.join(cols)] + ['\t'.join(tsv_escape(x.get(k)) for k in cols) for x in rows]
    out.write_text('\n'.join(head + body) + '\n', encoding='utf-8')
    return out, len(rows), 0


def cmd_vectors(args, cfg):
    evid = Path(cfg['PROBES_EVIDENCE'])
    ids = [d.name for d in sorted(evid.iterdir()) if d.is_dir()] if args.all else args.probe
    out_root = Path(args.out) if args.out else REPO / 'tests' / 'vectors'
    for pid in ids:
        path, n, c = compile_vectors(evid / pid, out_root, args.allow_conflicts)
        print(f'{pid}: {n} rows{f", {c} conflicts" if c else ""} -> {path}')


def cmd_extract(args, cfg):
    """Rerun a probe's extract script on an archived run; an existing <ts>.rows.jsonl is kept as
    <ts>.rows.<first 8 hex of its sha256>.jsonl."""
    d = Path(cfg['PROBES_EVIDENCE']) / args.probe
    meta_f = d / f'{args.ts}.meta.json'
    meta = json.loads(meta_f.read_text())
    script = Path(args.script) if args.script else Path(meta.get('extract', {}).get('script') or
                                                       Path(meta['spec']) / 'extract.py')
    jl, files, rows = d / f'{args.ts}.jsonl', d / f'{args.ts}.files', d / f'{args.ts}.rows.jsonl'
    if rows.is_file():
        old = rows.with_name(f'{args.ts}.rows.{sha256_bytes(rows.read_bytes())[:8]}.jsonl')
        rows.rename(old)
        print(f'kept the previous rows as {old.name}')
    r = subprocess.run(['python3', '-I', str(script), str(jl), str(files)], capture_output=True, text=True,
                       timeout=3600)
    if r.returncode:
        raise ProbeError(f'{script.name} failed: {r.stderr.strip()[-2000:]}')
    rows.write_text(r.stdout, encoding='utf-8')
    meta['extract'] = {'script': str(script), 'script_sha256': sha256_bytes(script.read_bytes()),
                       'rows': sum(1 for x in r.stdout.split('\n') if x.strip()), 'rerun_utc': f'{utc_now():%FT%TZ}'}
    meta_f.write_text(json.dumps(meta, indent=2, ensure_ascii=False) + '\n')
    listed = [f for f in sorted(d.glob(f'{args.ts}*')) if f.is_file() and not f.name.endswith('.sha256')]
    listed += sorted((d / f'{args.ts}.files').iterdir()) if (d / f'{args.ts}.files').is_dir() else []
    (d / f'{args.ts}.sha256').write_text(''.join(f'{sha256_bytes(f.read_bytes())}  {f.relative_to(d)}\n'
                                                 for f in listed))
    print(f'{args.probe} {args.ts}: {meta["extract"]["rows"]} rows -> {rows}')


def cmd_list(args, cfg):
    evid = Path(cfg['PROBES_EVIDENCE'])
    print('specs:')
    for f in sorted(SPEC_DIR.glob('*.toml')) + sorted(SPEC_DIR.glob('*.al')):
        try:
            p = load_probe(f)
            print(f'  {p.id:30} {p.area:12} {p.cells if p.kind == "grid" else "AL":>7}  {p.title}')
        except Exception as e:  # noqa: BLE001
            print(f'  {f.name}: {e}')
    print(f'runs in {evid}:')
    if evid.is_dir():
        for d in sorted(evid.iterdir()):
            runs = sorted(d.glob('*.meta.json'))
            if runs:
                print(f'  {d.name:30} {len(runs)} runs, newest {runs[-1].name[:-10]}')


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    g = sub.add_parser('gen', help='generate the AL app only')
    g.add_argument('spec', nargs='+')
    g.add_argument('--out')
    g.add_argument('--target', choices=['Cloud', 'OnPrem'])
    g.add_argument('--area', help='area of hand-written probes without an "// area:" header')
    r = sub.add_parser('run', help='generate, publish, run, archive')
    r.add_argument('spec', nargs='+')
    r.add_argument('--ctx', action='append', help="session context, e.g. 'lang=1031' (repeat for several)")
    r.add_argument('--background', action='store_true', help='run in a background session (report probes)')
    r.add_argument('--company', help='company to run in (default PROBES_COMPANY)')
    r.add_argument('--steps', help='comma-separated steps of hand-written RunAll/Take probes to run (default all)')
    r.add_argument('--republish', action='store_true', help='publish even if the app source is unchanged')
    r.add_argument('--target', choices=['Cloud', 'OnPrem'])
    r.add_argument('--vectors', action='store_true', help='compile the vectors of the probes afterwards')
    r.add_argument('--vectors-out')
    r.add_argument('--area', help='area of hand-written probes without an "// area:" header')
    v = sub.add_parser('vectors', help='compile archived runs into tests/vectors')
    v.add_argument('probe', nargs='*')
    v.add_argument('--all', action='store_true')
    v.add_argument('--out')
    v.add_argument('--allow-conflicts', action='store_true')
    e = sub.add_parser('extract', help="rerun a probe's extract script on an archived run")
    e.add_argument('probe')
    e.add_argument('ts', help='run timestamp, e.g. 20261110T113804Z')
    e.add_argument('--script', help='extract script (default: the one recorded in the run, or <spec>/extract.py)')
    sub.add_parser('list', help='list specs and archived runs')
    args = ap.parse_args(argv)
    cfg = load_config()
    try:
        if args.cmd == 'gen':
            cmd_gen(args, cfg)
        elif args.cmd == 'run':
            cmd_run(args, cfg)
            if args.vectors:
                ids = [load_spec(s, args.area).id for s in args.spec]
                cmd_vectors(argparse.Namespace(probe=ids, all=False, out=args.vectors_out,
                                               allow_conflicts=False), cfg)
        elif args.cmd == 'extract':
            cmd_extract(args, cfg)
        elif args.cmd == 'vectors':
            if not args.all and not args.probe:
                ap.error('give probe ids or --all')
            cmd_vectors(args, cfg)
        else:
            cmd_list(args, cfg)
    except ProbeError as e:
        print(f'error: {e}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
