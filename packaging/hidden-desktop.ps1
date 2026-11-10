# Run a command line on a desktop of its own that is never shown (Windows): the windows the packaging
# tests start (0.3.4's Electron window, the Tauri app, dialogs) never reach the screen or take the
# focus of whoever works at this machine. Every process the command starts inherits the desktop.
# Output goes to -Log (the console window, too, is on the hidden desktop). Waits for the command
# and exits with its exit code.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File packaging/hidden-desktop.ps1 -Log out.txt `
#       -CommandLine 'node packaging/update-from-034.mjs portable OLD.zip NEW.zip'
#
# Local use only; CI runners have no one at the screen.

param(
    [Parameter(Mandatory = $true)][string]$Log,
    [Parameter(Mandatory = $true)][string]$CommandLine,
    [int]$TimeoutMinutes = 30
)

$ErrorActionPreference = "Stop"

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class SkHiddenDesktop {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct STARTUPINFO {
        public int cb; public string lpReserved; public string lpDesktop; public string lpTitle;
        public int dwX; public int dwY; public int dwXSize; public int dwYSize; public int dwXCountChars;
        public int dwYCountChars; public int dwFillAttribute; public int dwFlags; public short wShowWindow;
        public short cbReserved2; public IntPtr lpReserved2; public IntPtr hStdInput; public IntPtr hStdOutput;
        public IntPtr hStdError;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct PROCESS_INFORMATION { public IntPtr hProcess; public IntPtr hThread; public int dwProcessId; public int dwThreadId; }
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr CreateDesktopW(string name, IntPtr device, IntPtr mode, int flags, uint access, IntPtr sa);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool CreateProcessW(IntPtr app, System.Text.StringBuilder cmd, IntPtr pa, IntPtr ta, bool inherit,
        int flags, IntPtr env, string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern uint WaitForSingleObject(IntPtr handle, uint ms);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool GetExitCodeProcess(IntPtr process, out uint code);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool CloseHandle(IntPtr handle);
}
"@

$name = "spaceknife-tests-$PID"
$desktop = [SkHiddenDesktop]::CreateDesktopW($name, [IntPtr]::Zero, [IntPtr]::Zero, 0, 0x10000000, [IntPtr]::Zero) # GENERIC_ALL
if ($desktop -eq [IntPtr]::Zero) { throw "CreateDesktop failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
try {
    $logPath = [IO.Path]::GetFullPath($Log)
    # cmd redirects the output into the log; it runs on the hidden desktop like the rest
    $line = New-Object System.Text.StringBuilder ("cmd.exe /d /s /c `"" + $CommandLine + " > `"" + $logPath + "`" 2>&1`"")
    $si = New-Object SkHiddenDesktop+STARTUPINFO
    $si.cb = [Runtime.InteropServices.Marshal]::SizeOf($si)
    $si.lpDesktop = "winsta0\$name"
    $si.dwFlags = 0x1 # STARTF_USESHOWWINDOW
    $si.wShowWindow = 7 # SW_SHOWMINNOACTIVE, should the desktop ever be shown
    $pi = New-Object SkHiddenDesktop+PROCESS_INFORMATION
    if (-not [SkHiddenDesktop]::CreateProcessW([IntPtr]::Zero, $line, [IntPtr]::Zero, [IntPtr]::Zero, $false, 0, [IntPtr]::Zero, (Get-Location).Path, [ref]$si, [ref]$pi)) {
        throw "CreateProcess failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    try {
        $waited = [SkHiddenDesktop]::WaitForSingleObject($pi.hProcess, [uint32]($TimeoutMinutes * 60000))
        if ($waited -ne 0) {
            [SkHiddenDesktop]::TerminateProcess($pi.hProcess, 124) | Out-Null
            Write-Output "timed out after $TimeoutMinutes minutes"
            exit 124
        }
        $code = [uint32]0
        [SkHiddenDesktop]::GetExitCodeProcess($pi.hProcess, [ref]$code) | Out-Null
        exit [int]$code
    } finally {
        [SkHiddenDesktop]::CloseHandle($pi.hProcess) | Out-Null
        [SkHiddenDesktop]::CloseHandle($pi.hThread) | Out-Null
    }
} finally {
    [SkHiddenDesktop]::CloseDesktop($desktop) | Out-Null
}
