# CI-only (windows-probe.yml `guest2`, W20b): runs as runneradmin, makes a STANDARD local user and runs guest2-run.ps1 as that
# user against boxprobe.exe (variant A AppContainer and variant R restricted token). The admin side does only what the
# standard user cannot: create the user, install the pinned PortableGit and busybox where a box can read them, the G10
# steps (python grant, loopback exemption, firewall filters) and cleanup. Not shipped.
param(
  [Parameter(Mandatory)][string]$Boxprobe,
  [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force 'C:\probe\bin', 'C:\guest\out', $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$LogFile = Join-Path $Out 'guest2-admin.log'
function Log($m) { Write-Host $m; Add-Content -Path $LogFile -Value $m }
function Section($t) { Log ''; Log "=== $t ===" }
function Try-Do($what, [scriptblock]$b) { try { & $b } catch { Log "ERROR in ${what}: $($_ | Out-String)" } }
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

Section 'environment'
Log "date=$(Get-Date -Format o) ImageVersion=$env:ImageVersion"
Log ((Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber | Format-List | Out-String).Trim())
Log "runner user: $(whoami)"
Log "python=$((python --version 2>&1)) node=$(node --version)"
Log ("where python: " + ((where.exe python 2>&1) -join ' ; '))
Log ("where node: " + ((where.exe node 2>&1) -join ' ; '))
Log ((Get-NetFirewallProfile | Select-Object Name, Enabled | Format-Table -AutoSize | Out-String).Trim())

Section 'the standard user'
$pw = 'Kp1!' + [guid]::NewGuid().ToString('N')
Write-Host "::add-mask::$pw"
net user kithguest $pw /add /y | Out-Null
Log "kithguest is in Administrators: $(([bool](net localgroup Administrators | Select-String -SimpleMatch 'kithguest')))"

Section 'files a box can read, owned by the admin'
Copy-Item $Boxprobe 'C:\probe\bin\boxprobe.exe' -Force
Copy-Item $Boxprobe 'C:\probe\bin\kith-boxsh.exe' -Force
foreach ($f in 'g3n.js', 'guest2-run.ps1', 'corpus.txt') { Copy-Item (Join-Path $here $f) "C:\probe\bin\$f" -Force }
icacls C:\probe /grant '*S-1-15-2-1:(OI)(CI)(RX)' /Q | Out-Null

Section 'busybox-w32 (pinned, committed to the repo: no download)'
Try-Do 'busybox' {
  New-Item -ItemType Directory -Force C:\probe\busybox | Out-Null
  Copy-Item (Join-Path $here 'pinned\busybox64.exe') C:\probe\busybox\busybox.exe -Force
  $h = (Get-FileHash C:\probe\busybox\busybox.exe -Algorithm SHA256).Hash.ToLower()
  Log "busybox sha256=$h expected=9d04939789a8d458ea25ffe8fdd788ae4edc15408936da0e5c1394f02cb21a43 match=$($h -eq '9d04939789a8d458ea25ffe8fdd788ae4edc15408936da0e5c1394f02cb21a43')"
}

Section 'PortableGit (pinned release, sha256 checked), NOT the runner Git'
$pgUrl = 'https://github.com/git-for-windows/git/releases/download/v2.56.1.windows.1/PortableGit-2.56.1-64-bit.7z.exe'
$pgSha = '9dd04a608ceb5f6ab32a73709e0ecb159d211da71b697c45ff81f6dd0c6b49c7'
Try-Do 'portablegit' {
  $dl = 'C:\probe\PortableGit.7z.exe'
  Invoke-WebRequest -UseBasicParsing -TimeoutSec 300 -Uri $pgUrl -OutFile $dl
  $h = (Get-FileHash $dl -Algorithm SHA256).Hash.ToLower()
  Log "PortableGit bytes=$((Get-Item $dl).Length) sha256=$h match=$($h -eq $pgSha)"
  if ($h -eq $pgSha) {
    & $dl -y -o'C:\probe\pgit' | Out-Null
    Log "extracted: $(Test-Path 'C:\probe\pgit\usr\bin\bash.exe')"
    cmd /c mklink /J C:\probe\pgitj C:\probe\pgit | ForEach-Object { Log "$_" }
    robocopy C:\probe\pgit C:\probe\pgit-copy /E /NFL /NDL /NJH /NJS /NP | Out-Null
    Log "copy: $(Test-Path 'C:\probe\pgit-copy\usr\bin\bash.exe')"
    Log ((icacls C:\probe\pgit\usr\bin\bash.exe) -join ' ')
  }
}
$py = (Get-Command python -ErrorAction SilentlyContinue | Select-Object -First 1).Source
$nd = (Get-Command node -ErrorAction SilentlyContinue | Select-Object -First 1).Source
Set-Content 'C:\probe\tools.txt' "$py`n$nd"
foreach ($d in @((Split-Path $py), (Split-Path $nd))) { Log "$d : $((icacls $d 2>&1 | Select-String 'Users|APPLICATION PACKAGES|Everyone') -join ' | ')" }

function Run-Guest($only, $ms = 1200000) {
  Section "run guest2-run.ps1 as kithguest (-Only $only)"
  $sec = ConvertTo-SecureString $pw -AsPlainText -Force
  $cred = New-Object System.Management.Automation.PSCredential('.\kithguest', $sec)
  $o = Join-Path $Out "guest2-$only.stdout.txt"
  $e = Join-Path $Out "guest2-$only.stderr.txt"
  try {
    $p = Start-Process powershell.exe -Credential $cred -LoadUserProfile -WorkingDirectory C:\guest `
      -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'C:\probe\bin\guest2-run.ps1', '-Only', $only `
      -RedirectStandardOutput $o -RedirectStandardError $e -PassThru -WindowStyle Hidden
    if (-not $p.WaitForExit($ms)) { Log 'TIMEOUT: killing the guest run'; taskkill /T /F /PID $p.Id | Out-Null; Get-Process boxprobe -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue }
    Copy-Item 'C:\guest\out\*' $Out -Recurse -Force -ErrorAction SilentlyContinue
    Log "guest run exit code: $($p.ExitCode)"
  } catch { Log "Start-Process as kithguest FAILED: $($_ | Out-String)" }
  Log '--- guest stdout'; Log ((Get-Content $o -ErrorAction SilentlyContinue) -join "`n")
  Log '--- guest stderr'; Log ((Get-Content $e -ErrorAction SilentlyContinue) -join "`n")
}

icacls C:\guest /grant 'kithguest:(OI)(CI)(F)' /Q | Out-Null
Run-Guest 'main' 2400000

# ---- G10: the admin steps -------------------------------------------------------------------------------------------
Section 'G10: ALL APPLICATION PACKAGES read on the Python folder (variant A only; variant R reads it as Users)'
Try-Do 'python acl' {
  icacls (Split-Path $py) /grant '*S-1-15-2-1:(OI)(CI)(RX)' /Q 2>&1 | Out-Null
  Log "granted on $(Split-Path $py)"
}
Run-Guest 'python2' 300000

# ---- P-G14: persistent firewall filters (Windows Defender Firewall rules compile to persistent WFP filters) -----------
$bp = 'C:\probe\bin\boxprobe.exe'
Section 'P-G14 R: block rules keyed on the box token (-LocalUser SDDL naming the run restricting SID)'
Try-Do 'fw-r' {
  Set-NetFirewallProfile -Profile Domain, Public, Private -Enabled True
  $runSid = (& $bp capsid kith.run.net).Trim()
  Log "run SID kith.run.net = $runSid"
  $sd = "D:(A;;CC;;;$runSid)"
  $v4 = '0.0.0.0-126.255.255.255', '128.0.0.0-255.255.255.255'
  $v6 = '::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff'
  New-NetFirewallRule -DisplayName 'kithbox-r-1' -Direction Outbound -Action Block -Protocol Any -RemoteAddress ($v4 + $v6) -LocalUser $sd | Out-Null
  New-NetFirewallRule -DisplayName 'kithbox-r-2' -Direction Outbound -Action Block -Protocol TCP -RemoteAddress 127.0.0.1 -RemotePort '1-18079', '18081-65535' -LocalUser $sd | Out-Null
  New-NetFirewallRule -DisplayName 'kithbox-r-3' -Direction Outbound -Action Block -Protocol UDP -RemoteAddress 127.0.0.1 -LocalUser $sd | Out-Null
  Log ((Get-NetFirewallRule -DisplayName 'kithbox-r-*' | Select-Object DisplayName, Enabled, Action, Direction | Format-Table -AutoSize | Out-String).Trim())
  netsh wfp show filters file=C:\guest\out\wfp-r.xml | Out-Null
  Log ("WFP filters containing kithbox-r: " + ((Select-String -Path C:\guest\out\wfp-r.xml -Pattern 'kithbox-r' -SimpleMatch -Context 0, 40 | Select-Object -First 3 | Out-String)))
}
Run-Guest 'net-r' 300000
Try-Do 'fw-r cleanup' { Remove-NetFirewallRule -DisplayName 'kithbox-r-*' -ErrorAction SilentlyContinue }

Section 'P-G14 A (N2): one per-install AppContainer SID, loopback exemption + block rules keyed on the package'
Try-Do 'fw-a' {
  $acSid = (& $bp acsid n2).Trim()
  Log "AppContainer SID of kithprobe.n2 = $acSid"
  Log ("LoopbackExempt -a: " + ((CheckNetIsolation.exe LoopbackExempt -a "-p=$acSid" 2>&1) -join ' '))
  $v4 = '0.0.0.0-126.255.255.255', '128.0.0.0-255.255.255.255'
  $v6 = '::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff'
  New-NetFirewallRule -DisplayName 'kithbox-a-1' -Direction Outbound -Action Block -Protocol Any -RemoteAddress ($v4 + $v6) -Package $acSid | Out-Null
  New-NetFirewallRule -DisplayName 'kithbox-a-2' -Direction Outbound -Action Block -Protocol TCP -RemoteAddress 127.0.0.1 -RemotePort '1-18079', '18081-65535' -Package $acSid | Out-Null
  New-NetFirewallRule -DisplayName 'kithbox-a-3' -Direction Outbound -Action Block -Protocol UDP -RemoteAddress 127.0.0.1 -Package $acSid | Out-Null
  Log ((Get-NetFirewallRule -DisplayName 'kithbox-a-*' | Select-Object DisplayName, Enabled, Action | Format-Table -AutoSize | Out-String).Trim())
  netsh wfp show filters file=C:\guest\out\wfp-a.xml | Out-Null
  Log ("WFP filters containing kithbox-a: " + ((Select-String -Path C:\guest\out\wfp-a.xml -Pattern 'kithbox-a' -SimpleMatch -Context 0, 40 | Select-Object -First 3 | Out-String)))
}
Run-Guest 'net-a' 300000
Try-Do 'fw-a cleanup' {
  Remove-NetFirewallRule -DisplayName 'kithbox-a-*' -ErrorAction SilentlyContinue
  CheckNetIsolation.exe LoopbackExempt -c 2>&1 | Out-Null
}

Section 'cleanup: leftovers, the user'
Try-Do 'cleanup' {
  Get-CimInstance Win32_Process -Filter "Name='PING.EXE'" | Where-Object { $_.CommandLine -match 'ping(\.exe)? -n (4[1-9]|[5-9]\d|[1-3]\d\d) ' } | ForEach-Object {
    Log "leftover: pid=$($_.ProcessId) $($_.CommandLine)"; Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
  }
  sc.exe stop kithprobesvc59 2>&1 | Out-Null; sc.exe delete kithprobesvc59 2>&1 | Out-Null
  schtasks.exe /delete /tn kithprobe58 /f 2>&1 | Out-Null
  bitsadmin.exe /reset 2>&1 | Out-Null
  Copy-Item 'C:\guest\out\*' $Out -Recurse -Force -ErrorAction SilentlyContinue
  net user kithguest /delete 2>&1 | Out-Null
}
Section 'done'
exit 0
