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

## Guest box facts (W20: what a Windows guest sandbox can rest on)

Source: the `guest` job of `windows-probe.yml` (scripts `guest.ps1`, `guest-run.ps1`, `g3-inbox.ps1`, `guest-claude.ps1`,
helper `boxprobe.exe`). Raw output: artifact `windows-guest-probe-output`. Identity: the OS probes run as `kithguest`, a
STANDARD local user (not an Administrator) on `windows-2025`; the boxed child is an AppContainer process (Low integrity,
only custom capabilities) in a job object. Admin-only steps (grants, `CheckNetIsolation`) ran as `runneradmin`. The Claude
Code probes (F11, F19, F20, F21 part) ran as `runneradmin`: what Claude Code does with these settings does not depend on
the account. Claude Code 2.1.331. Run for F12-F22 unless stated: [45524581007](https://github.com/Ying-Kai-Liao/kith/actions/runs/45524581007) (an earlier run, which had a TEMP/cwd
bug that left P-G3 survivors, P-G4 cmd, P-G5 squat, pair test and P-G10 unmeasured). Run [45526277104](https://github.com/Ying-Kai-Liao/kith/actions/runs/45526277104) is the rerun with
those fixed and is the evidence for F11b, F13, F14 cmd, F15, F22, F23; both runs are green (a failing premise is a recorded
result, not a red job).

### F11. P-G1: `CLAUDE_CODE_GIT_BASH_PATH` selects the shell for the Bash tool AND hooks (HOLDS when Git is off PATH)

Premise: setting the variable to a kith shim named `bash.exe` makes Claude Code run the Bash tool and hooks through it.
Observed, Git for Windows on PATH (run 45524581007, 5 variants): the Git bash on PATH always ran and the stand-in's log
stayed empty, so the variable is ignored while Git is findable on PATH. Observed, Git removed from PATH (run
[45526277104](https://github.com/Ying-Kai-Liao/kith/actions/runs/45526277104), `g1b-*`): with the variable set to `C:\probe\fakegit\bin\bash.exe` (a copy of the logging stand-in that
proxies to the real bash) the stand-in was called 6 times: the SessionStart hook (`-c "echo HOOK_RAN_SESSIONSTART"`), the
PreToolUse hook (`-c "echo HOOK_RAN_PRETOOL"`), `-c env`, `-lc 'echo "$PATH"'`, the shell-snapshot setup
(`-c -l SNAPSHOT_FILE=...`) and the tool command itself (`-c "source <snapshot> ... && eval <command>"`), all with
cwd `C:\probe\cwd`. So hooks and the Bash tool use the same shell. The control (Git off PATH, variable unset) still ran
Git bash: Claude Code also finds Git at its default install path. Result: HOLDS only when Git is not on PATH, so a kith
guest launch must also scrub Git from the child's PATH (or the shim is bypassed). The shim must accept `-c`, `-lc` and
`-c -l <script>` and pass `$0`-style argv through. Identity: runneradmin.

### F12. P-G2: what runs inside the AppContainer box (FAILS for Git Bash)

Premise: Git Bash (or any shell) runs as the Bash tool inside the box with a minimal grant set.
Observed: the box token is AppContainer, Low IL, only the custom capabilities. `node` and `whoami` run. Real Git Bash
fails: `msys-2.0 fatal error NtCreateDirectoryObject(\BaseNamedObjects\msys-2.0S5-...) 0xC0000022` (access denied); git
through bash fails the same way. `git.exe` alone: `could not open '/dev/null': Permission denied`. The box cannot open
the NUL device (Rust null stdin: os error 5; `cmd start >nul`: Access denied). Python fails 0xC0000135 (cannot load its
DLLs) until an admin grants ALL APPLICATION PACKAGES read on its folder; Git, cmd and System32 already grant it, node and
Python folders do not (node ran anyway). Result: FAILS for Git Bash as is. What it needs: F12b. Identity: kithguest
(standard), boxed.

### F13. P-G3: a boxed process cannot leave the job (UNPROVEN, probe method broken)

Observed (both runs): zero survivors after the box job was terminated (`job_active_after_exit=0`), but the measurement is
weak. The rerun showed why: Windows PowerShell inside the AppContainer cannot use its FileSystem provider ("Cannot find
drive. A drive with the name 'Microsoft.PowerShell.Core\FileSystem' does not exist"), so the `cmd start`, `Start-Process`,
`schtasks` and service attempts threw before they ever tried; WMI was denied ("Access to a CIM resource was not
available"); `Shell.Application.ShellExecute` and `WScript.Shell.Run` returned ok but no process was alive afterwards,
so whether they started is still unknown. Result: unproven. A box built on cmd.exe or a native probe (not PowerShell) is
the next try. Do not rely on "cannot escape the job" without it: rule 3 should treat job kill as a belt, not the proof.

### F14. P-G4: what a boxed process can open of the user's unboxed world (HOLDS, with a registry caveat)

Cmd checks added in run [45526277104](https://github.com/Ying-Kai-Liao/kith/actions/runs/45526277104): `cmdkey /list` in the box prints `NONE` (it sees an empty store, not the owner's), and
`tasklist`/`taskkill` style cross-process control returns `Access is denied` (g4e).

Observed: opening an unboxed same-user process: err 5 for every access mask; Credential Manager (CredRead): err 5;
`HKCU\Software\kithprobe`: denied. BUT `HKCU\Environment` and `HKCU\Software\Microsoft\Windows\CurrentVersion` open for
read, so HKCU is partly readable (environment variables of the user are visible). Result: holds for process, credentials,
private keys; partial for registry. Identity: kithguest, boxed.

### F15. P-G5: named pipes (the box cannot create one)

Observed: a boxed process cannot create a named pipe (err 5, with or without FILE_FLAG_FIRST_PIPE_INSTANCE). So the host
side of every box-to-host pipe is created by the host. The squat test (box racing the owner for the name) is moot for
the box; Run [45526277104](https://github.com/Ying-Kai-Liao/kith/actions/runs/45526277104): the box got err 5 creating `kith-host-g5test` (first instance) and `kith-run-g5test`; when the HOST then
created the same name as first instance it succeeded and the pipe's owner SID equals the host user's SID
(`S-1-5-21-...-1003`), so the owner SID is a usable identity check. A box connecting to the not-yet-existing name gets
err 2. Result: holds (box cannot squat; host can verify the owner).

### F16. P-G6: host pipe reachable from the box by ACE (HOLDS)

Observed: a box connects to a host-created pipe whose DACL has an ACE for the capability SID (variants A, B, E) or for the
AppContainer SID (D); err 5 with no ACE (C). No Low mandatory label is needed. The pipe's owner SID, read from the handle,
equals the host user. Result: holds.

### F17. P-G7: loopback to the host (BREAKS without admin)

Observed: a box connecting to an unboxed host listener on 127.0.0.1 times out (6 s) even with `internetClient`; it works
(95 ms) after `CheckNetIsolation LoopbackExempt -a -p=<AppContainer SID>` run as admin. Inside one box loopback works. The
box cannot listen (err 5). With no `internetClient`: DNS fails and 1.1.1.1 gives 10013; with it, both work. The exemption
is per AppContainer SID, so a fresh SID per run needs an admin step per run. Result: FAILS for a per-run SID unless the
exemption is made once for a stable SID (or the box talks to the host only over a pipe, F16). Identity: kithguest for the
box, runneradmin for the exemption.

### F18. P-G8: deny ACE on a capability SID (FAILS)

Observed: with a DENY(write) ACE for the capability SID on a folder and on a file, write and create still succeed; only
removing the allow ACE blocks them. icacls of one (OI)(CI) ACE over 100,000 files: 15.7 s; revoke 15.8 s. Untested:
deny on the AppContainer SID itself, `/inheritance:d`. Result: a deny ACE cannot carve holes; protect by not granting.

### F19. P-G9: permission-rule path forms (partial)

Observed (guest-claude, run 45524581007): `Read(//c/probe/secret/**)`, `Read(C:\probe\secret\**)` and
`Read(C:/probe/secret/**)` each deny reads of every spelling tried (backslash, forward slash, `/c/`, upper/lower case,
`..`); `Read(//C:/probe/secret/**)` and `Read(/c/probe/secret/**)` deny nothing (read returned the secret). Result:
write deny rules as `//c/...`, `C:\...` or `C:/...`.

### F20. P-G12: Claude Code's own Windows sandbox (OFF)

Observed: `sandbox.enabled` gives "Windows sandbox is not active on this session (feature gate off)" and commands run
unsandboxed; `failIfUnavailable` refuses to start; none of four env-name guesses flips it. Result: kith must box the
guest's Bash itself.

### F21. P-G11: tools inside the box (partial)

Observed: `chrome-headless-shell --no-sandbox` in the box takes a screenshot (4917 bytes); with Chrome's own sandbox
nested it hangs (90 s); ffmpeg (libx264) works; `WSADuplicateSocket` into a boxed child works. Result: Chrome needs
`--no-sandbox` inside the box.

### F22. Box against box and against the host (rule 2: one AppContainer SID per run) (HOLDS)

Run [45526277104](https://github.com/Ying-Kai-Liao/kith/actions/runs/45526277104). A process inside box A can open its own box's processes with every access mask (QUERY, VM_READ, DUP_HANDLE,
TERMINATE, ALL_ACCESS). A process in a different box B opens none of A's (err 5 on every mask). The unboxed host (same
user) opens A's process with every mask, so the host can always signal and kill a run. Result: holds, so one AppContainer
SID per run separates runs from each other; it also means two runs sharing an SID share process access, so do not reuse an
SID across guests. The cost is F17 (loopback needs an admin exemption per SID).

### F23. P-G10: TerminateJobObject on the run job with a box in a nested job (PARTIAL)

Run [45526277104](https://github.com/Ying-Kai-Liao/kith/actions/runs/45526277104). The outer job showed 2 active processes; `TerminateJobObject(outer)` brought `ActiveProcesses` to 0 in about
0.2 ms. An immediate liveness check of the two pre-kill pids still reported both alive (termination is asynchronous; the
check was a single zero-wait). The boxed child inside the nested job printed only `Access is denied` (it did not run a
real workload: same NUL/PowerShell limits as F12/F13), so the kill of a working box tree was not exercised. Result: the
job count drops at once; a poll of process handles after the kill is needed to say when the tree is really gone.

### F12b. P-G2: what Git Bash needs (MSYS cannot be granted; no non-MSYS shell tested)

Run [45526277104](https://github.com/Ying-Kai-Liao/kith/actions/runs/45526277104). The failure is the MSYS runtime making a named object directly in `\BaseNamedObjects` (a fixed global name
`msys-2.0S5-<hash>`); an AppContainer is only allowed its own `\Sessions\N\AppContainerNamedObjects\<SID>` directory,
so no file grant or capability we hold fixes it. Setting `MSYS=winsymlinks:nativestrict` and `MSYS2_NOSTART=1` made no
difference (same 0xC0000022; Git's `bash.exe` and `usr\bin\bash.exe` alike). The busybox-w32 try did not run: the
download in the admin step failed (`ERROR in busybox` in `guest-admin.log`, Invoke-WebRequest error), so
a non-MSYS shell is untested. Result: Git Bash cannot run in the box; the guest Bash tool needs a different shell or a
box that is not an AppContainer (a restricted token and job, as on W21's fallback), to be decided by the design.


## Not determinable here

- Status line shell (F5).
- Whether the Windows sandbox works when its feature gate is on (F8).
- Behaviour with no Git for Windows installed, or with `CLAUDE_CODE_GIT_BASH_PATH` pointing elsewhere.
- Interactive-mode behaviour (everything above is `claude -p`).
- Project folder naming for paths with spaces, dots or non-ASCII (F10).

## Guest box facts, round 2 (W20b: A = AppContainer, R = restricted token, U = second local account) — DRAFT, handoff

Source: `guest2` job of `windows-probe.yml` (`guest2.ps1`, `guest2-run.ps1`, `boxprobe xbox/ubox/watch/g3n-box/objdir-hold/wfp-install`).
Identity: probes run as `kithguest` (standard user); admin steps as `runneradmin`. PortableGit 2.56.1 (sha256-checked), busybox-w32 pinned in
`pinned/`. Runs: [45564093216](https://github.com/Ying-Kai-Liao/kith/actions/runs/45564093216), [45565519132](https://github.com/Ying-Kai-Liao/kith/actions/runs/45565519132),
[45566929880](https://github.com/Ying-Kai-Liao/kith/actions/runs/45566929880), [45568261500](https://github.com/Ying-Kai-Liao/kith/actions/runs/45568261500),
[45569554059](https://github.com/Ying-Kai-Liao/kith/actions/runs/45569554059). Holds / fails / partial per entry.

- **F24 (R, holds):** a restricted token starts only with the logon SID (or RESTRICTED) in its restricting list (else 0xC0000142). Capability SIDs are refused as restricting SIDs (err 87); use arbitrary `S-1-5-21-...` SIDs (icacls cannot name them; use .NET/SetNamedSecurityInfo).
- **F25 (R, FAILS):** Git Bash in R dies with `couldn't create signal pipe, Win32 error 5` (MSYS makes the pipe with a DACL of user/SYSTEM/Admins; the restricted second check fails). Only with the user's own SID in the restricting list does bash run (all 15 bash-isms pass), and then the box reads the profile secret and writes anywhere the user can. Low IL limits writes to Low-labelled folders but not reads.
- **F26 (A, FAILS):** Git Bash in A: host can pre-create `\BaseNamedObjects\msys-2.0S5-<key>` and `\Sessions\BNOLINKS\2\...` (P-G2c), then the same signal-pipe error. Key depends on install path: junction and copy of PortableGit give different keys (P-G2e holds).
- **F27 (P-G2f):** A cannot create a named pipe, anonymous CreatePipe works; R can create named pipes (owner = host user SID).
- **F28 (P-G18):** `\Device\Null` DACL has Everyone but no ALL APPLICATION PACKAGES ACE; A cannot open NUL with any capability set (none, custom, 85, 87). R and U open it. Hence `/dev/null` fails in A (git, busybox).
- **F29 (P-G2d):** busybox-w32 runs in A and R; Claude's argv forms work; 12/15 corpus items pass; arrays, here-strings, `declare -A` fail, `shopt` missing, no `$BASH_VERSION`. In A `/dev/null` fails.
- **F30 (P-G3n):** in A and R every routes that could run stayed in the job (breakaway denied, schtasks/sc/BITS/cscript fail; PowerShell and cscript do not start in R: HKCU closed). WMI/COM untested in A and R. In U, WMI `Win32_Process.Create` started a process outside the job that survived TerminateJobObject (U fails G11).
- **F31 (P-G7b, A, holds):** in-box listen plus a second process of the same box connecting works with no capability and with 87/86; internet only with internetClientServer; LAN inconclusive (timeouts for all sets). N1 is possible at the OS level but A has no shell.
- **F32 (P-G17, partial):** one AC SID with explicit process SD + token default DACL: box B cannot open box A's process; a named mutex of A is opened by B.
- **F33 (P-G14):** Firewall rules never filter loopback and did not match an R box by restricting SID. Raw persistent WFP filters (`wfp` feature): A (package SID + LoopbackExempt) reaches only 127.0.0.1:18080; R works with a per-install restricting SID in every box token (block SD user+install SID, permit SD user only); U keyed on the kithbox user works. Owner processes unaffected. Reboot survival is W19.
- **F34 (R vs premises):** profile reads and writes denied; `C:\Windows\Temp` writable at Medium (Users create), denied at Low; Low needs a Low-labelled workspace; deny ACE for a restricting SID blocks writes (not in A); CredRead and HKCU denied; other run SID cannot open the box's processes; ancestors of a grant need a non-inherited ReadAttributes ACE (git stat).
- **F35 (G8/G9, holds in A and R):** level-by-level carve-outs: carved file denied, siblings readable, later files invisible, recreated carved file still denied.
- **F36 (F23 again):** after TerminateJobObject all pre-opened handles signalled within 1-2 ms (A; R with real bash children). In U the host cannot open descendant pids (err 5).
- **F37 (P-G19, holds):** removing the RESTRICTED, ALL APPLICATION PACKAGES and ALL RESTRICTED APPLICATION PACKAGES read ACEs on HKCU\Environment (no admin) closes it to the box; unboxed reads and new processes unaffected; Calculator not on the image.
- **F38 (U, mixed):** CreateProcessWithLogonW + AssignProcessToJobObject on the suspended child works; OS ACLs isolate profile, Cred store, HKCU; node/python/npm fine; fails F30 and F36. Final U bash result: see the last run of this PR.

**Verdict:** both A and R FAIL as designed (A: no POSIX shell runs; R: bash runs only without isolation). Network is solvable in A (N2) and R (G6-R) with raw WFP. Windows guests wait for Claude Code's own sandbox unless U (or another design) is ratified.
