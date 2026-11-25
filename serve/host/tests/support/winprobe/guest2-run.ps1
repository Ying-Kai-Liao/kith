# CI-only (windows-probe.yml `guest2`), runs as the STANDARD local user `kithguest`, never as runneradmin. W20b: variant A
# (AppContainer) and variant R (restricted token, Medium and Low IL), each in a job, with boxprobe.exe's `xbox`.
# Windows PowerShell 5.1 syntax only. Every box call has its own timeout; every section is wrapped.
# -Only: main (everything), python2 (after an admin granted ALL APPLICATION PACKAGES read on Python), net-r / net-a (after an
# admin installed the firewall filters).
param([string]$Only = 'main')
$ErrorActionPreference = 'Continue'
$bp = 'C:\probe\bin\boxprobe.exe'
$Out = 'C:\guest\out'
$UP = "$env:SystemDrive\Users\kithguest"
$DD = '--'
$K = "$UP\kw"
$W = "$K\w"
$R = "$K\r"
$LogFile = "$Out\guest2-$Only.log"
function Log($m) { Write-Output $m; Add-Content -Path $LogFile -Value $m }
function Section($t) { Log ''; Log "=== $t ===" }
function Bp { $o = & $bp @args 2>&1 | Out-String; Log $o.TrimEnd() }
function Try-Do($what, [scriptblock]$b) { try { & $b } catch { Log "ERROR in ${what}: $($_ | Out-String)" } }
function Msys($p) { $q = $p -replace '\\', '/'; if ($q -match '^([A-Za-z]):(.*)$') { '/' + $Matches[1].ToLower() + $Matches[2] } else { $q } }
# Start-Process joins -ArgumentList with spaces and never quotes: quote the elements that need it
function Q($a) { $a | ForEach-Object { if ("$_" -match '\s') { '"' + $_ + '"' } else { "$_" } } }

$userSid = (& $bp usersid).Trim()
$capW = (& $bp capsid kith.w.probe).Trim()
$capR = (& $bp capsid kith.r.probe).Trim()
$rW = (& $bp rsid kith.w.probe).Trim()
$rR = (& $bp rsid kith.r.probe).Trim()
$tools = @(Get-Content 'C:\probe\tools.txt' -ErrorAction SilentlyContinue)
$py = $tools[0]; $node = $tools[1]
$pgit = 'C:\probe\pgit\usr\bin\bash.exe'
$busy = 'C:\probe\busybox\busybox.exe'
$sysPath = "C:\probe\pgit\usr\bin;C:\probe\pgit\mingw64\bin;$env:SystemRoot\System32;$env:SystemRoot;$(Split-Path $node);$(Split-Path $py)"
$envW = @('--env', "HOME=$W", '--env', "TEMP=$W\tmp", '--env', "TMP=$W\tmp", '--env', "USERPROFILE=$W", '--env', "PATH=$sysPath", '--cwd', $W)
$VA = @('--kind', 'a', '--cap', 'kith.r.probe', '--cap', 'kith.w.probe') + $envW
$VRm = @('--kind', 'r', '--il', 'medium', '--run', 'kith.run.x', '--grant', 'kith.r.probe', '--grant', 'kith.w.probe') + $envW
$VRl = @('--kind', 'r', '--il', 'low', '--run', 'kith.run.x', '--grant', 'kith.r.probe', '--grant', 'kith.w.probe') + $envW
$VRr = $VRm + @('--restricted-sid')
$V = [ordered]@{ A = $VA; Rm = $VRm; Rl = $VRl }

# Bx NAME OPTS CMD...: one boxed run; output of the box lands in $Out\NAME.txt and in the log
function Bx($name, $opts, $cmd, $timeout = 60, $more = @()) {
  Bp xbox --name $name @opts @more --timeout $timeout --out "$Out\$name.txt" $DD @cmd
}
function Txt($name) { (Get-Content "$Out\$name.txt" -Raw -ErrorAction SilentlyContinue) }

function Setup-Workspace {
  New-Item -ItemType Directory -Force $K, $W, $R, "$K\hosttmp" | Out-Null
  $env:TEMP = "$K\hosttmp"; $env:TMP = "$K\hosttmp"
  Log "run SIDs: A caps w=$capW r=$capR ; R arbitrary SIDs w=$rW r=$rR"
  Log ((icacls $W /grant "*${capW}:(OI)(CI)(M)") -join ' ')
  Log ((icacls $W /grant "*${rW}:(OI)(CI)(M)") -join ' ')
  Log ((icacls $R /grant "*${capR}:(OI)(CI)(RX)") -join ' ')
  Log ((icacls $R /grant "*${rR}:(OI)(CI)(RX)") -join ' ')
  New-Item -ItemType Directory -Force "$W\tmp" | Out-Null
  Set-Content "$R\hello.txt" 'readable'
  Set-Content "$UP\secret.txt" 'SECRETDATA'
  Set-Content "$W\empty.gitconfig" ''
  attrib +r "$W\empty.gitconfig"
}

function Do-Net($tag, $optsList) {
  Section "network ($tag): host listeners on 127.0.0.1:18080 (the proxy), 127.0.0.1:18081 (a serve-like API), 0.0.0.0:18082 (LAN-ish)"
  $ip = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.*' } | Select-Object -First 1).IPAddress
  Log "host LAN-ish address: $ip"
  $ls = @()
  foreach ($x in @(@('127.0.0.1', 18080), @('127.0.0.1', 18081), @('0.0.0.0', 18082))) {
    $ls += Start-Process $bp -ArgumentList 'tcp-listen', '--bind', $x[0], '--port', $x[1], '--hold', '150' -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\lis-$tag-$($x[1]).txt"
  }
  Start-Sleep -Seconds 2
  Log 'CONTROL, unboxed owner process (must stay open when a filter exists):'
  foreach ($t in @('127.0.0.1:18080', '127.0.0.1:18081', "${ip}:18082", 'example.com:443')) { Bp connect $t }
  foreach ($k in $optsList.Keys) {
    Log "--- $k"
    $n = 0
    foreach ($t in @('127.0.0.1:18080', '127.0.0.1:18081', "${ip}:18082", 'example.com:443', '1.1.1.1:443')) {
      $n++
      Bx "net-$tag-$k-$n" $optsList[$k] @($bp, 'connect', $t) 40
    }
  }
  $ls | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
}

if ($Only -eq 'python2') {
  Setup-Workspace
  Section 'python in the box after the admin grant (A) and without it (R reads it as Users)'
  Bx 'py-A' $VA @($py, '-c', "import sys;open(r'$W\py-a.txt','w').write('ok');print('PY_OK',sys.version)") 40
  Bx 'py-Rm' $VRm @($py, '-c', "import sys;open(r'$W\py-r.txt','w').write('ok');print('PY_OK',sys.version)") 40
  exit 0
}
if ($Only -eq 'net-r') {
  Setup-Workspace
  $VRn = @('--kind', 'r', '--il', 'medium', '--run', 'kith.run.net') + $envW
  $VRnl = @('--kind', 'r', '--il', 'low', '--run', 'kith.run.net') + $envW
  $VRo = @('--kind', 'r', '--il', 'medium', '--run', 'kith.run.other') + $envW
  Do-Net 'filtered' ([ordered]@{ 'R-medium-keyed-run-SID' = $VRn; 'R-low-keyed-run-SID' = $VRnl; 'R-medium-other-run-SID-control' = $VRo })
  exit 0
}
if ($Only -eq 'net-a') {
  Setup-Workspace
  $VAn = @('--kind', 'a', '--ac', 'kithprobe.n2', '--cap', 'kith.run.n2x') + $envW
  $VAo = @('--kind', 'a', '--cap', 'kith.run.n2y') + $envW
  Do-Net 'filtered' ([ordered]@{ 'A-n2-package-SID' = $VAn; 'A-other-package-control' = $VAo })
  exit 0
}

# =====================================================================================================================
Section 'identity'
Log "whoami: $(whoami)"
Log "user SID: $userSid  profile: $UP"
Setup-Workspace
Log ((icacls $W) -join "`n")
Log ((icacls $UP 2>&1 | Select-Object -First 8) -join "`n")
Log ((icacls C:\guest 2>&1 | Select-Object -First 8) -join "`n")

Section 'tokens: what each box process is'
foreach ($k in $V.Keys) { Bx "tok-$k" $V[$k] @($bp, 'ac-info') 30 }
Bx 'tok-Rr' $VRr @($bp, 'ac-info') 30
Bp ac-info
Section 'P-G13: which restricting SIDs does a process need to start at all (0xC0000142 = DLL init failed)?'
$matrix = [ordered]@{
  'run+Everyone+Users'              = @('--no-logon-sid')
  '+logon'                          = @()
  '+RESTRICTED'                     = @('--no-logon-sid', '--restricted-sid')
  '+logon+RESTRICTED'               = @('--restricted-sid')
  '+logon+AuthenticatedUsers'       = @('--auth-users')
  '+user(diagnostic only)'          = @('--no-logon-sid', '--user-sid')
  'Low:+logon'                      = @('--il', 'low')
  'Low:+logon+RESTRICTED'           = @('--il', 'low', '--restricted-sid')
}
$mi = 0
foreach ($k in $matrix.Keys) {
  $mi++
  Log "--- restricting list: $k"
  Bx "matrix-$mi" (@('--kind', 'r', '--run', 'kith.run.x') + $matrix[$k]) @($bp, 'ac-info') 30
}

# ---- P-G13 (R) and the same checks in A -------------------------------------------------------------------------------
$bashScript = "echo BASH_START `$BASH_VERSION; pwd; ls / | head -3; echo wrote > $(Msys $W)/bash-wrote.txt; cat $(Msys $W)/bash-wrote.txt; " +
  "cat $(Msys $R)/hello.txt; echo x > $(Msys $R)/should-fail.txt && echo R_WRITABLE_BAD || echo r_is_readonly_ok; " +
  "echo hi > /dev/null && echo DEVNULL_OK || echo DEVNULL_FAIL; echo a | cat | tr a b; echo PIPE_RC=`$?; v=`$(echo cmdsub); echo SUB=`$v; " +
  "cat <(echo procsub); echo PS_RC=`$?; (echo subshell-ok); sleep 1 & wait; echo BG_OK; " +
  "echo outside > $(Msys $UP)/should-fail.txt && echo OUTSIDE_WRITABLE_BAD || echo outside_denied_ok; " +
  "cat $(Msys $UP)/secret.txt && echo PROFILE_READABLE_BAD || echo profile_denied_ok; echo BASH_END"
foreach ($k in $V.Keys) {
  Section "P-G13/P-G2: pinned PortableGit bash in the box: $k"
  Try-Do "bash-$k" { Bx "bash-$k" $V[$k] @($pgit, '-c', $bashScript) 60 }
}
Bx 'bash-Rr' $VRr @($pgit, '-c', 'echo BASH_RR_OK; ls /c/Users 2>&1 | head -3') 40

Section 'P-G13 node, python, git (native) in each box'
foreach ($k in $V.Keys) {
  Bx "node-$k" $V[$k] @($node, '-e', "require('fs').writeFileSync(String.raw``$W\node.txt``,'ok');console.log('NODE_OK',process.version)") 40
  Bx "py-$k" $V[$k] @($py, '-c', "import sys;open(r'$W\py-$k.txt','w').write('ok');print('PY_OK',sys.version)") 40
  Bx "git-$k" $V[$k] @('C:\probe\pgit\cmd\git.exe', '--version') 40
}

Section 'P-G13 reads and writes outside the grants (a profile secret, the profile, C:\guest, Windows\Temp)'
New-Item -ItemType Directory -Force C:\guest\open | Out-Null
foreach ($k in $V.Keys) {
  Log "--- $k"
  Bx "rd-secret-$k" $V[$k] @($bp, 'try-open', "$UP\secret.txt", 'r') 30
  Bx "wr-profile-$k" $V[$k] @($bp, 'try-open', "$UP\kw\outside.txt", 'c') 30
  Bx "wr-guestroot-$k" $V[$k] @($bp, 'try-open', 'C:\guest\open\box-made.txt', 'c') 30
  Bx "wr-wintemp-$k" $V[$k] @($bp, 'try-open', 'C:\Windows\Temp\box-made.txt', 'c') 30
  Bx "wr-grant-$k" $V[$k] @($bp, 'try-open', "$W\ok.txt", 'c') 30
  Bx "rd-ro-$k" $V[$k] @($bp, 'try-open', "$R\hello.txt", 'r') 30
  Bx "wr-ro-$k" $V[$k] @($bp, 'try-open', "$R\hello.txt", 'w') 30
}

Section 'P-G13 a deny ACE for a restricting SID (does it block a write?) and one-box-versus-another'
New-Item -ItemType Directory -Force "$W\dn" | Out-Null
Set-Content "$W\dn\f.txt" 'x'
Log ((icacls "$W\dn" /deny "*${capW}:(OI)(CI)(W)" /Q) -join ' ')
Log ((icacls "$W\dn" /deny "*${rW}:(OI)(CI)(W)" /Q) -join ' ')
foreach ($k in $V.Keys) {
  Bx "deny-write-$k" $V[$k] @($bp, 'try-open', "$W\dn\f.txt", 'w') 30
  Bx "deny-read-$k" $V[$k] @($bp, 'try-open', "$W\dn\f.txt", 'r') 30
}
Log ((icacls "$W\dn" /remove:d "*${capW}" /Q) -join ' ')
Log ((icacls "$W\dn" /remove:d "*${rW}" /Q) -join ' ')

Section 'P-G13/G12 can the box create a named pipe; anonymous pipes (P-G2f)'
foreach ($k in $V.Keys) {
  Bx "pipecreate-$k" $V[$k] @($bp, 'pipe-create', 'kith-run-w20b', '--first') 30
  Bx "pipeanon-$k" $V[$k] @($bp, 'pipe-anon') 30
}

Section 'P-G13 CredRead and HKCU\Environment (with and without RESTRICTED in the restricting list)'
cmdkey /generic:kithprobe-master /user:kith /pass:notasecret | Out-Null
Bp cred-read kithprobe-master
foreach ($k in $V.Keys) {
  Bx "cred-$k" $V[$k] @($bp, 'cred-read', 'kithprobe-master') 30
  Bx "reg-$k" $V[$k] @($bp, 'reg-read') 30
}
Bx 'reg-Rr' $VRr @($bp, 'reg-read') 30
Bp reg-read
cmdkey /delete:kithprobe-master | Out-Null

Section 'P-G13 box B opens box A processes: R with separate run SIDs and default DACLs, then A with separate AppContainer SIDs'
Try-Do 'pair' {
  $m = Start-Process $bp -ArgumentList @('xbox', '--kind', 'r', '--name', 'pairA', '--run', 'kith.run.pa', '--timeout', '50', '--out', "$Out\pairA.txt", '--', 'C:\Windows\System32\ping.exe', '-n', '45', '127.0.0.1') -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\pairA.master.txt"
  Start-Sleep -Seconds 4
  $spid = [regex]::Match((Get-Content "$Out\pairA.master.txt" -Raw), 'spawned pid=(\d+)').Groups[1].Value
  Log "box A (R, run SID kith.run.pa) sleeper pid=$spid"
  Bx 'pairB' @('--kind', 'r', '--run', 'kith.run.pb') @($bp, 'open-proc', $spid) 30
  Bx 'pairA2' @('--kind', 'r', '--run', 'kith.run.pa') @($bp, 'open-proc', $spid) 30
  Log 'the unboxed host (same user) opens it:'
  Bp open-proc $spid
  Stop-Process -Id $m.Id -Force -ErrorAction SilentlyContinue
}
Bp pair-test --cap kith.r.probe

# ---- P-G2c / P-G2e (A): MSYS in an AppContainer --------------------------------------------------------------------------
# ObjLoop: run boxed bash; each time it fails on NtCreateDirectoryObject(<path>) the host makes that directory (SD below) and holds it
function ObjLoop($tag, $opts, $extraSids) {
  Section "P-G2c ($tag): host-made MSYS object directories, one per failure, until bash starts or stops moving"
  $held = @()
  $made = @()
  $sd = "D:(A;;GA;;;WD)(A;;GA;;;S-1-15-2-1)(A;;GA;;;$userSid)" + (($extraSids | ForEach-Object { "(A;;GA;;;$_)" }) -join '')
  Log "object directory SD: $sd"
  for ($it = 0; $it -lt 8; $it++) {
    Bx "g2c-$tag-$it" $opts @($pgit, '-c', 'echo MSYS_UP_IN_BOX; echo a | cat; v=$(echo sub); echo $v; echo done') 30
    $txt = Txt "g2c-$tag-$it"
    $m = [regex]::Match($txt, 'NtCreateDirectoryObject\(([^)]+)\): 0x([0-9A-Fa-f]+)')
    if (-not $m.Success) { Log "no NtCreateDirectoryObject failure after $it object directories: $(($txt -replace "`r?`n", ' | ').Substring(0, [Math]::Min(300, $txt.Length)))"; break }
    $path = $m.Groups[1].Value
    if ($made -contains $path) { Log "the same path failed again after the host made it: $path (0x$($m.Groups[2].Value))"; break }
    $made += $path
    Log "host makes $path"
    $held += Start-Process $bp -ArgumentList (Q @('objdir-hold', $path, '--sddl', $sd, '--hold', '100')) -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\g2c-$tag-od$it.txt"
    Start-Sleep -Seconds 1
    Log ((Get-Content "$Out\g2c-$tag-od$it.txt" -ErrorAction SilentlyContinue) -join "`n")
  }
  $held | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
}
Try-Do 'g2c-A' { ObjLoop 'A' ($VA + @('--ac', 'kithprobe.g2cA')) @((& $bp acsid g2cA).Trim()) }
Try-Do 'g2c-Rm' { ObjLoop 'Rm' $VRm @((& $bp rsid kith.run.x).Trim(), (& $bp rsid kith.w.probe).Trim()) }

Section 'P-G2e (A): do a junction and a copy of PortableGit get a different MSYS key?'
foreach ($p in @('C:\probe\pgit\usr\bin\bash.exe', 'C:\probe\pgitj\usr\bin\bash.exe', 'C:\probe\pgit-copy\usr\bin\bash.exe')) {
  $n = 'g2e-' + ($p -split '\\')[2]
  Bx $n $VA @($p, '-c', 'echo X') 30
  Log "$p -> key $([regex]::Match((Txt $n), 'msys-2\.0S5-[0-9a-f]+').Value)"
}
Section 'P-G2e: two boxes at the same time, in R, from the pinned install and from its copy'
Try-Do 'g2e-par' {
  $ps = @()
  foreach ($p in @('C:\probe\pgit\usr\bin\bash.exe', 'C:\probe\pgit-copy\usr\bin\bash.exe', 'C:\probe\pgitj\usr\bin\bash.exe')) {
    $n = 'par-' + ($p -split '\\')[2]
    $ps += Start-Process $bp -ArgumentList (Q (@('xbox') + $VRm + @('--name', $n, '--timeout', '40', '--out', "$Out\$n.txt", '--', $p, '-c', 'echo START; sleep 4; echo END'))) -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\$n.master.txt"
  }
  $ps | ForEach-Object { $_.WaitForExit(60000) | Out-Null }
  foreach ($p in @('pgit', 'pgit-copy', 'pgitj')) { Log "par-$p : $((Txt "par-$p") -replace "`r?`n", ' | ')" }
}

# ---- P-G2d busybox -------------------------------------------------------------------------------------------------------
Section 'P-G2d: busybox-w32 (pinned) sh, Claude Code argv forms and the bash-ism corpus'
$forms = [ordered]@{
  'c-hook'     = @('sh', '-c', 'echo HOOK_RAN_SESSIONSTART')
  'lc-path'    = @('sh', '-lc', 'echo "$PATH"')
  'c-l-snap'   = @('sh', '-c', '-l', 'SNAPSHOT_FILE=snap.sh; echo SNAP=$SNAPSHOT_FILE')
  'tool-wrap'  = @('sh', '-c', 'source /nonexistent/snapshot.sh 2>/dev/null || true && eval ''echo TOOL_OK''')
}
foreach ($k in @('A', 'Rm', 'Rl')) {
  foreach ($f in $forms.Keys) { Bx "busy-$k-$f" $V[$k] (@($busy) + $forms[$f]) 30 }
  $i = 0
  foreach ($line in (Get-Content 'C:\probe\bin\corpus.txt')) {
    $i++
    Bx "busy-$k-corpus$i" $V[$k] @($busy, 'sh', '-c', $line) 30
  }
}
Section 'P-G2d control: the same corpus in real Git Bash in R'
$i = 0
foreach ($line in (Get-Content 'C:\probe\bin\corpus.txt')) { $i++; Bx "bashc-Rm-corpus$i" $VRm @($pgit, '-c', $line) 30 }

# ---- P-G18 NUL -------------------------------------------------------------------------------------------------------------
Section 'P-G18: NUL, unboxed then in each box (and in A with extra capabilities)'
Bp nul-open
foreach ($k in $V.Keys) { Bx "nul-$k" $V[$k] @($bp, 'nul-open') 30 }
Bx 'nul-A-internet' $VA @($bp, 'nul-open') 30 @('--wk', '85')
Bx 'nul-A-privnet' $VA @($bp, 'nul-open') 30 @('--wk', '87')
Bx 'nul-A-nocaps' @('--kind', 'a') @($bp, 'nul-open') 30
Bp sddl-of 'C:\Windows\System32\cmd.exe'
Bx 'busy-A-devnull' $VA @($busy, 'sh', '-c', 'echo hi > /dev/null && echo DEVNULL_OK; echo hi > NUL && echo NUL_OK') 30

# ---- P-G7b loopback in A ------------------------------------------------------------------------------------------------
Section 'P-G7b (A): listen on 127.0.0.1 and accept a client from the same box, with each capability set; and what else opens'
$ip = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.*' } | Select-Object -First 1).IPAddress
$lan = Start-Process $bp -ArgumentList 'tcp-listen', '--bind', '0.0.0.0', '--port', '18082', '--hold', '200' -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\lis-lan.txt"
$sets = [ordered]@{ 'none' = @('--kind', 'a'); 'custom' = @('--kind', 'a', '--cap', 'kith.r.probe', '--cap', 'kith.w.probe'); 'privateNetworkClientServer' = @('--kind', 'a', '--wk', '87'); 'internetClientServer' = @('--kind', 'a', '--wk', '86') }
Start-Sleep -Seconds 2
foreach ($s in $sets.Keys) {
  Log "--- capability set: $s"
  $o = $sets[$s]
  Bx "g7b-$s-self" $o @($bp, 'loop-self') 40
  Bx "g7b-$s-two" $o @($bp, 'loop-two') 40
  Bx "g7b-$s-lan" $o @($bp, 'connect', "${ip}:18082") 40
  Bx "g7b-$s-net" $o @($bp, 'connect', 'example.com:443') 40
  Bx "g7b-$s-ip" $o @($bp, 'connect', '1.1.1.1:443') 40
}
Stop-Process -Id $lan.Id -Force -ErrorAction SilentlyContinue
Section 'P-G13 network unfiltered in R (expected open) and a listener inside an R box'
$l2 = Start-Process $bp -ArgumentList 'tcp-listen', '--port', '18080', '--hold', '60' -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\lis-r.txt"
Start-Sleep -Seconds 2
foreach ($k in @('Rm', 'Rl')) {
  Bx "rnet-$k-host" $V[$k] @($bp, 'connect', '127.0.0.1:18080') 30
  Bx "rnet-$k-net" $V[$k] @($bp, 'connect', 'example.com:443') 30
  Bx "rnet-$k-self" $V[$k] @($bp, 'loop-self') 30
  Bx "rnet-$k-two" $V[$k] @($bp, 'loop-two') 30
}
Stop-Process -Id $l2.Id -Force -ErrorAction SilentlyContinue

# ---- P-G17 ----------------------------------------------------------------------------------------------------------------
Section 'P-G17 (A): one AppContainer SID, per-run capability SIDs in the default DACL and an explicit SD on the first process'
Bp p17

# ---- old rules: G8/G9 carve-outs, G15, G4 -------------------------------------------------------------------------------
foreach ($k in $V.Keys) {
  Section "G8/G9 carve-outs level by level ($k)"
  Try-Do "carve-$k" {
    $T = "$K\carve-$k"
    New-Item -ItemType Directory -Force "$T\a\b", "$T\a\d", "$T\leaf" | Out-Null
    foreach ($f in 'c.txt', 'a\x.txt', 'a\b\secret.txt', 'a\b\other.txt', 'a\d\y.txt') { Set-Content "$T\$f" 'data' }
    icacls "$T\c.txt" /grant "*${capW}:(R)" /Q | Out-Null
    icacls "$T\c.txt" /grant "*${rW}:(R)" /Q | Out-Null
    icacls "$T\a\x.txt" /grant "*${capW}:(R)" /Q | Out-Null
    icacls "$T\a\x.txt" /grant "*${rW}:(R)" /Q | Out-Null
    icacls "$T\a\b\other.txt" /grant "*${capW}:(R)" /Q | Out-Null
    icacls "$T\a\b\other.txt" /grant "*${rW}:(R)" /Q | Out-Null
    icacls "$T\a\d" /grant "*${capW}:(OI)(CI)(R)" /Q | Out-Null
    icacls "$T\a\d" /grant "*${rW}:(OI)(CI)(R)" /Q | Out-Null
    icacls "$T\leaf" /grant "*${capW}:(OI)(CI)(M)" /Q | Out-Null
    icacls "$T\leaf" /grant "*${rW}:(OI)(CI)(M)" /Q | Out-Null
    Log 'no ACE on the folders along the path to the carved file ($T, a, a\b):'
    foreach ($d in @($T, "$T\a", "$T\a\b")) { Log ("$d : " + ((icacls $d) -join ' ' | Select-String -Pattern 'S-1-15-3|S-1-5-21-' -Quiet)) }
    $o = $V[$k]
    $i = 0
    foreach ($t in @(@('c.txt', 'r'), @('a\x.txt', 'r'), @('a\d\y.txt', 'r'), @('a\b\other.txt', 'r'), @('a\b\secret.txt', 'r'), @('a\b\secret.txt', 'w'), @('leaf\new.txt', 'c'), @('a\d\new.txt', 'c'))) {
      $i++
      Bx "carve-$k-$i" $o @($bp, 'try-open', "$T\$($t[0])", $t[1]) 30
    }
    Log 'a sibling created after the grant (new file in the carved folder and in a folder with no ACE):'
    Set-Content "$T\a\b\new.txt" 'late'; Set-Content "$T\a\new2.txt" 'late'
    Bx "carve-$k-late1" $o @($bp, 'try-open', "$T\a\b\new.txt", 'r') 30
    Bx "carve-$k-late2" $o @($bp, 'try-open', "$T\a\new2.txt", 'r') 30
    Log 'the carved file deleted and recreated:'
    Remove-Item "$T\a\b\secret.txt" -Force; Set-Content "$T\a\b\secret.txt" 'again'
    Bx "carve-$k-recreated" $o @($bp, 'try-open', "$T\a\b\secret.txt", 'r') 30
  }
}

Section 'G4: GIT_CONFIG_GLOBAL set to an empty read-only file'
foreach ($k in $V.Keys) {
  Bx "git4-$k" $V[$k] @('C:\probe\pgit\cmd\git.exe', 'config', '--global', '-l') 30 @('--env', "GIT_CONFIG_GLOBAL=$W\empty.gitconfig")
  Bx "git4init-$k" $V[$k] @('C:\probe\pgit\cmd\git.exe', 'init', '-q', "$W\repo4-$k") 30 @('--env', "GIT_CONFIG_GLOBAL=$W\empty.gitconfig", '--env', 'GIT_CONFIG_NOSYSTEM=1')
}
Bx 'git4-bash-Rm' $VRm @($pgit, '-c', "cd $(Msys $W) && git config --global -l; echo rc=`$?; rm -rf r4 && git init -q r4 && cd r4 && echo hi > f && git add f && git -c user.name=a -c user.email=a@b.c commit -q -m first && git log --oneline && echo GIT_OK") 60 @('--env', "GIT_CONFIG_GLOBAL=$(Msys $W)/empty.gitconfig")

Section 'G15: git, npm, pip, node honour TEMP/TMP/HOME and the cache variables (R medium; any other write is denied)'
$cache = @('--env', "APPDATA=$W\appdata", '--env', "LOCALAPPDATA=$W\local", '--env', "npm_config_cache=$W\npmcache", '--env', "PIP_CACHE_DIR=$W\pipcache",
  '--env', "XDG_CACHE_HOME=$W\xdg", '--env', "UV_CACHE_DIR=$W\uvcache", '--env', "TMPDIR=$(Msys $W)/tmp", '--env', "npm_config_userconfig=$W\npmrc")
New-Item -ItemType Directory -Force "$W\appdata", "$W\local" | Out-Null
Bx 'g15-bash' $VRm @($pgit, '-c', "node -e ""const o=require('os');console.log('tmpdir',o.tmpdir(),'home',o.homedir())""; npm config get cache; npm --version; python -c ""import tempfile,os;print('pytmp',tempfile.gettempdir(),os.path.expanduser('~'))""; python -m pip --version; python -m pip config debug 2>&1 | head -5; echo G15_END") 90 $cache
Bx 'g15-npm-init' $VRm @($pgit, '-c', "cd $(Msys $W) && mkdir -p npmt && cd npmt && npm init -y >/dev/null && npm ls 2>&1 | head -3; echo NPM_RC=`$?") 90 $cache
Log 'files the boxed tools made outside the workspace (should be none):'
Log ((Get-ChildItem "$UP\AppData" -Recurse -ErrorAction SilentlyContinue | Where-Object { $_.LastWriteTime -gt (Get-Date).AddMinutes(-15) } | Select-Object -First 10 -ExpandProperty FullName) -join "`n")

# ---- P-G3n -----------------------------------------------------------------------------------------------------------------
foreach ($k in $V.Keys) {
  Section "P-G3n ($k): native escape attempts from a boxed cmd and a boxed native exe; the host watches every new process"
  Try-Do "g3n-$k" {
    $d = "$W\g3n-$k"
    New-Item -ItemType Directory -Force $d | Out-Null
    $job = "Local\kith-g3n-$k"
    $watch = Start-Process $bp -ArgumentList 'watch', '--secs', '80', '--job', $job -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\g3n-$k.watch.txt"
    Start-Sleep -Seconds 1
    $bx = Start-Process $bp -ArgumentList (Q (@('xbox') + $V[$k] + @('--name', "g3n$k", '--job-name', $job, '--timeout', '70', '--linger', '10', '--out', "$Out\g3n-$k.box.txt", '--', $bp, 'g3n-box', $d, 'C:\probe\bin\g3n.js'))) -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\g3n-$k.master.txt"
    for ($i = 0; $i -lt 40 -and -not (Test-Path "$d\g3n-done.txt") -and -not $bx.HasExited; $i++) { Start-Sleep -Seconds 2 }
    Start-Sleep -Seconds 3
    Log '--- survivors named by their ping -n counts (51..61), while the run job is still alive; is each in the job?'
    Get-CimInstance Win32_Process -Filter "Name='PING.EXE'" | Where-Object { $_.CommandLine -match 'ping(\.exe)? -n (5[1-9]|6[01]) ' } | ForEach-Object {
      $n = [regex]::Match($_.CommandLine, '-n (\d\d)').Groups[1].Value
      Log "survivor method=$n pid=$($_.ProcessId) parent=$($_.ParentProcessId) cmd=$($_.CommandLine)"
      Bp injob --pid $_.ProcessId --job $job
    }
    Log '--- what the box wrote (g3n.log):'
    Log ((Get-Content "$d\g3n.log" -ErrorAction SilentlyContinue) -join "`n")
    $bx.WaitForExit(120000) | Out-Null
    Log ((Get-Content "$Out\g3n-$k.master.txt" -ErrorAction SilentlyContinue) -join "`n")
    Start-Sleep -Seconds 2
    Log '--- after the run job was terminated: still running (escaped the job):'
    Get-CimInstance Win32_Process -Filter "Name='PING.EXE'" | Where-Object { $_.CommandLine -match 'ping(\.exe)? -n (5[1-9]|6[01]) ' } | ForEach-Object {
      Log "STILL RUNNING after job terminate: method=$([regex]::Match($_.CommandLine, '-n (\d\d)').Groups[1].Value) pid=$($_.ProcessId) parent=$($_.ParentProcessId) cmd=$($_.CommandLine)"
      Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 1
    Stop-Process -Id $watch.Id -Force -ErrorAction SilentlyContinue
    Log '--- the host-side watcher (every new process: parent, token, integrity, in the run job?):'
    Log ((Get-Content "$Out\g3n-$k.watch.txt" -ErrorAction SilentlyContinue) -join "`n")
    schtasks.exe /delete /tn kithprobe58 /f 2>&1 | Out-Null
    sc.exe delete kithprobesvc59 2>&1 | Out-Null
    bitsadmin.exe /reset 2>&1 | Out-Null
  }
}

# ---- F23 again -----------------------------------------------------------------------------------------------------------
Section 'F23 again, with real work: boxed bash with a child loop and detached children (R), native detached children (A)'
Bx 'f23-R' $VRm @($pgit, '-c', 'sleep 100 & sleep 101 & (sleep 102 &); nohup sleep 103 >/dev/null 2>&1 & sleep 50') 12 @('--kill-measure')
Bx 'f23-Rl' $VRl @($pgit, '-c', 'sleep 100 & sleep 101 & (sleep 102 &); sleep 50') 12 @('--kill-measure')
Bx 'f23-A' $VA @($bp, 'kill-test') 10 @('--kill-measure')
Get-Process sleep -ErrorAction SilentlyContinue | ForEach-Object { Log "LEFTOVER sleep.exe pid=$($_.Id)" }

# ---- P-G19 last (it changes the user's registry key) ----------------------------------------------------------------------
Section 'P-G19: remove the lowbox/RESTRICTED read on HKCU\Environment, then what still works'
Try-Do 'g19' {
  setx KITH_PROBE_SECRET sekret | Out-Null
  $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', 'ReadWriteSubTree', 'ChangePermissions')
  $acl = $key.GetAccessControl()
  Log "before: $($acl.Sddl)"
  Bx 'g19-before-A' $VA @($bp, 'reg-read') 30
  Bx 'g19-before-Rr' $VRr @($bp, 'reg-read') 30
  $sd = $acl.Sddl
  foreach ($ace in @('(A;OICI;KR;;;RC)', '(A;OICI;KR;;;AC)')) { $sd = $sd.Replace($ace, '') }
  $sd = [regex]::Replace($sd, '\(A;OICI;KR;;;S-1-15-3-1024-[0-9-]+\)', '')
  Log "new SDDL: $sd"
  $acl.SetSecurityDescriptorSddlForm($sd)
  $key.SetAccessControl($acl)
  $key.Close()
  $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
  Log "after: $($key.GetAccessControl().Sddl)"
  $key.Close()
  Bx 'g19-after-A' $VA @($bp, 'reg-read') 30
  Bx 'g19-after-Rr' $VRr @($bp, 'reg-read') 30
  Bx 'g19-after-A-works' $VA @($node, '-e', 'console.log("NODE_STILL_OK")') 30
  Bx 'g19-after-A-cmd' $VA @('C:\Windows\System32\cmd.exe', '/c', 'echo CMD_STILL_OK') 30
  Log ("unboxed, user var via the registry API: " + [Environment]::GetEnvironmentVariable('KITH_PROBE_SECRET', 'User'))
  Log ("unboxed reg query: " + ((reg query HKCU\Environment /v KITH_PROBE_SECRET 2>&1) -join ' '))
  $c = Start-Process cmd.exe -ArgumentList '/c', 'echo %USERNAME% > nul' -PassThru -Wait -WindowStyle Hidden
  Log "a new unboxed process starts: exit $($c.ExitCode)"
  Log ("Calculator packages: " + ((Get-AppxPackage -Name '*Calculator*' -ErrorAction SilentlyContinue | Select-Object -ExpandProperty PackageFullName) -join ', '))
  $calc = Start-Process calc.exe -PassThru -ErrorAction SilentlyContinue
  Start-Sleep -Seconds 6
  Log ("calc.exe started: pid=$($calc.Id) ; processes: " + ((Get-Process -Name 'CalculatorApp', 'calc', 'Calculator*' -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name) -join ','))
  Get-Process -Name 'CalculatorApp', 'calc' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

Section 'done (main pass)'
exit 0
