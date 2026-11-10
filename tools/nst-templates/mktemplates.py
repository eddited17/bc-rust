# Build bc-nst's NST templates from a capture of your own BC service tier.
#
# usage: python3 tools/nst-templates/mktemplates.py [<nst.jsonl> [<outDir>]]
#   defaults: data/capture/nst-real/nst.jsonl and data/nst-templates/
#
# <nst.jsonl> is a tools/nst-proxy/proxy.mjs log of Microsoft's web client talking to a real
# BC 29.1 service tier. The capture must open the pages listed in SRC (Customer List 22,
# Customer Card 21, Sales Order 42 with its lines part 46, a card part 9080 and the Business
# Manager role center 9022) and show the Notes and Links FactBoxes.
#
# Writes into <outDir>, which crates/bc-nst/build.rs reads (data/ is not committed):
#   sysactions.json                  the command bar skeleton per page type. Nodes the page
#                                    defines become {"$slot": <area>} markers, and groups that
#                                    only exist when their slot has actions carry "$ifslot".
#   systemparts/notes.json           the GET client/metadata/systemparts/Notes reply, as is
#   systemparts/recordlinks.json     the same for RecordLinks
import json, os, sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..'))
src_path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, 'data/capture/nst-real/nst.jsonl')
out_dir = sys.argv[2] if len(sys.argv) > 2 else os.path.join(ROOT, 'data/nst-templates')
records = [json.loads(l) for l in open(src_path, encoding='utf-8')]

def masters_in(r):
    b = r.get('body')
    if r.get('kind') != 'response' or not isinstance(b, dict) or not isinstance(b.get('MasterPages'), list):
        return {}
    return {mp['MasterPage']['obj']['IDED_1']: mp['MasterPage']['obj'] for mp in b['MasterPages']}

def master(line, pid):
    # the record numbers are those of the original capture; other captures fall back to the
    # first pagewithdependencies reply that holds the page
    if line < len(records) and pid in masters_in(records[line]):
        return masters_in(records[line])[pid]
    for r in records:
        m = masters_in(r)
        if pid in m:
            return m[pid]
    sys.exit(f'no pagewithdependencies reply for page {pid} in {src_path}')

SRC = {  # type -> (record number in the original capture, page id)
    'list': (260, 22), 'card': (316, 21), 'document': (569, 42),
    'listpart': (569, 46), 'cardpart': (260, 9080), 'rolecenter': (36, 9022),
}
RANGES = [(2000001000, 2000001100), (2000003000, 2000003100), (2000004000, 2000004200),
          (2000005000, 2000005100), (2000009000, 2000009100), (2000013000, 2000013100)]
def is_sys(o):
    i = o.get('IDED_1')
    return i is not None and any(a <= i < b for a, b in RANGES)
SLOT = {'Control_Promoted': 'promoted', 'Control_New_Document': 'creation', 'Control_Actions': 'processing',
        'Control_Reports': 'reporting', 'Control_Related_Information': 'navigation'}
def conv(n, report):
    o = dict(n['obj'])
    name = o.get('NED_1')
    if name == 'Control_Auto_Query':
        return None
    kids = o.get('CBD_1', [])
    slot = SLOT.get(name)
    out = []
    placed = False
    for k in kids:
        if is_sys(k['obj']):
            c = conv(k, report)
            if c is not None:
                out.append(c)
        elif slot and not placed:
            out.append({'$slot': slot}); placed = True
    if slot and not placed:
        # no page node captured here: the slot goes before trailing system groups that follow
        # page actions in other captures (role center: Page/Refresh), else at the end
        pos = len(out)
        if name == 'Control_Actions':
            for j, c in enumerate(out):
                if c['obj'].get('NED_1') in ('Page', 'Control_List'):
                    pos = j; break
        out.insert(pos, {'$slot': slot})
        report.append(f'  slot {slot} in {name} placed at {pos} of {len(out)} (no page nodes captured)')
    if 'CBD_1' in o:
        o['CBD_1'] = out
    if name == 'Control_New_Document':
        o = {'$ifslot': 'creation', **o}
    return {'$polyType': n['$polyType'], 'obj': o}
res = {}
for ty, (line, pid) in SRC.items():
    mp = master(line, pid)
    cb = mp['MP_1']
    report = []
    top = [c for c in (conv(k, report) for k in cb['obj']['CBD0_1']) if c is not None]
    root = {k: v for k, v in cb['obj'].items() if k in ('CBD0_2', 'CBD0_3')}
    res[ty] = {'source': f'page {pid}', 'root': root, 'CBD0_1': top}
    print(ty, 'from page', pid, 'top:', [t['obj'].get('NED_1') or t['$polyType'].split('.')[-1] for t in top])
    for r in report: print(r)
# page 42 has no area(Creation): give the document template the card's "New Document" group
# (card: separator 2000001048, group 2000001049) behind its separators, id 2000003050 (unused there)
def find(nodes, name):
    for n in nodes:
        if n.get('obj', {}).get('NED_1') == name: return n
card_nd = find(find(res['card']['CBD0_1'], 'Control_Actions')['obj']['CBD_1'], 'Control_New_Document')
doc_actions = find(res['document']['CBD0_1'], 'Control_Actions')['obj']['CBD_1']
nd = json.loads(json.dumps(card_nd)); nd['obj']['IDED_1'] = 2000003050
slot_at = next(i for i, n in enumerate(doc_actions) if n.get('$slot') == 'processing')
doc_actions.insert(slot_at, nd)
os.makedirs(os.path.join(out_dir, 'systemparts'), exist_ok=True)
out = os.path.join(out_dir, 'sysactions.json')
with open(out, 'w', encoding='utf-8') as f:
    f.write('{\n')
    items = list(res.items())
    for i, (k, v) in enumerate(items):
        f.write(json.dumps(k) + ': ' + json.dumps(v, ensure_ascii=False) + (',\n' if i < len(items) - 1 else '\n'))
    f.write('}\n')
print('wrote', out)
for part, name in [('Notes', 'notes'), ('RecordLinks', 'recordlinks')]:
    reply = next((r for r in records if r.get('kind') == 'response' and r.get('status') == 200
                  and r.get('path', '').lower().endswith('/client/metadata/systemparts/' + part.lower())), None)
    if reply is None:
        print(f'no systemparts/{part} reply in the capture; bc-nst keeps its built-in {name} part')
        continue
    out = os.path.join(out_dir, 'systemparts', name + '.json')
    with open(out, 'w', encoding='utf-8') as f:
        f.write(json.dumps(reply['body']) + '\n')
    print('wrote', out)
