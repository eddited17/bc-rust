#!/usr/bin/env python3
"""Select a compact, deduplicated set of rows from the U1 probe vectors (tools/probes output) and
write them in one normalised format that crates/al-runtime/tests/u1_vectors.rs replays.

usage: u1-dateformula-compact.py FULL_TSV OUT_TSV
  FULL_TSV: the output of `tools/probes/probes.py vectors u1-dateformula` (69,911 rows, not committed);
  OUT_TSV: crates/al-runtime/tests/vectors/u1-dateformula.tsv

Columns: src (probe family), op, lang, input, arg, expect.  op:
  calc    CalcDate(input as Text, arg) in session language lang
  calcdf  Evaluate(DF, input) in lang, then CalcDate(DF, arg)   (arg 'E1033:<base>': evaluated in 1033)
  eval    Evaluate(DF, input) in lang; ok:<Format 9>|<Format 0>|<CalcDate(DF, 2024-01-15)>; '*' = not recorded
  evaln   Evaluate(DF, input, arg) in lang; ok:<Format 9>
  fmt     Evaluate(DF, input) in 1033, Format(DF, 0, arg) in lang
  len     Evaluate(DF, input) in 1033, Format(DF, arg) in lang
  eq      input 'A=B': Evaluate both in 1033; ok:Yes|No
  text    Evaluate(DF, input) in 1033, StrSubstNo('%1', DF) in lang
expect: ok:<value>, or err:<code>:<kind> with :<part> added in English sessions; kind E1..E4[min-max],
E5, E6, E7 (too long), E8 (a week past 9999), IS (standard format); part: the %1 of the message (E1..E4,
E7), its formula (E5, E8), else empty. The message templates are asserted once each in
crates/al-runtime/tests/u1_date_formula.rs. Dates as yyyy-mm-dd, 'C' before a closing date.
Not selected: U+0001..U+0007 in P09 (not settled; the test lists them as open).
Run with python3 -I. Reads only the input file.
"""
import collections
import random
import re
import sys

FULL, OUT = sys.argv[1], sys.argv[2]


def unesc(s):
    out = []
    i = 0
    while i < len(s):
        c = s[i]
        if c == '\\' and i + 1 < len(s):
            n = s[i + 1]
            if n in '\\tnr':
                out.append({'\\': '\\', 't': '\t', 'n': '\n', 'r': '\r'}[n])
                i += 2
                continue
            if n == 'x':
                out.append(chr(int(s[i + 2:i + 4], 16)))
                i += 4
                continue
        out.append(c)
        i += 1
    return ''.join(out)


def esc(s):
    out = []
    for ch in s:
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


rows = []
hdr = None
for line in open(FULL, encoding='utf-8'):
    if line.startswith('#'):
        continue
    f = line.rstrip('\n').split('\t')
    if hdr is None:
        hdr = f
        continue
    rows.append(dict(zip(hdr, [unesc(x) for x in f])))


def fam(p):
    p = re.sub(r' U\+.*', ' U+*', p)
    p = re.sub(r' \d+$', ' N', p)
    return p


def kind(code, msg):
    if code == 'InvalidStandardFormat':
        return 'IS'
    m = re.search(r'range (\d+) - (\d+)', msg)
    if 'time unit' in msg or 'Zeiteinheit enthalten' in msg:
        return 'E1'
    if 'include a number' in msg or 'Zahl enthalten' in msg:
        return 'E2'
    if 'must include plus' in msg:
        return 'E3'
    if m:
        return f'E4[{m.group(1)}-{m.group(2)}]'
    if 'exceeds the maximum length' in msg:
        return 'E7'
    if 'undefined date' in msg:
        return 'E5'
    if 'date is not valid' in msg:
        return 'E6'
    if 'resulted in an invalid date' in msg:
        return 'E8'
    return '?'


def part(k, msg):
    """The variable part of an English message: %1 (E1..E4, E7) or the formula (E5, E8)."""
    if k in ('E1', 'E2', 'E3', 'E7') or k.startswith('E4'):
        m = re.match(r"The date formula '(.*)' (should include|must include|contains a number|exceeds)", msg, re.S)
        return m.group(1)
    if k == 'E5':
        return re.search(r'Formula: (.*)\.$', msg, re.S).group(1)
    if k == 'E8':
        return re.match(r'The calculation of the date formula (.*) resulted', msg, re.S).group(1)
    return ''


def err(lang, code, msg):
    k = kind(code, msg)
    assert ':' not in code and k != '?' or lang != '1033', (code, msg)
    if lang != '1033':
        return f'err:{code}:{k}'
    return f'err:{code}:{k}:{part(k, msg)}'


def calc_expect(r):
    if r['ok'] == 'true':
        return 'ok:' + r['result']
    return err(r['in.lang'], r['code'], r['error'])


def eval_expect(r):
    if r['ok'] != 'true' or r['result'].startswith('ERROR') or r['result'].startswith('EVALUATE ERROR'):
        return err(r['in.lang'], r['code'], r['error'])
    res = r['result']
    f9 = re.search(r'f9=\[(.*?)\]', res, re.S)
    f0 = re.search(r'f0=\[(.*?)\]', res, re.S)
    c = re.search(r'calc@2024-01-15=(\S+)', res)
    ce = re.search(r'calc-error=(.*)$', res)
    calc = c.group(1) if c else ('E6' if ce and 'not valid' in ce.group(1) else '*')
    return 'ok:' + '|'.join([f9.group(1) if f9 else '*', f0.group(1) if f0 else '*', calc])


out = []
seen = set()


def add(op, lang, inp, arg, expect, src):
    key = (op, lang, inp, arg)
    if key in seen:
        return
    seen.add(key)
    out.append((src, op, lang, inp, arg, expect))


def split_at(inp):
    f, b = inp.rsplit(' @ ', 1)
    return f, b


by = collections.defaultdict(list)
for r in rows:
    by[fam(r['in.probe'])].append(r)
rnd = random.Random(20261110)

# --- CalcDate(Text): P01 P02 P06 P07 P08 P12calc P13calc P17text all; P03 P04 P05 stratified
for f in ['P01', 'P02', 'P06', 'P07', 'P08', 'P12calc', 'P13calc', 'P17text']:
    for r in by[f]:
        if f == 'P02' and r['ok'] == 'true' and not re.search(r'[DWMQY]0*(1|2|4|7|12|31|53|99|0)> ', r['in.input']):
            continue
        if f == 'P02' and r['ok'] != 'true' and not re.search(r'[DWMQY](0|00|8|13|5|32|54|100|10000)> ', r['in.input']):
            continue
        if f == 'P12calc' and r['in.lang'] not in ('1033', '1030', '1031', '1036', '1049', '1055') and rnd.random() < 0.6:
            continue
        if f == 'P01' and r['ok'] == 'true' and rnd.random() < 0.6:
            continue
        a, b = split_at(r['in.input'])
        add('calc', r['in.lang'], a, b, calc_expect(r), f)
# P03: per (sign, unit, base) one row, numbers taken in turn; every error kind kept once per (sign, unit)
groups = collections.defaultdict(list)
for r in by['P03']:
    m = re.match(r'<([+-]?)(WD|D|W|M|Q|Y)(\d+)> @ (\S+)$', r['in.input'])
    if not m:
        continue
    s, u, n, b = m.groups()
    groups[(s, u, b, r['ok'])].append(r)
keep_bases = sorted({k[2] for k in groups})[::2]
for (s, u, b, ok), lst in sorted(groups.items()):
    if b not in keep_bases and not (u == 'W' and ok == 'true'):
        continue
    picks = lst[len(lst) // 2:][:1]
    for r in picks:
        a, bb = split_at(r['in.input'])
        add('calc', r['in.lang'], a, bb, calc_expect(r), 'P03')
# P04: every form on 12 bases
bases4 = sorted({split_at(r['in.input'])[1] for r in by['P04']})
keep4 = set(bases4[:: max(1, len(bases4) // 12)])
for r in by['P04']:
    a, b = split_at(r['in.input'])
    if b in keep4:
        add('calc', r['in.lang'], a, b, calc_expect(r), 'P04')
# P05: per (form, base) one row
groups = collections.defaultdict(list)
for r in by['P05']:
    m = re.match(r'<([+-]?)(\d+)(WD|D|W|M|Q|Y)> @ (\S+)$', r['in.input'])
    if not m:
        a, b = split_at(r['in.input'])
        groups[('x', a, b, r['ok'])].append(r)
        continue
    s, n, u, b = m.groups()
    groups[(s, u, b, r['ok'])].append(r)
keep5 = sorted({k[2] for k in groups})[::2]
for k, lst in sorted(groups.items()):
    if k[2] not in keep5 and k[0] != 'x':
        continue
    for r in lst[len(lst) // 2:][:1]:
        a, b = split_at(r['in.input'])
        add('calc', r['in.lang'], a, b, calc_expect(r), 'P05')

# --- CalcDate(DateFormula): P07df P08df all, P17df
for f in ['P07df', 'P08df']:
    for r in by[f]:
        a, b = split_at(r['in.input'])
        add('calcdf', r['in.lang'], a, b, calc_expect(r), f)
for r in by['P17df']:
    a = r['in.input'].replace(' evaluated in 1033', '')
    add('calcdf', r['in.lang'], a, 'E1033:2024-01-15', 'ok:' + r['result'], 'P17df')

# --- Evaluate
for f in ['P01e', 'P04e', 'P13eval', 'P18ret', 'P18stmt', 'P12fixed', 'P11back', 'P11back-lower']:
    for r in by[f]:
        if f == 'P01e' and r['ok'] == 'true' and rnd.random() < 0.6:
            continue
        if f in ('P12fixed', 'P11back', 'P11back-lower') and r['in.lang'] not in ('1033', '1030', '1031', '1036', '1034', '1049', '1055', '1032', '1038', '1062', '2057') and rnd.random() < 0.85:
            continue
        exp = eval_expect(r)
        if f.startswith('P18') and r['ok'] == 'true':
            exp = 'err:Evaluate:*'  # return form: false; the text is in P18stmt
        add('eval', r['in.lang'], r['in.input'], '', exp, f)
for f in ['P01s-count', 'P01s-pos']:
    for r in by[f]:
        res = r['result']
        if res == 'ok':
            add('eval', r['in.lang'], r['in.input'], '', 'ok:*|*|*', f)
        elif res.startswith('evaluate Evaluate: '):
            add('eval', r['in.lang'], r['in.input'], '', f"err:Evaluate:{kind('Evaluate', res)}", f)
for r in by['P01s-digits']:
    if (r['ok'] != 'true' and rnd.random() < 0.4) or rnd.random() < 0.1:
        add('eval', r['in.lang'], r['in.input'], '', eval_expect(r), 'P01s-digits')
# P10: lengths 30..34 and the shortest refused one of each family
for f in [k for k in by if k.startswith('P10')]:
    for r in by[f]:
        n = len(r['in.input'])
        if 29 <= n <= 35 or n <= 3 or rnd.random() < 0.05:
            add('eval', r['in.lang'], r['in.input'], '', eval_expect(r), f.split(' ')[0])
# P12: every language: all accepted rows of a few shapes, refused ones sampled
for r in by['P12']:
    if (r['ok'] == 'true' and rnd.random() < 0.5) or rnd.random() < 0.05:
        add('eval', r['in.lang'], r['in.input'], '', eval_expect(r), 'P12')
# P09: per behaviour signature, one code point (and three more for the large letter class)
sig_rows = collections.defaultdict(list)
for r in by['P09 U+*']:
    sig_rows[int(r['in.probe'].split('U+')[1])].append(r)


def sig(cp, lst):
    def one(r):
        if r['ok'] == 'true':
            return 'ok:' + re.sub(r'calc@\S+', '', r['result']).replace(chr(cp), '#')
        return r['code'] + ':' + kind(r['code'], r['error'])
    return tuple(one(r) for r in lst)


by_sig = collections.defaultdict(list)
for cp, lst in sorted(sig_rows.items()):
    if 1 <= cp <= 7:
        continue
    by_sig[sig(cp, lst)].append(cp)
for s, cps in by_sig.items():
    pick = cps[:1] + ([c for c in cps if c > 0x7f][:3] if len(cps) > 100 else [])
    for cp in pick:
        for r in sig_rows[cp]:
            add('eval', r['in.lang'], r['in.input'], '', eval_expect(r), f'P09 U+{cp:04X}')

# --- Format, Format with length, Evaluate with a number
for r in by['P11']:
    m = re.match(r'f0=\[(.*?)\] f1=\[(.*?)\] f2=\[(.*?)\] f9=\[(.*?)\]', r['result'])
    few = r['in.lang'] in ('1033', '1030', '1031')
    forms = ('<1D>', '<1WD>', '<1W>', '<1M>', '<1Q>', '<1Y>', '<CD>', '<CWD>', '<-CM+1D>', '<D15>', '<+1Y-2Q+3M-4W+5D>')
    for std, v in zip(('0', '1', '2', '9'), m.groups()):
        if few or (std == '0' and r['in.input'] in forms):
            add('fmt', r['in.lang'], r['in.input'], std, 'ok:' + v, 'P11')
for r in by['P11all']:
    m = re.match(r'f0=\[(.*?)\] f2=\[(.*?)\] lang=(\d+)', r['result'])
    add('fmt', m.group(3), '<CD+1WD+1W+1M+1Q+1Y>', '0', 'ok:' + m.group(1), 'P11all ' + r['in.input'])
for r in by['P15fmt']:
    a, n = r['in.input'].rsplit(' std ', 1)
    if int(n) in (-3, -1, 0, 1, 2, 3, 8, 9, 10, 99) or r['in.input'].startswith('<-CM'):
        exp = ('ok:' + r['result'][1:-1]) if r['ok'] == 'true' else err(r['in.lang'], r['code'], r['error'])
        add('fmt', r['in.lang'], a, n, exp, 'P15fmt')
for r in by['P15len']:
    a, n = r['in.input'].rsplit(' len ', 1)
    if int(n) in (-3, -2, -1, 0, 1, 2, 3, 5, 6, 7, 18, 100):
        add('len', r['in.lang'], a, n, 'ok:' + r['result'][1:-1], 'P15len')
for r in by['P15eval']:
    a, n = r['in.input'].rsplit(' n ', 1)
    if int(n) in (-1, 0, 1, 2, 9, 10):
        exp = ('ok:' + r['result'][4:-1]) if r['result'].startswith('ok [') else err(r['in.lang'], r['code'], r['error'])
        add('evaln', r['in.lang'], a, n, exp, 'P15eval')

# --- equality, text outside Format
for r in by['P14eq']:
    add('eq', r['in.lang'], r['in.input'], '', 'ok:' + r['result'].split(' ')[0], 'P14eq')
for r in by['P16']:
    if r['in.input'] == 'StrSubstNo':
        add('text', r['in.lang'], '<-1Y+CM+1D>', '', 'ok:' + r['result'][1:-1], 'P16')

with open(OUT, 'w', encoding='utf-8') as fo:
    fo.write('# U1 DateFormula vectors: a compact selection of the probe runs on BC 29.1.57326.57497-W1\n')
    fo.write('# (tools/probes, probe u1-dateformula, runs 2026-11-10T11:21:08Z..11:21:28Z, 69,911 rows).\n')
    fo.write('# Selected and normalised by tools/vectors/u1-dateformula-compact.py; format: see\n')
    fo.write('# crates/al-runtime/tests/u1_vectors.rs. Escapes \\\\ \\t \\n \\r \\xNN.\n')
    fo.write('src\top\tlang\tinput\targ\texpect\n')
    for row in out:
        fo.write('\t'.join(esc(x) for x in row) + '\n')
c = collections.Counter(r[0].split(' ')[0] for r in out)
print(len(out), 'rows', dict(sorted(c.items())))
