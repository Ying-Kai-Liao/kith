"""Self-test of kith-difftest: the same line twice is the same; a deliberately different NEW side fails, with a diff;
the same difference listed as intended passes; a listed difference that does not occur is stale.
Run by kernel/tests/kith-difftest-check."""
import io
import os
import stat
import sys
import tempfile
import unittest
from importlib.machinery import SourceFileLoader
from pathlib import Path

HERE = Path(__file__).resolve().parent
D = SourceFileLoader('kith_difftest', str(HERE / 'kith-difftest')).load_module()

CASE = {
    'name': 'ls-one', 'command': 'ls',
    'setup': [{'write': 'kith/agents/echo/role/agent.md', 'text': '---\nname: echo\ndescription: d\nmodel: sonnet\n---\nbody\n'},
              {'mkdir': 'kith/state'}],
    'run': [['ls'], ['ls', '--json']],
}
KITH = [str(D.KERNEL / 'bin' / 'kith')]


def prefixes(new):
    return {'default': {'old': KITH, 'new': KITH}, 'old': None, 'new': new}


def go(case, new=None, intended=()):
    out = io.StringIO()
    failed, stale, summary = D.run_all([case], prefixes(new), list(intended), jobs=1, out=out)
    return failed, stale, summary, out.getvalue()


class SelfTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        t = Path(cls.tmp.name)
        # NEW = the real kith, but it also prints a line, and drops a file and a SQLite row into HOME
        w = t / 'differs'
        w.write_text('#!/bin/sh\n"%s" "$@"; rc=$?\necho "extra line"\necho x > "$HOME/stray.txt"\n'
                     'python3 -c "import sqlite3,os;c=sqlite3.connect(os.environ[\'HOME\']+\'/s.db\');'
                     'c.execute(\'create table if not exists t(a)\');c.execute(\'insert into t values (1)\');c.commit()"\n'
                     'exit $rc\n' % KITH[0])
        w.chmod(w.stat().st_mode | stat.S_IXUSR)
        cls.differs = [str(w)]
        # NEW = the real kith run from another cwd path form: must still be the same after normalising
        s = t / 'same'
        s.write_text('#!/bin/sh\nexec "%s" "$@"\n' % KITH[0])
        s.chmod(s.stat().st_mode | stat.S_IXUSR)
        cls.same = [str(s)]

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_same_line_twice_is_the_same(self):
        failed, stale, summary, out = go(CASE, self.same)
        self.assertEqual((failed, stale), ([], []), out)
        self.assertTrue(summary['ls']['green'])
        self.assertIn('ok    ls-one', out)

    def test_different_side_fails_with_a_diff(self):
        failed, stale, summary, out = go(CASE, self.differs)
        self.assertEqual(failed, ['ls-one'])
        self.assertFalse(summary['ls']['green'])
        self.assertEqual(summary['ls']['cases'], {'ls-one': 'differs'})
        self.assertIn('+extra line', out)                       # stdout
        self.assertIn('state:home/stray.txt', out)              # a file only NEW wrote
        self.assertIn('state:home/s.db', out)                   # a SQLite row only NEW wrote
        self.assertIn('+{"a": 1}', out)

    def test_listed_difference_passes(self):
        intended = [{'case': 'ls-*', 'facet': 'step*.stdout', 'why': 'x'},
                    {'case': 'ls-one', 'facet': 'step*.json', 'why': 'y'},
                    {'case': 'ls-one', 'facet': 'state:home/*', 'why': 'z'}]
        failed, stale, summary, out = go(CASE, self.differs, intended)
        self.assertEqual((failed, stale), ([], []), out)
        self.assertEqual(summary['ls']['cases'], {'ls-one': 'intended'})

    def test_partly_listed_still_fails_and_unused_entry_is_stale(self):
        intended = [{'case': 'ls-one', 'facet': 'step*.stdout', 'why': 'x'},
                    {'case': 'ls-one', 'facet': 'step*.exit', 'why': 'never differs'}]
        failed, stale, _, out = go(CASE, self.differs, intended)
        self.assertEqual(failed, ['ls-one'])
        self.assertEqual([e['facet'] for e in stale], ['step*.exit'])
        self.assertIn('STALE', out)

    def test_exit_code_and_stderr_are_compared(self):
        with tempfile.TemporaryDirectory() as t:
            w = Path(t) / 'fail'
            w.write_text('#!/bin/sh\necho oops >&2\nexit 7\n')
            w.chmod(0o755)
            failed, _, _, out = go(CASE, [str(w)])
        self.assertEqual(failed, ['ls-one'])
        self.assertIn('+7', out)
        self.assertIn('+oops', out)

    def test_json_field_compare_and_facet_filter(self):
        """pick: one field of each side's JSON, under its own key; only: the other facets (state, stderr) are left out."""
        def sh(script):
            return ['/bin/sh', '-c', script, 'sh']
        case = {'name': 'pick', 'run': [['a']], 'run_new': [['b']], 'pick': {'old': 'system_prompt', 'new': 'prompt'},
                'only': ['pick', 'exit']}
        old = sh('echo \'{"system_prompt": "hello", "env": 1}\'; echo noise >&2; echo x > "$HOME/stray"')
        same = sh('echo \'{"prompt": "hello", "other": 2}\'')
        differs = sh('echo \'{"prompt": "bye"}\'')
        pre = lambda new: {'default': {'old': KITH, 'new': KITH}, 'old': old, 'new': new}   # noqa: E731
        out = io.StringIO()
        failed, _, _, _ = (lambda r: (r[0], r[1], r[2], None))(D.run_all([case], pre(same), [], jobs=1, out=out))
        self.assertEqual(failed, [], out.getvalue())
        out = io.StringIO()
        failed, _, _, _ = (lambda r: (r[0], r[1], r[2], None))(D.run_all([case], pre(differs), [], jobs=1, out=out))
        self.assertEqual(failed, ['pick'])
        self.assertIn('step0.pick', out.getvalue())
        self.assertIn('+bye', out.getvalue())

    def test_throwaway_roots_never_leak(self):
        # Two different roots and a generated id/time in the output normalise to the same text.
        with tempfile.TemporaryDirectory() as t:
            w = Path(t) / 'noisy'
            w.write_text('#!/bin/sh\necho "$HOME $KITH_HOME $TMPDIR 2026-11-03T10:11:12Z 1762160000 '
                         '123e4567-e89b-12d3-a456-426614174000"\n')
            w.chmod(0o755)
            case = dict(CASE, run=[['x']])
            both = {'default': {}, 'old': [str(w)], 'new': [str(w)]}
            out = io.StringIO()
            failed, _, _ = D.run_all([case], both, [], jobs=1, out=out)
        self.assertEqual(failed, [], out.getvalue())

    def test_unknown_setup_step_is_an_error_not_a_pass(self):
        failed, _, summary, out = go(dict(CASE, setup=[{'bogus': 1}]), self.same)
        self.assertEqual(failed, ['ls-one'])
        self.assertIn('ERROR', out)

    def test_real_home_is_scrubbed(self):
        os.environ['KITH_HOME'] = '/nonexistent/real'
        try:
            failed, _, _, out = go(CASE, self.same)
        finally:
            del os.environ['KITH_HOME']
        self.assertEqual(failed, [], out)

    def test_mask_keys_and_masked_file_names(self):
        """mask_keys: a case's own meaningless values (a clock, a pid) are masked in JSON; normalize_paths: a file named by
        an id the program makes up is one facet whatever the id, and two files with one masked name are an error."""
        def sh(script):
            return ['/bin/sh', '-c', script, 'sh']
        prog = sh('id=$(date +%N)$$; mkdir -p "$HOME/w"; echo "{\\"n\\": 1, \\"at\\": $(date +%N), \\"pid\\": $$}" > "$HOME/w/id$id.json"')
        base = {'name': 'mask', 'run': [['x']], 'normalize': [['id\\d+', 'ID']]}
        both = {'default': {}, 'old': prog, 'new': prog}
        out = io.StringIO()
        failed, _, _ = D.run_all([base], both, [], jobs=1, out=out)
        self.assertEqual(failed, ['mask'], 'unmasked, the two runs differ: ' + out.getvalue())
        out = io.StringIO()
        failed, _, _ = D.run_all([dict(base, normalize_paths=True, mask_keys=['at', 'pid'])], both, [], jobs=1, out=out)
        self.assertEqual(failed, [], out.getvalue())
        two = sh('mkdir -p "$HOME/w"; echo 1 > "$HOME/w/id1.json"; echo 2 > "$HOME/w/id2.json"')
        out = io.StringIO()
        failed, _, _ = D.run_all([dict(base, normalize_paths=True)], {'default': {}, 'old': two, 'new': two}, [], jobs=1, out=out)
        self.assertEqual(failed, ['mask'])
        self.assertIn('once masked', out.getvalue())

    def test_unswitched_golden_cases_are_skipped_out_loud_until_required(self):
        """A golden replay on the Rust program skips a case marked unswitched and says so; --require-switched runs it."""
        case = dict(CASE, unswitched='no Rust side yet')
        D.GOLDEN, D.REPLAY_SIDE = Path('/nonexistent-golden'), 'new'
        try:
            out = io.StringIO()
            failed, _, summary = D.run_all([case], prefixes(None), [], jobs=1, out=out)
            self.assertEqual(failed, [])
            self.assertIn('skip  ls-one  (not switched: no Rust side yet)', out.getvalue())
            self.assertEqual(summary['ls']['cases'], {'ls-one': 'unswitched'})
            self.assertFalse(summary['ls']['green'])
            D.REQUIRE_SWITCHED = True
            out = io.StringIO()
            failed, _, _ = D.run_all([case], prefixes(None), [], jobs=1, out=out)
            self.assertEqual(failed, ['ls-one'], 'with --require-switched it runs (here: no record, so it errors)')
        finally:
            D.GOLDEN, D.REPLAY_SIDE, D.REQUIRE_SWITCHED = None, 'old', False


if __name__ == '__main__':
    unittest.main(argv=[sys.argv[0], '-v'])
