#!/usr/bin/env python3
"""A stand-in for the `kith` CLI: `ls --json` lists one agent, `<agent> --launch-json` describes how to start it,
`project ls --json` lists the projects in `projects.json` (`{"projects": [...]}`) and `project add <id> <path>` adds one.
What it prints for those three is the kernel contract's (serve/contract/kernel/contract.json), as the real kith's is.

STUB_HQ_HOME is the agent's folder. Every call is appended to calls.jsonl there (arguments, KITH_VIA, working
directory). `house set permission MODE` writes the file `permission` there, which --info-json and --launch-json report
(an empty MODE removes it), as kith keeps its per-agent choice. `cost limit house --json` prints `limit.json` there (no
limit without it); `house set limit N` writes `limit_set` and removes `limit.json`. A file named `fail` in that folder
makes --launch-json fail with its text on stderr. For an invited person's launch (KITH_PERSON set), `grants.json` there
(`{"read": [...]}`) stands in for what the real kernel's `kith-permit apply` puts in the settings: the folders their
conversation was given, as `sandbox.filesystem.allowRead`.
"""
import json
import os
import sys

home = os.environ["STUB_HQ_HOME"]
args = sys.argv[1:]
with open(os.path.join(home, "calls.jsonl"), "a", encoding="utf-8") as log:
    log.write(json.dumps({"args": args, "via": os.environ.get("KITH_VIA") or os.environ.get("HQ_VIA"), "cwd": os.getcwd()}) + "\n")

def call_model():
    """`set call-model` keeps the choice in `call_model`; like kith, default (or none) is sonnet and `own` is the agent's own."""
    path = os.path.join(home, "call_model")
    chosen = open(path, encoding="utf-8").read().strip() if os.path.exists(path) else ""
    return chosen or "sonnet", ("" if chosen == "own" else chosen or "sonnet")


def main_account():
    """The agent's main account (`kith account use`), kept in the file `main` there; without it, the default login."""
    path = os.path.join(home, "main")
    return (open(path, encoding="utf-8").read().strip() if os.path.exists(path) else "") or "claude"


def main_json():
    path = os.path.join(home, "main")
    chosen = open(path, encoding="utf-8").read().strip() if os.path.exists(path) else ""
    return {"id": main_account(), "source": "agent" if chosen else "default", "harness": "claude", "ready": True}


def permission(default):
    path = os.path.join(home, "permission")
    return open(path, encoding="utf-8").read().strip() if os.path.exists(path) else default


if args == ["ls", "--json"]:
    print(json.dumps([{"name": "house", "description": "Keeps the house", "home": home, "model": None,
                       "effort": None, "harness": "claude"}]))
elif args[:2] == ["project", "ls"]:
    path = os.path.join(home, "projects.json")
    print(open(path, encoding="utf-8").read() if os.path.exists(path) else '{"projects": []}')
elif args[:2] == ["project", "add"]:
    path = os.path.join(home, "projects.json")
    projects = json.load(open(path, encoding="utf-8"))["projects"] if os.path.exists(path) else []
    if any(p["id"] == args[2] for p in projects):
        print(f"project {args[2]} already exists", file=sys.stderr)
        sys.exit(1)
    projects.append({"id": args[2], "paths": [args[3]], "home": args[3], "remotes": [], "parent": None,
                     "memory": os.path.join(home, "state", "projects", args[2]), "exists": True})
    json.dump({"projects": projects}, open(path, "w", encoding="utf-8"))
elif args[:3] == ["house", "set", "permission"] and len(args) == 4:
    path = os.path.join(home, "permission")
    if args[3] == "nonsense":
        print("kith: permission_mode 'nonsense' is not one of default, manual", file=sys.stderr)
        sys.exit(1)
    if args[3]:
        open(path, "w", encoding="utf-8").write(args[3])
    elif os.path.exists(path):
        os.remove(path)
elif args[:3] == ["house", "set", "call-model"] and len(args) == 4:
    path = os.path.join(home, "call_model")
    if args[3] == "default":
        if os.path.exists(path):
            os.remove(path)
    else:
        open(path, "w", encoding="utf-8").write(args[3])
elif args[:2] == ["account", "use"]:
    # `account use ID --agent house` / `account use --reset --agent house`: the main account, in `main`.
    path = os.path.join(home, "main")
    if "--reset" in args:
        if os.path.exists(path):
            os.remove(path)
    elif args[2].split(":")[0] not in ("claude", "claude-api"):
        print(f"{args[2]} can't be the main account of a claude agent", file=sys.stderr)
        sys.exit(2)
    else:
        open(path, "w", encoding="utf-8").write(args[2])
elif args[:2] in (["backup", "list"], ["account", "list"]) or args[:3] in (["account", "backup", "set"], ["account", "backup", "reset"]):
    # The chain settings (`chain_settings.json`: {"backups": [...], "agents": {name: [...]}}) and the extra accounts
    # (`accounts.json`: [{"id", "logged_in", "why"?}]). An id listed in `not_ready.json` ({id: why}) is not ready.
    path = os.path.join(home, "chain_settings.json")
    st = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {"backups": [], "agents": {}}
    opt = args[args.index("--agent") + 1] if "--agent" in args else None
    if args[:2] == ["account", "list"]:
        acc = os.path.join(home, "accounts.json")
        print(open(acc, encoding="utf-8").read() if os.path.exists(acc) else "[]")
    elif args[0] == "backup":
        bad = os.path.join(home, "not_ready.json")
        bad = json.load(open(bad, encoding="utf-8")) if os.path.exists(bad) else {}
        ids = st["agents"].get(opt, st["backups"]) if opt else st["backups"]
        rows = []
        for i in ids:
            row = {"id": i, "ready": i not in bad}
            if i in bad:
                row["why"] = bad[i]
                row["fix"] = "kith account add " + i.replace(":", " ")
            rows.append(row)
        main = main_json() if opt else {"id": None, "source": "global", "harness": None}
        print(json.dumps({"agent": opt, "source": "agent" if opt and opt in st["agents"] else "global", "backups": rows,
                          "main": main}))
    else:
        if args[2] == "reset":
            st["agents"].pop(opt, None)
        else:
            ids = [a for a in args[3:] if a != "--agent" and a != opt]
            if opt:
                st["agents"][opt] = ids
            else:
                st["backups"] = ids
        json.dump(st, open(path, "w", encoding="utf-8"))
elif args == ["house", "--info-json"]:
    fail = os.path.join(home, "info_fail")
    if os.path.exists(fail):
        print(open(fail, encoding="utf-8").read(), file=sys.stderr)
        sys.exit(1)
    memory = os.path.join(home, "memory")
    names = sorted(n for n in os.listdir(memory) if os.path.isfile(os.path.join(memory, n))) if os.path.isdir(memory) else []
    files = [{"path": n, "bytes": os.path.getsize(os.path.join(memory, n)), "modified": "2026-10-22T00:00:00Z"} for n in names]
    print(json.dumps({
        "version": 1, "agent": "house", "description": "Keeps the house", "harness": "claude", "model": None,
        "effort": None, "permission_mode": permission("auto"), "home": home,
        "call_model": call_model()[0], "call_model_effective": call_model()[1],
        "role": {"path": home + "/role", "intro": "Keeps the house.", "sections": [{"title": "Not yours", "text": "Money."}]},
        "package": {"version": None, "remote": None, "dirty": False, "unpushed": 0, "pinned": None},
        "skills": [], "tools": [], "mcp_servers": [], "plugins": [], "projects": [], "schedules": [], "cost": None,
        "memory": {"dir": memory, "files": files, "total_bytes": sum(f["bytes"] for f in files)}, "errors": {},
        "future_key": True,
    }))
elif args[:2] == ["memory", "search"]:
    if os.path.exists(os.path.join(home, "search_fail")):
        print("search broke", file=sys.stderr)
        sys.exit(1)
    n = int(args[args.index("-n") + 1])
    hit = {"path": "notes.md", "line": 1, "agent": args[args.index("--agent") + 1], "snippet": f"{args[2]} agent={args[args.index('--agent') + 1]}"}
    print(json.dumps({"hits": ([hit] * 3)[:n]}))
elif args == ["cost", "record"]:
    # A tidy-up's cost row: the JSON on stdin and who it is for.
    with open(os.path.join(home, "cost.jsonl"), "a", encoding="utf-8") as cost:
        cost.write(json.dumps({"row": json.load(sys.stdin), "agent": os.environ.get("KITH_AGENT"),
                               "via": os.environ.get("KITH_VIA")}) + "\n")
elif args == ["cost", "limit", "house", "--json"]:
    # `limit.json` there is where house stands against its monthly limit; without it, no limit.
    path = os.path.join(home, "limit.json")
    print(open(path, encoding="utf-8").read() if os.path.exists(path)
          else json.dumps({"agent": "house", "limit": None, "at": "ask", "over": False}))
elif args[:3] == ["house", "set", "limit"] and len(args) == 4:
    # Raising it puts house under the limit again.
    open(os.path.join(home, "limit_set"), "w", encoding="utf-8").write(args[3])
    if os.path.exists(os.path.join(home, "limit.json")):
        os.remove(os.path.join(home, "limit.json"))
elif args == ["house", "--backups-json"]:
    # `chain.json` is the chain after the agent's own login; without one the agent has no backups.
    path = os.path.join(home, "chain.json")
    chain = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else []
    main = main_account()
    primary = {"id": main, "harness": "claude", "account": main.split(":")[1] if ":" in main else None,
               "kind": "primary", "ready": True}
    chain = [e for e in chain if e["id"] != main]
    print(json.dumps({"version": 1, "agent": "house", "chain": [primary] + chain}))
elif args[:2] == ["house", "--launch-json"]:
    fail = os.path.join(home, "fail")
    if os.path.exists(fail):
        print(open(fail, encoding="utf-8").read(), file=sys.stderr)
        sys.exit(1)
    backup = args[args.index("--backup") + 1] if "--backup" in args else None
    # A file named `launch_fail_<entry>` (":" as "_") makes hq refuse that entry, as it does one it can't run.
    if backup and os.path.exists(os.path.join(home, "launch_fail_" + backup.replace(":", "_"))):
        print(f"{backup} is not logged in", file=sys.stderr)
        sys.exit(4)
    harness = "codex" if backup and backup.split(":")[0] == "codex" else "claude"
    account_id = backup or main_account()
    account = account_id.split(":")[1] if ":" in account_id else None
    model = args[args.index("-m") + 1] if "-m" in args else "haiku"
    project = args[args.index("-p") + 1] if "-p" in args else None
    settings = {"hooks": {"SessionStart": []}}
    grants = os.path.join(home, "grants.json")
    if os.environ.get("KITH_PERSON") and os.path.exists(grants):
        settings["sandbox"] = {"filesystem": {"allowRead": json.load(open(grants, encoding="utf-8"))["read"]}}
    print(json.dumps({
        "version": 1, "agent": "house", "agent_version": "v1", "harness": harness, "cwd": home,
        "backup": backup, "provider": harness, "account": account, "account_id": account_id,
        "env": {"KITH_AGENT": "house", "HQ_AGENT": "house", "KITH_VIA": os.environ.get("KITH_VIA", ""), "HQ_VIA": os.environ.get("KITH_VIA", ""), "KITH_MODEL": model, "HQ_MODEL": model,
                "KITH_PROVIDER": harness, "KITH_ACCOUNT": account or "", "KITH_ACCOUNT_ID": account_id, "KITH_BACKUP": backup or ""},
        "system_prompt": "You are house, the household keeper.",
        "settings": settings,
        "mcp_servers": {"pantry": {"command": "pantry-mcp", "args": [], "env": {}}},
        "add_dirs": [home + "/data"], "allowed_tools": ["Read"], "disable_slash_commands": True,
        "model": model, "effort": None, "project": project, "call_model": call_model()[1] or None,
        "permission_mode": permission("default") or None,
        "cost_tag": {"agent": "house", "version": "v1", "model": model, "effort": None, "via": os.environ.get("KITH_VIA") or "cli",
                     "project": project, "person": os.environ.get("KITH_PERSON") or None},
        "claude_args": [],
    }))
else:
    print("unknown agent", file=sys.stderr)
    sys.exit(2)
