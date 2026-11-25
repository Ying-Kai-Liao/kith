# Windows facts: what real Claude Code does on Windows

Measured, not assumed. Source: the `windows-probe` workflow (`.github/workflows/windows-probe.yml`, script
`serve/host/tests/support/winprobe/probe.ps1`), which runs real Claude Code with `claude -p` against `kith-fakemodel`
(dummy API key, throwaway `CLAUDE_CONFIG_DIR`, working folder `C:\probe\cwd`). Raw output: the `windows-probe-output`
artifact of the run named below.

- Date: 2026-11-24
- Runner: `windows-2025`, image `win25-vs2026` version `20261116.315.1`, Windows Server 2025 Datacenter 10.0.26100,
  PowerShell 7.6.10, Git for Windows 2.56.1 (bash 5.3.15, MSYSTEM=UCRT64), node 22.23.5, npm 10.9.9
- Claude Code: `2.1.331` (both installs, `claude --version` prints `2.1.331 (Claude Code)`)
- Run used for the evidence below: the second run of PR #1161 (GitHub Actions run 45365311734; the first run,
  45364782356, gave the same results for everything except the two extra unquoted-path hook forms and the sandbox grep).

Feeds: W6 (engines and accounts) F1, F2, F9; W9 (hook form) F3, F4, F5; W10 (guard and PowerShell deny) F3, F6, F7, F8;
W13 (cost and transcripts) F10.

## F1. Where each install puts the program (feeds W6)

npm (`npm install -g @anthropic-ai/claude-code`, npm prefix `C:\npm\prefix`):

```
Name       Length
claude        308          (sh shim)
claude.cmd    160
claude.ps1    520
package dir: C:\npm\prefix\node_modules\@anthropic-ai\claude-code   (bin: {"claude": "bin/claude.exe"}, version 2.1.331)
C:\npm\prefix\node_modules\@anthropic-ai\claude-code\bin\claude.exe                                                  (native binary)
C:\npm\prefix\node_modules\@anthropic-ai\claude-code\node_modules\@anthropic-ai\claude-code-win32-x64\claude.exe     (native binary)
claude.cmd: "%dp0%\node_modules\@anthropic-ai\claude-code\bin\claude.exe"   %*
where.exe claude: C:\npm\prefix\claude ; C:\npm\prefix\claude.cmd
```

So an npm install of this version is not a JS program: `claude.cmd` only launches a native `claude.exe` inside the
package. A caller can run `claude.cmd` (needs `cmd.exe`, which `Start-Process` and `CreateProcess` do for `.cmd`) or the
`bin\claude.exe` directly. PATH needs the npm global prefix (`C:\npm\prefix` here, from `npm prefix -g`) for the bare
name `claude` to resolve; on this runner that directory was already on PATH.

Native `install.ps1` (`irm https://claude.ai/install.ps1 | iex`):

```
Location: C:\Users\runneradmin\.local\bin\claude.exe          (405,798,560 bytes)
C:\Users\runneradmin\.local\share\claude   C:\Users\runneradmin\.local\state\claude   C:\Users\runneradmin\.claude\downloads
Setup notes: Native installation exists but C:\Users\runneradmin\.local\bin is not in your PATH. ...
user PATH changed by install.ps1: False
```

The installer does not touch the user PATH, so `%USERPROFILE%\.local\bin` must be added by us (or the full path used).
`where.exe claude` with that directory on PATH: `C:\Users\runneradmin\.local\bin\claude.exe`.

Each install was probed on its own PATH (the other's directory removed). Everything below came out identical for both.

## F2. Which shell runs hook commands (feeds W6, W9)

Git Bash. A hook command `echo SHELLPROBE $0 %ComSpec% $BASH_VERSION $PSVersionTable.PSVersion > file` wrote:

```
SHELLPROBE /usr/bin/bash %ComSpec% 5.3.15(2)-release .PSVersion
```

(`$0` and `$BASH_VERSION` expanded, `%ComSpec%` and `$PSVersionTable` did not: bash.) The process chain recorded by the
hook (npm install):

```
powershell.exe ... <- bash.exe:"C:\Program Files\Git\bin\..\usr\bin\bash.exe" -c "<hook command>"
  <- bash.exe:"C:\Program Files\Git\bin\bash.exe" -c "<hook command>" <- claude.exe <- cmd.exe /c ""C:\npm\prefix\claude.cmd" ...
```

Same for SessionStart, PreToolUse and PostToolUse, and for both installs. The hook command string is passed whole to
`bash -c`, so bash quoting rules apply to it. The hook gets the event JSON on stdin (the recorder read
`hook_event_name` from it).

## F3. Quoting forms (feeds W9, W10)

Each form launched `powershell.exe` or `pwsh.exe` on a recorder script; "ok" means the recorder ran:

| Form | Command (JSON-unescaped) | Result |
|---|---|---|
| double-quoted forward-slash exe, double-quoted args | `"C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -File "C:/probe/rec.ps1" "fwd"` | ok |
| double-quoted backslash exe, double-quoted args | `"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -File "C:\probe\rec.ps1" "back"` | ok |
| single-quoted exe and args | `'C:/Windows/System32/.../powershell.exe' -NoProfile -File 'C:/probe/rec.ps1' 'single'` | ok |
| double-quoted path with spaces | `"C:/Program Files/PowerShell/7/pwsh.exe" -NoProfile -File "C:/probe/rec.ps1" "spaced"` | ok |
| bare name on PATH | `powershell -NoProfile -File "C:/probe/rec.ps1" "bare"` | ok |
| unquoted forward-slash path (no spaces) | `C:/Windows/System32/.../powershell.exe -NoProfile -File C:/probe/rec.ps1 unqfwd` | ok |
| unquoted backslash path | `C:\Windows\System32\...\powershell.exe -NoProfile -File C:\probe\rec.ps1 unqback` | FAILS |

Failure evidence (stream-json `hook_response`, exit 127, outcome `error`):

```
/usr/bin/bash: line 1: C:WindowsSystem32WindowsPowerShellv1.0powershell.exe: command not found
```

Rule for W9: because bash runs the command, an unquoted backslash path loses its backslashes. Always write the exe path
in double quotes (either slash direction works inside double quotes) or with forward slashes. Single quotes also work.
A failing hook is non-blocking here (the session continued and the Bash tool ran).

## F4. Hook events fire in `-p` mode (feeds W9)

SessionStart, PreToolUse (matcher `Bash`) and PostToolUse (matcher `Bash`) all ran; `--include-hook-events` produced
`hook_started` / `hook_response` lines with `exit_code` and `stdout`/`stderr`. Hooks of the same event run in parallel
(six PreToolUse `hook_started` lines before the first `hook_response`).

## F5. Status line (feeds W9)

Not determinable by this probe. With `statusLine: {"type":"command", ...}` in settings, `claude -p` never ran it (no
file written, in any run). The status line only runs in the interactive UI, which CI cannot drive here. By F2 hook
commands are run by Git Bash; whether the status line uses the same shell is unverified.

## F6. PowerShell tool (feeds W10)

Present in this version, but only behind an opt-in. Tool names in the request body fakemodel logged:

```
default:                      Agent,Bash,Edit,Glob,Grep,ListAgents,Read,ReportFindings,ScheduleWakeup,Skill,ToolSearch,Workflow,DeferredToolPlaceholder,Write
CLAUDE_CODE_USE_POWERSHELL_TOOL=1:  Agent,Bash,Edit,Glob,Grep,ListAgents,PowerShell,Read,...,Write
```

The exact tool name is `PowerShell`. It is absent by default and appears with `CLAUDE_CODE_USE_POWERSHELL_TOOL=1`
(the stream-json `init` message's `tools` list also gained `"PowerShell"`). Because a later version may turn it on by
default, W10 should deny it regardless (F7).

## F7. `permissions.deny: ["PowerShell"]` removes the tool (feeds W10)

With `CLAUDE_CODE_USE_POWERSHELL_TOOL=1` and `{"permissions":{"deny":["PowerShell"]}}` in settings, the request's tool
list was again `Agent,Bash,Edit,Glob,Grep,ListAgents,Read,ReportFindings,ScheduleWakeup,Skill,ToolSearch,Workflow,
DeferredToolPlaceholder,Write`: `PowerShell` is removed from the request, not just refused at call time. Same for both
installs.

## F8. `sandbox.enabled: true` with `failIfUnavailable: true` (feeds W10)

Refuses to start (exit 1, no model request made):

```
Error: sandbox required but unavailable: sandbox is enabled but the Windows sandbox is not active on this session (feature gate off)
  Refusing to start without a working sandbox (see sandbox.failIfUnavailable).
result: {"is_error":true,"subtype":"error_during_execution","startup_failure_reason":"sandbox_required_unavailable",
 "errors":["Sandbox required but unavailable: ... Set sandbox.failIfUnavailable=false to allow unsandboxed execution."]}
```

The settings key is `sandbox.failIfUnavailable` (named in the message; the string is also present in both binaries).
So on Windows with this version a "sandbox on, fail if unavailable" setting turns every run into a startup failure; the
cause reported is a feature gate being off, so a later version or an account with the gate on may behave differently
(not determinable here).

## F9. Bash tool: which bash, and a `#!/bin/sh` stub on PATH (feeds W6, W10)

The Bash tool runs Git Bash, the same one hooks use. Tool result (npm install; native identical):

```
BASH_VERSION=5.3.15(2)-release   BASH=/usr/bin/bash   SHELL=/bin/bash.exe   MSYSTEM=UCRT64
command -v bash -> /usr/bin/bash   cygpath -w -> C:\Program Files\Git\usr\bin\bash.exe
uname -a: MINGW64_NT-10.0-26100 ... Msys        pwd: /c/probe/cwd
command -v kithstub -> /c/probe/bin/kithstub
STUB_RAN args=a1 a 2 zero=/c/probe/bin/kithstub     stub_rc=0
```

A file `C:\probe\bin\kithstub` with no extension, containing `#!/bin/sh`, in a directory on the Windows PATH, runs from
the Bash tool by bare name, with arguments (including one with a space) intact. The Windows PATH is converted to
`/c/...` form inside bash (`/c/probe/bin` appears in `$PATH`, after Git's own `/usr/bin`). Git Bash was found without
`CLAUDE_CODE_GIT_BASH_PATH` set; the runner has `C:\Program Files\Git\bin\bash.exe` first on PATH and also a
`C:\Windows\System32\bash.exe` (WSL launcher) which Claude Code did not use. A machine with no Git for Windows was not
tested.

## F10. Project folder name (feeds W13)

Working folder `C:\probe\cwd`, `CLAUDE_CONFIG_DIR=C:\probe\cfg-npm-base`:

```
C:\probe\cfg-npm-base\projects\C--probe-cwd
```

Inferred from this one sample: `:` and `\` each become `-`, so `C:\probe\cwd` -> `C--probe-cwd`
(drive colon and the following backslash give two dashes). Only one folder shape was sampled, so how it treats `.`,
spaces, long paths and non-ASCII is not determined here. With `CLAUDE_CONFIG_DIR` set, `projects` lives under it
rather than under `%USERPROFILE%\.claude`.

## Not determinable here

- Status line shell (F5).
- Whether the Windows sandbox works when its feature gate is on (F8).
- Behaviour with no Git for Windows installed, or with `CLAUDE_CODE_GIT_BASH_PATH` pointing elsewhere.
- Interactive-mode behaviour (everything above is `claude -p`).
- Project folder naming for paths with spaces, dots or non-ASCII (F10).
