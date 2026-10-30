"""Generated-input properties of kith-auth's access rules (people, memberships, bot persons, person removal and
revoke, the bots cap, access beyond the box, membership env and modes, job ticks and helper grants), run in-process against a throwaway auth.json. Method: .claude/skills/invitee-access/SKILL.md.
Started by kernel/tests/kith-auth-props, which pins hypothesis; run that, not this."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import shutil
import tempfile
import time
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
# house's one job holds a host beyond the box, garden as a helper and a door: what a tick may grant
(T / 'kith' / 'agents' / 'house' / 'role' / 'capabilities.yaml').write_text(
    'helpers: [garden]\ndoors:\n  - name: render\n    argv: [echo, hi]\n'
    'jobs:\n  - name: work\n    domains: [a.example]\n    helpers: [garden]\n    doors: [render]\n')

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
# access beyond the box (the always half of an escape card): absolute paths outside every secret and rule-W place,
# one inside another so a write grant of the folder covers a read below it, and plain host names
BEYOND = [('read', '/work/a'), ('read', '/work/a/b'), ('write', '/work/a'), ('write', '/work/c'),
          ('domain', 'a.example'), ('domain', 'b.example')]
KINDS = {'read': 'read', 'write': 'write', 'domain': 'domains'}
FREE_SHELL = {'bypassPermissions', 'auto'}
# env a membership may set, and names that would change how the run starts or which kith it talks to
ENV_OK, ENV_BAD = ['FOO'], ['PATH', 'KITH_HOME', 'DYLD_INSERT_LIBRARIES']


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


# auth.json as an older kith-auth, or a hand edit, may leave it: a row, a grant or an invite for an id that is no person
# (yet), and a free-shell mode written without --trusted
HAND = ['row', 'row-auto', 'grant', 'invite']
SETTINGS = ['', '', 'preset=guest', 'preset=member', 'helpers=garden', 'deny=Bash', 'budget=7', 'mode=auto',
            'mode=bypassPermissions'] + [f'env.{k}=x' for k in ENV_OK + ENV_BAD]
OPS = st.one_of(
    st.tuples(st.just('person'), st.sampled_from(PEOPLE)),
    st.tuples(st.just('person-rm'), st.sampled_from(PEOPLE)),
    st.tuples(st.just('member'), st.sampled_from(AGENTS), st.sampled_from([OWNER] + PEOPLE + BOTS + [EVERYONE]),
              st.sampled_from(SETTINGS)),
    st.tuples(st.just('member-rm'), st.sampled_from(AGENTS), st.sampled_from(PEOPLE + BOTS + [EVERYONE])),
    st.tuples(st.just('bot'), st.sampled_from(BOTS), st.sampled_from([OWNER] + PEOPLE + BOTS)),
    st.tuples(st.just('bot-rm'), st.sampled_from(BOTS)),
    st.tuples(st.just('cap'), st.sampled_from(['off', '50', '0', 'x'])),
    st.tuples(st.just('revoke'), st.sampled_from([OWNER] + PEOPLE + BOTS)),
    st.tuples(st.just('unrevoke'), st.sampled_from(PEOPLE)),
    st.tuples(st.just('access'), st.sampled_from(AGENTS), st.sampled_from(ALL + [EVERYONE]), st.sampled_from(BEYOND)),
    st.tuples(st.just('access-rm'), st.sampled_from(AGENTS), st.sampled_from(PEOPLE + BOTS), st.sampled_from(BEYOND)),
    st.tuples(st.just('hand'), st.sampled_from(AGENTS), st.sampled_from(PEOPLE + BOTS + ['ghost']), st.sampled_from(HAND)),
    st.tuples(st.just('job'), st.sampled_from(AGENTS), st.sampled_from(ALL + [EVERYONE])),
    st.tuples(st.just('job-rm'), st.sampled_from(AGENTS), st.sampled_from(PEOPLE + BOTS)),
    st.tuples(st.just('helper'), st.sampled_from(AGENTS), st.sampled_from(ALL + [EVERYONE]), st.sampled_from(['', '--once'])),
)


# the owner may have a row too (kith-auth takes one): nothing beyond the box is ever written to it
MEMBER = st.tuples(st.just('member'), st.sampled_from(AGENTS), st.sampled_from([OWNER] + PEOPLE + BOTS),
                   st.sampled_from(SETTINGS))
# a person with a bot, both members of one agent: random commands alone rarely build the pair the bot rules are about
TEAM = st.tuples(st.sampled_from(AGENTS), st.sampled_from(PEOPLE), st.sampled_from(BOTS)).map(
    lambda t: [('person', t[1]), ('bot', t[2], t[1]), ('member', t[0], t[1], ''), ('member', t[0], t[2], '')])
# maybe the person is cut off first (revoked, or their row taken), so a row of their own no longer means they may use
# the agent now; a place for the person (or not), then the same place for each bot: a bot's grant sticks only under an
# owner who holds it; then, maybe, the person loses it again, which must take it from their bot at the next resolve
ACCESS = st.tuples(st.sampled_from(AGENTS), st.sampled_from(PEOPLE), st.sampled_from(BEYOND), st.booleans(),
                   st.booleans(), st.sampled_from(['', '', 'revoke', 'member-rm'])).map(
    lambda t: {'': [], 'revoke': [('revoke', t[1])], 'member-rm': [('member-rm', t[0], t[1])]}[t[5]]
    + [('access', t[0], who, t[2]) for who in ([t[1]] if t[3] else []) + BOTS]
    + ([('access-rm', t[0], t[1], t[2])] if t[4] else []))
# the same for a job tick and a helper's always or once (the edge cards): the person, then each bot
JOBS = st.tuples(st.sampled_from(PEOPLE), st.booleans(), st.sampled_from(['job', 'helper']),
                 st.sampled_from(['', '--once']), st.sampled_from(['', '', 'revoke', 'member-rm'])).map(
    lambda t: {'': [], 'revoke': [('revoke', t[0])], 'member-rm': [('member-rm', 'house', t[0])]}[t[4]]
    + [(t[2], 'house', who) + ((t[3],) if t[2] == 'helper' else ()) for who in ([t[0]] if t[1] else []) + BOTS])
# A table first (people, their bots, rows with settings), then anything: random commands alone rarely build one.
SEQUENCES = st.tuples(
    st.lists(st.tuples(st.just('hand'), st.sampled_from(AGENTS), st.sampled_from(PEOPLE + BOTS), st.sampled_from(HAND)),
             max_size=2),
    st.lists(st.tuples(st.just('person'), st.sampled_from(PEOPLE)), max_size=2),
    st.lists(st.tuples(st.just('bot'), st.sampled_from(BOTS), st.sampled_from([OWNER] + PEOPLE)), max_size=2),
    st.lists(MEMBER, max_size=7),
    st.lists(TEAM, max_size=2).map(lambda xs: [op for x in xs for op in x]),
    # hand edits again, now that people exist (the ones above are wiped by the person or bot they come before)
    st.lists(st.tuples(st.just('hand'), st.sampled_from(AGENTS), st.sampled_from(PEOPLE + BOTS), st.sampled_from(HAND)),
             max_size=2),
    st.sampled_from([[], [('cap', '50')]]),
    st.lists(ACCESS, max_size=3).map(lambda xs: [op for x in xs for op in x]),
    st.lists(JOBS, max_size=2).map(lambda xs: [op for x in xs for op in x]),
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
    if kind == 'hand':
        _, agent, who, what = op
        a = json.loads(ka.AUTH.read_text())
        if what == 'grant':
            a.setdefault('grants', {})[f'{agent}@{who}'] = {'mail': ['work']}
        elif what == 'invite':
            a.setdefault('invites', {})[f'HAND{who.upper()}'] = {'person': who, 'expires': time.time() + 600}
        else:
            a.setdefault('members', {}).setdefault(agent, {})[who] = {'mode': 'auto'} if what == 'row-auto' else {}
        ka.AUTH.write_text(json.dumps(a))
        return True
    if kind in ('access', 'access-rm'):
        return run('access', op[1], op[2], op[3][0], op[3][1], *(['--rm'] if kind == 'access-rm' else []))
    if kind in ('job', 'job-rm'):
        return run('job', op[1], op[2], 'work', *(['--rm'] if kind == 'job-rm' else []))
    if kind == 'helper':
        return run('helper', op[1], op[2], 'garden', *([op[3]] if op[3] else []))
    return run('bots-cap', op[1])


def bot_ids(a):
    return [p for p, v in a['people'].items() if v.get('bot_of')]


def covered(have, kind, value):
    """Whether an access dict reaches value: a domain it lists; a path at or below a held folder (write covers read).
    Written here, not taken from kith-auth, so the cap is checked against the rule and not against itself."""
    if kind == 'domains':
        return value in have.get('domains', [])
    roots = have.get('write', []) + (have.get('read', []) if kind == 'read' else [])
    return any(value == r or value.startswith(r.rstrip('/') + '/') for r in roots)


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
                # (D) a helper's "just this once" is checked in the same step as revoke: it never lets them in.
                for via in AGENTS:
                    assert ka.may(a, who, agent, via, once=True)[0] is False, f'{who} is revoked but a once lets {agent} in'
                continue
            parent = a['people'][who].get('bot_of')
            if r['allowed'] is True:
                # (F11 step 1) a free shell (bypassPermissions, auto) only with --trusted, which these commands never set.
                assert r['mode'] not in FREE_SHELL, f'{who} runs {agent} in {r["mode"]} without --trusted'
                # (membership env) nothing that changes how the run starts or which kith it talks to.
                bad = [k for k in r['env'] if k not in ENV_OK]
                assert not bad, f'{who} on {agent} carries env {bad}'
            if not parent:
                # (grant) a person is allowed only through a membership row of theirs or the everyone row.
                assert r['allowed'] is False or who in rows or EVERYONE in rows, f'{who} allowed on {agent} with no row: {r}'
                if r['allowed'] is True and who not in rows:
                    # (escape always) access beyond the box is a person's own: the everyone row never carries it.
                    assert not any(r['access'].values()), f'{who} reaches {r["access"]} on {agent} through the everyone row'
                    # (job) ticks sit on a person's own row: the everyone row carries no job and no door.
                    assert not r['jobs'] and not r['doors'], f'{who} has {r["jobs"]} {r["doors"]} on {agent} through the everyone row'
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
                # (escape always) a bot reaches beyond the box only where its owner does, right now: an owner who loses
                # a place takes it from their bot at the next resolve, whatever the bot's row still lists.
                for k, vals in r['access'].items():
                    for v in vals:
                        assert covered(pr['access'], k, v), f'bot {who} reaches {k} {v} on {agent}; its owner {pr["access"]}'
                # (job) a ticked job gives a bot only the doors its owner has.
                assert set(r['doors']) <= set(pr['doors']), f'bot {who} has doors {r["doors"]}, its owner {pr["doors"]}'
                # (D) a bot's once for a helper counts only while its owner may call that helper now.
                for via in AGENTS:
                    if via != agent and ka.may(a, who, agent, via, once=True)[0]:
                        assert ka.may(a, parent, agent, via)[0], f'bot {who} reaches {agent} via {via} past its owner'
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
            # (retired) the id of a removed person, never a bot's, is never handed out again.
            p = before['people'].get(op[1])
            assert (op[1] in a['retired']) == (p is not None and not p.get('bot_of')) or op[1] in before['retired'], \
                f'{op[1]}: retired {op[1] in a["retired"]}'
        if op[0] in ('person', 'bot') and op[1] not in before['people'] and op[1] in before['retired']:
            assert not ok and op[1] not in a['people'], f'{op}: a retired id was given to someone new'
        if op[0] == 'member-rm' and ok and op[2] != EVERYONE:
            _, agent, who = op
            everyone = EVERYONE in a['members'].get(agent, {}) and who not in bot_ids(a)
            assert everyone or not allowed(agent, who), f'{who} still uses {agent} after the row was removed'
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
        if op[0] in ('person', 'bot') and ok and op[1] not in before['people']:
            # (forget) a new person or bot starts with nothing, whatever an earlier removal of that id left behind.
            x = op[1]
            assert all(x not in rows for rows in a['members'].values()), f'new {x} starts with a membership'
            assert not any('@' in k and k.rsplit('@', 1)[1] == x for k in a['grants']), f'new {x} starts with a grant'
            assert all(i.get('person') != x for i in a['invites'].values()), f'new {x} starts with an invite'
        if op[0] == 'member' and op[3].startswith('env.') and op[3][4:].split('=')[0] in ENV_BAD:
            assert not ok and a['members'] == before['members'], f'{op}: a forbidden env name was accepted'
        if op[0] == 'access':
            # (C2) the always half of an escape card writes only for a person (not the owner, not someone removed or
            # revoked) who holds the agent now through a row of their own; for a bot of an invitee, only what that
            # invitee holds. Exactly then, and it writes only that row.
            _, agent, who, (kind, value) = op
            rows = before['members'].get(agent, {})
            p = before['people'].get(who)
            parent = (p or {}).get('bot_of')
            want = (p is not None and not p.get('owner') and who in rows and ka.resolve(before, agent, who)['allowed'] is True
                    and (not parent or before['people'].get(parent, {}).get('owner')
                         or covered(ka.resolve(before, agent, parent).get('access', {}), KINDS[kind], value)))
            assert ok == want, f'{op}: accepted {ok}, expected {want}'
            if ok:
                assert value in a['members'][agent][who]['access'][KINDS[kind]], f'{op}: accepted but not written'
                others = {g: {x: r for x, r in rs.items() if not (g == agent and x == who)} for g, rs in a['members'].items()}
                olds = {g: {x: r for x, r in rs.items() if not (g == agent and x == who)} for g, rs in before['members'].items()}
                assert others == olds, f'{op}: changed another row'
            else:
                assert a == before, f'{op}: refused but wrote'
        if op[0] in ('job', 'helper'):
            # (C2) a tick, or a helper's always or once, writes only on a person's own row (never the owner's or the
            # everyone row) while they hold the agent now; for an invitee's bot, a helper only its owner may call. It
            # writes only that row, and a refusal writes nothing.
            agent, who = op[1], op[2]
            p = before['people'].get(who)
            parent = (p or {}).get('bot_of')
            pr = ka.resolve(before, agent, parent) if parent else {}
            want = (p is not None and not p.get('owner') and isinstance(before['members'].get(agent, {}).get(who), dict)
                    and ka.may(before, who, agent)[0] and ('work' in ka.jobs_of(agent) if op[0] == 'job' else agent != 'garden')
                    and (op[0] == 'job' or not parent or pr.get('owner') or 'garden' in pr.get('helpers', [])))
            assert ok == want, f'{op}: accepted {ok}, expected {want}'
            if ok:
                others = {g: {x: r for x, r in rs.items() if not (g == agent and x == who)} for g, rs in a['members'].items()}
                olds = {g: {x: r for x, r in rs.items() if not (g == agent and x == who)} for g, rs in before['members'].items()}
                assert others == olds, f'{op}: changed another row'
            else:
                assert a == before, f'{op}: refused but wrote'
        if op[0] == 'access-rm' and ok:
            _, agent, who, (kind, value) = op
            row = a['members'].get(agent, {}).get(who, {})
            assert value not in (row.get('access') or {}).get(KINDS[kind], []), f'{op}: still listed'
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
