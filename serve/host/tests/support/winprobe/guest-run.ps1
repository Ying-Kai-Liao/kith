# CI-only (windows-probe.yml `guest`), runs as the STANDARD local user `kithguest`, never as runneradmin.
# Probes P-G2..P-G8, P-G10, P-G11 with boxprobe.exe. Windows PowerShell 5.1 syntax only. Every box call has its own timeout.
# -Only: main (everything), loopback2 (after an admin granted the loopback exemption), python2 (after an admin granted
# ALL APPLICATION PACKAGES read on the Python folder).
param([string]$Only = 'main')
$ErrorActionPreference = 'Continue'
$bp = 'C:\probe\bin\boxprobe.exe'
$Out = 'C:\guest\out'
$W = 'C:\guest\w'
$R = 'C:\guest\r'
$LogFile = "$Out\guest-$Only.log"
function Log($m) { Write-Output $m; Add-Content -Path $LogFile -Value $m }
function Section($t) { Log ''; Log "=== $t ==="; }
function Bp { $o = & $bp @args 2>&1 | Out-String; Log $o.TrimEnd() }
function Try-Do($what, [scriptblock]$b) { try { & $b } catch { Log "ERROR in ${what}: $($_ | Out-String)" } }

$DD = '--'
$userSid = (& $bp usersid).Trim()
$capR = (& $bp capsid kith.r.probe).Trim()
$capW = (& $bp capsid kith.w.probe).Trim()
$capPipe = (& $bp capsid kith.pipe).Trim()
$boxEnv = @('--env', "HOME=$W", '--env', "TEMP=$W\tmp", '--env', "TMP=$W\tmp", '--env', "USERPROFILE=$W")
$caps = @('--cap', 'kith.r.probe', '--cap', 'kith.w.probe')

function Do-Loop($tag) {
  Section "P-G7 loopback and outbound ($tag): box named 'loop' (AppContainer kithprobe.loop), no internetClient"
  $pf = "$W\lport-$tag.txt"
  Remove-Item $pf -ErrorAction SilentlyContinue
  $lis = Start-Process $bp -ArgumentList 'tcp-listen', '--port-file', $pf, '--hold', '90' -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\lis-$tag.txt"
  Start-Sleep -Seconds 2
  $port = (Get-Content $pf -ErrorAction SilentlyContinue | Select-Object -First 1)
  Log "unboxed host listener (guest user, 127.0.0.1) port=$port"
  Log 'one box, listener and client both inside it, one process (loop-self):'
  Bp box --name loop @caps --timeout 40 --out "$Out\lp-$tag-self.txt" $DD $bp loop-self
  Log 'one box, listener in the parent and client in a second process of the same AppContainer (loop-two):'
  Bp box --name loop @caps --timeout 40 --out "$Out\lp-$tag-two.txt" $DD $bp loop-two
  Log 'box -> unboxed host listener on 127.0.0.1 (the engine/host direction):'
  Bp box --name loop @caps --timeout 40 --out "$Out\lp-$tag-host.txt" $DD $bp connect "127.0.0.1:$port"
  Log 'box -> outside, no internetClient (by name and by address):'
  Bp box --name loop @caps --timeout 40 --out "$Out\lp-$tag-ext1.txt" $DD $bp connect example.com:443
  Bp box --name loop @caps --timeout 40 --out "$Out\lp-$tag-ext2.txt" $DD $bp connect 1.1.1.1:443
  Log 'CONTROL: same with internetClient:'
  Bp box --name loopi @caps --internet --timeout 40 --out "$Out\lp-$tag-ext3.txt" $DD $bp connect example.com:443
  Bp box --name loopi @caps --internet --timeout 40 --out "$Out\lp-$tag-host2.txt" $DD $bp connect "127.0.0.1:$port"
  Log 'host (unboxed) -> a listener inside the box (inbound to an AppContainer):'
  $bf = "$W\bport-$tag.txt"
  Remove-Item $bf -ErrorAction SilentlyContinue
  $bx = Start-Process $bp -ArgumentList 'box', '--name', 'loop', '--cap', 'kith.r.probe', '--timeout', '30', '--out', "$Out\lp-$tag-boxlisten.txt", '--', $bp, 'tcp-listen', '--port-file', $bf, '--hold', '12' -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\lp-$tag-boxlisten.master.txt"
  Start-Sleep -Seconds 3
  $bport = (Get-Content $bf -ErrorAction SilentlyContinue | Select-Object -First 1)
  Log "box listener port file: '$bport'"
  if ($bport) { Bp connect "127.0.0.1:$bport" }
  $bx.WaitForExit(40000) | Out-Null
  Log ((Get-Content "$Out\lp-$tag-boxlisten.master.txt" -ErrorAction SilentlyContinue) -join "`n")
  Stop-Process -Id $lis.Id -Force -ErrorAction SilentlyContinue
}

function Do-Python($tag) {
  Section "P-G2 python in the box ($tag)"
  $py = (Get-Command python -ErrorAction SilentlyContinue | Select-Object -First 1).Source
  Log "python = $py"
  if ($py) {
    Bp box --name g2py @caps @boxEnv --timeout 40 --out "$Out\g2-python-$tag.txt" $DD $py -c "import sys;open('C:/guest/w/py-$tag.txt','w').write('ok');print(sys.version)"
  }
}

if ($Only -eq 'loopback2') { Do-Loop 'exempt'; exit 0 }
if ($Only -eq 'python2') { Do-Python 'granted'; exit 0 }

# ---------------------------------------------------------------------------------------------------------------------
Section 'identity (the probes run as this user)'
Log "whoami: $(whoami)"
Log ((whoami /groups) -join "`n")
Log "user SID: $userSid"
Log "capability SIDs: kith.r.probe=$capR kith.w.probe=$capW kith.pipe=$capPipe"
Try-Do 'unboxed ac-info' { Bp ac-info }

New-Item -ItemType Directory -Force "$W\tmp", "$W\g3" | Out-Null
Set-Content "$R\hello.txt" 'readable'
Section 'grant: one inheritable ACE per capability SID, made by the folder owner (this user)'
Log ((icacls $W /grant "*${capW}:(OI)(CI)(M)") -join "`n")
Log ((icacls $R /grant "*${capR}:(OI)(CI)(RX)") -join "`n")
# created after the grant, so they inherit it
New-Item -ItemType Directory -Force "$W\tmp", "$W\g3" | Out-Null
Log ((icacls $W) -join "`n")
# the guest user's own TEMP is not readable to a box, and a box starts in the parent's cwd: give both a place it can use
$env:TEMP = "$W\tmp"; $env:TMP = "$W\tmp"
Set-Location $R

# --- P-G2 ------------------------------------------------------------------------------------------------------------
Section 'P-G2 token of a box: AppContainer with only custom capabilities'
Bp box --name g2tok @caps --timeout 30 --out "$Out\g2-actoken.txt" $DD $bp ac-info
Bp box --name g2who @caps --timeout 30 --out "$Out\g2-whoami.txt" $DD C:\Windows\System32\whoami.exe /all

Section 'P-G2 Git Bash (MSYS2) in the box'
$bashScript = 'echo BASH_START $BASH_VERSION; pwd; ls / | head -5; echo wrote > /c/guest/w/bash-wrote.txt; cat /c/guest/w/bash-wrote.txt; ' +
  'cat /c/guest/r/hello.txt; echo x > /c/guest/r/should-fail.txt && echo R_WRITABLE_BAD || echo r_is_readonly_ok; ' +
  'echo SUB; (echo child-ok); ls /c/guest | head; echo BASH_END'
Bp box --name g2bash @caps @boxEnv --cwd $W --timeout 60 --out "$Out\g2-bash.txt" $DD 'C:\Program Files\Git\usr\bin\bash.exe' -c $bashScript
Bp box --name g2bash2 @caps @boxEnv --cwd $W --timeout 60 --out "$Out\g2-bash-login.txt" $DD 'C:\Program Files\Git\bin\bash.exe' -lc 'echo LOGIN_BASH_OK; pwd'

Section 'P-G2 git in the box (and GIT_CONFIG_GLOBAL forms, W21 rule 10)'
$gitScript = 'cd /c/guest/w && rm -rf repo && git init -q repo && cd repo && echo hi > f.txt && git add f.txt && ' +
  'git -c user.name=a -c user.email=a@b.c commit -q -m first && git log --oneline && git status --short && echo GIT_OK'
Bp box --name g2git @caps @boxEnv --env 'GIT_CONFIG_GLOBAL=NUL' --cwd $W --timeout 60 --out "$Out\g2-git-NUL.txt" $DD 'C:\Program Files\Git\usr\bin\bash.exe' -c $gitScript
Bp box --name g2git2 @caps @boxEnv --env 'GIT_CONFIG_GLOBAL=/dev/null' --cwd $W --timeout 60 --out "$Out\g2-git-devnull.txt" $DD 'C:\Program Files\Git\usr\bin\bash.exe' -c 'git config --global -l; echo rc=$?; echo GIT_CONFIG_GLOBAL=$GIT_CONFIG_GLOBAL'
Bp box --name g2git3 @caps @boxEnv --env 'GIT_CONFIG_GLOBAL=NUL' --cwd $W --timeout 60 --out "$Out\g2-git-NUL-cfg.txt" $DD 'C:\Program Files\Git\usr\bin\bash.exe' -c 'git config --global -l; echo rc=$?; echo GIT_CONFIG_GLOBAL=$GIT_CONFIG_GLOBAL'
Bp box --name g2git4 @caps @boxEnv --cwd $W --timeout 60 --out "$Out\g2-git-plain.txt" $DD 'C:\Program Files\Git\cmd\git.exe' --version

Section 'P-G2 node in the box'
$node = (Get-Command node -ErrorAction SilentlyContinue | Select-Object -First 1).Source
Log "node = $node"
if ($node) {
  Bp box --name g2node @caps @boxEnv --timeout 40 --out "$Out\g2-node.txt" $DD $node -e "require('fs').writeFileSync('C:/guest/w/node.txt','ok');console.log('NODE_OK',process.version)"
}
Do-Python 'plain'

# --- P-G3 ------------------------------------------------------------------------------------------------------------
Section 'P-G3 escaping the job: cmd start, Start-Process, WMI, schtasks, COM, service (box powershell runs g3-inbox.ps1)'
Remove-Item "$W\g3\*" -Force -ErrorAction SilentlyContinue
$g3 = Start-Process $bp -ArgumentList (@('box', '--name', 'g3', '--cap', 'kith.r.probe', '--cap', 'kith.w.probe', '--job-name', 'Local\kith-g3-job', '--timeout', '100', '--linger', '1', '--out', "$Out\g3-box.txt", '--env', "TEMP=$W\tmp", '--env', "TMP=$W\tmp", '--env', "HOME=$W", '--', 'powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'C:\probe\bin\g3-inbox.ps1', "$W\g3")) -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\g3-master.txt"
for ($i = 0; $i -lt 45 -and -not (Test-Path "$W\g3\g3-done.txt") -and -not $g3.HasExited; $i++) { Start-Sleep -Seconds 2 }
Log "g3 inbox done file: $(Test-Path "$W\g3\g3-done.txt")  box process exited: $($g3.HasExited)"
Start-Sleep -Seconds 2
Log '--- survivors named by their ping -n counts (41..47), and whether each is in the box job:'
Get-CimInstance Win32_Process -Filter "Name='PING.EXE'" | Where-Object { $_.CommandLine -match 'ping(\.exe)? -n 4[1-7] ' } | ForEach-Object {
  $n = [regex]::Match($_.CommandLine, '-n (4\d)').Groups[1].Value
  Log "survivor method=$n pid=$($_.ProcessId) parent=$($_.ParentProcessId) cmd=$($_.CommandLine)"
  Bp injob --pid $_.ProcessId --job 'Local\kith-g3-job'
}
Log '--- what the box wrote (g3.log):'
Log ((Get-Content "$W\g3\g3.log" -ErrorAction SilentlyContinue) -join "`n")
$g3.WaitForExit(120000) | Out-Null
Log ((Get-Content "$Out\g3-master.txt" -ErrorAction SilentlyContinue) -join "`n")
Log '--- after the box job was terminated: survivors still running (these escaped the job):'
Start-Sleep -Seconds 2
Get-CimInstance Win32_Process -Filter "Name='PING.EXE'" | Where-Object { $_.CommandLine -match 'ping(\.exe)? -n 4[1-7] ' } | ForEach-Object {
  Log "STILL RUNNING after job terminate: method=$([regex]::Match($_.CommandLine, '-n (4\d)').Groups[1].Value) pid=$($_.ProcessId) cmd=$($_.CommandLine)"
  Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
}
schtasks.exe /delete /tn kithprobe44 /f 2>&1 | Out-Null

# --- P-G4 ------------------------------------------------------------------------------------------------------------
Section 'P-G4 the box must not reach the engine/host process, Credential Manager or HKCU'
$engine = Start-Process ping.exe -ArgumentList '-n', '300', '127.0.0.1' -PassThru -WindowStyle Hidden
Log "stand-in engine process (same user, unboxed) pid=$($engine.Id)"
cmdkey /generic:kithprobe-master /user:kith /pass:notasecret | Out-Null
reg add 'HKCU\Software\kithprobe' /v k /d v /f | Out-Null
Log 'CONTROL, unboxed:'
Bp open-proc $engine.Id
Bp cred-read kithprobe-master
Bp reg-read
Log 'BOXED:'
Bp box --name g4a @caps --timeout 30 --out "$Out\g4-openproc.txt" $DD $bp open-proc $engine.Id
Bp box --name g4b @caps --timeout 30 --out "$Out\g4-cred.txt" $DD $bp cred-read kithprobe-master
Bp box --name g4c @caps --timeout 30 --out "$Out\g4-cmdkey.txt" $DD cmd.exe /c cmdkey /list
Bp box --name g4d @caps --timeout 30 --out "$Out\g4-reg.txt" $DD $bp reg-read
Bp box --name g4e @caps --timeout 30 --out "$Out\g4-regquery.txt" $DD cmd.exe /c reg query HKCU\Software\kithprobe
Log 'boxes against each other (W21 rule 2: one AppContainer SID per run):'
Bp pair-test @caps
Stop-Process -Id $engine.Id -Force -ErrorAction SilentlyContinue
cmdkey /delete:kithprobe-master | Out-Null

# --- P-G5 / P-G6 -----------------------------------------------------------------------------------------------------
Section 'P-G5 can a box create kith-host-<hash> / kith-run-<hash> in the global pipe namespace, and who owns it'
Bp box --name g5a --cap kith.pipe --timeout 30 --out "$Out\g5-create.txt" $DD $bp pipe-create kith-host-g5test --first
Bp box --name g5b --cap kith.pipe --timeout 30 --out "$Out\g5-create2.txt" $DD $bp pipe-create kith-run-g5test
Log 'box takes the name first, then the host tries FIRST_PIPE_INSTANCE on it (squatting):'
Bp pipe-squat --cap kith.pipe

Section 'P-G6 box connects to a host pipe: Low label + capability ACE, owner SID read from the handle'
$acSid = (& $bp acsid g6).Trim()
Log "AppContainer SID of box g6: $acSid"
$variants = [ordered]@{
  A_low_label_and_cap_ace = "D:(A;;GRGW;;;${capPipe})(A;;GA;;;${userSid})S:(ML;;NW;;;LW)"
  B_cap_ace_default_label = "D:(A;;GRGW;;;${capPipe})(A;;GA;;;${userSid})"
  C_low_label_no_ace      = "D:(A;;GA;;;${userSid})S:(ML;;NW;;;LW)"
  D_low_label_ac_sid_ace  = "D:(A;;GRGW;;;${acSid})(A;;GA;;;${userSid})S:(ML;;NW;;;LW)"
  E_cap_ace_everyone_none = "D:(A;;GRGW;;;${capPipe})(A;;GA;;;${userSid})S:(ML;;;;;LW)"
}
$k = 0
foreach ($name in $variants.Keys) {
  $k++
  $pipe = "kith-run-g6v$k"
  Log "--- variant $name  SDDL=$($variants[$name])"
  $host1 = Start-Process $bp -ArgumentList 'pipe-create', $pipe, '--sddl', $variants[$name], '--hold', '9' -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\g6-host-$k.txt"
  Start-Sleep -Seconds 2
  Log ((Get-Content "$Out\g6-host-$k.txt" -ErrorAction SilentlyContinue) -join "`n")
  Bp box --name g6 --cap kith.pipe --timeout 30 --out "$Out\g6-box-$k.txt" $DD $bp pipe-connect $pipe --expect-owner $userSid
  $host1.WaitForExit(15000) | Out-Null
  Stop-Process -Id $host1.Id -Force -ErrorAction SilentlyContinue
}

# --- P-G7 ------------------------------------------------------------------------------------------------------------
Do-Loop 'plain'

# --- P-G8 ------------------------------------------------------------------------------------------------------------
Section 'P-G8 deny ACE vs inherited allow, and inheritance time on a 100k-file tree'
$tree = 'C:\guest\tree'
Bp mktree $tree 1000 100
$t = Measure-Command { icacls $tree /grant "*${capW}:(OI)(CI)(M)" /Q | Out-Null }
Log "RESULT inheritance: icacls grant of one (OI)(CI) capability ACE on the 100000-file tree took $([int]$t.TotalMilliseconds) ms"
Log ((icacls "$tree\d999\f99.txt") -join "`n")
Bp box --name g8a @caps --timeout 30 --out "$Out\g8-write-allowed.txt" $DD $bp try-open "$tree\d999\f99.txt" w
Bp box --name g8a @caps --timeout 30 --out "$Out\g8-create-allowed.txt" $DD $bp try-open "$tree\d999\new.txt" c
$t = Measure-Command { icacls "$tree\d500" /deny "*${capW}:(OI)(CI)(W)" /Q | Out-Null }
Log "RESULT deny propagation on one 100-file folder: $([int]$t.TotalMilliseconds) ms"
Log 'ACL of a file under the denied folder (order: deny should come before the inherited allow):'
Log ((icacls "$tree\d500\f10.txt") -join "`n")
Bp box --name g8b @caps --timeout 30 --out "$Out\g8-write-denied.txt" $DD $bp try-open "$tree\d500\f10.txt" w
Bp box --name g8b @caps --timeout 30 --out "$Out\g8-create-denied.txt" $DD $bp try-open "$tree\d500\new.txt" c
Bp box --name g8b @caps --timeout 30 --out "$Out\g8-read-still.txt" $DD $bp try-open "$tree\d500\f10.txt" r
Bp box --name g8b @caps --timeout 30 --out "$Out\g8-sibling-allowed.txt" $DD $bp try-open "$tree\d499\f10.txt" w
icacls "$tree\d501\f5.txt" /deny "*${capW}:(W)" /Q | Out-Null
Log 'deny on a single file:'
Log ((icacls "$tree\d501\f5.txt") -join "`n")
Bp box --name g8c @caps --timeout 30 --out "$Out\g8-file-denied.txt" $DD $bp try-open "$tree\d501\f5.txt" w
Bp box --name g8c @caps --timeout 30 --out "$Out\g8-file-sibling.txt" $DD $bp try-open "$tree\d501\f6.txt" w
icacls "$tree\d502" /deny "*${capW}:(OI)(CI)(R)" /Q | Out-Null
Log 'deny READ on a folder under a (M) allow:'
Bp box --name g8d @caps --timeout 30 --out "$Out\g8-read-denied.txt" $DD $bp try-open "$tree\d502\f1.txt" r
$t = Measure-Command { icacls $tree /remove:g "*${capW}" /Q | Out-Null }
Log "RESULT revoke (remove the allow ACE from the root; propagates): $([int]$t.TotalMilliseconds) ms"
Bp box --name g8e @caps --timeout 30 --out "$Out\g8-after-remove.txt" $DD $bp try-open "$tree\d999\f99.txt" w

# --- P-G10 -----------------------------------------------------------------------------------------------------------
Section 'P-G10 TerminateJobObject on the run job with a box in a nested job'
Bp g10

# --- P-G11 -----------------------------------------------------------------------------------------------------------
Section 'P-G11 chrome-headless-shell and ffmpeg in a box; WSADuplicateSocket'
Set-Content "$R\t.html" '<html><body><h1>kith probe</h1></body></html>'
$chrome = if (Test-Path 'C:\probe\chrome-path.txt') { (Get-Content 'C:\probe\chrome-path.txt' -Raw).Trim() } else { '' }
Log "chrome-headless-shell = '$chrome'"
if ($chrome -and (Test-Path $chrome)) {
  Bp box --name g11a @caps @boxEnv --cwd $W --timeout 90 --out "$Out\g11-chrome-nosandbox.txt" $DD $chrome --no-sandbox --disable-gpu "--user-data-dir=$W\ud1" "--screenshot=$W\shot1.png" file:///C:/guest/r/t.html
  Log "shot1.png (--no-sandbox --disable-gpu): $(if (Test-Path "$W\shot1.png") { (Get-Item "$W\shot1.png").Length } else { 'MISSING' }) bytes"
  Bp box --name g11b @caps @boxEnv --cwd $W --timeout 90 --out "$Out\g11-chrome-ownsandbox.txt" $DD $chrome --disable-gpu "--user-data-dir=$W\ud2" "--screenshot=$W\shot2.png" file:///C:/guest/r/t.html
  Log "shot2.png (Chrome's own sandbox nested, --disable-gpu): $(if (Test-Path "$W\shot2.png") { (Get-Item "$W\shot2.png").Length } else { 'MISSING' }) bytes"
  Bp box --name g11c @caps @boxEnv --cwd $W --timeout 90 --out "$Out\g11-chrome-gpu.txt" $DD $chrome --no-sandbox "--user-data-dir=$W\ud3" "--screenshot=$W\shot3.png" file:///C:/guest/r/t.html
  Log "shot3.png (--no-sandbox, GPU/SwiftShader default): $(if (Test-Path "$W\shot3.png") { (Get-Item "$W\shot3.png").Length } else { 'MISSING' }) bytes"
}
$ff = if (Test-Path 'C:\probe\ffmpeg\ffmpeg.exe') { 'C:\probe\ffmpeg\ffmpeg.exe' } else { '' }
Log "ffmpeg = '$ff'"
if ($ff) {
  Bp box --name g11d @caps @boxEnv --cwd $W --timeout 60 --out "$Out\g11-ffmpeg.txt" $DD $ff -y -f lavfi -i testsrc=duration=1:size=320x240:rate=10 "$W\o.mp4"
  Log "o.mp4: $(if (Test-Path "$W\o.mp4") { (Get-Item "$W\o.mp4").Length } else { 'MISSING' }) bytes"
}
Log 'WSADuplicateSocket from the unboxed host into a boxed child (the child then uses the socket):'
Bp sock-dup --dir $W @caps

Section 'done (main pass)'
exit 0
