#!/usr/bin/env python3
"""The ACP command a test host gives every kith agent (`KITH_SERVE_HQ_ACP_COMMAND`): it starts the scripted agent that
`<agents home>/<agent>/acp` names (a shell command line), where `<agent>` is the `KITH_AGENT` of the launch
(`kernel_agents.py` prints it). So one host can run agents that each speak a different fixture.

Usage: acp_dispatch.py <agents home>
"""
import os
import sys

home = sys.argv[1]
agent = os.environ["KITH_AGENT"]
with open(os.path.join(home, agent, "acp"), encoding="utf-8") as f:
    command = f.read().strip()
os.execvp("sh", ["sh", "-c", command])
