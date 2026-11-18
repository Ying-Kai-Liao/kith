#!/usr/bin/env python3
"""`kith schedule ls|add|rm|pause|resume|run|log` once the schedules are the Kith core's. kith-sched hands these verbs here
when <machine>/core/cutover/schedules.switch says so (`takes`), whoever called it (bin/kith, a skill, kith-chain): its own
state.json is a tombstone by then, and its jobs.json editor must not write beside the core.

The words, the text and the --json shapes are kith-sched's (see its help), so callers, skills and the app do not notice which
one answered. The work is the core's, over the owner socket: `kith kernel core-call METHOD --params-stdin` with
schedule.jobs, .runs, .put, .drop, .setPaused and .runNow. Without a daemon nothing runs and the line says so: there is no
fallback to the files, which the core owns.

Differences, because the core fires and the host runs: `run` queues the job's turn (the host starts it), so its answer has
`pid` and `log` null and a `turn`; `ls` leaves out the jobs that carry only a `trigger` (as kith-sched does) and always says
`ticking`.

KITH_BIN names the kith program (else the one kith_paths finds). KITH_PERSON is the person whose run adds a job (none is
the owner). Like kith-sched, a change to a job is told to the app through the run's outbox (the Schedule card)."""
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
import kith_paths  # noqa: E402

KITH = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
EXIT_BAD = 1
VERBS = ('ls', 'add', 'rm', 'pause', 'resume', 'run', 'log')
tell_app = lambda agent, jid, op: None   # set by main(): kith-sched's, which leaves the card in the run's outbox
# core-call: 0 answered, 3 nothing ran (no daemon), 4 the core refused, 5 lost after the call went out.
NO_CORE = 'the Kith core is not answering, so nothing was done (is `kith serve` running?)'


class Fail(Exception):
    pass


def takes(argv, machine):
    """Whether `kith-sched ARGV` is the core's: one of its verbs, with the schedules switched on this Mac (the switch file
    exists and does not say `stepping`, as kith_cutover_refuse reads it)."""
    words = [a for a in argv if a != '--json']
    if not words or words[0] not in VERBS:
        return False
    try:
        with open(os.path.join(str(machine), 'core', 'cutover', 'schedules.switch')) as f:
            return 'state=stepping' not in f.read().split()
    except OSError:
        return False


def core(method, params, lost='the Kith core went away before it answered; `kith schedule ls` shows whether it was done'):
    """One owner call; the answer as a dict. Fail with the core's own words when it refuses."""
    bin_ = os.environ.get('KITH_BIN') or kith_paths.kith_bin()
    if not bin_:
        raise Fail('the schedules are the Kith core\'s on this Mac, and there is no kith program to ask it with')
    try:
        r = subprocess.run([bin_, 'kernel', 'core-call', method, '--params-stdin'], input=json.dumps(params),
                           capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError) as e:
        raise Fail('%s (%s)' % (NO_CORE, e))
    if r.returncode == 0:
        try:
            return json.loads(r.stdout)
        except ValueError:
            raise Fail('the Kith core answered something that is not JSON')
    if r.returncode == 3:
        raise Fail(NO_CORE)
    if r.returncode == 5:
        raise Fail(lost)
    if r.returncode == 2:
        raise Fail('this kith program does not know the core\'s schedule calls (kith update)')
    msg = re.sub(r'^kith-core: (schedule: )?', '', (r.stderr.strip().splitlines() or ['refused'])[-1])
    raise Fail(msg)


def minute(text):
    """'2026-11-04T08:00' as the text mode prints it ('Wed 04 Nov 08:00')."""
    try:
        return dt.datetime.strptime(text, '%Y-%m-%dT%H:%M').strftime('%a %d %b %H:%M')
    except (TypeError, ValueError):
        return str(text)


def log_of(agent, jid):
    """The newest log file of the job's runs (run-turn names them <job>-<stamp>.log), which the core does not record."""
    runs = os.path.join(str(kith_paths.paths()['KITH_LOGS']), 'sched', 'runs', agent)
    try:
        names = sorted(n for n in os.listdir(runs) if n.startswith(jid + '-') and n.endswith('.log'))
    except OSError:
        return None
    return os.path.join(runs, names[-1]) if names else None


def runs_of(agent=None, jid=None):
    """The newest run of each job, from the core: {(agent, id): run} with `log` filled in."""
    params = {}
    if agent:
        params['agent'] = agent
    if jid:
        params['id'] = jid
    out = {}
    for v in core('schedule.runs', params).get('runs', []):
        run = {'agent': v['agent'], 'id': v['id'], 'at': v.get('at'), 'why': v.get('why'), 'pid': v.get('pid'),
               'log': v.get('log') or log_of(v['agent'], v['id']), 'running': bool(v.get('running'))}
        if v.get('state') == 'failed':
            run['error'] = v.get('outcome') or 'failed'
        out[(v['agent'], v['id'])] = run
    return out


def job_row(r, last=None):
    """The core's row in kith-sched's shape and key order; `last` is the job's newest run (runs_of)."""
    row = {k: r.get(k) for k in ('agent', 'id', 'cron', 'task')}
    row['paused'] = bool(r.get('paused'))
    for k in ('person', 'model', 'effort', 'harness'):
        if r.get(k):
            row[k] = r[k]
    if r.get('error'):
        row['error'] = r['error']
    row['next'] = r.get('next')
    row['last'] = {k: last[k] for k in ('at', 'why', 'pid', 'log', 'running', 'error') if k in last} if last else None
    return row


def listing(agent=None):
    """The jobs kith-sched lists: the cron ones (a routine that has only a trigger is the host's to show)."""
    ans = core('schedule.jobs', {'agent': agent} if agent else {})
    return [r for r in ans.get('jobs', []) if r.get('cron') is not None]


def cmd_ls(rest, as_json):
    runs = runs_of(rest[0] if rest else None)
    rows = [job_row(r, runs.get((r['agent'], r['id']))) for r in listing(rest[0] if rest else None)]
    if as_json:
        return {'jobs': rows, 'ticking': True}
    if not rows:
        print('no jobs%s. Add one: kith schedule add <agent> "0 8 * * *" "the task"' % (' for ' + rest[0] if rest else ''))
    for r in rows:
        if r['paused']:
            n = 'paused'
        elif r.get('error'):
            n = r['error']
        else:
            n = minute(r['next']) if r['next'] else 'never'
        print('%-10s %-18s %-16s next %-18s last %s' % (r['agent'], r['id'], r['cron'], n, (r['last'] or {}).get('at') or '-'))
        print('%-10s %s%s' % ('', (r['task'] or '')[:100].replace('\n', ' '), '  (for %s)' % r['person'] if r.get('person') else ''))
    return None


def cmd_log(rest, as_json):
    agent, jid = (rest[0].split('/', 1) + [None])[:2] if rest and '/' in rest[0] else ((rest + [None, None])[:2])
    n = None
    if rest and '/' in rest[0]:
        n = rest[1] if len(rest) > 1 else None
    elif len(rest) > 2:
        n = rest[2]
    if n is not None and (not n.isdigit() or int(n) < 1):
        raise Fail('n must be a positive number')
    runs = list(runs_of(agent, jid).values())
    if n:
        runs = runs[-int(n):]
    if as_json:
        return {'agent': agent, 'id': jid, 'runs': runs}
    home = os.path.expanduser('~')
    for r in runs:
        state = 'failed' if r.get('error') else 'running' if r['running'] else 'done'
        print('%s  %-24s %-7s %s' % (r['at'], '%s/%s' % (r['agent'], r['id']), state, (r['log'] or '-').replace(home, '~', 1)))
    if agent and jid and runs and runs[-1]['log']:
        try:
            with open(runs[-1]['log']) as f:
                print('\n' + f.read()[-3000:])
        except OSError:
            pass
    return None


def cmd_add(rest, as_json):
    opts, pos, i = {}, [], 0
    names = {'--id': 'id', '-m': 'model', '--model': 'model', '-e': 'effort', '--effort': 'effort', '-H': 'harness',
             '--harness': 'harness'}
    while i < len(rest):
        if rest[i] in names:
            if i + 1 >= len(rest):
                raise Fail('%s needs a value' % rest[i])
            opts[names[rest[i]]] = rest[i + 1]
            i += 2
        else:
            pos.append(rest[i])
            i += 1
    if len(pos) != 3:
        raise Fail('usage: kith-sched add AGENT "CRON" "task" [--id ID] [-m MODEL] [-e EFFORT] [-H HARNESS]')
    agent, cron, task = pos
    params = dict(opts, agent=agent, cron=cron, task=task)
    if os.environ.get('KITH_PERSON'):
        params['person'] = os.environ['KITH_PERSON']
    ans = core('schedule.put', params)
    tell_app(agent, ans['job']['id'], 'updated' if ans.get('replaced') else 'added')
    row = job_row(ans['job'], runs_of(agent, ans['job']['id']).get((agent, ans['job']['id'])))
    if as_json:
        return {'ok': True, 'replaced': bool(ans.get('replaced')), 'job': row}
    print('%s/%s: next run %s' % (agent, row['id'], minute(row['next']) if row['next'] else 'never'))
    return None


def resolve(agent, person):
    """kith-auth's answer for a run of agent for person, as kith-sched asks it."""
    return json.loads(subprocess.run([os.path.join(KITH, 'bin/kith-auth'), 'resolve', agent, '--person', person],
                                     capture_output=True, text=True, timeout=30, check=True).stdout)


def cmd_run(agent, jid, as_json):
    jobs = [r for r in listing(agent) if r['agent'] == agent]
    job = next((r for r in jobs if r['id'] == jid), None)
    if job is None:
        raise Fail('%s has no job %r (have: %s)' % (agent, jid, ' '.join(r['id'] for r in jobs) or 'none'))
    caller = os.environ.get('KITH_PERSON')
    if job.get('person') and caller:   # by hand, never as someone else (the owner may)
        try:
            me, them = resolve(agent, caller), resolve(agent, job['person'])
        except (OSError, ValueError, subprocess.SubprocessError) as e:
            raise Fail("can't check %s with kith-auth (%s)" % (caller, e))
        if not me.get('owner') and me.get('person') != them.get('person'):
            raise Fail('%s/%s runs for %s; only they or the owner can start it by hand' % (agent, jid, job['person']))
    key = 'cli-%d-%d' % (int(time.time() * 1000), os.getpid())
    # A lost answer may have been a run: the same key is the same run if the call is made again.
    ans = core('schedule.runNow', {'agent': agent, 'id': jid, 'key': key},
               lost='the Kith core went away before it answered, so the run may have been queued (run key %s)' % key)
    outcome = ans.get('outcome')
    if outcome != 'fired':
        raise Fail('%s/%s is %s' % (agent, jid, {'busy': 'still running', 'paused': 'paused'}.get(outcome, 'not started (%s)' % outcome)))
    if as_json:
        return {'ok': True, 'agent': agent, 'id': jid, 'pid': None, 'log': None, 'turn': ans.get('turn'), 'queued': True}
    print('queued turn %s (the host starts it)' % ans.get('turn'))
    return None


def cmd_edit(verb, agent, jid, as_json):
    if verb == 'rm':
        ans = core('schedule.drop', {'agent': agent, 'id': jid})
    else:
        ans = core('schedule.setPaused', {'agent': agent, 'id': jid, 'paused': verb == 'pause'})
    tell_app(agent, jid, {'rm': 'removed', 'pause': 'paused', 'resume': 'resumed'}[verb])
    if as_json:
        return {'ok': True, 'agent': agent, 'id': jid, 'changed': bool(ans.get('changed'))}
    print('%s/%s %s' % (agent, jid, {'rm': 'removed', 'pause': 'paused', 'resume': 'resumed'}[verb]))
    return None


def main(argv, tell=None):
    global tell_app
    if tell:
        tell_app = tell
    as_json = '--json' in argv
    argv = [a for a in argv if a != '--json']
    verb, rest = (argv[0], argv[1:]) if argv else ('ls', [])
    try:
        if verb == 'ls':
            out = cmd_ls(rest, as_json)
        elif verb == 'log':
            out = cmd_log(rest, as_json)
        elif verb == 'add':
            out = cmd_add(rest, as_json)
        elif verb in ('rm', 'pause', 'resume', 'run') and len(rest) == 2:
            out = cmd_run(rest[0], rest[1], as_json) if verb == 'run' else cmd_edit(verb, rest[0], rest[1], as_json)
        else:
            raise Fail('--json: usage: kith-sched ls|add|rm|pause|resume|run|log ... --json' if as_json
                       else 'unknown command; see kith-sched --help')
    except Fail as e:
        if as_json:
            print(json.dumps({'ok': False, 'error': str(e)}, ensure_ascii=False))
        else:
            print('kith-sched: %s' % e, file=sys.stderr)
        return EXIT_BAD
    if as_json:
        print(json.dumps(out, ensure_ascii=False))
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
