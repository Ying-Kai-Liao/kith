"""ACP fixture for an agent that wakes itself: its turn ends, then a "background task" finishes and the agent acts
with no host turn running (a session/update, then send_message and send_file through the real chat MCP)."""
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

sessions = {}
out = threading.Lock()

def send(value):
    with out:
        print(json.dumps(value), flush=True)

def update(sid, body):
    send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': sid, 'update': body}})

def wake(sid, text):
    time.sleep(1)
    session = sessions[sid]
    tool = next(server for server in session['mcpServers'] if server['name'] == 'chat')
    update(sid, {'sessionUpdate': 'tool_call', 'toolCallId': 'bg-1', 'title': 'Render finished', 'kind': 'other', 'status': 'completed'})
    with subprocess.Popen([tool['command'], *tool['args']], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=os.environ.copy()) as mcp:
        sequence = 0
        def request(method, arguments):
            nonlocal sequence
            sequence += 1
            mcp.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': sequence, 'method': method, 'params': arguments}) + '\n')
            mcp.stdin.flush()
            return json.loads(mcp.stdout.readline())['result']
        request('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'bg-test', 'version': '1'}})
        results = []
        results.append(request('tools/call', {'name': 'send_message', 'arguments': {'text': 'The render is done.'}}))
        path = 'missing.mp4' if 'bad' in text else 'report.pdf'
        results.append(request('tools/call', {'name': 'send_file', 'arguments': {'path': path}}))
        Path(session['cwd'], 'offturn-results.json').write_text(json.dumps(results))
        mcp.stdin.close()

for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    if method == 'initialize':
        result = {'protocolVersion': 1, 'agentCapabilities': {'loadSession': True}}
    elif method in ['session/new', 'session/load']:
        session_id = message['params'].get('sessionId', 'bg-test-' + str(len(sessions)))
        sessions[session_id] = message['params']
        result = {'sessionId': session_id}
    elif method == 'session/prompt':
        sid = message['params']['sessionId']
        text = message['params']['prompt'][0]['text']
        update(sid, {'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'Rendering in the background.'}})
        if 'background' in text:
            threading.Thread(target=wake, args=(sid, text), daemon=True).start()
        result = {'stopReason': 'end_turn'}
    else:
        continue
    send({'jsonrpc': '2.0', 'id': message['id'], 'result': result})
