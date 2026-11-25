# Windows 11 manual checklist

Checks that CI cannot run. CI has no sign-in, no sleep, no real SmartScreen, and no OneDrive profile. Each check below names the
invariant or decision it proves, so a failure tells you what broke.

The plan is `docs/plans/windows-owner.md` (sections 2 and 3, package W19). Until that plan is merged to main, read it at
`origin/flow/windows-owner-plan-doc`.

## Where, when, and with what

- **Where.** A free UTM virtual machine on the Mac, running Windows 11 ARM. The x64 build runs under emulation. This is the default
  (Q3 in the plan, pending Kai's confirmation). A check that cannot be judged under emulation says so in its section, as
  "needs x64 hardware". Those checks need a real x64 machine.
- **When.** Before any public Windows release. Run it together with code signing (Q2 in the plan). Kai's own unsigned use does
  not need it.
- **Build.** The release zip from W7, installed with `install.ps1` from W14. Neither is built yet. Until they are, this checklist
  cannot be run. Fill in the zip name and `install.ps1` version in the results table when you run it.
- **Account.** Use a local Windows account for most checks. Use a Microsoft account with OneDrive for the OneDrive check.
- **Windows build.** Record the exact build number (`winver`). Q4 in the plan uses `windows-2025`, which shares the 24H2 base.

## Results

| Check | Build (zip name, sha) | Machine (VM or x64, Windows build) | Date | Pass or fail | Evidence (path) | Notes |
|---|---|---|---|---|---|---|
| 1. Start at sign-in | | | | | | |
| 2. Sleep and wake during a turn | | | | | | needs x64 hardware |
| 3. SmartScreen and Defender, first run | | | | | | |
| 4. Non-English display language | | | | | | |
| 5. OneDrive-redirected profile | | | | | | |
| 6. Path over 260 characters | | | | | | |
| 7. Antivirus holds a file during update rename | | | | | | |
| 8. Windows Terminal attach and resize | | | | | | |
| 9. Ctrl-C in the `kith.exe` trampoline | | | | | | |

Record a pass only when the expected result in the section is seen. Record a fail with what you saw. Keep screenshots and logs
in a folder outside the repository, and put its path in the Evidence column.

## How to record pass or fail

1. Run the steps in order. Do not skip a step.
2. Compare what you see with "Expected". Every point must hold.
3. Write `pass` or `fail` in the table, with the date and the build.
4. On a fail, write the exact message or the observed behaviour in Notes. Open a bug with the check number in its title.
5. A check that cannot run (for example, no OneDrive account) is recorded as `not run`, with the reason. It blocks the release
   until it is run or Kai accepts the gap in writing.

---

## 1. Start at sign-in

**Proves:** D3. The HKCU Run key and `kith-servew` start the host at sign-in. Run keys are not Task Scheduler jobs, and a session-0
service could not own the user's ConPTYs.

**Judged under emulation:** yes.

**Setup.** Install the build with `install.ps1`. Start `kith serve` once so the Run key is written. Note the Run key value
(it names the versioned folder under `%LOCALAPPDATA%\Kith\cli\<version>\`, per D2).

**Steps.**
1. Open `regedit` and find `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. Check there is a Kith entry.
2. Confirm the value names the versioned `kith-servew.exe`, not `cli\current`.
3. Sign out, then sign in again.
4. Wait 60 seconds. Open Task Manager and look for `kith-servew.exe`.
5. Run `kith serve status` in a terminal.

**Expected.** The Run key exists and names the versioned path. After sign-in, `kith-servew.exe` runs without a terminal. The status
command reports the host as running.

## 2. Sleep and wake during a turn

**Proves:** WI5 (one writer per conversation, with LockFileEx and one job object per spawn). A sleep must not leave a claim held
by a process that no longer runs. A wake must not start a second writer on the same thread.

**Judged under emulation:** no. A virtual machine suspend does not reproduce Windows sleep states. Needs x64 hardware, a laptop
or desktop with real S3 or modern standby sleep.

**Setup.** Start a turn in an app chat, with a long answer. Note the thread id.

**Steps.**
1. While the turn is streaming, put the machine to sleep (Start, Power, Sleep).
2. Wait 2 minutes. Wake the machine.
3. Read the thread. Check the turn either finished or ended with a named reason.
4. Send a new message on the same thread.
5. Check the process list: there must be one writer for the thread.

**Expected.** The turn ends with a named reason, or it completes. No second writer starts on the thread. The new message is
answered once. The job object for the old spawn has no active process.

## 3. SmartScreen and Defender on first run

**Proves:** Q2. The unsigned build runs for Kai's own use, and the first-run prompts are as expected. This check is a gate before
signing.

**Judged under emulation:** yes. The unsigned binary and its prompts behave the same under emulation.

**Setup.** Copy the release zip to the VM through a browser download, so the Mark of the Web is set. Do not unzip it with a shell
command.

**Steps.**
1. Unzip, then run `install.ps1`.
2. Note every SmartScreen or Defender prompt, in order, with its text.
3. Accept only what a user would accept. Do not add Defender exclusions.
4. Run `kith serve status` and `kith doctor`.

**Expected.** Prompts name the publisher as unknown, as expected for an unsigned build. No prompt blocks the install outright. Defender
does not quarantine `kith.exe`, `kith-holder.exe`, `kith-stub.exe` or `kith-servew.exe`. Record any quarantine as a fail.

## 4. Non-English display language

**Proves:** WI4. The owner-only check on core.db, the sockets and pipes, the secrets and the machine folder is native. It does not
parse `icacls` text, which is locale-dependent. `kith` refuses to start if the rule does not hold.

**Judged under emulation:** yes.

**Setup.** Settings, Time and language, Language and region. Add a display language that is not English (for example German).
Set it as the display language and sign out and in. Keep English installed as a fallback.

**Steps.**
1. Run `kith serve status`. It must start normally.
2. Run `kith doctor`. The owner-only check must report pass.
3. Right-click `%USERPROFILE%\.kith\core.db`, Properties, Security. Check that only the owner, SYSTEM and Administrators have
   access.
4. Remove an access entry on a test copy of a state file, then run `kith doctor`. It must report a fail.

**Expected.** Steps 1 and 2 pass under the non-English language. Step 4 is refused with a named reason, not a locale error or a crash.

## 5. OneDrive-redirected profile

**Proves:** D1. State stays at `%USERPROFILE%\.kith` and agents at `%USERPROFILE%\kith\agents`. Those are the same paths on
the Mac, and the rules and package placeholders do not change. OneDrive can move the Documents folder. It must not move these.

**Judged under emulation:** yes.

**Setup.** Sign in with a Microsoft account. Turn on OneDrive backup for Documents and Desktop. Wait for the sync to finish.

**Steps.**
1. Run `install.ps1`. Start `kith serve`.
2. Create an agent. Check its folder is `%USERPROFILE%\kith\agents\<name>`, not under OneDrive.
3. Run one owner turn.
4. Check that no file under `.kith` appears in OneDrive's sync status.

**Expected.** Paths are under `%USERPROFILE%` as in D1. OneDrive does not sync `core.db`, the sockets or the secrets. The turn
completes.

## 6. Path over 260 characters

**Proves:** the longPathAware manifest on each binary, and D1. A deep agent folder must work when the path is longer than 260
characters.

**Judged under emulation:** yes.

**Setup.** Check that `kith.exe`, `kith-holder.exe`, `kith-stub.exe` and `kith-servew.exe` each carry the longPathAware manifest.
Use `mt.exe -inputresource` or the build's own check, and record the result.

**Steps.**
1. Create an agent with a long name, so that its folder path is over 260 characters. Put deep subfolders under it until the
   full path is over 260 characters.
2. Run an owner turn that writes a file in that deep folder.
3. Run `kith doctor`.

**Expected.** The turn writes the file. No error about a path that is too long. `kith doctor` reports pass.

## 7. Antivirus holds a file during the update rename

**Proves:** D2. The `current` file is replaced with a file rename, and a running exe is never deleted. A failed delete counts as
kept. An antivirus scan that holds a file must not leave a gap in which a starting hook fails.

**Judged under emulation:** yes.

**Setup.** Install an older build. Start `kith serve`. Turn on Defender real-time protection (it is on by default). Prepare a newer
zip.

**Steps.**
1. Start a scan of the new versioned folder with a tool that locks files, for example `Get-ChildItem -Recurse | Get-Content` in a
   loop. Hold the file open.
2. Run the update (W15 when it exists; until then, `install.ps1` with the newer zip).
3. Check that `cli\current` names the new version after the update.
4. Start a new terminal and run `kith --version`.
5. Check the old version folder is still present, or was removed only after its process exited.

**Expected.** The update either finishes, or stops with a named reason and leaves the old version working. `current` never
names a missing folder. A new terminal starts the trampoline without error. Note the exact wait the update took.

## 8. Windows Terminal attach and resize

**Proves:** D5 (the ConPTY owned by the holder) and D3. A terminal run can be attached to from Windows Terminal, and resizing the
window resizes the ConPTY. Both are needed for the owner to watch a run.

**Judged under emulation:** yes.

**Setup.** Install Windows Terminal from the Microsoft Store. Start a held terminal run with `kith`.

**Steps.**
1. Open a second tab in Windows Terminal. Attach to the held run from it.
2. Type a command in the attached tab. Check the output appears in both tabs.
3. Resize the window to a narrow width, then a wide one.
4. Run `tput cols` (Git Bash) or `$host.UI.RawUI.WindowSize` (PowerShell). Check the value follows the resize.
5. Close the attached tab. Check the held run keeps going.

**Expected.** Attach works. Output matches in both tabs. Resize changes the reported width and height. Closing the attached tab
does not stop the run.

## 9. Ctrl-C in the `kith.exe` trampoline

**Proves:** D2 (the trampoline runs the versioned `kith.exe` as a child in a kill-on-close job, and forwards stdio, Ctrl-C and the
exit code) and D6 (stop on Windows is two steps: a graceful cancel, then `TerminateJobObject`).

**Judged under emulation:** yes.

**Setup.** Start a turn in a terminal run that takes several seconds, for example a long `sleep` in a shell tool.

**Steps.**
1. Press Ctrl-C once in the terminal. Watch the turn.
2. Check the agent gets a graceful cancel: the turn ends with a cancelled reason, and no child is left running.
3. Start another turn. Press Ctrl-C and hold it. Check the process tree stops after the grace period.
4. Check the exit code of `kith.exe` with `echo %ERRORLEVEL%` (cmd) or `$LASTEXITCODE` (PowerShell).
5. Check that no `kith-holder.exe` or stray child process is left, in Task Manager.

**Expected.** One Ctrl-C ends the turn with a named cancel reason. A held Ctrl-C ends the tree after the grace period. The exit code is
passed through. No orphan process stays alive.

---

## Not in this checklist

- Guest runs on a Windows host. They are refused (WI6, Q1). Check (j) in W18 covers the refusal. It is not a manual check.
- Signing. This is a gate on public release (Q2), with its own package when signing is chosen.
- Anything CI already runs. Keep this list to what needs a person, a real sign-in, a real sleep, or a real Windows display.
