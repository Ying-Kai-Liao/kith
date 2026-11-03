#!/usr/bin/env python3
"""kith_readfence.py — the PreToolUse hook that keeps the file tools of a guest run inside its folders.

  kith_readfence.py --root DIR [--root DIR ...]      the hook JSON on stdin; prints a deny decision, or nothing

Read, Grep, Glob, LS and NotebookRead of a path outside the roots (the working directory and the --add-dir folders of the run, by real
path, so a link out of them does not count) are refused. This is what `permissions.blockReadsOutsideWorkingDirectories`
did, but that setting also makes Claude Code ask, whatever a hook says, for every Bash command it cannot trace
(`cd sub && export X=1; ls`), and the ask is a card to the owner. Bash itself stays in the OS sandbox (denyRead).
Fails closed: a payload that cannot be read or a path that cannot be placed is refused.
"""
import json
import os
import re
import sys

TOOLS = {"Read": "file_path", "Grep": "path", "Glob": "path", "LS": "path", "NotebookRead": "notebook_path"}


def deny(why):
    return {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny",
                                   "permissionDecisionReason": why}}


def inside(path, root):
    return path == root or path.startswith(root.rstrip("/") + "/")


def own_results(path):
    """The file Claude Code itself wrote for a long tool result: <config>/projects/<cwd>/<session>/tool-results/<file>."""
    config = os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~/.claude")
    rel = os.path.relpath(path, os.path.realpath(config))
    return re.fullmatch(r"projects/[^/]+/[^/]+/tool-results/[^/]+", rel) is not None


def check(payload, roots):
    param = TOOLS.get(payload.get("tool_name"))
    if param is None:
        return None
    given = (payload.get("tool_input") or {}).get(param)
    cwd = payload.get("cwd") or os.getcwd()
    if given in (None, ""):
        if param != "path":   # Grep and Glob search the working directory when they get no path
            return deny("no path to check")
        given = cwd
    given = os.path.expanduser(str(given))
    path = os.path.realpath(given if os.path.isabs(given) else os.path.join(cwd, given))
    if any(inside(path, os.path.realpath(r)) for r in roots) or own_results(path):
        return None
    return deny("outside the folders this run was given (%s)" % path)


def main(argv):
    roots = [argv[i + 1] for i, a in enumerate(argv) if a == "--root" and i + 1 < len(argv)]
    try:
        payload = json.load(sys.stdin)
        decision = check(payload, roots)
    except (OSError, ValueError, AttributeError, TypeError):
        decision = deny("the read check could not run, so this read is refused")
    if decision:
        print(json.dumps(decision))


if __name__ == "__main__":
    main(sys.argv[1:])
