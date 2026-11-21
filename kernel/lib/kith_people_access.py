"""`kith people grant|revoke|suspend|undo|requests|approve|deny`: the owner's access to people, at the terminal, on a Mac where
authority is the core's (auth.json is its tombstone, so `kith-auth member|person` is app-only there and exits 3).
kith-people hands these verbs here; the words, the text and the --json shapes are its own (see its help).

Where each verb goes, because the host does what the core cannot (stop runs, tell the cloud, add a device):
  grant, revoke      the core's people.grant / people.revoke over the owner socket (`kith kernel core-call`, marked
                     viaSwitch), then the host's enforcePersonAccess, which ends the turns, bot turns and room turns the
                     rows no longer allow. Authority must be switched and a core daemon must answer, else nothing runs.
  suspend, undo      the host's revokePerson / unrevokePerson: the cloud is told first (a refusal when it cannot be
                     reached) and then the person is cut off here. Never a raw core call, which skips both.
  requests, approve, deny   the host's accessRequests / decideAccessRequest, as `kith serve access` does.

KITH_BIN names the kith program (else the one kith_paths finds)."""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
import kith_paths  # noqa: E402
import kith_tombstone  # noqa: E402
from kith_host import host_call  # noqa: E402

VERBS = ('grant', 'revoke', 'suspend', 'undo', 'requests', 'approve', 'deny')
KITH = Path(os.path.dirname(os.path.dirname(os.path.realpath(__file__))))
CORE_ENV = ('KITH_STATE', 'HQ_STATE', 'KITH_HOME', 'HQ_HOME', 'KITH_AGENTS', 'HQ_AGENTS')
NO_CORE = 'the Kith core is not answering, so nothing was done (is `kith serve` running?)'
NOT_SWITCHED = ('authority is not the core\'s on this Mac, so grant and revoke are `kith-auth member` here '
                '(kith people suspend, undo and requests work either way)')
GRANT_FLAGS = {'--preset': 'preset', '--mode': 'mode', '--budget': 'budget', '--deny': 'deny', '--model': 'model',
               '--helpers': 'helpers'}


class Fail(Exception):
    def __init__(self, msg, code=1):
        super().__init__(msg)
        self.code = code


def switched():
    """Whether auth.json is the authority tombstone: the core holds the people on this Mac."""
    auth = Path(os.environ.get('KITH_AUTH') or kith_paths.paths()['KITH_SECRETS'] / 'auth.json')
    try:
        kith_tombstone.read_text(auth)
    except kith_tombstone.Tombstoned as e:
        return e.module == 'authority'
    except (OSError, ValueError):
        pass
    return False


def core(method, params, lost):
    """One owner call of the core; its answer as a dict. Exit 3 for no daemon (nothing ran)."""
    bin_ = os.environ.get('KITH_BIN') or kith_paths.kith_bin()
    if not bin_:
        raise Fail('there is no kith program to ask the core with', 3)
    body = {**params, 'viaSwitch': True, 'kernel': str(KITH), 'home': str(Path.home()),
            'env': {k: os.environ[k] for k in CORE_ENV if os.environ.get(k)}}
    try:
        r = subprocess.run([bin_, 'kernel', 'core-call', method, '--params-stdin'], input=json.dumps(body),
                           capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired:
        raise Fail(lost)
    except (OSError, subprocess.SubprocessError) as e:
        raise Fail(f'{NO_CORE} ({e})', 3)
    if r.returncode == 3:
        raise Fail(NO_CORE, 3)
    if r.returncode == 5:
        raise Fail(lost)
    if r.returncode == 2:
        raise Fail('this kith program does not know the core\'s people calls (kith update)')
    if r.returncode != 0:
        raise Fail(re.sub(r'^kith-core: ', '', (r.stderr.strip().splitlines() or ['refused'])[-1]))
    try:
        ans = json.loads(r.stdout)
    except ValueError:
        raise Fail('the Kith core answered something that is not JSON')
    if not isinstance(ans, dict):
        return {}
    if ans.get('noCore'):
        # An embedded core with no database: on a switched Mac that is a lost core database, never a fresh start.
        raise Fail('there is no core database on this Mac, so nothing was recorded')
    return ans


def host(method, body):
    ans, why = host_call(method, body)
    if why:
        raise Fail(why)
    return ans or {}


def invited(person):
    """Whether the core knows `person` as someone invited. The owner, a bot and `*` are refused: the host's enforce
    follows one invited person's turns, so for those it would leave running turns running."""
    rows = core('people.listPeople', {}, 'the Kith core went away before it answered').get('rows')
    if not isinstance(rows, list):
        raise Fail('the Kith core answered no list of people')
    row = next((r for r in rows if isinstance(r, dict) and r.get('id') == person), None)
    if row is None:
        raise Fail(f"no invited person '{person}' (kith people ls)")
    if row.get('owner') or row.get('bot_of'):
        raise Fail(f"'{person}' is the owner or a bot, not someone invited: a bot's access is its owner's (kith bot)")


def person_arg(s):
    if not re.fullmatch(r'[a-z][a-z0-9-]*|\*', s):
        raise Fail(f"'{s}' is not a person id (kith people ls)", 2)
    return s


def agent_arg(s):
    if not all(re.fullmatch(r'[a-z0-9][a-z0-9_-]*', a) for a in s.split(',')):
        raise Fail(f"'{s}': an agent name, or names separated by commas", 2)
    return s


def run(verb, pos, one, js, out):
    """Does one verb; `pos` are its words, `one` its --flag values (grant's, plus --trusted as True). `out` prints."""
    if verb in ('grant', 'revoke'):
        if len(pos) != 2:
            raise Fail(f'usage: kith people {verb} <person> <agent[,agent]>'
                       + (' [--preset P] [--mode M] [--budget N] [--deny T] [--model X] [--helpers H] [--trusted]'
                          if verb == 'grant' else '') + ' [--json]', 2)
        person, agents = person_arg(pos[0]), agent_arg(pos[1])
        if person == '*':
            raise Fail('`*` (everyone) is not done from here: name one invited person (kith people ls)', 2)
        if not switched():
            raise Fail(NOT_SWITCHED)
        invited(person)
        params = {'person': person, 'agent': agents}
        if verb == 'grant':
            params.update({k: v for k, v in one.items() if k != 'trusted'})
            if one.get('trusted'):
                params['trusted'] = True
        done = core(f'people.{verb}', params,
                    f'the Kith core went away before it answered; `kith people ls` shows whether it was done')
        try:
            host('enforcePersonAccess', {'personId': person, 'lost': verb == 'revoke'})
        except Fail as e:
            raise Fail(f'the rows were changed, but what is running was not stopped ({e}); start kith serve and '
                       f'run the same line again')
        res = {'ok': True, 'person': person, 'agents': agents.split(','), 'enforced': True,
               'lines': done.get('lines') or []}
        out(js, res, f"{'gave' if verb == 'grant' else 'took away'} {person} {agents}")
    elif verb in ('suspend', 'undo'):
        if len(pos) != 1:
            raise Fail(f'usage: kith people {verb} <person> [--json]', 2)
        person = person_arg(pos[0])
        host('revokePerson' if verb == 'suspend' else 'unrevokePerson', {'personId': person})
        out(js, {'ok': True, 'person': person, 'suspended': verb == 'suspend'},
            f'{person} cut off (kith people undo {person} gives it back)' if verb == 'suspend'
            else f'{person} has their access back')
    elif verb == 'requests':
        if pos:
            raise Fail('usage: kith people requests [--json]', 2)
        reqs = host('accessRequests', {}).get('requests') or []
        out(js, {'ok': True, 'requests': reqs},
            '\n'.join(f"{r.get('requestId')}  {r.get('deviceName', '')} ({r.get('platform', '')}, "
                      f"{r.get('email') or 'no email'})  code {r.get('code') or '(waiting for the device)'}"
                      for r in reqs) or 'no devices are asking for access')
    else:   # approve, deny
        if len(pos) != 1:
            raise Fail(f'usage: kith people {verb} <requestId> [--json]', 2)
        # The id, or the code the device shows (as `kith serve access`): it must pick one request.
        reqs = host('accessRequests', {}).get('requests') or []
        hit = [r for r in reqs if pos[0] in (r.get('requestId'), r.get('code'))]
        if len(hit) != 1:
            raise Fail(f'no single request matches {pos[0]} (kith people requests)')
        host('decideAccessRequest', {'requestId': hit[0]['requestId'], 'approve': verb == 'approve'})
        out(js, {'ok': True, 'requestId': hit[0]['requestId'], 'approved': verb == 'approve'},
            f'{hit[0]["requestId"]} {"approved" if verb == "approve" else "denied"}')
