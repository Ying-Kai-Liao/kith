"""Deterministic ACP agent for the Writer-mode actor tests (agent/bot/writer_tests.rs); no provider or network access.

One session per folder, named after it (`sess-<folder>`), so a test can seed the core's claim for the conversation before the agent
exists. A prompt says what to do: HANG waits for session/cancel, DIE exits at once (a process killed under its turn), WAKE answers
and then writes by itself half a second later (the agent waking itself), STUBBORN ignores cancel, SLOW answers after two seconds, anything else is echoed.
"""

import json
import os
import sys
import threading
import time

lock = threading.Lock()
cancel = threading.Event()
folder = os.path.basename(os.getcwd())
sid = "sess-" + folder


def send(value):
    with lock:
        print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)


def chunk(session, text):
    send({"method": "session/update", "params": {"sessionId": session, "update": {
        "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}}})


def prompt(m):
    session = m["params"]["sessionId"]
    text = "".join(p.get("text", "") for p in m["params"]["prompt"])
    with open("prompts.jsonl", "a") as log:
        log.write(json.dumps(text) + "\n")
    if "DIE" in text:
        os._exit(3)
    if "STUBBORN" in text:
        # Ignores session/cancel: only the host's kill ends it.
        while True:
            time.sleep(1)
    if "HANG" in text:
        cancel.clear()
        cancel.wait()
        send({"id": m["id"], "result": {"stopReason": "cancelled"}})
        return
    if "SLOW" in text:
        time.sleep(2)
    chunk(session, "ok " + text)
    send({"id": m["id"], "result": {"stopReason": "end_turn"}})
    if "WAKE" in text:
        def later():
            time.sleep(0.5)
            chunk(session, "woke by itself")
        threading.Thread(target=later, daemon=True).start()


for line in sys.stdin:
    m = json.loads(line)
    if "method" not in m:
        continue
    method = m["method"]
    if method == "initialize":
        send({"id": m["id"], "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}}})
    elif method == "session/new":
        send({"id": m["id"], "result": {"sessionId": sid}})
    elif method == "session/load":
        send({"id": m["id"], "result": {}})
    elif method == "session/prompt":
        threading.Thread(target=prompt, args=(m,), daemon=True).start()
    elif method == "session/cancel":
        cancel.set()
    elif "id" in m:
        send({"id": m["id"], "result": {}})
