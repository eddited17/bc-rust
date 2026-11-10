"""Unit tests for tools/probes (no VM): python3 -B -m unittest test_probes (from tools/probes; -B keeps __pycache__ out of the repo)."""
import json
import tempfile
import unittest
from pathlib import Path

import probes as P


class Generators(unittest.TestCase):
    def test_nines(self):
        self.assertEqual(P.gen_values({'kind': 'nines', 'max_digits': 2}), [0, 1, 9, 10, 99, 100])
        v = P.gen_values({'kind': 'nines', 'max_digits': 1, 'signs': True, 'fractions': ['.5']})
        self.assertEqual(v, ['0', '0.5', '1', '1.5', '9', '9.5', '10', '10.5', '-1', '-1.5', '-9', '-9.5',
                             '-10', '-10.5'])

    def test_text_lengths_and_dates(self):
        self.assertEqual(P.gen_values({'kind': 'text_lengths', 'fill': 'ab', 'from': 1, 'to': 3}), ['a', 'ab', 'aba'])
        self.assertEqual(P.gen_values({'kind': 'month_ends', 'years': [2024]})[1], '2024-02-29')


class Rendering(unittest.TestCase):
    def test_text_expr(self):
        self.assertEqual(P.al_text_expr("it's"), "'it''s'")
        self.assertEqual(P.al_text_expr('a\tb'), "'a' + BcrCh(9) + 'b'")
        self.assertEqual(P.al_text_expr(''), "''")

    def test_dates_times(self):
        self.assertEqual(P.al_date('2024-02-29'), '20240229D')
        self.assertEqual(P.al_date('C2024-12-31'), 'ClosingDate(20241231D)')
        self.assertEqual(P.al_time('13:45:01.5'), '134501.5T')

    def test_bigint(self):
        self.assertEqual(P.al_bigint_stmts(5), ['R := 5;'])
        self.assertEqual(P.al_bigint_stmts(-3000000000),
                         ['R := 3000;', 'R := R * 1000000 + 0;', 'R := -R;'])

    def test_json_fragment(self):
        d = P.Dim('v', 'Decimal', [])
        self.assertEqual(P.json_fragment(d, '1.50'), '"1.50"')
        self.assertEqual(P.json_fragment(P.Dim('i', 'Integer', []), 7), '7')


class Build(unittest.TestCase):
    def test_grid_and_raw(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            raw = t / 'X.Codeunit.al'
            raw.write_text('// area: misc\ntable 50911 "X Row"\n{\n}\n\ncodeunit 50910 "X Probes"\n{\n'
                           '    procedure RunAll(): Text\n    begin\n        P01_A();\n        exit(Take());\n    end;\n\n'
                           '    procedure Take(): Text\n    begin\n    end;\n\n    procedure P01_A()\n    begin\n    end;\n}\n')
            probes = [P.load_probe(P.SPEC_DIR / 'selftest.toml'), P.load_probe(raw, 'x-probe')]
            sha, files = P.build_app(probes, '1.0.0.1', t / 'app')
            src = {f.name: f.read_text() for f in (t / 'app' / 'src').iterdir()}
            raw_al = src['Raw_x-probe_X.Codeunit.al']
            self.assertIn('table 50410 "X Row"', raw_al)
            self.assertIn('codeunit 50411 "X Probes"', raw_al)
            self.assertIn("'selftest'", src['ProbeRegistry.Codeunit.al'])
            self.assertEqual(probes[1].steps, ['P01_A'])
            self.assertTrue(any(n.startswith('Wrap') for n in files['x-probe']))
            app = json.loads((t / 'app' / 'app.json').read_text())
            self.assertEqual(app['idRanges'], [{'from': 50400, 'to': 50499}])


class Vectors(unittest.TestCase):
    def test_normalize_raw(self):
        x = P.normalize_line({'probe': 'p', 'ctx': {}, 'cell': 3,
                              'raw': {'probe': 'P01', 'input': '1D', 'result': 'ERROR', 'error': 'bad', 'code': 'E'}})
        self.assertEqual(x['in'], {'probe': 'P01', 'input': '1D'})
        self.assertFalse(x['ok'])

    def test_escape(self):
        self.assertEqual(P.tsv_escape('a\tb\nc\\d\x01'), 'a\\tb\\nc\\\\d\\x01')
        self.assertEqual(P.tsv_escape(True), 'true')

    def test_compile(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / 'ev' / 'p1'
            d.mkdir(parents=True)
            lines = [{'probe': 'p1', 'ctx': {'lang': 1031}, 'meta': {'globalLanguage': 1031}},
                     {'probe': 'p1', 'ctx': {'lang': 1031}, 'cell': 2, 'in': {'v': 2}, 'ok': True, 'result': 'b',
                      'error': '', 'code': ''},
                     {'probe': 'p1', 'ctx': {'lang': 1031}, 'cell': 1, 'in': {'v': 1}, 'ok': False, 'result': '',
                      'error': 'x\ty', 'code': 'E'}]
            for ts in ('20260101T000000Z', '20260101T000001Z'):
                (d / f'{ts}.jsonl').write_text(''.join(json.dumps(x) + '\n' for x in lines))
                (d / f'{ts}.meta.json').write_text(json.dumps({
                    'probe': 'p1', 'area': 'a', 'utc': ts, 'bc_version': '29', 'probe_source_sha256': 'abc'}))
            out, n, c = P.compile_vectors(d, Path(t) / 'vec')
            self.assertEqual((n, c), (2, 0))
            body = [l for l in out.read_text().split('\n') if l and not l.startswith('#')]
            self.assertEqual(body[0], 'ctx.lang\tin.v\tok\tresult\tcode\terror')
            self.assertEqual(body[1], '1031\t1\tfalse\t\tE\tx\\ty')
            # a different result for the same input in a later run is a conflict
            lines[1]['result'] = 'changed'
            (d / '20260101T000002Z.jsonl').write_text(''.join(json.dumps(x) + '\n' for x in lines))
            (d / '20260101T000002Z.meta.json').write_text(json.dumps({
                'probe': 'p1', 'area': 'a', 'utc': '20260101T000002Z', 'bc_version': '29',
                'probe_source_sha256': 'abc'}))
            with self.assertRaises(P.ProbeError):
                P.compile_vectors(d, Path(t) / 'vec')


if __name__ == '__main__':
    unittest.main()
