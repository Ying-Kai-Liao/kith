"""Persistent deterministic ACP sessions for routine restart and queue tests."""
import json
import os
import pathlib
import re
import sys
import threading
import time
import uuid

lock = threading.RLock()
sessions_file = pathlib.Path("sessions.json")
sessions = json.loads(sessions_file.read_text()) if sessions_file.exists() else {}
cancel = threading.Event()


def send(value):
    with lock:
        print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)


def log(event, sid, text):
    with lock:
        with pathlib.Path("events.jsonl").open("a") as output:
            output.write(json.dumps({"event": event, "session": sid, "text": text}) + "\n")


def prompt(request, stop):
    sid = request["params"]["sessionId"]
    text = request["params"]["prompt"][0]["text"]
    resumed = "[Kith restarted" in text
    with lock:
        if resumed:
            text = sessions[sid]
        else:
            sessions[sid] = text
            temp = sessions_file.with_suffix(".tmp")
            temp.write_text(json.dumps(sessions))
            temp.replace(sessions_file)
    log("resumed" if resumed else "started", sid, text)
    block = re.search(r"BLOCK\[([^]]+)\]", text)
    if block:
        while not pathlib.Path("release-" + block[1]).exists():
            if stop.wait(0.01) and "IGNORE_CANCEL" not in text:
                send({"id": request["id"], "result": {"stopReason": "cancelled"}})
                return
    fail = re.search(r"FAIL_ONCE\[([^]]+)\]", text)
    if fail and not pathlib.Path("failed-" + fail[1]).exists():
        pathlib.Path("failed-" + fail[1]).touch()
        log("failed", sid, text)
        send({"id": request["id"], "error": {"code": -32000, "message": "transient fixture failure"}})
        return
    log("finished", sid, text)
    reply = "(pass)" if "SILENT" in text else "Routine completed successfully."
    send({"method": "session/update", "params": {"sessionId": sid, "update": {
        "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": reply}}}})
    send({"id": request["id"], "result": {"stopReason": "end_turn"}})


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        log("initialize", "", str(os.getpid()))
        capabilities = {"loadSession": not pathlib.Path("no-load").exists(), "sessionCapabilities": {"close": {}}}
        send({"id": request["id"], "result": {"agentCapabilities": capabilities}})
    elif method == "session/new":
        send({"id": request["id"], "result": {"sessionId": str(uuid.uuid4())}})
    elif method == "session/load":
        sid = request["params"]["sessionId"]
        if sid in sessions:
            log("loaded", sid, "")
            send({"id": request["id"], "result": {"sessionId": sid}})
        else:
            send({"id": request["id"], "error": {"code": -32000, "message": "unknown session"}})
    elif method == "session/prompt":
        cancel = threading.Event()
        threading.Thread(target=prompt, args=(request, cancel), daemon=True).start()
    elif method == "session/cancel":
        cancel.set()
    elif method == "session/close":
        log("closed", request["params"]["sessionId"], "")
        send({"id": request["id"], "result": {}})
    elif "id" in request:
        send({"id": request["id"], "result": {}})
