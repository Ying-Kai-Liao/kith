"""ACP agent that runs out of usage on cue, for the backup-account tests.

The account it runs as comes from KITH_BACKUP (the chain entry the stub kith launched; empty is the agent's main account:
`primary` when that is the default login, else its id from KITH_ACCOUNT_ID).
`limits.json` in the working folder maps an account (`primary`, `claude:work`, `codex`…) to how its next prompts fail:
`{"kind": "rate_limit", "resetsAt": <epoch seconds or null>, "via": "error" | "rejected"}`. `rejected` also sends
Claude's `_claude/rateLimit` update with status `rejected` first, and then fails with no category at all. A prompt
containing FILL95 answers, and reports the 5-hour window 97% used (resetting in 30 minutes). Every request is appended
to `requests.jsonl` with the account that got it.
"""
import json
import os
import sys
import time

entry = os.environ.get("KITH_BACKUP", "")
# The agent's main account when it isn't the default login (`claude:home`): named as itself.
main = os.environ.get("KITH_ACCOUNT_ID", "claude")
account = entry or ("primary" if main in ("", "claude") else main)
sessions = 0


def send(value):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", **value}) + "\n")
    sys.stdout.flush()


def update(session, value, meta=None):
    if meta:
        value = {**value, "_meta": meta}
    send({"method": "session/update", "params": {"sessionId": session, "update": value}})


def limits():
    try:
        with open("limits.json", encoding="utf-8") as f:
            return json.load(f).get(account)
    except (OSError, ValueError):
        return None


for line in sys.stdin:
    message = json.loads(line)
    method, params = message.get("method"), message.get("params") or {}
    if "id" not in message or method is None:
        continue
    with open("requests.jsonl", "a", encoding="utf-8") as log:
        log.write(json.dumps({"account": account, "method": method, "params": params}) + "\n")
    if method == "initialize":
        capabilities = {"loadSession": True, "sessionCapabilities": {}, "_meta": {"claudeCode": {}}}
        send({"id": message["id"], "result": {"protocolVersion": 1, "agentCapabilities": capabilities}})
    elif method == "session/new":
        sessions += 1
        modes = {"availableModes": [{"id": "read-only"}, {"id": "auto"}, {"id": "full-access"}]}
        send({"id": message["id"], "result": {"sessionId": f"{account}-s{sessions}", "modes": modes}})
    elif method == "session/prompt":
        sid = params["sessionId"]
        text = " ".join(b.get("text", "") for b in params["prompt"])
        limit = limits()
        if limit:
            if "TOOL" in text:
                # It did something before the limit hit.
                update(sid, {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit", "kind": "edit",
                             "status": "completed"})
            if limit.get("via") == "rejected":
                info = {"status": "rejected", "rateLimitType": "five_hour", "resetsAt": limit.get("resetsAt")}
                update(sid, {"sessionUpdate": "usage_update", "used": 1, "size": 200000}, {"_claude/rateLimit": info})
                error = {"code": -32603, "message": "Internal error"}
            else:
                data = {"errorKind": limit["kind"]}
                if limit.get("resetsAt"):
                    data["resetsAt"] = limit["resetsAt"]
                error = {"code": -32603, "message": "Internal error", "data": data}
            send({"id": message["id"], "error": error})
            continue
        if "FILL95" in text:
            info = {"status": "allowed_warning", "rateLimitType": "five_hour", "utilization": 0.97,
                    "resetsAt": int(time.time()) + 1800}
            update(sid, {"sessionUpdate": "usage_update", "used": 1, "size": 200000}, {"_claude/rateLimit": info})
        update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": f"answered by {account}"}})
        send({"id": message["id"], "result": {"stopReason": "end_turn"}})
    else:
        send({"id": message["id"], "result": {}})
