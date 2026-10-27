#!/usr/bin/env python3
"""One stand-in for kith, kith-auth and kith-live in the channels' tests: it acts as whichever name it is called by.

The test makes symlinks <rig>/bin/kith, kith-auth and kith-live to this file; the rig's files are in <rig>/stub/:
  people.json   {"<platform>:<uid>": {person, owner, was}}      what `kith-auth who` says ("*": for anyone else)
  access.json   {"<person>": {"owner": bool, "agents": [..]}}   what `kith-auth resolve` says
  codes.json    {"<CODE>": "<person>"}                          what `kith-auth link` accepts
  granted.json  {"<platform>": ["<profile>", ..]}               what `kith-auth granted` lists
  secrets.json  {"<profile>": {"<VAR>": "<value>"}}             what `kith-auth get` prints
  live.json     {"running": bool}                               whether `kith-live send` finds a session
  chain.json    the backup chain `kith <agent> --backups-json` prints (default: the primary alone)
  depleted.json {"<primary|entry id>": <epoch the limit lifts or null>}  accounts a run on which ends on a usage limit
  unavailable.json ["<entry id>", ..]                                    entries `kith --backup` refuses with exit 4
  calls.jsonl   every call, one JSON object per line

`kith <agent> <task>` answers {"result": "echo:<task>", "session_id": "sess-<agent>-<n>"}. A task that starts with
SLOW sleeps until killed, SLEEP for 1.5 s; one containing SEND leaves two posts in KITH_BRIDGE_OUTBOX; FAIL exits 1;
BIGCTX reports a context 60% full, BGHOLD leaves a process holding its output open. `kith <agent> --backup ID <task>`
runs on that chain entry; `kith <agent> --backups-json` prints the chain.
"""
import json
import os
import sys
import time

me = os.path.basename(sys.argv[0])
root = os.path.dirname(os.path.dirname(os.path.abspath(sys.argv[0])))
stub = os.path.join(root, "stub")
args = sys.argv[1:]


def load(name, default):
    try:
        with open(os.path.join(stub, name + ".json"), encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def save(name, value):
    with open(os.path.join(stub, name + ".json"), "w", encoding="utf-8") as f:
        json.dump(value, f)


def log(record):
    with open(os.path.join(stub, "calls.jsonl"), "a", encoding="utf-8") as f:
        f.write(json.dumps(record) + "\n")


def fail(text):
    print(text, file=sys.stderr)
    sys.exit(1)


if me == "kith-auth":
    cmd = args[0] if args else ""
    if cmd == "who":
        people = load("people", {})
        nobody = people.get("*", {"person": None, "owner": False, "was": []})
        print(json.dumps(people.get(f"{args[1]}:{args[2]}", nobody)))
    elif cmd == "resolve":
        agent, person = args[1], args[args.index("--person") + 1]
        entry = load("access", {}).get(person)
        if entry is None:
            print(json.dumps({"allowed": False, "why": f"{person} is not in kith", "owner": False}))
        else:
            owner = bool(entry.get("owner"))
            allowed = owner or agent in entry.get("agents", [])
            print(json.dumps({"allowed": allowed, "why": f"{person} may not use {agent}", "owner": owner}))
    elif cmd == "link":
        platform, uid, code = args[1:4]
        person = load("codes", {}).get(code)
        if person is None:
            fail("kith-auth: that code did not work")
        people = load("people", {})
        people[f"{platform}:{uid}"] = {"person": person, "owner": False, "was": []}
        save("people", people)
        print(person)
    elif cmd == "granted":
        granted = load("granted", {})
        if granted.get("_fail"):
            fail("kith-auth: the auth file is locked")
        print("\n".join(granted.get(args[-1], [])))
    elif cmd == "get":
        profile, var = args[args.index("--profile") + 1], args[-1]
        value = load("secrets", {}).get(profile, {}).get(var)
        if value is None:
            fail(f"kith-auth: {var} is not set")
        print(value)
    else:
        fail("kith-auth: unknown command")
elif me == "kith-live":
    log({"tool": "kith-live", "args": args})
    if not load("live", {"running": True}).get("running"):
        fail("no live session for that agent")
elif me == "kith":
    agent = args[0]
    if args[1] == "--backups-json":
        log({"tool": "kith-chain", "agent": agent})
        default = {"chain": [{"id": "claude", "harness": "claude", "account": None, "kind": "primary", "ready": True}]}
        print(json.dumps({"version": 1, "agent": agent, **load("chain", default)}))
        sys.exit(0)
    backup = args[2] if args[1] == "--backup" else None
    task = args[-1]
    env = os.environ
    if backup and backup in load("unavailable", []):
        print(f"kith: {backup}: not logged in", file=sys.stderr)
        sys.exit(4)
    log({
        "tool": "kith", "agent": agent, "task": task,
        "resume": env.get("KITH_RESUME"), "person": env.get("KITH_PERSON"), "via": env.get("KITH_VIA"),
        "output": env.get("KITH_OUTPUT"), "extra": env.get("FAKE_EXTRA"), "mode": env.get("KITH_PERMISSION_MODE"),
        "outbox": bool(env.get("KITH_BRIDGE_OUTBOX")), "backup": backup,
        "withheld": sorted(k for k in env if k.startswith(("JEV_", "TELEGRAM_", "SLACK_", "DISCORD_", "HQ_DISCORD_", "KITH_DISCORD_"))
                           or k in ("HQ_AGENT", "KITH_AGENT")),
        "cwd": os.getcwd(),
    })
    counts = load("sessions", {})
    counts[agent] = counts.get(agent, 0) + 1
    save("sessions", counts)
    if "BGHOLD" in task:
        # A background process that keeps stdout open after the agent is done.
        import subprocess
        bg = subprocess.Popen(["sleep", "60"], stdin=subprocess.DEVNULL)
        with open(os.path.join(stub, "bg.pid"), "w", encoding="utf-8") as f:
            f.write(str(bg.pid))
    if "SEND" in task:
        outbox = env["KITH_BRIDGE_OUTBOX"]
        for item in ({"as": "image", "text": "tool says hi"},
                     {"as": "image", "files": ["/tmp/pic.png"], "caption": "a picture"}):
            name = os.path.join(outbox, f"{time.time_ns()}.json")
            with open(name + ".tmp", "w", encoding="utf-8") as f:
                json.dump(item, f)
            os.rename(name + ".tmp", name)
            time.sleep(0.01)
    depleted = load("depleted", {})
    account = backup or "primary"
    if account in depleted:
        # What a spent account prints: Claude Code's error result with the API status, or kith's usage_limit for Codex.
        res = {"type": "result", "subtype": "success", "is_error": True, "num_turns": 1,
               "result": "You've hit your limit"}
        if backup and backup.startswith("codex"):
            res = {"type": "result", "subtype": "error", "is_error": True, "num_turns": 0, "usage_limit": {"resets_at": depleted[account]},
                   "result": "usage limit reached"}
        else:
            res["api_error_status"] = 429
            if depleted[account]:
                res["result"] = f"Claude AI usage limit reached|{depleted[account]}"
        print(json.dumps(res))
        sys.exit(1)
    if task.startswith("SLOW"):
        time.sleep(60)
    if task.startswith("SLEEP"):
        time.sleep(1.5)
    if "FAIL" in task:
        fail("boom")
    out = {"result": "echo:" + task, "session_id": f"sess-{backup or agent}-{counts[agent]}", "is_error": False}
    if "BIGCTX" in task:
        out["usage"] = {"iterations": [{"input_tokens": 600}]}
        out["modelUsage"] = {"m": {"contextWindow": 1000, "inputTokens": 600}}
    print(json.dumps(out))
