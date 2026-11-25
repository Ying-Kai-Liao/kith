# CI-only (windows-probe.yml `guest`): the Claude Code half of the guest probe. Real Claude Code against kith-fakemodel
# (dummy key). P-G1 (CLAUDE_CODE_GIT_BASH_PATH and hooks), P-G9 (permission-rule path forms), P-G12 (Windows sandbox).
# Runs as the runner user: what Claude Code does with these settings does not depend on the account's privileges (not
# measured as the standard user). Every step is wrapped; one failure never stops the rest.
param(
  [Parameter(Mandatory)][string]$Fakemodel,
  [Parameter(Mandatory)][string]$Boxprobe,
  [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = 'Continue'
$Root = 'C:\probe'
New-Item -ItemType Directory -Force $Root, "$Root\bin", "$Root\cwd", "$Root\secret", "$Root\ok", $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$LogFile = Join-Path $Out 'guest-claude.log'
function Log($m) { Write-Host $m; Add-Content -Path $LogFile -Value $m }
function Section($t) { Log ''; Log "=== $t ===" }
function Try-Do($what, [scriptblock]$b) { try { & $b } catch { Log "ERROR in ${what}: $($_ | Out-String)" } }

Section 'install Claude Code (npm)'
Try-Do 'npm install' {
  npm install -g '@anthropic-ai/claude-code' 2>&1 | Select-Object -Last 5 | ForEach-Object { Log "$_" }
}
$npmPrefix = (npm prefix -g).Trim()
$claude = "$npmPrefix\claude.cmd"
$env:PATH = "$npmPrefix;$env:PATH"
Log "claude: $claude"
Log ("version: " + ((& $claude --version 2>&1) -join ' '))

Copy-Item $Boxprobe "$Root\bin\kith-boxsh.exe" -Force
Copy-Item $Boxprobe "$Root\bin\notbash.exe" -Force
Set-Content "$Root\secret\s.txt" 'SECRETDATA'
Set-Content "$Root\ok\o.txt" 'OKDATA'
$realBash = 'C:\Program Files\Git\bin\bash.exe'

function Hook($cmd) { @{ type = 'command'; command = $cmd } }

# One claude -p run against a script of tool calls. Returns a hashtable: tool results in order, the stream, the requests.
function Run-Claude($tag, $settings, $envExtra, $turns, $allowed) {
  Section "run $tag"
  $cfg = "$Root\cfg-$tag"
  New-Item -ItemType Directory -Force $cfg | Out-Null
  ($settings | ConvertTo-Json -Depth 12) | Set-Content "$cfg\settings.json"
  Log "settings.json: $(Get-Content "$cfg\settings.json" -Raw)"
  @{ turns = $turns; fallback = 'Done.' } | ConvertTo-Json -Depth 12 | Set-Content "$Root\script-$tag.json"
  $fake = Start-Process $Fakemodel -ArgumentList '--script', "$Root\script-$tag.json", '--log', "$Out\$tag.requests.jsonl" `
    -RedirectStandardOutput "$Out\$tag.fake.out" -RedirectStandardError "$Out\$tag.fake.err" -PassThru -WindowStyle Hidden
  $addr = $null
  for ($i = 0; $i -lt 40 -and -not $addr; $i++) { Start-Sleep -Milliseconds 500; $addr = (Get-Content "$Out\$tag.fake.out" -ErrorAction SilentlyContinue | Select-Object -First 1) }
  if (-not $addr) { Log 'fakemodel did not start'; Stop-Process -Id $fake.Id -Force -ErrorAction SilentlyContinue; return }
  $saved = @{}
  $vars = @{ ANTHROPIC_BASE_URL = "http://$addr"; ANTHROPIC_API_KEY = 'dummy'; CLAUDE_CONFIG_DIR = $cfg
    DISABLE_AUTOUPDATER = '1'; CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'; DISABLE_TELEMETRY = '1' } + $envExtra
  foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
  $exit = $null
  try {
    "" | Set-Content "$Root\empty.txt"
    $p = Start-Process $claude -ArgumentList "-p go --model sonnet --allowedTools $allowed --output-format stream-json --verbose --include-hook-events" `
      -WorkingDirectory "$Root\cwd" -RedirectStandardInput "$Root\empty.txt" -RedirectStandardOutput "$Out\$tag.claude.out" `
      -RedirectStandardError "$Out\$tag.claude.err" -PassThru -WindowStyle Hidden
    if (-not $p.WaitForExit(100000)) { Log 'TIMEOUT: killing claude'; taskkill /T /F /PID $p.Id | Out-Null }
    $p.WaitForExit(); $exit = $p.ExitCode
  } finally {
    foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
    Stop-Process -Id $fake.Id -Force -ErrorAction SilentlyContinue
  }
  Log "exit=$exit"
  Log '--- claude stderr'; Log ((Get-Content "$Out\$tag.claude.err" -ErrorAction SilentlyContinue) -join "`n")
  Log '--- claude stdout (stream-json, lines cut at 900 chars)'
  Get-Content "$Out\$tag.claude.out" -ErrorAction SilentlyContinue | ForEach-Object { Log ($_.Substring(0, [Math]::Min(900, $_.Length))) }
  $f = "$Out\$tag.requests.jsonl"
  if (-not (Test-Path $f)) { Log '(no request log)'; return }
  $reqs = @(Get-Content $f | ForEach-Object { try { $_ | ConvertFrom-Json -Depth 50 } catch { $null } } | Where-Object { $_ })
  $last = $null
  foreach ($r in $reqs) { if ($r.body.messages) { $last = $r } }
  $n = 0
  if ($last) {
    foreach ($m in @($last.body.messages)) {
      if ($m.content -is [array]) {
        foreach ($c in $m.content) {
          if ($c.type -eq 'tool_result') {
            $n++
            $txt = if ($c.content -is [array]) { ($c.content | ForEach-Object { $_.text }) -join ' ' } else { "$($c.content)" }
            Log ("TOOL_RESULT[$n]($tag) is_error=$($c.is_error): " + $txt.Substring(0, [Math]::Min(300, $txt.Length)))
          }
        }
      }
    }
  }
  if ($n -eq 0) { Log "TOOL_RESULT($tag): none seen" }
}

$bashCmd = 'echo BASH_TOOL_RAN; echo "BASH_VERSION=$BASH_VERSION"; echo "arg0=$0"; pwd'
$bashTurn = @(@{ text = 'Probing.'; tool_use = @(@{ name = 'Bash'; input = @{ command = $bashCmd; description = 'probe' } }) })
$hookSettings = @{ hooks = @{ PreToolUse = @(@{ matcher = 'Bash'; hooks = @((Hook 'echo HOOK_RAN_PRETOOL')) }); SessionStart = @(@{ hooks = @((Hook 'echo HOOK_RAN_SESSIONSTART')) }) } }

# --- P-G1 ------------------------------------------------------------------------------------------------------------
Section 'P-G1 CLAUDE_CODE_GIT_BASH_PATH pointing at a non-Git exe (boxprobe.exe used as a logging shell stand-in)'
$env:KITH_REAL_BASH = $realBash
$g1 = [ordered]@{
  control      = $null
  boxsh        = "$Root\bin\kith-boxsh.exe"
  notbash      = "$Root\bin\notbash.exe"
  missing      = "$Root\bin\missing.exe"
  realexplicit = $realBash
}
foreach ($k in $g1.Keys) {
  $env:KITH_SH_LOG = "$Out\sh-$k.log"
  Remove-Item $env:KITH_SH_LOG -ErrorAction SilentlyContinue
  $ex = @{ KITH_SH_LOG = $env:KITH_SH_LOG; KITH_REAL_BASH = $realBash }
  if ($g1[$k]) { $ex['CLAUDE_CODE_GIT_BASH_PATH'] = $g1[$k] }
  Try-Do "g1 $k" { Run-Claude "g1-$k" $hookSettings $ex $bashTurn 'Bash' }
  Log "--- how the stand-in shell was called ($k): $env:KITH_SH_LOG"
  Log ((Get-Content $env:KITH_SH_LOG -ErrorAction SilentlyContinue | ForEach-Object { $_.Substring(0, [Math]::Min(700, $_.Length)) }) -join "`n")
}

# --- P-G9 ------------------------------------------------------------------------------------------------------------
Section 'P-G9 which permission-rule path forms match the file tools on Windows'
$forms = [ordered]@{
  T1_backslash   = 'C:\probe\secret\s.txt'
  T2_forward     = 'C:/probe/secret/s.txt'
  T3_msys        = '/c/probe/secret/s.txt'
  T4_lower_case  = 'c:\probe\SECRET\S.TXT'
  T5_dotdot      = 'C:\probe\ok\..\secret\s.txt'
}
$reads = @($forms.Values | ForEach-Object { @{ name = 'Read'; input = @{ file_path = $_ } } }) + @(@{ name = 'Read'; input = @{ file_path = 'C:\probe\ok\o.txt' } })
$readTurn = @(@{ text = 'Reading.'; tool_use = $reads })
Log ('tool path forms, in order: ' + (($forms.Keys + 'ok_file_must_be_readable') -join ', '))
$rules = [ordered]@{
  R1_slash_slash_c   = 'Read(//c/probe/secret/**)'
  R2_backslash       = 'Read(C:\probe\secret\**)'
  R3_forward         = 'Read(C:/probe/secret/**)'
  R4_slash_slash_C_colon = 'Read(//C:/probe/secret/**)'
  R5_msys_single     = 'Read(/c/probe/secret/**)'
}
foreach ($k in $rules.Keys) {
  Log "RULE $k = $($rules[$k])"
  Try-Do "g9 $k" { Run-Claude "g9-$k" @{ permissions = @{ deny = @($rules[$k]) } } @{} $readTurn 'Read' }
}
Log 'CONTROL: no deny rule at all (all five secret reads should return SECRETDATA):'
Try-Do 'g9 none' { Run-Claude 'g9-none' @{} @{} $readTurn 'Read' }

# --- P-G12 -----------------------------------------------------------------------------------------------------------
Section 'P-G12 can Claude Code''s own Windows sandbox be switched on, and which keys does it honour'
$bashOutside = @(@{ text = 'Writing.'; tool_use = @(@{ name = 'Bash'; input = @{ command = 'echo x > C:/probe/outside-cwd.txt; echo write_rc=$?; echo SANDBOX_ENV=$SANDBOX_RUNTIME'; description = 'probe' } }) })
Try-Do 'g12 strings' {
  $grep = 'C:\Program Files\Git\usr\bin\grep.exe'
  $bins = @(Get-ChildItem "$npmPrefix\node_modules\@anthropic-ai\claude-code" -Recurse -Filter claude.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName)
  foreach ($b in $bins) {
    Log "binary: $b"
    Log 'env/setting names containing SANDBOX:'
    Log ((& $grep -a -o -E '[A-Z][A-Z0-9_]*SANDBOX[A-Z0-9_]*' $b | Sort-Object -Unique | Select-Object -First 60) -join ' ')
    Log 'sandbox settings keys present:'
    Log ((& $grep -a -o -E 'failIfUnavailable|excludedCommands|allowUnsandboxedCommands|autoAllowBashIfSandboxed|enableWeakerNestedSandbox|enableWeakerNetworkIsolation|allowLocalBinding|allowUnixSockets|allowedDomains|deniedDomains|allowWrite|denyWrite|allowRead|denyRead|allowManagedDomainsOnly|ignoreViolations' $b | Sort-Object -Unique) -join ' ')
    Log 'the gate message:'
    Log ((& $grep -a -o -E '.{0,100}Windows sandbox is not active.{0,100}' $b | Select-Object -First 2) -join "`n")
    Log 'sandbox-ish strings with windows/appcontainer:'
    Log ((& $grep -a -o -E '[A-Za-z_ ]{0,40}[Aa]pp[Cc]ontainer[A-Za-z_ ]{0,40}' $b | Sort-Object -Unique | Select-Object -First 20) -join ' | ')
  }
}
Try-Do 'g12 enabled' { Run-Claude 'g12-enabled' @{ sandbox = @{ enabled = $true } } @{} $bashOutside 'Bash' }
Try-Do 'g12 enabled-fail' { Run-Claude 'g12-fail' @{ sandbox = @{ enabled = $true; failIfUnavailable = $true } } @{} $bashOutside 'Bash' }
Try-Do 'g12 keys' {
  Run-Claude 'g12-keys' @{ sandbox = @{ enabled = $true; failIfUnavailable = $true; autoAllowBashIfSandboxed = $true
      filesystem = @{ allowWrite = @('C:\probe\cwd'); denyRead = @('C:\probe\secret') }
      network = @{ allowedDomains = @('example.com'); allowLocalBinding = $true }; excludedCommands = @('git') } } @{} $bashOutside 'Bash'
}
Try-Do 'g12 env candidates' {
  $cands = @('CLAUDE_CODE_ENABLE_WINDOWS_SANDBOX', 'CLAUDE_CODE_WINDOWS_SANDBOX', 'CLAUDE_CODE_SANDBOX', 'CLAUDE_CODE_ENABLE_SANDBOX')
  foreach ($c in $cands) {
    Log "env candidate $c=1:"
    Run-Claude "g12-env-$c" @{ sandbox = @{ enabled = $true; failIfUnavailable = $true } } @{ $c = '1' } $bashOutside 'Bash'
  }
}
Section 'done'
exit 0
