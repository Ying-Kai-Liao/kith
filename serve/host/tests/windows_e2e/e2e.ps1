# Windows end-to-end, run by .github/workflows/windows-e2e.yml on a free runner (docs/plans/windows-owner.md W18).
# Every Check records pass or fail; the script exits 1 if any failed, after dumping the host log. A scenario that cannot
# run yet asserts TODAY's named refusal and says which package flips it: nothing passes silently.
param(
  [Parameter(Mandatory)][string]$Unpacked,    # the unpacked release zip (bin\, share\kith\)
  [Parameter(Mandatory)][string]$Fakemodel,   # kith-fakemodel.exe
  [Parameter(Mandatory)][string]$Work,        # throwaway profile root
  [Parameter(Mandatory)][string]$Out          # logs and evidence, uploaded as an artifact
)
$ErrorActionPreference = 'Stop'
$Here = Split-Path -Parent $MyInvocation.MyCommand.Path
New-Item -ItemType Directory -Force $Work, $Out | Out-Null
$Work = (Resolve-Path $Work).Path
$Out = (Resolve-Path $Out).Path
$Bin = (Resolve-Path (Join-Path $Unpacked 'bin')).Path
$Kith = Join-Path $Bin 'kith.exe'
$script:Failed = @()
$LogFile = Join-Path $Out 'e2e.log'
function Log($m) { Write-Host $m; Add-Content -Path $LogFile -Value $m }
function Section($t) { Log ''; Log "=== $t ===" }
function Check($name, [bool]$ok, $detail = '') {
  if ($ok) { Log "PASS  $name" } else { Log "FAIL  $name  $detail"; $script:Failed += $name }
}

# --- the throwaway profile ------------------------------------------------------------------------------------------
$ProfileDir = Join-Path $Work 'profile'
$KithHome = Join-Path $Work 'kith-home'
$KithState = Join-Path $Work 'kith-state'
$ServeHome = Join-Path $KithHome 'serve'
New-Item -ItemType Directory -Force $ProfileDir, $KithHome, $KithState, $ServeHome | Out-Null
$Port = Get-Random -Minimum 20000 -Maximum 29000   # unique per row; the default 19224 stays free
$env:USERPROFILE = $ProfileDir
$env:HOME = $ProfileDir
$env:KITH_HOME = $KithHome
$env:KITH_STATE = $KithState
$env:KITH_SERVE_HOME = $ServeHome

# Runs a program to completion with a timeout; returns exit code, stdout and stderr (all logged).
function Run-Cap([string]$Exe, [string[]]$Arguments = @(), [int]$TimeoutSec = 60, [string]$Tag = 'cmd') {
  $o = Join-Path $Out "$Tag.out"; $e = Join-Path $Out "$Tag.err"
  $sp = @{ FilePath = $Exe; RedirectStandardOutput = $o; RedirectStandardError = $e; PassThru = $true; WindowStyle = 'Hidden' }
  if ($Arguments.Count) { $sp.ArgumentList = $Arguments }
  $p = Start-Process @sp
  if (-not $p.WaitForExit($TimeoutSec * 1000)) { taskkill /T /F /PID $p.Id | Out-Null; Log "TIMEOUT after ${TimeoutSec}s: $Exe $Arguments" }
  $p.WaitForExit()
  $r = [pscustomobject]@{
    Code = $p.ExitCode
    Out = ((Get-Content $o -Raw -ErrorAction SilentlyContinue) + '').Trim()
    Err = ((Get-Content $e -Raw -ErrorAction SilentlyContinue) + '').Trim()
  }
  Log "`$ $([IO.Path]::GetFileName($Exe)) $($Arguments -join ' ')  -> exit $($r.Code)"
  if ($r.Out) { Log "  stdout: $($r.Out)" }
  if ($r.Err) { Log "  stderr: $($r.Err)" }
  return $r
}

function Host-Procs { @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($Bin, [StringComparison]::OrdinalIgnoreCase) }) }
function Api([string]$Method, $Body = @{}) {
  $token = (Get-Content (Join-Path $ServeHome 'token') -Raw).Trim()
  $json = $Body | ConvertTo-Json -Depth 10 -Compress
  try {
    $r = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/api/$Method" -Method Post -Body $json -ContentType 'application/json' `
      -Headers @{ Authorization = "Bearer $token" } -UseBasicParsing -TimeoutSec 30 -SkipHttpErrorCheck
    return [pscustomobject]@{ Status = [int]$r.StatusCode; Body = $r.Content }
  } catch {
    return [pscustomobject]@{ Status = 0; Body = "$_" }
  }
}
function Dump-HostLogs {
  Section 'host logs'
  Get-ChildItem $ServeHome, $KithHome, $KithState -Recurse -Include *.log, *.txt -ErrorAction SilentlyContinue | ForEach-Object {
    Log "--- $($_.FullName)"; Log ((Get-Content $_.FullName -Tail 60 -ErrorAction SilentlyContinue) -join "`n")
  }
}

Section 'environment'
Log "date=$(Get-Date -Format o) ImageOS=$env:ImageOS port=$Port"
Log "profile=$ProfileDir home=$KithHome state=$KithState"

# --- install: unpack by script (W14's install.ps1 does not exist yet) ----------------------------------------------------
Section 'install from the zip'
# TODO W14: replace the unpack-by-workflow-step with install.ps1 (it also puts the versioned cli\<version>\ layout in place).
$ver = Run-Cap $Kith @('--version') 30 'version'
Check 'kith.exe --version runs from the unpacked zip' ($ver.Code -eq 0 -and $ver.Out -match 'kith') $ver.Out
Check 'the zip carries the data folder' (Test-Path (Join-Path $Unpacked 'share\kith\kernel\VERSION'))
Check 'the zip leaves out kernel\bin (Windows data only)' (-not (Test-Path (Join-Path $Unpacked 'share\kith\kernel\bin')))

# kith-servew.exe starts kith-serve.exe beside itself, so the zip must carry it (scripts/windows-exes.txt).
Check 'the zip carries kith-serve.exe (what kith-servew.exe starts)' (Test-Path (Join-Path $Bin 'kith-serve.exe'))

# --- init -------------------------------------------------------------------------------------------------------------------
Section 'kith init'
# kith init is the bash router's verb (kernel/bin/kith) and Windows has no router until retire-python P4-13 (kith-init).
# Asserted: it does not succeed and does not fail silently. Flips to a real init when P4-13 lands.
$init = Run-Cap $Kith @('init') 30 'init'
Check 'kith init refuses today (until retire-python P4-13)' ($init.Code -ne 0 -and ($init.Out + $init.Err).Length -gt 0)

# --- host: install the Run key and start it ---------------------------------------------------------------------------------
Section 'start the host'
$inst = Run-Cap $Kith @('install', '--port', "$Port") 60 'install'
Check 'kith install writes the Run key and starts the host' ($inst.Code -eq 0) ($inst.Out + $inst.Err)
$runVal = (reg query 'HKCU\Software\Microsoft\Windows\CurrentVersion\Run' /v KithServe 2>&1) -join ' '
Check 'HKCU Run key names kith-servew.exe' ($runVal -match 'kith-servew\.exe') $runVal
$up = $false
for ($i = 0; $i -lt 45 -and -not $up; $i++) {
  try { $h = Invoke-RestMethod "http://127.0.0.1:$Port/health" -TimeoutSec 2; $up = [bool]$h.ok } catch { Start-Sleep -Seconds 1 }
}
Check 'the host answers /health' $up
if (-not $up) { Dump-HostLogs; Log 'host did not start; the scenarios below need it'; exit 1 }
Log "health: $($h | ConvertTo-Json -Compress)"
Check 'the host wrote its local token' (Test-Path (Join-Path $ServeHome 'token'))
$st = Run-Cap $Kith @('status', '--port', "$Port") 30 'status'
Check 'kith status reports the host' ($st.Code -eq 0) ($st.Out + $st.Err)
$hello = Api 'hello'
Log "hello -> $($hello.Status) $($hello.Body)"
$helloJson = try { $hello.Body | ConvertFrom-Json } catch { $null }
Check 'the app API answers an authorised call (hello, with the token)' ($hello.Status -eq 200 -and $helloJson.os -eq 'windows' -and $helloJson.hostId) "$($hello.Status) $($hello.Body)"
$noauth = (Invoke-WebRequest "http://127.0.0.1:$Port/api/people" -Method Post -UseBasicParsing -TimeoutSec 10 -SkipHttpErrorCheck).StatusCode
Check 'the app API refuses a call without the token' ($noauth -eq 401) "$noauth"

# --- Claude Code, Bash only, against the fake model --------------------------------------------------------------------------
Section 'Claude Code with Bash only (fakemodel)'
$npmPrefix = (npm prefix -g).Trim()
$cl = Run-Cap 'npm.cmd' @('install', '-g', '@anthropic-ai/claude-code') 300 'npm-claude'
$claude = Join-Path $npmPrefix 'claude.cmd'
Check 'Claude Code installed from npm' (($cl.Code -eq 0) -and (Test-Path $claude)) ($cl.Out + $cl.Err)
$cfg = Join-Path $Work 'claude-cfg'; $cwd = Join-Path $Work 'claude-cwd'
New-Item -ItemType Directory -Force $cfg, $cwd | Out-Null
# What W10 will generate for an owner run: the PowerShell tool denied. Nothing generates owner settings on Windows yet
# (`kith kernel launch settings` is Unix-only), so the e2e writes the rule itself and checks Claude Code honours it.
# TODO W10: replace this hand-written settings.json with the host-generated owner settings and assert the deny rule is in them.
'{"permissions":{"deny":["PowerShell"]}}' | Set-Content (Join-Path $cfg 'settings.json')
$reqLog = Join-Path $Out 'claude.requests.jsonl'
$fake = Start-Process $Fakemodel -ArgumentList '--script', (Join-Path $Here 'bash-turn.json'), '--log', $reqLog `
  -RedirectStandardOutput (Join-Path $Out 'fake.out') -RedirectStandardError (Join-Path $Out 'fake.err') -PassThru -WindowStyle Hidden
$addr = $null
for ($i = 0; $i -lt 40 -and -not $addr; $i++) { Start-Sleep -Milliseconds 500; $addr = Get-Content (Join-Path $Out 'fake.out') -ErrorAction SilentlyContinue | Select-Object -First 1 }
Check 'the fake model server is up' ([bool]$addr)
if ($addr -and (Test-Path $claude)) {
  try {
    $env:ANTHROPIC_BASE_URL = "http://$addr"; $env:ANTHROPIC_API_KEY = 'dummy'; $env:CLAUDE_CONFIG_DIR = $cfg
    $env:DISABLE_AUTOUPDATER = '1'; $env:CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'; $env:DISABLE_TELEMETRY = '1'
    $env:CLAUDE_CODE_USE_POWERSHELL_TOOL = '1'   # on, so the deny rule is what keeps the tool out
    '' | Set-Content (Join-Path $Work 'empty.txt')
    $sp = @{ FilePath = $claude; WorkingDirectory = $cwd; RedirectStandardInput = (Join-Path $Work 'empty.txt'); PassThru = $true; WindowStyle = 'Hidden'
      RedirectStandardOutput = (Join-Path $Out 'claude.out'); RedirectStandardError = (Join-Path $Out 'claude.err')
      ArgumentList = '-p go --model sonnet --allowedTools Bash --output-format stream-json --verbose' }
    $p = Start-Process @sp
    if (-not $p.WaitForExit(120000)) { taskkill /T /F /PID $p.Id | Out-Null; Log 'TIMEOUT: killed claude' }
    $p.WaitForExit()
    Log "claude exit=$($p.ExitCode)"
  } finally {
    Remove-Item Env:ANTHROPIC_BASE_URL, Env:ANTHROPIC_API_KEY, Env:CLAUDE_CONFIG_DIR, Env:CLAUDE_CODE_USE_POWERSHELL_TOOL -ErrorAction SilentlyContinue
  }
  $cout = (Get-Content (Join-Path $Out 'claude.out') -Raw -ErrorAction SilentlyContinue) + ''
  Log ('claude stdout (first 3000): ' + $cout.Substring(0, [Math]::Min(3000, $cout.Length)))
  Log ('claude stderr: ' + ((Get-Content (Join-Path $Out 'claude.err') -Raw -ErrorAction SilentlyContinue) + ''))
  Check 'the run ended with the scripted reply' ($cout -match 'E2E-REPLY: the Bash tool ran\.')
  Check 'the Bash tool ran under Git Bash (e2e-bash-ok and a bash version came back)' ($cout -match 'e2e-bash-ok' -and $cout -match 'shell=\d')
  $reqs = @(Get-Content $reqLog -ErrorAction SilentlyContinue | ForEach-Object { try { $_ | ConvertFrom-Json -Depth 50 } catch { $null } } | Where-Object { $_ })
  $withTools = @($reqs | Where-Object { $_.body.tools -and @($_.body.tools).Count -gt 0 })
  $names = @($withTools | ForEach-Object { $_.body.tools } | ForEach-Object { $_.name } | Sort-Object -Unique)
  Log "tools offered to the model: $($names -join ',')"
  Check 'the Bash tool was offered' ($names -contains 'Bash')
  Check 'the PowerShell tool was never offered (permissions.deny)' (@($withTools).Count -gt 0 -and -not ($names -contains 'PowerShell'))
  Check 'the run never used the PowerShell tool' (-not ($cout -match '"name"\s*:\s*"PowerShell"'))
}
if ($fake) { Stop-Process -Id $fake.Id -Force -ErrorAction SilentlyContinue }

# --- revoke -------------------------------------------------------------------------------------------------------------------
Section 'invite and revoke'
# Today's exact behaviour on Windows, asserted so a package that fixes it flips the check on purpose.
$inv = Api 'createInvite' @{ name = 'Sam'; agents = @{} }
Log "createInvite -> $($inv.Status) $($inv.Body)"
# FLIPS: needs a signed-in Kith cloud account. No test cloud without secrets exists, so this needs a person (or a later
# package with a cloud test double); it then becomes a 200 with an invite link.
Check 'createInvite refuses today: sign in to a Kith account first' ($inv.Status -eq 400 -and $inv.Body -match 'sign in to a Kith account')
$people = Api 'people'
Log "people -> $($people.Status) $($people.Body)"
# FLIPS: W22b (core authority on Windows) plus retire-python P3-1 (kith-auth in Rust): people then answers 200 with a list.
Check 'people refuses today: kith-auth not found' ($people.Status -eq 400 -and $people.Body -match 'kith-auth not found')
$rev = Api 'revokePerson' @{ personId = 'sam' }
Log "revokePerson -> $($rev.Status) $($rev.Body)"
# FLIPS: same as people (W22b, P3-1); W28 then adds the mid-run revoke (a running guest run is cut off).
Check 'revokePerson refuses today: kith-auth not found' ($rev.Status -eq 400 -and $rev.Body -match 'kith-auth not found')
$after = Api 'hello'
Check 'the host still answers after the refused invite and revoke' ($after.Status -eq 200) "$($after.Status)"

# --- guests (refused today, WI6) --------------------------------------------------------------------------------------------
Section 'guest run (refused today)'
$launch = Run-Cap $Kith @('kernel', 'launch', 'prompt', 'house', '--person', 'sam') 30 'guest-launch'
# flips to a real guest run (scenario j) when W24b (guest box launch) lands. Guest scenarios will run as a standard
# (non-admin) user in their own job: k revoke mid-run (W28), l pipe squatting (W23), m two-person room turn (W24b),
# n door render (W27), o bot guest turn (W24b).
Check 'a guest launch refuses by name today' ($launch.Code -ne 0 -and ($launch.Err + $launch.Out) -match 'not on Windows yet')

# --- cost row ---------------------------------------------------------------------------------------------------------------
Section 'cost row'
$cost = Run-Cap $Kith @('kernel', 'cost') 30 'cost'
# flips to a ledger row assertion when W5a (cost) and W13 land
Check 'kernel cost refuses by name until W5a' ($cost.Code -ne 0 -and ($cost.Err + $cost.Out) -match 'not on Windows yet')

# --- stop and uninstall ------------------------------------------------------------------------------------------------------
Section 'stop and uninstall'
$stop = Run-Cap $Kith @('stop') 60 'stop'
Check 'kith stop exits 0' ($stop.Code -eq 0) ($stop.Out + $stop.Err)
Start-Sleep -Seconds 2
$left = Host-Procs
Check 'stop ended the whole tree: no program of the zip is left running' (@($left).Count -eq 0) (($left | ForEach-Object { $_.Name }) -join ',')
$down = $false
try { Invoke-RestMethod "http://127.0.0.1:$Port/health" -TimeoutSec 3 | Out-Null } catch { $down = $true }
Check 'the host no longer answers' $down
$un = Run-Cap $Kith @('uninstall') 60 'uninstall'
$runVal2 = (reg query 'HKCU\Software\Microsoft\Windows\CurrentVersion\Run' /v KithServe 2>&1) -join ' '
Check 'uninstall removes the Run key' ($un.Code -eq 0 -and $runVal2 -notmatch 'kith-servew') $runVal2

if ($script:Failed.Count) { Dump-HostLogs; Log "FAILED: $($script:Failed -join ' | ')"; exit 1 }
Log 'all checks passed'
