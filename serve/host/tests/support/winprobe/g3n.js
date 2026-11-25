// CI-only (W20b P-G3n): runs INSIDE the box under cscript. Appends to the log named by argument 0.
// 55 WScript.Shell.Run, 56 Shell.Application.ShellExecute, 57 WMI Win32_Process.Create, 60 MMC20.Application (out-of-process COM).
var log = WScript.Arguments(0);
function L(m) {
  try { var f = new ActiveXObject("Scripting.FileSystemObject").OpenTextFile(log, 8, true); f.WriteLine(m); f.Close(); } catch (e) {}
}
L("cscript started");
try { new ActiveXObject("WScript.Shell").Run("ping.exe -n 55 127.0.0.1", 0, false); L("55 WScript.Shell.Run ok"); } catch (e) { L("55 WScript.Shell.Run ERR " + e.message); }
try { new ActiveXObject("Shell.Application").ShellExecute("ping.exe", "-n 56 127.0.0.1", "", "open", 0); L("56 Shell.Application ok"); } catch (e) { L("56 Shell.Application ERR " + e.message); }
try {
  var w = GetObject("winmgmts:\\\\.\\root\\cimv2");
  var inp = w.Get("Win32_Process").Methods_("Create").InParameters.SpawnInstance_();
  inp.CommandLine = "ping.exe -n 57 127.0.0.1";
  var o = w.ExecMethod("Win32_Process", "Create", inp);
  L("57 WMI Win32_Process.Create ReturnValue=" + o.ReturnValue + " pid=" + o.ProcessId);
} catch (e) { L("57 WMI ERR " + e.message); }
try {
  var m = new ActiveXObject("MMC20.Application");
  m.Document.ActiveView.ExecuteShellCommand("cmd.exe", null, "/c ping -n 60 127.0.0.1", "Minimized");
  L("60 MMC20.Application ok");
} catch (e) { L("60 MMC20 ERR " + e.message); }
L("cscript done");
