#!/usr/bin/env python3
"""A stand-in `kith` for tests that run several kith agents at once, each on its own scripted ACP agent (stub_hq.py is the
one-agent `house` kernel with the rich settings). One-bot-model C5: every bot of a test is a kith agent now.

An agent's folder is `$STUB_AGENTS_HOME/<agent>` (or, without that variable, `<tmp>/<agent with "." as "/">`, which is how a
unit test names a folder it made before the kernel ever ran). `ls --json` lists the folders of `$STUB_AGENTS_HOME`;
`<agent> --launch-json` describes how to start the agent in its folder (the kernel contract's `Launch`); `project ls --json`
lists none, `cost limit <agent> --json` no limit and `memory search` no hits. Anything else is refused (exit 2).
The ACP agent is `acp_dispatch.py`'s business.
"""
import json
import os
import sys
import tempfile

args = sys.argv[1:]


def home_of(agent):
    root = os.environ.get("STUB_AGENTS_HOME")
    if root:
        return os.path.join(root, agent)
    return os.path.join(tempfile.gettempdir(), agent.replace(".", "/"))


def launch(agent):
    home = home_of(agent)
    model = args[args.index("-m") + 1] if "-m" in args else "haiku"
    project = args[args.index("-p") + 1] if "-p" in args else None
    via = os.environ.get("KITH_VIA", "")
    # A test that wants a distinctive account writes it in the agent's folder (`<agent folder>/account`).
    try:
        with open(os.path.join(home, "account"), encoding="utf-8") as f:
            account = f.read().strip() or "claude"
    except OSError:
        account = "claude"
    env = {"KITH_AGENT": agent, "HQ_AGENT": agent, "KITH_VIA": via, "HQ_VIA": via, "KITH_MODEL": model,
           "HQ_MODEL": model, "KITH_PROVIDER": "claude", "KITH_ACCOUNT": "", "KITH_ACCOUNT_ID": account,
           "KITH_BACKUP": ""}
    print(json.dumps({
        "version": 1, "agent": agent, "agent_version": "v1", "harness": "claude", "cwd": home,
        "backup": None, "provider": "claude", "account": None, "account_id": account, "env": env,
        "system_prompt": "You are " + agent + ".", "settings": {"hooks": {"SessionStart": []}},
        "mcp_servers": {}, "add_dirs": [], "allowed_tools": [], "disable_slash_commands": True,
        "model": model, "effort": None, "project": project, "call_model": None, "permission_mode": "default",
        "cost_tag": {"agent": agent, "version": "v1", "model": model, "effort": None, "via": via or "cli",
                     "project": project, "person": os.environ.get("KITH_PERSON") or None},
        "claude_args": [],
    }))


if args == ["ls", "--json"]:
    root = os.environ.get("STUB_AGENTS_HOME", "")
    names = sorted(n for n in os.listdir(root) if os.path.isdir(os.path.join(root, n))) if root and os.path.isdir(root) else []
    print(json.dumps([{"name": n, "description": "", "home": home_of(n), "model": None, "effort": None,
                       "harness": "claude"} for n in names]))
elif args[:2] == ["project", "ls"]:
    print('{"projects": []}')
elif args[:2] == ["cost", "limit"] and len(args) >= 3:
    print(json.dumps({"agent": args[2], "limit": None, "at": "ask", "over": False}))
elif args[:2] == ["memory", "search"]:
    print('{"hits": []}')
elif args == ["cost", "record"]:
    sys.stdin.read()
elif len(args) >= 2 and args[1] == "--launch-json":
    launch(args[0])
else:
    print("unknown call", file=sys.stderr)
    sys.exit(2)
