#!/usr/bin/env python3
"""Generate bc-rust's platform/ AL files from Microsoft's System.app.

    python3 -I tools/platform-gen/gen.py --app <platform.zip | System.app> [--repo DIR] [--bc-build VERSION]

See tools/platform-gen/README.md. Writes platform/SystemTables.al (namespace System.Reflection plus
the header) and platform/system/<namespace>.al, then checks through bc-rust's own parser that every
generated table and codeunit equals Microsoft's definition. Exits non-zero if one does not.
"""
import argparse, collections, io, os, re, subprocess, sys, tempfile, textwrap, urllib.parse, zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
PROBE = os.path.join(HERE, 'probe', 'Cargo.toml')

# Tables bc-rust's Rust code fills, computes or serves, plus the ones the first hand-written
# platform file had. They are included even if no AL code names them.
ALWAYS = [2000000006, 2000000120, 2000000041, 2000000038, 2000000058, 2000000136, 2000000026, 2000000007,
          2000000053, 2000000073, 2000000178, 2000000211, 2000000068, 2000000182, 2000000168]

# One-line notes for tables bc-rust fills or computes itself.
NOTES = {
    2000000006: ['bc-rust inserts the session company at startup (al-runtime platform.rs).'],
    2000000120: ['bc-rust inserts the session user at startup (al-runtime platform.rs).'],
    2000000053: ['bc-rust gives the session user SUPER at startup (al-runtime platform.rs).'],
    2000000041: ['Rows computed from the catalog (al-runtime virt.rs).'],
    2000000038: ['Rows computed from the catalog (al-runtime virt.rs).'],
    2000000058: ['Rows computed from the catalog (al-runtime virt.rs).'],
    2000000136: ['Rows computed from the catalog (al-runtime virt.rs).'],
    2000000026: ['Rows generated on the fly (al-runtime virt.rs).'],
    2000000007: ['Rows generated on the fly (al-runtime virt.rs).'],
    2000000178: ['The web server reads GetPage cells of this table by field number',
                 '(Microsoft.Dynamics.Nav.Types.AllProfileTableFields, ProfileAttributes).',
                 "bc-nst fills it at boot from the catalog's profile objects."],
    2000000073: ['Field numbers also in Microsoft.Dynamics.Nav.Types.UserPersonalizationFields.'],
}

# System.app folders that hold no objects the generator copies.
SKIP_DIRS = {'Permissions', 'Entitlements', 'Namespaces'}
KIND_OF_FOLDER = {'Virtual Tables': 'virtual', 'Tenant Database Tables': 'tenant',
                  'Application Database Tables': 'application', 'Temporary Tables': 'temporary'}
KINDS = ('table', 'enum', 'interface', 'codeunit', 'page', 'report')  # also the order within a file
CODE_KINDS = {'codeunit', 'page', 'report'}
OBJ_HEAD = re.compile(r'(?im)^(' + '|'.join(KINDS) + r')\s+(?:(\d+)\s+)?("([^"]+)"|(\w+))')
NAME = r'((?:\w+\.)*(?:"[^"\n]+"|\w+))'  # optionally namespace-qualified AL identifier
# `Codeunit "X"`, `Codeunit::"X"`, `TestPage X`, `RunObject = Page X`: a codeunit, page or report
CODE_REF = re.compile(r'(?i)\b(codeunit|page|testpage|report|testrequestpage)(?:\s*::\s*|\s+|(?="))' + NAME)
CODE_REF_KIND = {'testpage': 'page', 'testrequestpage': 'report'}
NATIVE = re.compile(r'(?im)^\s*\[Native\]')
EVENT = re.compile(r'(?im)^\s*\[(?:Business|Integration|Internal)Event\b')
# Comment above every copied event-publisher codeunit.
PUBLISHER_NOTE = ['Event publisher: the BC service tier raises these events itself. docs/platform-tables.md',
                  '("Platform events") lists when, and where bc-rust raises them.']


def die(msg):
    sys.exit(f'gen.py: {msg}')


def resolver(objs):
    """resolve(kind, name) -> the System.app object key for an AL name, or None. Accepts quoted or
    unquoted, namespace-qualified names (`System.Upgrade."Table Synch. Setup"`); names can contain dots."""
    names = collections.defaultdict(set)
    for kind, n in objs:
        names[kind].add(n)

    def resolve(kind, name):
        s = name.replace('"', '').strip().lower()
        if s in names[kind]:
            return (kind, s)
        if '.' in s:
            hits = [n for n in names[kind] if s.endswith('.' + n)]
            if hits:
                return (kind, max(hits, key=len))
        return None
    return resolve


def probe(*args):
    cmd = ['cargo', 'run', '--quiet', '--release', '--offline', '--manifest-path', PROBE, '--', *args]
    r = subprocess.run(cmd, stdout=subprocess.PIPE, text=True)
    if r.returncode != 0:
        die(f'probe failed: {" ".join(args[:2])}')
    return r.stdout


def extract_app(app, work):
    """Unpack System.app (from platform.zip or a .app file) into `work`.
    Returns (src dir, member path inside platform.zip or None, System app version)."""
    member = None
    if app.lower().endswith('.app'):
        data = open(app, 'rb').read()
    else:
        with zipfile.ZipFile(app) as z:
            hits = [n for n in z.namelist() if n.endswith('AL Development Environment/System.app')]
            if len(hits) != 1:
                die(f'expected one .../AL Development Environment/System.app in {app}, found {len(hits)}')
            member = hits[0]
            data = z.read(member)
    if data[:4] != b'NAVX':
        die(f'{app}: not a NAVX app package')
    start = int.from_bytes(data[4:8], 'little')  # header length, 40 in BC 29
    if data[start:start + 2] != b'PK':
        die(f'{app}: no zip after the {start}-byte NAVX header')
    dest = os.path.join(work, 'app')
    with zipfile.ZipFile(io.BytesIO(data[start:])) as z:
        z.extractall(dest)  # zipfile drops absolute and '..' path parts
    manifest = open(os.path.join(dest, 'NavxManifest.xml'), encoding='utf-8-sig').read()
    m = re.search(r'<App [^>]*\bName="System"[^>]*\bVersion="([\d.]+)"', manifest)
    if not m:
        die('NavxManifest.xml does not describe app "System"')
    return os.path.join(dest, 'src'), member, m.group(1)


def index_objects(src):
    """Tables, enums, interfaces, codeunits, pages and reports of System.app, keyed by (kind,
    lower-case name). `native` marks objects with [Native] procedures: the service tier implements
    those, and the copied AL has only empty bodies for them."""
    objs = {}
    for dp, dns, fs in os.walk(src):
        dns.sort()
        if os.path.relpath(dp, src).split(os.sep)[0] in SKIP_DIRS:
            continue
        for f in sorted(fs):
            if not f.lower().endswith('.al'):
                continue
            path = os.path.join(dp, f)
            text = open(path, encoding='utf-8-sig').read().replace('\r\n', '\n')
            heads = list(OBJ_HEAD.finditer(text))
            if len(heads) != 1:
                die(f'{path}: expected one object, found {len(heads)}')
            m = heads[0]
            kind, oid, name = m.group(1).lower(), m.group(2), m.group(4) or m.group(5)
            cleaned = []
            for line in (l.rstrip() for l in text[m.start():].split('\n') if not re.match(r'^\s*///', l)):
                if line == '' and (not cleaned or cleaned[-1] == ''):
                    continue
                cleaned.append(line)
            while cleaned and cleaned[-1] == '':
                cleaned.pop()
            rel = urllib.parse.unquote(urllib.parse.unquote(os.path.relpath(path, src))).replace(os.sep, '/')
            body = '\n'.join(cleaned)
            objs[(kind, name.lower())] = dict(
                kind=kind, id=int(oid) if oid else None, name=name,
                ns=re.search(r'(?m)^namespace\s+([\w.]+)\s*;', text).group(1),
                usings=re.findall(r'(?m)^using\s+([\w.]+)\s*;', text),
                native=bool(NATIVE.search(body)), events=len(EVENT.findall(body)),
                body=body, raw=text[m.start():], path=rel, folder=rel.split('/')[0])
    return objs


def code_refs(src, resolve):
    """System.app codeunits, pages and reports that AL text names: variable and parameter types,
    `Codeunit::` constants (event subscribers included), RunObject, TestPage."""
    return {resolve(CODE_REF_KIND.get(m.group(1).lower(), m.group(1).lower()), m.group(2)) for m in CODE_REF.finditer(src)}


def scan_app_source(dirs, resolve):
    """System.app objects the AL source names where the probe's `unknown` does not look:
    Database::, dataitem and tableelement tables, enum and interface types, codeunits, pages and
    reports. Also returns the (kind, lower-case name) of every object the source defines."""
    found, defined = set(), set()
    table_ctx = re.compile(r'(?i)(?:\bdatabase\s*::\s*|\bdataitem\s*\([^;)]*;\s*|\btableelement\s*\([^;)]*;\s*)' + NAME)
    type_ctx = re.compile(r'(?i)(?:\benum\s*::\s*|\benum\s+|\binterface\s+|\bextends\s+)' + NAME)
    impl_ctx = re.compile(r'(?i)\bimplements\s+([^{;\n]+)')
    for d in dirs:
        for dp, _, fs in os.walk(d):
            for f in fs:
                if not f.lower().endswith('.al'):
                    continue
                src = open(os.path.join(dp, f), encoding='utf-8-sig', errors='replace').read()
                for m in table_ctx.finditer(src):
                    found.add(resolve('table', m.group(1)))
                for m in type_ctx.finditer(src):
                    found |= {resolve('enum', m.group(1)), resolve('interface', m.group(1))}
                for m in impl_ctx.finditer(src):
                    found |= {resolve('interface', n) for n in m.group(1).split(',') if n.strip()}
                found |= code_refs(src, resolve)
                defined |= {(m.group(1).lower(), (m.group(4) or m.group(5)).lower()) for m in OBJ_HEAD.finditer(src)}
    return found - {None}, defined


def parse_dump(text):
    """The probe's dump as {(kind, id): lines}."""
    out, cur = {}, None
    for line in text.splitlines():
        m = re.match(r'(table|codeunit|page|report) (\d+) "', line)
        if m:
            cur = (m.group(1), int(m.group(2)))
            out[cur] = [line]
        elif cur is not None:
            out[cur].append(line)
    return out


def plural(counter):
    """'162 tables, 18 enums, ...' in KINDS order."""
    return ', '.join(f'{counter[k]} {k}s' for k in KINDS if counter[k])


def header(bc_build, sys_version, member):
    where = f'from {os.path.dirname(member)}/ in the platform artifact' if member else 'from the platform artifact'
    source = textwrap.fill(
        f"Source: Microsoft's System.app of BC {bc_build} (System {sys_version}, {where}). It ships the AL "
        "source of the system objects; the paths in the per-object comments are relative to its src/ folder. "
        "Objects are copied unchanged except that the /// doc comments are left out.",
        width=86, initial_indent='// ', subsequent_indent='// ', break_on_hyphens=False)
    return f"""\
// Platform (system) tables that the BC service tier provides natively.
// bc-rust defines them in AL so the same catalog/runtime handles them.
// Content of virtual tables (Field, AllObj, AllObjWithCaption, Table Metadata,
// Integer, Date) is generated by the server; Company, User, Access Control and
// All Profile get their rows at startup.
// System codeunits that only publish events (System Action Triggers, Company
// Triggers, ...) are copied as well, so AL subscribers to them resolve. The server
// raises those events. Codeunits with [Native] procedures are not copied.
//
// Generated by tools/platform-gen/gen.py. Edit the generator, not this file.
//
{source}
// Copyright (c) Microsoft Corporation. Licensed under the MIT License.
//
// AL allows one namespace per file: this file holds namespace System.Reflection and
// platform/system/<namespace>.al hold the others. Only system objects that the
// loaded AL source references are included; docs/platform-tables.md lists the rest.
"""


def sub_header(ns, bc_build):
    return f"""\
// System objects in namespace {ns}. Source and conventions: platform/SystemTables.al.
// Generated by tools/platform-gen/gen.py from Microsoft's System.app of BC {bc_build}.
// Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""


def main():
    ap = argparse.ArgumentParser(description='Generate platform/ AL files from System.app.')
    ap.add_argument('--app', required=True, help='BC platform artifact (platform.zip) or a System.app file')
    ap.add_argument('--repo', default=os.path.abspath(os.path.join(HERE, '..', '..')), help='bc-rust checkout (default: this one)')
    ap.add_argument('--bc-build', help='BC build named in the headers (default: the a.b.c.d folder in the --app path)')
    ap.add_argument('--missing', action='store_true', help='print the System.app tables left out, as markdown table rows')
    a = ap.parse_args()

    repo = os.path.abspath(a.repo)
    platform = os.path.join(repo, 'platform')
    bc_build = a.bc_build or next((p for p in reversed(os.path.abspath(a.app).split(os.sep)) if re.fullmatch(r'\d+\.\d+\.\d+\.\d+', p)), None)
    if not bc_build:
        die('pass --bc-build: the BC build is not a folder name in the --app path')

    with tempfile.TemporaryDirectory(prefix='platform-gen-') as work:
        src, member, sys_version = extract_app(a.app, work)
        objs = index_objects(src)
        by_id = {o['id']: k for k, o in objs.items() if o['kind'] == 'table'}
        resolve = resolver(objs)
        print(f'System.app {sys_version}: {plural(collections.Counter(o["kind"] for o in objs.values()))}', file=sys.stderr)

        dirs = [d for d in probe('default-dirs', repo).splitlines() if d]
        app_dirs = [d for d in dirs if os.path.abspath(d) != platform]
        real = parse_dump(probe('dump', src))

        # seeds: tables the AL source (without platform/) uses but does not define, the scan, ALWAYS
        sel = {by_id[t] for t in ALWAYS}
        for line in probe('unknown', *app_dirs).splitlines():
            k = resolve('table', line.split('\t')[1])
            if k:
                sel.add(k)
        found, defined = scan_app_source(app_dirs, resolve)

        # Codeunits, pages and reports stay out when the app source defines one of the same name, and
        # when they have [Native] procedures: the service tier implements those, the AL copy has empty
        # bodies, and bc-rust's runtime treats a codeunit it has no AL for as a platform object
        # (al-runtime dotnet::platform_codeunit implements Base64Convert natively).
        left_out = collections.defaultdict(set)

        def wanted(k):
            if k[0] in CODE_KINDS and k in defined:
                left_out['the app source defines an object of that name'].add(k)
                return False
            if objs[k]['native']:
                left_out['platform-implemented: [Native] procedures'].add(k)
                return False
            return True
        sel |= {k for k in found if wanted(k)}

        # closure: table relations, CalcFormulas and enum types; enum interfaces; interface signatures;
        # the types, object constants and SourceTable that copied codeunits, pages and reports name
        refs = collections.defaultdict(set)
        for line in probe('refs', src).splitlines():
            tid, kind, target = line.split('\t')
            refs[int(tid)].add(by_id[int(target)] if kind == 'table' else resolve('enum', target))
        todo = list(sel)
        while todo:
            o = objs[todo.pop()]
            if o['kind'] == 'table':
                deps = refs[o['id']]
            elif o['kind'] == 'enum':
                m = re.search(r'(?im)^enum\s+\d+\s+(?:"[^"]+"|\w+)\s+implements\s+([^\n{]+)', o['raw'])
                deps = {resolve('interface', n) for n in m.group(1).split(',')} if m else set()
            elif o['kind'] == 'interface':
                deps = {resolve('table', n) for n in re.findall(r'(?i)\bRecord\s+' + NAME, o['raw'])}
                deps |= {resolve('enum', n) for n in re.findall(r'(?i)\bEnum\s+' + NAME, o['raw'])}
            else:
                b = o['body']
                deps = {resolve('table', n) for n in re.findall(r'(?i)(?:\bRecord\s+|\bDatabase\s*::\s*|\bSourceTable\s*=\s*)' + NAME, b)}
                deps |= {resolve('enum', n) for n in re.findall(r'(?i)\bEnum\s*(?:::\s*|\s+)' + NAME, b)}
                deps |= {resolve('interface', n) for n in re.findall(r'(?i)\bInterface\s+' + NAME, b)}
                m = re.search(r'(?im)^\w+\s+\d+\s+(?:"[^"]+"|\w+)\s+implements\s+([^\n{]+)', b)
                deps |= {resolve('interface', n) for n in m.group(1).split(',')} if m else set()
                deps |= code_refs(b, resolve)
            for d in deps:
                if d and d not in sel and wanted(d):
                    sel.add(d)
                    todo.append(d)
        for why, ks in sorted(left_out.items()):
            names = ', '.join(f'{k[0]} "{objs[k]["name"]}"' for k in sorted(ks))
            print(f'left out ({why}): {names}', file=sys.stderr)

        # objects that hand-written platform/*.al files define stay out
        for f in sorted(os.listdir(platform)):
            if f.endswith('.al') and f != 'SystemTables.al':
                for m in OBJ_HEAD.finditer(open(os.path.join(platform, f), encoding='utf-8-sig').read()):
                    k = (m.group(1).lower(), (m.group(4) or m.group(5)).lower())
                    if k in sel:
                        print(f'skip {k[0]} "{objs[k]["name"]}": defined in platform/{f}', file=sys.stderr)
                        sel.discard(k)

        # write: platform/system/ holds only generated files
        by_ns = collections.defaultdict(list)
        for k in sel:
            by_ns[objs[k]['ns']].append(objs[k])
        sysdir = os.path.join(platform, 'system')
        os.makedirs(sysdir, exist_ok=True)
        for f in os.listdir(sysdir):
            if f.endswith('.al'):
                os.remove(os.path.join(sysdir, f))
        for ns, items in sorted(by_ns.items()):
            items.sort(key=lambda o: (KINDS.index(o['kind']), o['id'] or 0, o['name']))
            parts = [header(bc_build, sys_version, member) if ns == 'System.Reflection' else sub_header(ns, bc_build), f'namespace {ns};\n']
            usings = sorted({u for o in items for u in o['usings'] if u != ns})
            if usings:
                parts.append(''.join(f'using {u};\n' for u in usings))
            for o in items:
                if o['kind'] == 'table':
                    notes = NOTES.get(o['id'], [])
                else:
                    notes = PUBLISHER_NOTE if o['kind'] == 'codeunit' and o['events'] else []
                parts.append('\n'.join([f'// {o["path"]}'] + ['// ' + n for n in notes]) + '\n' + o['body'] + '\n')
            out = os.path.join(platform, 'SystemTables.al' if ns == 'System.Reflection' else f'system/{ns}.al')
            with open(out, 'w', encoding='utf-8', newline='\n') as fh:
                fh.write('\n'.join(parts))
        kinds = collections.Counter(objs[k]['kind'] for k in sel)
        print(f'wrote {plural(kinds)} in {len(by_ns)} files', file=sys.stderr)

        # verify through bc-rust's parser: every generated object equals Microsoft's
        ours = parse_dump(probe('dump', *dirs))
        made = sorted((k[0], objs[k]['id']) for k in sel if k[0] == 'table' or k[0] in CODE_KINDS)
        bad = [k for k in made if ours.get(k) != real[k]]
        stats = probe('stats', *dirs).splitlines()
        print(stats[0], file=sys.stderr)
        unknown_pub = collections.Counter(l.split('publisher ', 1)[1] for l in stats[1:] if 'unknown publisher' in l)
        print(f'subscribers to unknown publishers: {sum(unknown_pub.values())}'
              + ''.join(f'\n  {n} x{c}' for n, c in unknown_pub.most_common()), file=sys.stderr)
        left = [l.split('\t')[1] for l in probe('unknown', *dirs).splitlines()]
        print(f'unknown table names left: {len(left)} ({", ".join(left)})', file=sys.stderr)
        if a.missing:
            for t, k in sorted(by_id.items()):
                if k not in sel:
                    print(f'| {t} | {objs[k]["name"]} | {KIND_OF_FOLDER.get(objs[k]["folder"], objs[k]["folder"])} |')
        if bad:
            die(f'{len(bad)} generated objects differ from System.app: {bad}')
        print(f'check: all {plural(collections.Counter(k for k, _ in made))} generated equal System.app', file=sys.stderr)


if __name__ == '__main__':
    main()
