# CI-only (windows-probe.yml `guest`), runs INSIDE the AppContainer box. P-G3: tries every way to start a process outside
# the run's job. Each start is a `ping -n <N>`: N names the method, so the caller can find the survivor by its command line.
# 41 cmd start, 42 Start-Process, 43 WMI Win32_Process.Create, 44 schtasks, 45 Shell.Application, 46 WScript.Shell,
# 47 service. Written for Windows PowerShell 5.1.
param([string]$Out)
$ErrorActionPreference = 'Continue'
function L($m) { Add-Content -Path "$Out\g3.log" -Value $m }
L "box powershell started; user=$(whoami) PSVersion=$($PSVersionTable.PSVersion)"

try { $r = cmd.exe /c 'start /b ping -n 41 127.0.0.1 >nul' 2>&1; L "41 cmd start rc=$LASTEXITCODE $r" } catch { L "41 cmd start EXCEPTION $_" }
try { Start-Process ping.exe -ArgumentList '-n', '42', '127.0.0.1' -WindowStyle Hidden; L '42 Start-Process ok' } catch { L "42 Start-Process EXCEPTION $_" }
try {
  $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = 'ping.exe -n 43 127.0.0.1' }
  L "43 WMI Win32_Process.Create ReturnValue=$($r.ReturnValue) pid=$($r.ProcessId)"
} catch { L "43 WMI EXCEPTION $_" }
try {
  $o = schtasks.exe /create /tn kithprobe44 /tr 'ping.exe -n 44 127.0.0.1' /sc once /st 23:59 /f 2>&1
  L "44 schtasks create rc=$LASTEXITCODE $o"
  $o = schtasks.exe /run /tn kithprobe44 2>&1
  L "44 schtasks run rc=$LASTEXITCODE $o"
} catch { L "44 schtasks EXCEPTION $_" }
try { (New-Object -ComObject Shell.Application).ShellExecute('ping.exe', '-n 45 127.0.0.1', '', 'open', 0); L '45 Shell.Application ShellExecute ok' } catch { L "45 Shell.Application EXCEPTION $_" }
try { [void](New-Object -ComObject WScript.Shell).Run('ping.exe -n 46 127.0.0.1', 0, $false); L '46 WScript.Shell Run ok' } catch { L "46 WScript.Shell EXCEPTION $_" }
try {
  $o = sc.exe create kithprobesvc binPath= 'cmd.exe /c ping -n 47 127.0.0.1' 2>&1
  L "47 sc create rc=$LASTEXITCODE $o"
  $o = sc.exe start kithprobesvc 2>&1
  L "47 sc start rc=$LASTEXITCODE $o"
} catch { L "47 service EXCEPTION $_" }

Start-Sleep -Seconds 2
Set-Content -Path "$Out\g3-done.txt" -Value 'done'
