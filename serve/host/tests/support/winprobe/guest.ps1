# CI-only (windows-probe.yml `guest`): runs as runneradmin, makes a STANDARD local user, and runs guest-run.ps1 as that user
# against boxprobe.exe. The admin side only does what the standard user cannot: create the user, put tools where an
# AppContainer can read them, grant the loopback exemption (to measure it) and clean up. Not shipped.
param(
  [Parameter(Mandatory)][string]$Boxprobe,
  [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force 'C:\probe\bin', 'C:\guest\w', 'C:\guest\r', 'C:\guest\out', $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$LogFile = Join-Path $Out 'guest-admin.log'
function Log($m) { Write-Host $m; Add-Content -Path $LogFile -Value $m }
function Section($t) { Log ''; Log "=== $t ===" }
function Try-Do($what, [scriptblock]$b) { try { & $b } catch { Log "ERROR in ${what}: $($_ | Out-String)" } }
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

Section 'environment'
Log "date=$(Get-Date -Format o) ImageVersion=$env:ImageVersion"
Log ((Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber | Format-List | Out-String).Trim())
Log "runner user: $(whoami)"
Log ((whoami /groups | Select-String 'Administrators|High Mandatory') -join "`n")
Log "git=$(git --version) node=$(node --version) python=$((python --version 2>&1))"
Log ("where python: " + ((where.exe python 2>&1) -join ' ; '))
Log ("where node: " + ((where.exe node 2>&1) -join ' ; '))

Section 'the standard user'
$pw = 'Kp1!' + [guid]::NewGuid().ToString('N')
Write-Host "::add-mask::$pw"
net user kithguest $pw /add /y | Out-Null
Log ((net user kithguest | Select-String 'User name|Local Group|Global Group|Account active') -join "`n")
Log "kithguest is in Administrators: $(([bool](net localgroup Administrators | Select-String -SimpleMatch 'kithguest')))"

Section 'places an AppContainer can read (ALL APPLICATION PACKAGES = S-1-15-2-1), owned by the admin'
Copy-Item $Boxprobe 'C:\probe\bin\boxprobe.exe' -Force
Copy-Item $Boxprobe 'C:\probe\bin\kith-boxsh.exe' -Force
Copy-Item (Join-Path $here 'g3-inbox.ps1') 'C:\probe\bin\g3-inbox.ps1' -Force
Copy-Item (Join-Path $here 'guest-run.ps1') 'C:\probe\bin\guest-run.ps1' -Force
icacls C:\probe /grant '*S-1-15-2-1:(OI)(CI)(RX)' /Q | Out-Null
icacls C:\guest /grant 'kithguest:(OI)(CI)(F)' /Q | Out-Null
Log ((icacls C:\probe) -join "`n")
Log 'Do the tool folders already grant ALL APPLICATION PACKAGES (so a box can run them without any admin step)?'
foreach ($d in @('C:\Program Files\Git', 'C:\Windows\System32\cmd.exe', 'C:\Program Files\nodejs')) {
  Log "$d : $((icacls $d 2>&1 | Select-String 'APPLICATION PACKAGES') -join ' | ')"
}
$py = (Get-Command python -ErrorAction SilentlyContinue | Select-Object -First 1).Source
if ($py) { Log "python $py : $((icacls (Split-Path $py) 2>&1 | Select-String 'APPLICATION PACKAGES') -join ' | ')" }

Section 'install chrome-headless-shell and ffmpeg where a box can read them (P-G11)'
Try-Do 'chrome' {
  New-Item -ItemType Directory -Force C:\probe\chrome | Out-Null
  Push-Location C:\probe
  npx --yes '@puppeteer/browsers' install chrome-headless-shell@stable --path C:\probe\chrome 2>&1 | ForEach-Object { Log "$_" }
  Pop-Location
  $exe = Get-ChildItem C:\probe\chrome -Recurse -Filter chrome-headless-shell.exe -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($exe) { Set-Content 'C:\probe\chrome-path.txt' $exe.FullName; Log "chrome-headless-shell: $($exe.FullName)" } else { Log 'chrome-headless-shell not installed' }
}
Try-Do 'ffmpeg' {
  $f = (Get-Command ffmpeg -ErrorAction SilentlyContinue | Select-Object -First 1).Source
  if (-not $f) {
    choco install ffmpeg -y --no-progress 2>&1 | Select-Object -Last 5 | ForEach-Object { Log "$_" }
    $f = (Get-Command ffmpeg -ErrorAction SilentlyContinue | Select-Object -First 1).Source
  }
  Log "ffmpeg found at: $f"
  $real = Get-ChildItem 'C:\ProgramData\chocolatey\lib', 'C:\ffmpeg', (Split-Path $f -ErrorAction SilentlyContinue) -Recurse -Filter ffmpeg.exe -ErrorAction SilentlyContinue |
    Where-Object { $_.Length -gt 5MB } | Select-Object -First 1
  if (-not $real -and $f -and (Get-Item $f).Length -gt 5MB) { $real = Get-Item $f }
  if ($real) { New-Item -ItemType Directory -Force C:\probe\ffmpeg | Out-Null; Copy-Item $real.FullName C:\probe\ffmpeg\ffmpeg.exe; Log "ffmpeg copied from $($real.FullName)" } else { Log 'no real ffmpeg.exe found' }
}

function Run-Guest($only, $ms = 900000) {
  Section "run guest-run.ps1 as kithguest (-Only $only)"
  $sec = ConvertTo-SecureString $pw -AsPlainText -Force
  $cred = New-Object System.Management.Automation.PSCredential('.\kithguest', $sec)
  $o = Join-Path $Out "guest-$only.stdout.txt"
  $e = Join-Path $Out "guest-$only.stderr.txt"
  try {
    $p = Start-Process powershell.exe -Credential $cred -LoadUserProfile -WorkingDirectory C:\guest `
      -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'C:\probe\bin\guest-run.ps1', '-Only', $only `
      -RedirectStandardOutput $o -RedirectStandardError $e -PassThru -WindowStyle Hidden
    if (-not $p.WaitForExit($ms)) { Log 'TIMEOUT: killing the guest run'; taskkill /T /F /PID $p.Id | Out-Null; Get-Process boxprobe -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue }
    Copy-Item 'C:\guest\out\*' $Out -Recurse -Force -ErrorAction SilentlyContinue
    Log "guest run exit code: $($p.ExitCode)"
  } catch { Log "Start-Process as kithguest FAILED: $($_ | Out-String)" }
  Log '--- guest stdout'; Log ((Get-Content $o -ErrorAction SilentlyContinue) -join "`n")
  Log '--- guest stderr'; Log ((Get-Content $e -ErrorAction SilentlyContinue) -join "`n")
}

Run-Guest 'main'

Section 'admin interlude: loopback exemption (Q6: one UAC prompt would be acceptable), measured on box name "loop"'
Try-Do 'loopback' {
  $sid = (& 'C:\probe\bin\boxprobe.exe' acsid loop).Trim()
  Log "AppContainer SID of kithprobe.loop: $sid"
  Log ("CheckNetIsolation LoopbackExempt -a: " + ((CheckNetIsolation.exe LoopbackExempt -a "-p=$sid" 2>&1) -join ' '))
  Log ("CheckNetIsolation LoopbackExempt -s: " + ((CheckNetIsolation.exe LoopbackExempt -s 2>&1) -join ' | '))
}
Run-Guest 'loopback2' 300000
Try-Do 'loopback cleanup' { CheckNetIsolation.exe LoopbackExempt -c 2>&1 | Out-Null }

Section 'admin interlude: ALL APPLICATION PACKAGES read on the Python folder'
Try-Do 'python acl' {
  if ($py) {
    icacls (Split-Path $py) /grant '*S-1-15-2-1:(OI)(CI)(RX)' /Q 2>&1 | Out-Null
    Log "granted on $(Split-Path $py)"
  }
}
Run-Guest 'python2' 120000

Section 'cleanup: leftovers, profiles, the user'
Try-Do 'cleanup' {
  Get-CimInstance Win32_Process -Filter "Name='PING.EXE'" | Where-Object { $_.CommandLine -match 'ping(\.exe)? -n (4[1-7]|[1-3]\d\d) ' } | ForEach-Object {
    Log "leftover: pid=$($_.ProcessId) $($_.CommandLine)"; Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
  }
  sc.exe stop kithprobesvc 2>&1 | Out-Null; sc.exe delete kithprobesvc 2>&1 | Out-Null
  schtasks.exe /delete /tn kithprobe44 /f 2>&1 | Out-Null
  Copy-Item 'C:\guest\out\*' $Out -Recurse -Force -ErrorAction SilentlyContinue
  net user kithguest /delete 2>&1 | Out-Null
}
Section 'done'
exit 0
