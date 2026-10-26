"""Generated-input properties of kith-auth's access rules (people, memberships, bot persons, person removal and
revoke, the bots cap), run in-process against a throwaway auth.json. Method: .claude/skills/invitee-access/SKILL.md.
Started by kernel/tests/kith-auth-props, which pins hypothesis; run that, not this."""
import contextlib
import importlib.machinery
import importlib.util
import io
import os
import shutil
import tempfile
from pathlib import Path

from hypothesis import HealthCheck, given, settings, strategies as st

T = Path(tempfile.mkdtemp(prefix='kith-auth-props.'))
# a kith serve dir with no cloud-sync.json: a Mac never linked to the Kith cloud, so revoke writes here at once
os.environ.update(KITH_AUTH=str(T / 'auth.json'), HOME=str(T / 'home'), KITH_HOME=str(T / 'kith'),
                  KITH_SERVE_DIR=str(T / 'serve'), KITH_SERVE_PORT='1')
for k in ('KITH_STATE', 'HQ_STATE', 'KITH_PERSON', 'KITH_AGENT', 'HQ_HOME'):
    os.environ.pop(k, None)
(T / 'home').mkdir()
AGENTS = ['house', 'garden']
for a in AGENTS:
    d = T / 'kith' / 'agents' / a / 'role'
    d.mkdir(parents=True)
    (d / 'agent.md').write_text(f'---\nname: {a}\ndescription: test\n---\nbody\n')

KERNEL = Path(__file__).resolve().parent.parent
loader = importlib.machinery.SourceFileLoader('kith_auth', str(KERNEL / 'bin' / 'kith-auth'))
spec = importlib.util.spec_from_loader('kith_auth', loader)
ka = importlib.util.module_from_spec(spec)
loader.exec_module(ka)

OWNER = 'kai'
PEOPLE = ['sam', 'amy']
BOTS = ['bot-a', 'bot-b']
EVERYONE = '*'
ALL = [OWNER] + PEOPLE + BOTS + ['ghost']


def run(*argv):
    """One kith-auth command, in process. True when it was accepted (a refusal is an exit, not a crash)."""
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
        try:
            ka.main(list(argv))
        except SystemExit as e:
            return e.code in (None, 0)
    return True


def reset():
    if ka.AUTH.exists():
        ka.AUTH.unlink()
    assert run('person', OWNER, 'owner')


def auth():
    return ka.load_auth()


def allowed(agent, who):
    return ka.resolve(auth(), agent, who)['allowed'] is True


OPS = st.one_of(
    st.tuples(st.just('person'), st.sampled_from(PEOPLE)),
    st.tuples(st.just('person-rm'), st.sampled_from(PEOPLE)),
    st.tuples(st.just('member'), st.sampled_from(AGENTS), st.sampled_from(PEOPLE + BOTS + [EVERYONE]),
              st.sampled_from(['', 'preset=guest', 'preset=member', 'helpers=garden', 'deny=Bash', 'budget=7'])),
    st.tuples(st.just('member-rm'), st.sampled_from(AGENTS), st.sampled_from(PEOPLE + BOTS + [EVERYONE])),
    st.tuples(st.just('bot'), st.sampled_from(BOTS), st.sampled_from([OWNER] + PEOPLE + BOTS)),
    st.tuples(st.just('bot-rm'), st.sampled_from(BOTS)),
    st.tuples(st.just('cap'), st.sampled_from(['off', '50', '0', 'x'])),
    st.tuples(st.just('revoke'), st.sampled_from([OWNER] + PEOPLE + BOTS)),
    st.tuples(st.just('unrevoke'), st.sampled_from(PEOPLE)),
)


MEMBER = OPS_MEMBER = st.tuples(st.just('member'), st.sampled_from(AGENTS), st.sampled_from(PEOPLE + BOTS),
                                st.sampled_from(['', 'preset=guest', 'helpers=garden', 'deny=Bash', 'budget=7', 'preset=member']))
# A table first (people, their bots, rows with settings), then anything: random commands alone rarely build one.
SEQUENCES = st.tuples(
    st.lists(st.tuples(st.just('person'), st.sampled_from(PEOPLE)), max_size=2),
    st.lists(st.tuples(st.just('bot'), st.sampled_from(BOTS), st.sampled_from([OWNER] + PEOPLE)), max_size=2),
    st.lists(MEMBER, max_size=7),
    st.sampled_from([[], [('cap', '50')]]),
    st.lists(OPS, max_size=8),
).map(lambda parts: [op for part in parts for op in part])


def apply(op):
    kind = op[0]
    if kind == 'person':
        return run('person', op[1])
    if kind == 'person-rm':
        return run('person', op[1], '--rm')
    if kind == 'member':
        return run('member', op[1], op[2], *([op[3]] if op[3] else []))
    if kind == 'member-rm':
        return run('member', op[1], op[2], '--rm')
    if kind == 'bot':
        return run('bot', op[1], '--of', op[2])
    if kind == 'bot-rm':
        return run('bot', op[1], '--rm')
    if kind in ('revoke', 'unrevoke'):
        return run('person', op[1], f'--{kind}')
    return run('bots-cap', op[1])


def bot_ids(a):
    return [p for p, v in a['people'].items() if v.get('bot_of')]


def state_properties():
    """What must hold of the access table after any sequence of commands."""
    a = auth()
    for agent in AGENTS:
        rows = a['members'].get(agent, {})
        for who in ALL:
            r = ka.resolve(a, agent, who)
            if who == OWNER:
                continue
            if who not in a['people']:
                assert r['allowed'] is False, f'{who} is not a person but may use {agent}: {r}'
                continue
            if ka.cut_off(a, who):
                # (R1, R2, I6) revoked, or a bot of someone revoked: nothing, whatever rows they hold, `*` included.
                assert r['allowed'] is False, f'{who} is revoked (or their owner is) but may use {agent}: {r}'
                continue
            parent = a['people'][who].get('bot_of')
            if not parent:
                # (grant) a person is allowed only through a membership row of theirs or the everyone row.
                assert r['allowed'] is False or who in rows or EVERYONE in rows, f'{who} allowed on {agent} with no row: {r}'
                continue
            if r['allowed'] is not True:
                continue
            pr = ka.resolve(a, agent, parent)
            # (a) a bot has its own row, and its owner holds the agent right now.
            assert who in rows, f'bot {who} allowed on {agent} with no row of its own (the everyone row must not reach it)'
            assert pr['allowed'] is True and parent in a['people'], f'bot {who} allowed on {agent} but its owner {parent} is not'
            assert r['preset'] == 'guest', f'bot {who} runs as {r["preset"]}, not guest'
            if parent != OWNER:
                # never more than its owner: what the owner is denied stays denied, helpers and spend are the owner's.
                assert set(pr['deny']) <= set(r['deny']), f'bot {who} lost a deny of its owner: {r["deny"]} vs {pr["deny"]}'
                assert set(r['helpers']) <= set(pr['helpers']), f'bot {who} has helpers its owner lacks'
                assert r['budget'] == pr['budget'], f'bot {who} spends from {r["budget"]!r}, its owner from {pr["budget"]!r}'
            else:
                # (c) the owner's bots together are capped by owner_bots_budget, when set.
                assert r['budget'] == (a.get('owner_bots_budget') or ''), f'owner bot {who}: budget {r["budget"]!r}'
    for b in bot_ids(a):
        # a bot belongs to a person who still exists.
        assert ka.canonical(a, a['people'][b]['bot_of']) in a['people'], f'bot {b} outlived its owner'
    assert OWNER in a['people'] and ka.resolve(a, AGENTS[0], OWNER)['owner'] is True


@settings(max_examples=int(os.environ.get('PROPS_EXAMPLES', 600)), deadline=None, database=None,
          suppress_health_check=list(HealthCheck))
@given(SEQUENCES)
def test_access_never_exceeds_the_grant(ops):
    reset()
    for op in ops:
        before = auth()
        ok = apply(op)
        a = auth()
        if op[0] == 'person-rm' and ok:
            # (b) removing a person removes their bots, and with them every way in.
            gone = [op[1]] + [b for b, v in before['people'].items() if v.get('bot_of') == op[1]]
            for x in gone:
                assert x not in a['people'], f'{x} survived removing {op[1]}'
                assert all(x not in rows for rows in a['members'].values()), f'{x} keeps a membership'
                assert not any(allowed(ag, x) for ag in AGENTS), f'{x} still gets in after {op[1]} was removed'
        if op[0] == 'member-rm' and ok and op[2] != EVERYONE:
            _, agent, who = op
            covered = EVERYONE in a['members'].get(agent, {}) and who not in bot_ids(a)
            assert covered or not allowed(agent, who), f'{who} still uses {agent} after the row was removed'
            for b in bot_ids(a):
                if a['people'][b]['bot_of'] == who and who != OWNER:
                    assert not allowed(agent, b), f'bot {b} keeps {agent} after its owner {who} lost it'
        if op[0] in ('revoke', 'unrevoke'):
            # (R4) revoke and undo touch no row, so undo gives back exactly what they had; the owner and bots can't be
            # revoked (a bot goes dormant with its owner).
            for table in ('members', 'grants', 'presets', 'profiles'):
                assert a[table] == before[table], f'{op[0]} {op[1]} changed {table}'
            who = op[1]
            assert ok == (who in before['people'] and who != OWNER and not before['people'][who].get('bot_of')), \
                f'{op}: accepted {ok}'
            if ok and op[0] == 'unrevoke':
                assert a['people'] == {
                    k: ({x: y for x, y in v.items() if x != 'revoked'} if k == who else v)
                    for k, v in before['people'].items()}, f'undo {who} changed more than the revoked state'
        if op[0] == 'cap' and ok:
            assert a.get('owner_bots_budget', '') == ('' if op[1] == 'off' else op[1])
        if op[0] == 'cap' and not ok:
            assert a.get('owner_bots_budget') == before.get('owner_bots_budget'), 'a refused cap changed the cap'
        state_properties()


if __name__ == '__main__':
    try:
        test_access_never_exceeds_the_grant()
        print('ok   kith-auth access properties')
    finally:
        shutil.rmtree(T, ignore_errors=True)
