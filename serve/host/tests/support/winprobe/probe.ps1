# CI-only probe (workflow windows-probe.yml): what real Claude Code does on Windows. Not shipped.
# Installs Claude Code through npm and through install.ps1, then runs `claude -p` against kith-fakemodel with a dummy
# key and a throwaway CLAUDE_CONFIG_DIR, and logs what happened. Every step is wrapped: one failure never stops the rest.
param(
  [Parameter(Mandatory)][string]$Fakemodel,
  [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = 'Continue'
$Root = 'C:\probe'
New-Item -ItemType Directory -Force $Root, "$Root\bin", "$Root\cwd", $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$LogFile = Join-Path $Out 'probe.log'
function Log($m) { Write-Host $m; Add-Content -Path $LogFile -Value $m }
function Section($t) { Log ''; Log "=== $t ===" }
function Try-Do($what, [scriptblock]$b) {
  try { & $b } catch { Log "ERROR in ${what}: $($_ | Out-String)" }
}

Section 'environment'
Log "date=$(Get-Date -Format o)"
Log "ImageOS=$env:ImageOS ImageVersion=$env:ImageVersion"
Log "PSVersion=$($PSVersionTable.PSVersion)"
Log ((Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber | Format-List | Out-String).Trim())
Log "node=$(node --version) npm=$(npm --version)"
Log "git=$(git --version)"
Log ("where git: " + ((where.exe git 2>&1) -join ' ; '))
Log ("where bash: " + ((where.exe bash 2>&1) -join ' ; '))
Log "USERPROFILE=$env:USERPROFILE"

# --- test scaffolding --------------------------------------------------------------------------------------------
[IO.File]::WriteAllText("$Root\bin\kithstub", "#!/bin/sh`necho `"STUB_RAN args=`$* zero=`$0`"`n")

# Records how a hook was launched: the quoting form it was given, the event, its stdin and its parent process chain.
@'
param([string]$Form)
$stdin = [Console]::In.ReadToEnd()
$ev = ''
try { $ev = ($stdin | ConvertFrom-Json).hook_event_name } catch { }
$chain = @(); $id = $PID
for ($i = 0; $i -lt 8 -and $id; $i++) {
  $p = Get-CimInstance Win32_Process -Filter "ProcessId=$id"
  if (-not $p) { break }
  $chain += ('{0}:{1}:{2}' -f $p.ProcessId, $p.Name, $p.CommandLine)
  $id = $p.ParentProcessId
}
$line = "FORM=$Form EVENT=$ev ARGS=" + ($args -join '|') + ' CHAIN=' + ($chain -join ' <- ')
Add-Content "C:/probe/forms/$Form-$ev.txt" $line
'@ | Set-Content "$Root\rec.ps1"

$ps5 = 'C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe'
$ps5b = 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
$forms = [ordered]@{
  fwd    = "`"$ps5`" -NoProfile -File `"C:/probe/rec.ps1`" `"fwd`""
  back   = "`"$ps5b`" -NoProfile -File `"C:\probe\rec.ps1`" `"back`""
  single = "'$ps5' -NoProfile -File 'C:/probe/rec.ps1' 'single'"
  spaced = "`"C:/Program Files/PowerShell/7/pwsh.exe`" -NoProfile -File `"C:/probe/rec.ps1`" `"spaced`""
  bare   = "powershell -NoProfile -File `"C:/probe/rec.ps1`" `"bare`""
  unqfwd = "$ps5 -NoProfile -File C:/probe/rec.ps1 unqfwd"
  unqback = "$ps5b -NoProfile -File C:\probe\rec.ps1 unqback"
}
# One command that reads differently in bash ($0 -> bash), cmd (%ComSpec% expands) and PowerShell (neither $0 nor
# %ComSpec% survive the same way), so the file it writes names the shell that ran it.
function ShellProbe($tag) { "echo SHELLPROBE `$0 %ComSpec% `$BASH_VERSION `$PSVersionTable.PSVersion > C:/probe/forms/shell-$tag.txt" }
function Hook($cmd) { @{ type = 'command'; command = $cmd } }

function Make-Settings($extra) {
  $pre = @(Hook (ShellProbe 'PreToolUse')) + @($forms.Values | ForEach-Object { Hook $_ })
  $s = @{
    hooks = @{
      SessionStart = @(@{ hooks = @((Hook (ShellProbe 'SessionStart')), (Hook $forms.fwd)) })
      PreToolUse   = @(@{ matcher = 'Bash'; hooks = $pre })
      PostToolUse  = @(@{ matcher = 'Bash'; hooks = @((Hook (ShellProbe 'PostToolUse')), (Hook $forms.fwd)) })
    }
    statusLine = @{ type = 'command'; command = (ShellProbe 'statusline') }
  }
  foreach ($k in $extra.Keys) { $s[$k] = $extra[$k] }
  $s | ConvertTo-Json -Depth 12
}

$bashCmd = 'echo BASHPROBE; echo "BASH_VERSION=$BASH_VERSION"; echo "BASH=$BASH"; echo "SHELL=$SHELL"; echo "MSYSTEM=$MSYSTEM"; ' +
  'command -v bash; cygpath -w "$(command -v bash)"; uname -a; pwd; command -v kithstub; kithstub a1 "a 2"; echo "stub_rc=$?"; echo "PATH=$PATH"'
@{
  turns    = @(@{ text = 'Probing.'; tool_use = @(@{ name = 'Bash'; input = @{ command = $bashCmd; description = 'probe' } }) })
  fallback = 'Done.'
} | ConvertTo-Json -Depth 10 | Set-Content "$Root\script.json"

# --- one claude -p run ---------------------------------------------------------------------------------------------
function Run-Claude($Install, $Name, $Claude, $Settings, $EnvExtra) {
  Section "run $Install/$Name"
  $tag = "$Install-$Name"
  $cfg = "$Root\cfg-$tag"
  Remove-Item -Recurse -Force "$Root\forms" -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force $cfg, "$Root\forms" | Out-Null
  Set-Content "$cfg\settings.json" $Settings
  Log "settings.json:`n$Settings"

  $fake = Start-Process $Fakemodel -ArgumentList '--script', "$Root\script.json", '--log', "$Out\$tag.requests.jsonl" `
    -RedirectStandardOutput "$Out\$tag.fake.out" -RedirectStandardError "$Out\$tag.fake.err" -PassThru -WindowStyle Hidden
  $addr = $null
  for ($i = 0; $i -lt 40 -and -not $addr; $i++) {
    Start-Sleep -Milliseconds 500
    $addr = (Get-Content "$Out\$tag.fake.out" -ErrorAction SilentlyContinue | Select-Object -First 1)
  }
  Log "fakemodel addr=$addr"
  if (-not $addr) { Stop-Process -Id $fake.Id -Force -ErrorAction SilentlyContinue; return }

  $saved = @{}
  $vars = @{
    ANTHROPIC_BASE_URL = "http://$addr"; ANTHROPIC_API_KEY = 'dummy'; CLAUDE_CONFIG_DIR = $cfg
    DISABLE_AUTOUPDATER = '1'; CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'; DISABLE_TELEMETRY = '1'
  } + $EnvExtra
  foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
  $env:PATH = "$Root\bin;$env:PATH"
  try {
    $argLine = '-p go --model sonnet --allowedTools Bash --output-format stream-json --verbose --include-hook-events'
    for ($attempt = 1; $attempt -le 2; $attempt++) {
      "" | Set-Content "$Root\empty.txt"
      $p = Start-Process $Claude -ArgumentList $argLine -WorkingDirectory "$Root\cwd" -RedirectStandardInput "$Root\empty.txt" `
        -RedirectStandardOutput "$Out\$tag.claude.out" -RedirectStandardError "$Out\$tag.claude.err" -PassThru -WindowStyle Hidden
      if (-not $p.WaitForExit(90000)) { Log 'TIMEOUT: killing claude'; taskkill /T /F /PID $p.Id | Out-Null }
      $p.WaitForExit()
      Log "attempt $attempt exit=$($p.ExitCode) argline=$argLine"
      if ($p.ExitCode -eq 0) { break }
      # Older versions lack --include-hook-events; retry without it.
      $argLine = '-p go --model sonnet --allowedTools Bash --output-format stream-json --verbose'
    }
  } finally {
    foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
    Stop-Process -Id $fake.Id -Force -ErrorAction SilentlyContinue
  }

  Log '--- claude stderr'; Log ((Get-Content "$Out\$tag.claude.err" -ErrorAction SilentlyContinue) -join "`n")
  Log '--- claude stdout (stream-json, truncated lines)'
  Get-Content "$Out\$tag.claude.out" -ErrorAction SilentlyContinue | ForEach-Object { Log ($_.Substring(0, [Math]::Min(1500, $_.Length))) }

  Log '--- hook and status line files'
  New-Item -ItemType Directory -Force "$Out\$tag.forms" | Out-Null
  Get-ChildItem "$Root\forms" -ErrorAction SilentlyContinue | ForEach-Object {
    Copy-Item $_.FullName "$Out\$tag.forms\"
    Log "[$($_.Name)] $((Get-Content $_.FullName -Raw).Trim())"
  }
  if (-not (Get-ChildItem "$Root\forms" -ErrorAction SilentlyContinue)) { Log '(no hook or status line file written)' }

  Log '--- config dir projects folder'
  Get-ChildItem "$cfg\projects" -ErrorAction SilentlyContinue | ForEach-Object { Log "projects\$($_.Name)" }
  if (Test-Path "$cfg\projects") { Copy-Item "$cfg\projects" "$Out\$tag.projects" -Recurse -ErrorAction SilentlyContinue }
  Log "cwd was: $Root\cwd"

  Log '--- requests seen by fakemodel'
  $f = "$Out\$tag.requests.jsonl"
  if (-not (Test-Path $f)) { Log '(no request log)'; return }
  $reqs = @(Get-Content $f | ForEach-Object { try { $_ | ConvertFrom-Json -Depth 50 } catch { $null } } | Where-Object { $_ })
  foreach ($r in $reqs) { Log ("{0} {1} model={2} tools={3}" -f $r.method, $r.path, $r.body.model, @($r.body.tools).Count) }
  $first = $reqs | Where-Object { $_.body.tools -and @($_.body.tools).Count -gt 0 } | Select-Object -First 1
  if ($first) {
    $names = @($first.body.tools | ForEach-Object { $_.name })
    Log "TOOLS($tag)=$($names -join ',')"
    Log "PowerShell tool in request: $($names -contains 'PowerShell')"
    Log "tool names matching shell: $(($names | Where-Object { $_ -match 'shell|power' }) -join ',')"
  } else { Log "TOOLS($tag)=(no request carried tools)" }
  foreach ($r in $reqs) {
    $last = @($r.body.messages) | Select-Object -Last 1
    if ($last.content -is [array]) {
      foreach ($c in $last.content) {
        if ($c.type -eq 'tool_result') { Log "TOOL_RESULT($tag) is_error=$($c.is_error):"; Log (($c.content | ConvertTo-Json -Depth 5)) }
      }
    }
  }
}

function Probe-Install($Install, $Claude, $PathAdd) {
  Section "install $Install : locations"
  $env:PATH = (($script:BasePath + $PathAdd) | Where-Object { $_ }) -join ';'
  Log "PATH for this probe: $env:PATH"
  Log ("where.exe claude: " + ((where.exe claude 2>&1) -join ' ; '))
  Try-Do 'Get-Command' { Log ((Get-Command claude -All -ErrorAction SilentlyContinue | Select-Object Name, CommandType, Source | Format-Table -AutoSize | Out-String).Trim()) }
  Log "claude path used: $Claude"
  Try-Do 'version' { Log ("claude --version: " + ((& $Claude --version 2>&1) -join ' ')) }
  Try-Do 'sandbox key grep' {
    $targets = @($Claude)
    if ($Install -eq 'npm') {
      $pkg = Join-Path (npm prefix -g) 'node_modules\@anthropic-ai\claude-code'
      $targets = @(Get-ChildItem $pkg -Recurse -Include claude.exe, cli.js, cli.mjs -ErrorAction SilentlyContinue | ForEach-Object FullName)
    }
    foreach ($t in $targets) {
      Log "grep failIfUnavailable in $t :"
      Log ((& 'C:\Program Files\Git\usr\bin\grep.exe' -a -o -m5 'failIfUnavailable' $t 2>&1 | Select-Object -First 5) -join "`n")
    }
  }

  $base = Make-Settings @{}
  Try-Do 'run base' { Run-Claude $Install 'base' $Claude $base @{} }
  Try-Do 'run pstool' { Run-Claude $Install 'pstool' $Claude $base @{ CLAUDE_CODE_USE_POWERSHELL_TOOL = '1' } }
  Try-Do 'run deny' {
    Run-Claude $Install 'deny' $Claude (Make-Settings @{ permissions = @{ deny = @('PowerShell') } }) @{ CLAUDE_CODE_USE_POWERSHELL_TOOL = '1' }
  }
  Try-Do 'run sandbox' {
    Run-Claude $Install 'sandbox' $Claude (Make-Settings @{ sandbox = @{ enabled = $true; failIfUnavailable = $true } }) @{}
  }
}

# PATH without any claude install dir, so each install is probed on its own.
$npmPrefix = (npm prefix -g).Trim()
$localBin = Join-Path $env:USERPROFILE '.local\bin'
$script:BasePath = @($env:PATH -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ne $npmPrefix.TrimEnd('\')) -and ($_.TrimEnd('\') -ne $localBin) })
Log "npm prefix -g = $npmPrefix"

# --- npm ---------------------------------------------------------------------------------------------------------
Section 'install via npm'
Try-Do 'npm install' {
  npm install -g '@anthropic-ai/claude-code' 2>&1 | ForEach-Object { Log "$_" }
  Log ((Get-ChildItem $npmPrefix -Filter 'claude*' | Select-Object Name, Length | Format-Table -AutoSize | Out-String).Trim())
  $pkg = Join-Path $npmPrefix 'node_modules\@anthropic-ai\claude-code'
  Log "package dir: $pkg"
  Log ((Get-Content "$pkg\package.json" -Raw | ConvertFrom-Json | Select-Object name, version, bin | ConvertTo-Json -Depth 5))
  Log ((Get-ChildItem $pkg | Select-Object Name | Format-Table -AutoSize | Out-String).Trim())
  Log "claude.cmd content:`n$(Get-Content "$npmPrefix\claude.cmd" -Raw)"
  Log ((Get-ChildItem $pkg -Recurse -Include 'claude*.exe' -ErrorAction SilentlyContinue | Select-Object FullName, Length | Format-Table -AutoSize | Out-String).Trim())
}
Try-Do 'npm probe' { Probe-Install 'npm' "$npmPrefix\claude.cmd" @($npmPrefix) }
Try-Do 'npm uninstall' { npm uninstall -g '@anthropic-ai/claude-code' 2>&1 | ForEach-Object { Log "$_" } }

# --- native install.ps1 ------------------------------------------------------------------------------------------
Section 'install via install.ps1'
$userPathBefore = [Environment]::GetEnvironmentVariable('PATH', 'User')
Try-Do 'install.ps1' {
  Invoke-RestMethod https://claude.ai/install.ps1 | Invoke-Expression 2>&1 | ForEach-Object { Log "$_" }
}
$userPathAfter = [Environment]::GetEnvironmentVariable('PATH', 'User')
Log "user PATH changed by install.ps1: $($userPathBefore -ne $userPathAfter)"
Log "user PATH before: $userPathBefore"
Log "user PATH after: $userPathAfter"
Try-Do 'native locations' {
  Log ((Get-ChildItem "$env:USERPROFILE\.local" -Recurse -ErrorAction SilentlyContinue | Where-Object { $_.Name -like 'claude*' } |
        Select-Object FullName, Length | Format-Table -AutoSize -Wrap | Out-String).Trim())
  Log ((Get-ChildItem "$env:USERPROFILE\.claude" -ErrorAction SilentlyContinue | Select-Object Name | Format-Table | Out-String).Trim())
}
Try-Do 'native probe' { Probe-Install 'native' "$localBin\claude.exe" @($localBin) }

Section 'done'
exit 0
