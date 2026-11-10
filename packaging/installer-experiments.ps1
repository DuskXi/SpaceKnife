<#
.SYNOPSIS
  SpaceKnife installer experiments: 0.3.4 (electron-builder NSIS) -> 0.3.5 (forked Tauri NSIS),
  unattended, on a THROWAWAY Windows machine only (a Hyper-V VM checkpoint or a GitHub Actions
  windows runner). Writes one JSON with every assertion.

.DESCRIPTION
  It installs, updates, uninstalls and deletes SpaceKnife, its data folders, shortcuts and registry
  keys, and kills SpaceKnife processes. It refuses to run unless -IAmOnATestMachine is given AND
  either GITHUB_ACTIONS=true or the file C:\spaceknife-test-vm.txt exists.

  Put in -Share:
    SpaceKnife-0.3.4-Setup-x64.exe   the real v0.3.4 release file (SHA-256 383f640a...8acb40)
    SpaceKnife-<NewVersion>-Setup-x64.exe
                                     the candidate built with the forked template (Tauri's
                                     SpaceKnife_<v>_x64-setup.exe, renamed as the release does:
                                     tools/release.mjs windows-installer); -NewVersion, default 0.3.5
    SpaceKnife-<NextVersion>-Setup-x64.exe
                                     optional (-NextVersion): a newer build of the fork (fork -> fork)
    stock-both-Setup.exe             optional: a stock Tauri installer built with installMode
                                     "both" (to show ERROR_ELEVATION_REQUIRED from CreateProcess)

  On GitHub Actions: .github/workflows/installer-experiments.yml (Windows Server 2022 and 2025).

  Machine set-up for the UAC legs (VM): an administrator account, UAC on (EnableLUA=1) and
  ConsentPromptBehaviorAdmin=0 ("elevate without prompting", so -Verb RunAs needs no click), the
  script started from a NON-elevated Windows PowerShell 5.1. GitHub runners run the script
  elevated: the UAC-dependent assertions are recorded as "skip" there (S2 and S7 run there all the
  same, their updates elevated from the start).

  Updates are started as 0.3.4 starts them (Spawn-Wait: CreateProcess, in the installation folder).
  A failed silent update tells the user in a message box: Spawn-Wait reads and closes each one (the
  failure legs assert there was one, Assert-Updated that a good update showed none).

  Each scenario runs on its own: an error in one is recorded as a failed assertion (the message
  and the line), SpaceKnife is reset, and the next one runs; the JSON is written after every
  scenario and at the end whatever happened.

  Scenarios (-Only to pick):
    S0  launchability: CreateProcess (what 0.3.4's Node spawn and 0.3.5's Rust Command do) of each
        installer, suspended and killed at once: the fork must start, a highestAvailable /
        requireAdministrator build fails with 740
    S1  per-user: 0.3.4 /currentuser, data + shortcuts + a pin copy, 0.3.4's exact update command
        (--updated /S --force-run /currentuser, started like 0.3.4 starts it, from the updates
        folder) -> one entry, same folder, no Electron files, SpaceKnife.com kept, shortcuts are the
        same files with the AUMID, data and 0.3.4's theme kept, the app restarted with --updated,
        not elevated
    S1U uninstall of S1's result right after the app S1 started was ended (its window's WebView2
        still running or still starting): program, entry and shortcuts gone, caches gone (the
        shell folder too, no WebView2 of it left, nothing written into it 5 s later), data kept;
        then a fresh install, the app started until its WebView2 runs, and uninstall with
        --delete-app-data while it runs (the uninstaller closes it): data gone, nothing left
    S2  all users: 0.3.4 /allusers, update with /allusers started NON-elevated (UAC legs) -> the
        fork elevates itself, HKLM entry, Program Files, all-users shortcuts, app restarted as the
        user (not elevated); on a runner the same update runs elevated from the start
    S4  0.3.4 running while updating (the user runs the new installer while SpaceKnife is open) ->
        its processes closed, updated; another program holding 0.3.4's app.asar (not SpaceKnife's
        to close) -> the update stops with nothing changed, that program left alone, 0.3.4 started
        again; (UAC legs) an ELEVATED 0.3.4 the installer can't end -> the same, and the log says why
    S5  WebView2: on a machine without the WebView2 Runtime, the update installs it before
        touching 0.3.4 (-RemoveWebView2 on Windows 10 removes it first; -Offline blocks the network
        to check that a failed WebView2 install leaves 0.3.4 intact and running)
    S6  downgrade / reinstall: 0.3.4's installer over 0.3.5 (electron-builder runs the fork's
        uninstaller with its own flags) -> one entry 0.3.4, no Tauri files, data kept; then 0.3.5
        again; then 0.3.5 over 0.3.5 while 0.3.5 runs (the fork's app closed, in place)
    S7  both scopes installed: per-user and per-machine 0.3.4, each updated by its own flag, the
        other untouched (runners too)
    S8  fork -> fork (-NextVersion; without it the candidate over itself): the update command
        every version uses (0.3.4's, install.rs nsis_update_args: --updated /S --force-run
        /currentuser) -> in place, a stale ui\ file the earlier build's list names removed, a file
        of the user's on no list kept, shortcuts kept, restarted
    S9  custom folder: 0.3.4 in C:\SK Test\SpaceKnife, update without a scope flag (0.3.4 passes
        none there) -> same folder
    S10 0.3.4 installed IN the data folder (pathological) -> data kept, program moved to
        Programs\SpaceKnife, the Electron files gone from the data folder
    S11 console: SpaceKnife.com --headless --help from the install folder
    S12 Defender: MpCmdRun custom scan of the installers, detections during the run
    S13 0.3.4 whose registry keys are gone (only its files): the update, with and without a scope
        flag, takes it over by its files -> no Electron file and no 0.3.4 uninstaller left

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File \\host\share\installer-experiments.ps1 -Share \\host\share -IAmOnATestMachine
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$Share,
  [string]$NewVersion = '0.3.5',
  [string]$NextVersion = '',
  [string]$OutDir = '',
  [string[]]$Only = @(),
  [switch]$RemoveWebView2,
  [switch]$Offline,
  [switch]$IAmOnATestMachine
)

Set-StrictMode -Version 2
$ErrorActionPreference = 'Stop'

# ------------------------------------------------------------------ the guard
if (-not $IAmOnATestMachine) { throw 'This script installs and deletes SpaceKnife and its data. Pass -IAmOnATestMachine on a throwaway VM / CI runner.' }
if (-not (($env:GITHUB_ACTIONS -eq 'true') -or (Test-Path 'C:\spaceknife-test-vm.txt'))) {
  throw 'Not a test machine: neither GITHUB_ACTIONS=true nor C:\spaceknife-test-vm.txt.'
}

# PowerShell's names ignore case: a scenario's $old would be $OLD. The installers' names are their own.
$GUID = '3cb3f1fc-eef4-5813-9008-c29b5510458c'
$AUMID = 'io.github.duskxi.spaceknife'
$Installer034 = Join-Path $Share 'SpaceKnife-0.3.4-Setup-x64.exe'
$InstallerNew = Join-Path $Share "SpaceKnife-$NewVersion-Setup-x64.exe"
$InstallerNext = if ($NextVersion) { Join-Path $Share "SpaceKnife-$NextVersion-Setup-x64.exe" } else { Join-Path $Share 'no-next-version' }
$InstallerStock = Join-Path $Share 'stock-both-Setup.exe'
$SHA_034 = '383f640a26e81948496594abf3ddbc7a0242bd5e2e7a2b155832b6c33d8acb40'
if (-not $OutDir) { $OutDir = $Share }
$stamp = (Get-Date).ToString('yyyyMMdd-HHmmss')
$envInfo = $null
$uacOn = $false
$s1Done = $false
$s1Seed = $null
$uninstallerBusy = ''
$boxes = @()

$LOCAL = $env:LOCALAPPDATA
$ROAM = $env:APPDATA
$DATA = Join-Path $LOCAL 'SpaceKnife'
$SHELLDIR = Join-Path $LOCAL $AUMID
$USERDIR = Join-Path $LOCAL 'Programs\SpaceKnife'
$MACHDIR = Join-Path $env:ProgramW6432 'SpaceKnife'
$SM_USER = Join-Path $ROAM 'Microsoft\Windows\Start Menu\Programs\SpaceKnife.lnk'
$SM_ALL = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\SpaceKnife.lnk'
$DT_USER = Join-Path ([Environment]::GetFolderPath('Desktop')) 'SpaceKnife.lnk'
$DT_ALL = Join-Path $env:PUBLIC 'Desktop\SpaceKnife.lnk'
$PIN = Join-Path $ROAM 'Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\SpaceKnife.lnk'
$INSTALL_LOG = Join-Path $env:TEMP 'SpaceKnife-install.log'
$ELECTRON_FILES = @('resources\app.asar', 'resources\elevate.exe', 'resources\backend\spaceknife-backend.exe', 'locales\en-US.pak',
  'chrome_100_percent.pak', 'resources.pak', 'd3dcompiler_47.dll', 'ffmpeg.dll', 'icudtl.dat', 'v8_context_snapshot.bin',
  'Uninstall SpaceKnife.exe', 'LICENSES.chromium.html')
$TAURI_FILES = @('SpaceKnife.exe', 'SpaceKnife.com', 'spaceknife-cli.exe', 'uninstall.exe')

# ------------------------------------------------------------------ native helpers
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class SkNative {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  struct STARTUPINFO { public int cb; public string r; public string d; public string t; public int x, y, xs, ys, xc, yc, fill, flags; public short show, r2; public IntPtr r3, i, o, e; }
  [StructLayout(LayoutKind.Sequential)]
  struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int pid, tid; }
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  static extern bool CreateProcessW(string app, StringBuilder cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags, IntPtr env, string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
  [DllImport("kernel32.dll")] static extern bool TerminateProcess(IntPtr h, uint code);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
  [DllImport("advapi32.dll", SetLastError = true)] static extern bool OpenProcessToken(IntPtr p, uint access, out IntPtr t);
  [DllImport("advapi32.dll", SetLastError = true)] static extern bool GetTokenInformation(IntPtr t, int cls, out int info, int len, out int ret);
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sa, uint disp, uint flags, IntPtr tmpl);
  [StructLayout(LayoutKind.Sequential, Pack = 4)]
  struct BY_HANDLE_FILE_INFORMATION { public uint attr; public long c, a, w; public uint vol, sizeHi, sizeLo, links, idxHi, idxLo; }
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetFileInformationByHandle(IntPtr h, out BY_HANDLE_FILE_INFORMATION info);

  /// CreateProcess the file suspended and kill it at once: 0 when it starts, else the Win32 error
  /// (740 = ERROR_ELEVATION_REQUIRED). Nothing of the program runs.
  public static int Launchable(string exe) {
    var si = new STARTUPINFO(); si.cb = Marshal.SizeOf(typeof(STARTUPINFO));
    PROCESS_INFORMATION pi;
    if (!CreateProcessW(exe, new StringBuilder("\"" + exe + "\""), IntPtr.Zero, IntPtr.Zero, false, 4 /*CREATE_SUSPENDED*/, IntPtr.Zero, null, ref si, out pi))
      return Marshal.GetLastWin32Error();
    TerminateProcess(pi.hProcess, 1); CloseHandle(pi.hThread); CloseHandle(pi.hProcess);
    return 0;
  }
  /// 1 elevated, 0 not, -1 unknown
  public static int Elevated(int pid) {
    IntPtr p = OpenProcess(0x1000, false, pid); if (p == IntPtr.Zero) return -1;
    IntPtr t; int e = -1, r;
    if (OpenProcessToken(p, 8, out t)) { int v; if (GetTokenInformation(t, 20, out v, 4, out r)) e = v != 0 ? 1 : 0; CloseHandle(t); }
    CloseHandle(p); return e;
  }
  /// volume serial + NTFS file index: the same file across a rename, a new one after delete + create
  public static string FileId(string path) {
    IntPtr h = CreateFileW(path, 0x80, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
    if (h == new IntPtr(-1)) return null;
    BY_HANDLE_FILE_INFORMATION i; string s = null;
    if (GetFileInformationByHandle(h, out i)) s = i.vol.ToString("x8") + ":" + i.idxHi.ToString("x8") + i.idxLo.ToString("x8");
    CloseHandle(h); return s;
  }

  [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  interface IPropertyStore { void GetCount(out uint c); void GetAt(uint i, out PROPERTYKEY k); void GetValue(ref PROPERTYKEY k, out PROPVARIANT v); void SetValue(ref PROPERTYKEY k, ref PROPVARIANT v); void Commit(); }
  [StructLayout(LayoutKind.Sequential, Pack = 4)] struct PROPERTYKEY { public Guid fmtid; public uint pid; }
  [StructLayout(LayoutKind.Explicit)] struct PROPVARIANT { [FieldOffset(0)] public ushort vt; [FieldOffset(8)] public IntPtr p; [FieldOffset(16)] public IntPtr p2; }
  [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = false)]
  static extern void SHGetPropertyStoreFromParsingName(string path, IntPtr bc, int flags, ref Guid iid, [MarshalAs(UnmanagedType.Interface)] out IPropertyStore ps);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern int GetWindowThreadProcessId(IntPtr h, out int pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string cls, string title);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetDlgItemTextW(IntPtr h, int id, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  /// the message boxes (a visible dialog with a button: not the installer's banner) of these
  /// processes: their text, and each one closed (WM_CLOSE, as its OK)
  public static string[] CloseBoxes(int[] pids) {
    var found = new System.Collections.Generic.List<string>();
    EnumWindows((h, l) => {
      int p; GetWindowThreadProcessId(h, out p);
      if (Array.IndexOf(pids, p) >= 0 && IsWindowVisible(h)) {
        var c = new StringBuilder(64); GetClassNameW(h, c, 64);
        if (c.ToString() == "#32770" && FindWindowExW(h, IntPtr.Zero, "Button", null) != IntPtr.Zero) {
          var t = new StringBuilder(4096); GetDlgItemTextW(h, 0xFFFF, t, 4096);
          found.Add(t.ToString());
          PostMessageW(h, 0x10, IntPtr.Zero, IntPtr.Zero);
        }
      }
      return true;
    }, IntPtr.Zero);
    return found.ToArray();
  }

  /// System.AppUserModel.ID of a .lnk (null when none)
  public static string Aumid(string lnk) {
    try {
      Guid iid = new Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"); IPropertyStore ps;
      SHGetPropertyStoreFromParsingName(lnk, IntPtr.Zero, 0, ref iid, out ps);
      var k = new PROPERTYKEY { fmtid = new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"), pid = 5 };
      PROPVARIANT v; ps.GetValue(ref k, out v);
      string s = (v.vt == 31) ? Marshal.PtrToStringUni(v.p) : null;
      Marshal.ReleaseComObject(ps); return s;
    } catch { return null; }
  }
}
'@

# ------------------------------------------------------------------ results
$results = New-Object System.Collections.ArrayList
$current = $null
$clock = $null
function Start-Scenario([string]$Id, [string]$Name) {
  $script:current = [ordered]@{ id = $Id; name = $Name; status = 'pass'; startedUtc = (Get-Date).ToUniversalTime().ToString('o'); durationMs = 0; assertions = New-Object System.Collections.ArrayList; notes = New-Object System.Collections.ArrayList }
  $script:clock = [Diagnostics.Stopwatch]::StartNew()
  Write-Host "== $Id $Name"
}
function Assert([string]$Name, [bool]$Ok, $Detail = '') {
  if ($null -ne $script:current) {
    [void]$script:current.assertions.Add([ordered]@{ name = $Name; ok = $Ok; detail = "$Detail" })
    if (-not $Ok) { $script:current.status = 'fail' }
  }
  if (-not $Ok) { Write-Host "   FAIL $Name : $Detail" -ForegroundColor Red } else { Write-Host "   ok   $Name" }
}
function Skip([string]$Name, [string]$Why) {
  if ($null -ne $script:current) { [void]$script:current.assertions.Add([ordered]@{ name = $Name; ok = $null; detail = "skip: $Why" }) }
  Write-Host "   skip $Name : $Why" -ForegroundColor Yellow
}
function Note([string]$Text) {
  if ($null -ne $script:current) { [void]$script:current.notes.Add($Text) }
  Write-Host "   .. $Text"
}
function End-Scenario {
  if ($null -eq $script:current) { return }
  $script:current.durationMs = $script:clock.ElapsedMilliseconds
  [void]$script:results.Add($script:current)
  Save-Results
}
function Save-Results {
  try {
    $doc = [ordered]@{ tool = 'installer-experiments.ps1'; machine = $env:COMPUTERNAME; environment = $script:envInfo; scenarios = $script:results }
    $path = Join-Path $OutDir ("installer-experiments-{0}-{1}.json" -f $env:COMPUTERNAME, $script:stamp)
    $doc | ConvertTo-Json -Depth 8 | Set-Content -Path $path -Encoding UTF8
  } catch { Write-Warning "could not save the results: $($_.Exception.Message)" }
}
function Want([string]$Id) { return ($Only.Count -eq 0) -or ($Only -contains $Id) }
# One scenario: an error in it is a failed assertion with the message and where it came from, then
# SpaceKnife is reset; the scenario is recorded (and the JSON saved) in any case. The body runs in
# a scope of its own: what it sets is gone afterwards ($script: for what a later one reads).
function Invoke-Scenario([string]$ScenarioId, [string]$ScenarioName, [scriptblock]$ScenarioBody) {
  if (-not (Want $ScenarioId)) { return }
  Start-Scenario $ScenarioId $ScenarioName
  try { & $ScenarioBody }
  catch {
    $err = $_
    $at = $err.InvocationInfo
    $where = if ($null -ne $at -and $at.ScriptLineNumber) { "line $($at.ScriptLineNumber): $("$($at.Line)".Trim())" } else { 'no line' }
    $stack = (@("$($err.ScriptStackTrace)" -split "`r?`n") | Select-Object -First 4) -join ' <- '
    Assert 'the scenario ran to its end' $false "$($err.Exception.Message) ($where; $stack)"
    try { Reset-SpaceKnife } catch { Note "Reset-SpaceKnife after that failed too: $($_.Exception.Message)" }
  }
  finally { End-Scenario }
}

# ------------------------------------------------------------------ probes
function Get-Elevated { $p = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent()); return $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator) }
function Get-RegValue($Path, $Name) { try { return (Get-ItemProperty -LiteralPath $Path -Name $Name -ErrorAction Stop).$Name } catch { return $null } }
function Get-WebView2 {
  foreach ($k in @('HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}')) {
    $v = Get-RegValue $k 'pv'; if ($v -and $v -ne '0.0.0.0') { return $v }
  }
  return $null
}
function Get-Entries {
  # every Apps & features entry that is SpaceKnife's, in both hives and both views
  $out = @()
  foreach ($root in @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')) {
    if (-not (Test-Path $root)) { continue }
    foreach ($k in @(Get-ChildItem $root)) {
      $name = Get-RegValue $k.PSPath 'DisplayName'
      if ($k.PSChildName -eq $GUID -or $k.PSChildName -eq 'SpaceKnife' -or ("$name" -like 'SpaceKnife*')) {
        $out += [pscustomobject]@{ hive = ($root -split ':')[0]; key = $k.PSChildName; version = (Get-RegValue $k.PSPath 'DisplayVersion'); uninstall = (Get-RegValue $k.PSPath 'UninstallString'); quiet = (Get-RegValue $k.PSPath 'QuietUninstallString'); location = (Get-RegValue $k.PSPath 'InstallLocation') }
      }
    }
  }
  # unrolled: callers wrap the call in @() (",$out" handed an empty list over as one item, so
  # foreach ran once on an empty array and @(Get-Entries).Count was never 0)
  return $out
}
# what an assertion about the entries found: their JSON, or that there is none
function Show-Entries($Entries) {
  $e = @($Entries | Where-Object { $null -ne $_ })
  if ($e.Count -eq 0) { return 'no SpaceKnife entry' }
  return (ConvertTo-Json -InputObject $e -Compress)
}
function Get-InstallLocation([string]$Hive) { return Get-RegValue "$($Hive):\Software\$GUID" 'InstallLocation' }
function Get-Lnk([string]$Path) {
  if (-not (Test-Path -LiteralPath $Path)) { return [pscustomobject]@{ exists = $false; target = $null; aumid = $null; id = $null } }
  $t = (New-Object -ComObject WScript.Shell).CreateShortcut($Path).TargetPath
  return [pscustomobject]@{ exists = $true; target = $t; aumid = [SkNative]::Aumid($Path); id = [SkNative]::FileId($Path) }
}
function Show-Lnk($Lnk) { if (-not $Lnk.exists) { return 'no such shortcut' }; return "target $($Lnk.target), AUMID $($Lnk.aumid)" }
function ConvertTo-Proc($Cim) { return [pscustomobject]@{ pid = [int]$Cim.ProcessId; name = $Cim.Name; path = $Cim.ExecutablePath; cmd = $Cim.CommandLine; elevated = [SkNative]::Elevated([int]$Cim.ProcessId); started = $Cim.CreationDate } }
function Get-AppProcs([string]$Dir) {
  $d = $Dir.TrimEnd('\') + '\'
  return @(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($d, [StringComparison]::OrdinalIgnoreCase) } | ForEach-Object { ConvertTo-Proc $_ })
}
# by name: an elevated process's path and token can't be read from a script that is not elevated
function Get-NamedProcs([string]$Name) { return @(Get-CimInstance Win32_Process -Filter "Name='$Name'" | ForEach-Object { ConvertTo-Proc $_ }) }
function Show-Procs($Procs) {
  $list = @($Procs | Where-Object { $null -ne $_ })
  if ($list.Count -eq 0) { return 'none' }
  return (($list | ForEach-Object { "$($_.name) $($_.pid) (started $($_.started), elevated=$($_.elevated)) [$($_.cmd)]" }) -join '; ')
}
# which of these (Get-AppProcs) still run: the same process number, started at the same time
function Get-StillRunning($Procs) {
  $now = @(Get-CimInstance Win32_Process)
  return @(@($Procs | Where-Object { $null -ne $_ }) | Where-Object { $p = $_; @($now | Where-Object { [int]$_.ProcessId -eq $p.pid -and $_.CreationDate -eq $p.started }).Count -gt 0 })
}
# the app (SpaceKnife.exe from $Dir) started at or after $Since, waited for up to $Seconds
function Wait-App([string]$Dir, [int]$Seconds = 60, [datetime]$Since = [datetime]::MinValue) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  do {
    $p = @(Get-AppProcs $Dir | Where-Object { $_.path -like '*\SpaceKnife.exe' -and $_.started -ge $Since })
    if ($p.Count -gt 0) { return $p }
    Start-Sleep -Milliseconds 500
  } while ($sw.Elapsed.TotalSeconds -lt $Seconds)
  return @()
}
# an NSIS uninstaller started without _?= (as Windows starts it) copies itself to
# %TEMP%\~nsu*.tmp\Au_.exe, starts the copy with _?=<its folder>\ and exits before the copy has done
# anything: wait (at most $Seconds) until no process runs from $Dir or has it on its command line
# (the copy), as packaging/lib.mjs untilUninstalled does; a copy whose command line cannot be read
# (elevated, seen from a script that is not) counts by its name. $true when it has ended; else
# $script:uninstallerBusy says what is still at work.
function Wait-Uninstaller([string]$Dir, [int]$Seconds = 180) {
  $d = [IO.Path]::GetFullPath($Dir).TrimEnd('\')
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $script:uninstallerBusy = ''
  do {
    $busy = @(Get-CimInstance Win32_Process | Where-Object {
        $_.ProcessId -ne $PID -and (
          ($_.ExecutablePath -and $_.ExecutablePath.StartsWith($d + '\', [StringComparison]::OrdinalIgnoreCase)) -or
          ($_.CommandLine -and $_.CommandLine.IndexOf($d, [StringComparison]::OrdinalIgnoreCase) -ge 0) -or
          (-not $_.CommandLine -and $_.Name -match '^[A-Z]u_\.exe$'))
      })
    if ($busy.Count -eq 0) { return $true }
    Start-Sleep -Milliseconds 500
  } while ($sw.Elapsed.TotalSeconds -lt $Seconds)
  $script:uninstallerBusy = "still at work after $Seconds s: " + (($busy | ForEach-Object { "$($_.ProcessId) $($_.Name) $($_.CommandLine)" }) -join '; ')
  Note $script:uninstallerBusy
  return $false
}
function Sha([string]$Path) { return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLower() }
function Is-Under([string]$Path, [string]$Dir) { $p = [IO.Path]::GetFullPath($Path).TrimEnd('\'); $d = [IO.Path]::GetFullPath($Dir).TrimEnd('\'); return $p.Equals($d, 'OrdinalIgnoreCase') -or $p.StartsWith($d + '\', 'OrdinalIgnoreCase') }
# those of the paths that exist ($Base\<each> when $Base is given)
function Get-Present([string[]]$Paths, [string]$Base = '') {
  return @($Paths | Where-Object { Test-Path -LiteralPath $(if ($Base) { Join-Path $Base $_ } else { $_ }) })
}
# a folder that should be gone: what is still in it (a few names)
function Show-Left([string]$Dir) {
  if (-not (Test-Path -LiteralPath $Dir)) { return "$Dir is gone" }
  $in = @(Get-ChildItem -LiteralPath $Dir -Recurse -Force -ErrorAction SilentlyContinue | Select-Object -First 8 | ForEach-Object { $_.FullName })
  return "$Dir is still there: " + $(if ($in.Count) { $in -join ', ' } else { 'empty' })
}
function Get-InstallLog([int]$Lines = 15) {
  if (-not (Test-Path -LiteralPath $INSTALL_LOG)) { return '(no install log)' }
  return ((@(Get-Content -LiteralPath $INSTALL_LOG -Encoding Unicode -Tail $Lines)) -join ' | ')
}
function Shorten([string]$Text, [int]$Max = 400) {
  $t = ("$Text" -replace '\s+', ' ').Trim()
  if ($t.Length -gt $Max) { return $t.Substring(0, $Max) + '...' }
  return $t
}

# ------------------------------------------------------------------ actions
function Run-Wait([string]$Exe, [string]$Arguments, [switch]$Elevated, [int]$Timeout = 900) {
  Note "run $(Split-Path $Exe -Leaf) $Arguments$(if ($Elevated) { ' (elevated)' })"
  $script:boxes = @()
  $how = @{ FilePath = $Exe; PassThru = $true }
  if ($Arguments) { $how.ArgumentList = $Arguments }
  if ($Elevated -and -not (Get-Elevated)) { $how.Verb = 'RunAs' }
  $p = Start-Process @how
  if (-not $p.WaitForExit($Timeout * 1000)) { try { $p.Kill() } catch {} ; return -999 }
  return $p.ExitCode
}
# what 0.3.4 (Node spawn) and 0.3.5 (Rust Command) do: CreateProcess, no shell, not elevated by
# itself, in the folder the app runs in ($WorkDir: its installation folder, the folder its shortcut
# starts it in). A failed silent update tells the user in a message box (spaceknife.nsh SkFailed):
# each one of the installer's (and its elevated copy's, when this script may) is read into
# $script:boxes and closed, as the user's OK.
function Spawn-Wait([string]$Exe, [string]$Arguments, [int]$Timeout = 900, [string]$WorkDir = '') {
  Note "spawn (CreateProcess) $(Split-Path $Exe -Leaf) $Arguments$(if ($WorkDir) { " (in $WorkDir)" })"
  $script:boxes = @()
  $psi = New-Object Diagnostics.ProcessStartInfo $Exe, $Arguments
  $psi.UseShellExecute = $false
  if ($WorkDir -and (Test-Path -LiteralPath $WorkDir)) { $psi.WorkingDirectory = $WorkDir }
  try { $p = [Diagnostics.Process]::Start($psi) } catch [ComponentModel.Win32Exception] { return (-1 * $_.Exception.NativeErrorCode) }
  $name = [IO.Path]::GetFileNameWithoutExtension($Exe)
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while (-not $p.WaitForExit(500)) {
    if ($sw.Elapsed.TotalSeconds -gt $Timeout) { try { $p.Kill() } catch {} ; return -999 }
    $ids = [int[]]@(Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
    if ($ids.Count) {
      foreach ($t in @([SkNative]::CloseBoxes($ids))) { $script:boxes += "$t"; Note "the installer's message: $(Shorten "$t")" }
    }
  }
  return $p.ExitCode
}
function Show-Boxes { $b = @($script:boxes); if ($b.Count -eq 0) { return 'no message box' }; return ($b | ForEach-Object { Shorten "$_" }) -join ' | ' }
# a console program, its output read (2>&1 under $ErrorActionPreference = 'Stop' would end the run
# at its first line on stderr), ended after $Timeout s: code (-999 on a timeout), out, err
function Run-Capture([string]$Exe, [string]$Arguments, [int]$Timeout = 60) {
  $psi = New-Object Diagnostics.ProcessStartInfo $Exe, $Arguments
  $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
  $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
  $p = [Diagnostics.Process]::Start($psi)
  $out = $p.StandardOutput.ReadToEndAsync(); $err = $p.StandardError.ReadToEndAsync()
  if (-not $p.WaitForExit($Timeout * 1000)) { try { $p.Kill() } catch {}; return [pscustomobject]@{ code = -999; out = ''; err = "timed out after $Timeout s" } }
  $p.WaitForExit()
  return [pscustomobject]@{ code = $p.ExitCode; out = $out.Result; err = $err.Result }
}
# -NoWait: what comes next runs at once (an uninstaller started right after the app was ended,
# while the WebView2 processes of its window are still there, or still starting)
function Stop-SpaceKnife([switch]$NoWait) {
  Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('SpaceKnife.exe', 'spaceknife-backend.exe', 'spaceknife-cli.exe', 'SpaceKnife.com') } | ForEach-Object { try { Stop-Process -Id $_.ProcessId -Force -ErrorAction Stop } catch {} }
  if (-not $NoWait) { Start-Sleep -Seconds 1 }
}
# the installed app started from $Dir as the user starts it (its window minimized)
function Start-App([string]$Dir) {
  $env:SPACEKNIFE_START_MINIMIZED = '1'
  Start-Process -FilePath (Join-Path $Dir 'SpaceKnife.exe')
}
# the WebView2 processes whose command line names $Dir (--user-data-dir: the window's profile is in
# it); WebView2's processes of other programs name their own folders
function Get-WebView2Of([string]$Dir) {
  return @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.IndexOf($Dir, [StringComparison]::OrdinalIgnoreCase) -ge 0 } | ForEach-Object { ConvertTo-Proc $_ })
}
# ... waited for (at most $Seconds) until there is one: the app's window is up
function Wait-WebView2Of([string]$Dir, [int]$Seconds = 60) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  do {
    $p = @(Get-WebView2Of $Dir)
    if ($p.Count -gt 0) { return $p }
    Start-Sleep -Milliseconds 500
  } while ($sw.Elapsed.TotalSeconds -lt $Seconds)
  return @()
}
# how many lines the install log has (what an uninstaller adds comes after them)
function Get-InstallLogCount { return @(Get-Content -LiteralPath $INSTALL_LOG -Encoding Unicode -ErrorAction SilentlyContinue).Count }
# the lines about WebView2 and what was left, after line $From
function Get-WebView2Log([int]$From) {
  $all = @(Get-Content -LiteralPath $INSTALL_LOG -Encoding Unicode -ErrorAction SilentlyContinue)
  if ($all.Count -le $From) { return '(nothing about it in the install log)' }
  return (@($all[$From..($all.Count - 1)] | Where-Object { $_ -match 'WebView2|still there|removed' }) -join ' | ')
}
# Once an uninstaller has ended: no WebView2 process of the shell's folder runs, the folder is gone,
# and 5 s later still is (a WebView2 still running, or still starting, writes its profile into the
# folder after it was deleted: Local State, lockfile and more was what the Windows Server 2022
# runner kept).
function Assert-ShellGone([string]$When, [int]$LogFrom) {
  $log = Get-WebView2Log $LogFrom
  $wv2 = @(Get-WebView2Of $SHELLDIR)
  Assert "$When : no WebView2 of the shell folder left running" ($wv2.Count -eq 0) ((Show-Procs $wv2) + "; log: $log")
  Assert "$When : the shell folder gone" (-not (Test-Path -LiteralPath $SHELLDIR)) ((Show-Left $SHELLDIR) + "; log: $log")
  Start-Sleep -Seconds 5
  Assert "$When : nothing written into it 5 s later" (-not (Test-Path -LiteralPath $SHELLDIR)) ((Show-Left $SHELLDIR) + "; WebView2: " + (Show-Procs (Get-WebView2Of $SHELLDIR)))
  Note "$When, the install log: $log"
}
function Reset-SpaceKnife {
  Stop-SpaceKnife
  foreach ($e in @(Get-Entries)) {
    try {
      if ($e.quiet -or $e.uninstall) {
        $cmd = if ($e.quiet) { "$($e.quiet)" } else { "$($e.uninstall) /S" }
        $exe = $cmd; $rest = '/S'
        if ($cmd -match '^"([^"]+)"\s*(.*)$') { $exe = $Matches[1]; $rest = $Matches[2] }
        if (Test-Path -LiteralPath $exe) {
          [void](Run-Wait $exe "$rest --delete-app-data" -Elevated:($e.hive -eq 'HKLM') -Timeout 300)
          [void](Wait-Uninstaller (Split-Path $exe) 300)
        }
      }
    } catch { Note "reset: uninstalling $($e.key) ($($e.hive)) failed: $($_.Exception.Message)" }
  }
  Start-Sleep -Seconds 3
  Stop-SpaceKnife
  foreach ($d in @($USERDIR, $MACHDIR, $DATA, $SHELLDIR, (Join-Path $ROAM 'SpaceKnife'), (Join-Path $LOCAL 'spaceknife-electron-updater'), 'C:\SK Test')) {
    if (Test-Path -LiteralPath $d) { Remove-Item -LiteralPath $d -Recurse -Force -ErrorAction SilentlyContinue }
  }
  foreach ($l in @($SM_USER, $DT_USER, $PIN)) { Remove-Item -LiteralPath $l -Force -ErrorAction SilentlyContinue }
  foreach ($k in @("HKCU:\Software\$GUID", "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$GUID", 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\SpaceKnife', 'HKCU:\Software\DuskXi')) {
    Remove-Item -LiteralPath $k -Recurse -Force -ErrorAction SilentlyContinue
  }
  if (Get-Elevated) {
    foreach ($l in @($SM_ALL, $DT_ALL)) { Remove-Item -LiteralPath $l -Force -ErrorAction SilentlyContinue }
    foreach ($k in @("HKLM:\SOFTWARE\$GUID", "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$GUID", 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\SpaceKnife', 'HKLM:\SOFTWARE\DuskXi')) {
      Remove-Item -LiteralPath $k -Recurse -Force -ErrorAction SilentlyContinue
    }
  } else {
    $leftovers = @(@($MACHDIR, $SM_ALL, $DT_ALL) | Where-Object { Test-Path -LiteralPath $_ })
    if ($leftovers.Count) { $ps = "Remove-Item -LiteralPath '$($leftovers -join "','")' -Recurse -Force; Remove-Item -LiteralPath 'HKLM:\SOFTWARE\$GUID','HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$GUID' -Recurse -Force -ErrorAction SilentlyContinue"; Start-Process powershell -Verb RunAs -Wait -ArgumentList "-NoProfile -Command $ps" }
  }
}
# markers the update must keep (data, 0.3.4's theme, the cache) and the folder 0.3.4 runs the setup from
function Seed-Data {
  $m = [ordered]@{
    data = Join-Path $DATA 'sk-e2e\keep.txt'
    cache = Join-Path $DATA 'Cache\sk-e2e.txt'
    electron = Join-Path $ROAM 'SpaceKnife\electron\sk-e2e.txt'
  }
  foreach ($k in $m.Keys) { New-Item -ItemType Directory -Force (Split-Path $m[$k]) | Out-Null; Set-Content -LiteralPath $m[$k] -Value ([guid]::NewGuid().ToString()) -Encoding ASCII }
  $h = [ordered]@{}; foreach ($k in $m.Keys) { $h[$k] = @{ path = $m[$k]; sha = (Sha $m[$k]) } }
  return $h
}
function Assert-Seeded($Seed, [string[]]$Which, [string]$When) {
  foreach ($k in $Which) {
    $p = $Seed[$k].path
    $there = Test-Path -LiteralPath $p
    Assert "$When : $k marker kept" ($there -and ((Sha $p) -eq $Seed[$k].sha)) $(if (-not $there) { "gone: $p" } else { "changed: $p" })
  }
}
function Stage-Update([string]$Setup) {
  $u = Join-Path $DATA 'updates'
  New-Item -ItemType Directory -Force $u | Out-Null
  $dst = Join-Path $u (Split-Path $Setup -Leaf)
  Copy-Item -LiteralPath $Setup -Destination $dst -Force
  return $dst
}
function Pin-Copy([string]$Lnk) {
  # a taskbar pin is a copy of the shortcut in User Pinned\TaskBar (+ the Taskband blob): the copy
  # tests that what a pin points at still works; real pins: the manual checklist
  New-Item -ItemType Directory -Force (Split-Path $PIN) | Out-Null
  Copy-Item -LiteralPath $Lnk -Destination $PIN -Force
}
# 0.3.4 started from $USERDIR (its window minimized), given $Seconds to be up: what runs from there
function Start-034([int]$Seconds = 15, [switch]$Elevated) {
  $env:SPACEKNIFE_START_MINIMIZED = '1'
  $how = @{ FilePath = (Join-Path $USERDIR 'SpaceKnife.exe') }
  if ($Elevated) { $how.Verb = 'RunAs' }
  Start-Process @how
  Start-Sleep -Seconds $Seconds
  $running = @(Get-AppProcs $USERDIR)
  Note ("running: " + (Show-Procs $running))
  return $running
}
function Assert-Layout034([string]$Dir, [string]$Hive) {
  $entries = @(Get-Entries)
  Assert '0.3.4 entry' (@($entries | Where-Object { $_.key -eq $GUID -and $_.hive -eq $Hive -and $_.version -eq '0.3.4' }).Count -eq 1) (Show-Entries $entries)
  $loc = Get-InstallLocation $Hive
  Assert '0.3.4 InstallLocation' ($loc -eq $Dir) "InstallLocation: $loc"
  $missing = @(@('SpaceKnife.exe', 'SpaceKnife.com', 'Uninstall SpaceKnife.exe', 'resources\app.asar') | Where-Object { -not (Test-Path -LiteralPath (Join-Path $Dir $_)) })
  Assert '0.3.4 files' ($missing.Count -eq 0) ("missing in ${Dir}: " + ($missing -join ', '))
}
function Assert-Updated([string]$Dir, [string]$Hive, [string]$Version, [hashtable]$LnkBefore, [string[]]$Lnks, [datetime]$Since, [switch]$NoRestartCheck) {
  Assert 'the installer showed no message' (@($script:boxes).Count -eq 0) (Show-Boxes)
  $entries = @(Get-Entries)
  Assert 'one SpaceKnife entry in this hive' (@($entries | Where-Object { $_.hive -eq $Hive }).Count -eq 1) (Show-Entries $entries)
  Assert "the entry is the GUID key, version $Version" (@($entries | Where-Object { $_.hive -eq $Hive -and $_.key -eq $GUID -and $_.version -eq $Version }).Count -eq 1) (Show-Entries $entries)
  $stock = @(Get-Present @("$($Hive):\Software\Microsoft\Windows\CurrentVersion\Uninstall\SpaceKnife", "$($Hive):\Software\DuskXi\SpaceKnife"))
  Assert 'no stock Tauri keys' ($stock.Count -eq 0) ($stock -join ', ')
  $loc = Get-InstallLocation $Hive
  Assert 'same folder' ($loc -eq $Dir) "InstallLocation: $loc"
  Assert 'not in the data folder' (-not (Is-Under $Dir $DATA) -and -not (Is-Under $DATA $Dir)) $Dir
  foreach ($f in $TAURI_FILES) { $p = Join-Path $Dir $f; Assert "has $f" (Test-Path -LiteralPath $p) "no $p" }
  Assert 'has ui\' (Test-Path -LiteralPath (Join-Path $Dir 'ui')) "no $(Join-Path $Dir 'ui')"
  $exe = Join-Path $Dir 'SpaceKnife.exe'
  $v = if (Test-Path -LiteralPath $exe) { (Get-Item -LiteralPath $exe).VersionInfo.ProductVersion } else { $null }
  Assert "SpaceKnife.exe is $Version" ("$v" -like "$Version*") $(if ($null -ne $v) { "version $v" } else { "no $exe" })
  $left = @(Get-Present $ELECTRON_FILES $Dir)
  Assert 'no Electron files left' ($left.Count -eq 0) ($left -join ', ')
  Assert 'no staging folder left' (-not (Test-Path -LiteralPath "$Dir.sk-staging")) (Show-Left "$Dir.sk-staging")
  foreach ($l in @($Lnks)) {
    $now = Get-Lnk $l
    if ($LnkBefore -and $LnkBefore.ContainsKey($l)) {
      $b = $LnkBefore[$l]
      if ($b.exists) {
        Assert "$(Split-Path $l -Leaf) [$l] still there" $now.exists 'gone'
        Assert "  the same file (pins keep working)" ($now.id -eq $b.id) "$($b.id) -> $($now.id)"
      } else { Assert "$l not made again (the user removed it)" (-not $now.exists) (Show-Lnk $now) }
    }
    if ($now.exists) {
      Assert "  points at $Dir\SpaceKnife.exe" ($now.target -eq (Join-Path $Dir 'SpaceKnife.exe')) "target $($now.target)"
      Assert "  AUMID $AUMID" ($now.aumid -eq $AUMID) "AUMID $($now.aumid)"
    }
  }
  if (-not $NoRestartCheck) {
    $app = @(Wait-App $Dir 60 $Since)
    Assert 'the app was started again' ($app.Count -ge 1) ("none from $Dir since $Since; running from there: " + (Show-Procs (Get-AppProcs $Dir)))
    if ($app.Count -ge 1) {
      Assert '  with --updated' ("$($app[0].cmd)" -like '*--updated*') "command line: $($app[0].cmd)"
      if ($script:uacOn) { Assert '  not elevated' ($app[0].elevated -eq 0) "elevated=$($app[0].elevated)" } else { Skip '  not elevated' 'the script runs elevated: every process it starts is' }
    }
  }
}

# ------------------------------------------------------------------ the run
try {
  # ---------------------------------------------------------------- environment
  $os = Get-CimInstance Win32_OperatingSystem
  $uac = Get-RegValue 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System' 'EnableLUA'
  $consent = Get-RegValue 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System' 'ConsentPromptBehaviorAdmin'
  $uacOn = ($uac -eq 1) -and -not (Get-Elevated)
  $envInfo = [ordered]@{
    os = "$($os.Caption) $($os.Version) build $($os.BuildNumber)"; elevated = (Get-Elevated); enableLUA = $uac; consentPromptBehaviorAdmin = $consent
    webview2 = (Get-WebView2); github = ($env:GITHUB_ACTIONS -eq 'true'); user = $env:USERNAME
    installers = [ordered]@{}
  }
  foreach ($f in @($Installer034, $InstallerNew, $InstallerNext, $InstallerStock)) { if (Test-Path -LiteralPath $f) { $envInfo.installers[(Split-Path $f -Leaf)] = (Sha $f) } }
  if (-not (Test-Path -LiteralPath $Installer034)) { throw "missing $Installer034" }
  if ((Sha $Installer034) -ne $SHA_034) { throw "$Installer034 is not the v0.3.4 release file" }
  if (-not (Test-Path -LiteralPath $InstallerNew)) { throw "missing $InstallerNew" }
  Write-Host "environment: $($envInfo | ConvertTo-Json -Compress)"

  # ---------------------------------------------------------------- S0 launchability
  Invoke-Scenario 'S0' 'CreateProcess can start the installer (0.3.4 and 0.3.5 use CreateProcess)' {
    if ($uacOn) {
      $e = [SkNative]::Launchable($Installer034); Assert 'the 0.3.4 installer starts (asInvoker)' ($e -eq 0) "error $e"
      $e = [SkNative]::Launchable($InstallerNew); Assert 'the fork starts (asInvoker)' ($e -eq 0) "error $e"
      if (Test-Path -LiteralPath $InstallerStock) { $e = [SkNative]::Launchable($InstallerStock); Assert 'a stock "both" build does NOT start (740)' ($e -eq 740) "error $e" } else { Skip 'stock "both" build' 'no stock-both-Setup.exe in the share' }
    } else { Skip 'launchability' 'needs a non-elevated admin with UAC on' }
  }

  # ---------------------------------------------------------------- S1 per user
  Invoke-Scenario 'S1' 'per user: 0.3.4 -> 0.3.5 with 0.3.4''s seamless-update command' {
    Reset-SpaceKnife
    $wv = Get-WebView2
    $code = Run-Wait $Installer034 '/S /currentuser'
    Assert 'installed 0.3.4 /currentuser' ($code -eq 0) "exit $code"
    Assert-Layout034 $USERDIR 'HKCU'
    $seed = Seed-Data
    $script:s1Seed = $seed
    Pin-Copy $SM_USER
    $lnks = @($SM_USER, $DT_USER, $PIN)
    $before = @{}; foreach ($l in $lnks) { $before[$l] = Get-Lnk $l }
    Assert '0.3.4 shortcuts carry the AUMID' (($before[$SM_USER].aumid -eq $AUMID) -and ($before[$DT_USER].aumid -eq $AUMID)) "Start menu: $(Show-Lnk $before[$SM_USER]); desktop: $(Show-Lnk $before[$DT_USER])"
    $setup = Stage-Update $InstallerNew
    $since = Get-Date
    $code = Spawn-Wait $setup '--updated /S --force-run /currentuser' -WorkDir $USERDIR
    Assert 'the update exits 0' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    Assert-Updated $USERDIR 'HKCU' $NewVersion $before $lnks $since
    Assert-Seeded $seed @('data', 'cache', 'electron') 'after the update'
    if (-not $wv) { Assert 'WebView2 installed by the update' ([bool](Get-WebView2)) "WebView2: $(Get-WebView2)" }
    $log = Get-InstallLog 60
    Assert 'the new program was asked --startup-check first' ($log -match 'SpaceKnife\.exe --startup-check: 0 ') $log
    Note ("install log: " + (Get-InstallLog 30))
    $script:s1Done = $true
  }

  if (Want 'S1') {
    Invoke-Scenario 'S1U' 'uninstall: data kept by default, gone with --delete-app-data' {
      if (-not $script:s1Done) { Skip 'S1U' 'S1 did not get to its end'; return }
      $uninst = Join-Path $USERDIR 'uninstall.exe'
      if (-not (Test-Path -LiteralPath $uninst)) { Assert 'an uninstaller to run' $false "no $uninst"; return }
      # the app S1's update started (its window's WebView2 maybe still starting) ended, and the
      # uninstaller run at once: WebView2's processes outlive the window a while
      Note ("WebView2 of the shell folder when the app is ended: " + (Show-Procs (Get-WebView2Of $SHELLDIR)))
      Stop-SpaceKnife -NoWait
      $logFrom = Get-InstallLogCount
      $code = Run-Wait $uninst '/S /currentuser'
      Assert 'uninstall /S /currentuser exits 0' ($code -eq 0) "exit $code"
      $ended = Wait-Uninstaller $USERDIR
      Assert '  its copy in %TEMP% has ended' $ended $script:uninstallerBusy
      Assert 'program gone' (-not (Test-Path -LiteralPath (Join-Path $USERDIR 'SpaceKnife.exe'))) (Show-Left $USERDIR)
      $entries = @(Get-Entries)
      Assert 'entry gone' ($entries.Count -eq 0) (Show-Entries $entries)
      $lnk = @(Get-Present @($SM_USER, $DT_USER))
      Assert 'Start menu and desktop shortcuts gone' ($lnk.Count -eq 0) ("still there: " + ($lnk -join ', '))
      if ($null -ne $script:s1Seed) { Assert-Seeded $script:s1Seed @('data') 'after uninstalling' }
      # 0.3.4's uninstaller (installer.nsh customUnInstall) removed these whatever the flags but
      # --updated; the updater copy with --updated too
      $caches = @((Join-Path $DATA 'Cache'), (Join-Path $DATA 'updates'), $SHELLDIR, (Join-Path $ROAM 'SpaceKnife\electron'), (Join-Path $LOCAL 'spaceknife-electron-updater'))
      $left = @(Get-Present $caches)
      $why = ($left | ForEach-Object { "$_ [" + ((@(Get-ChildItem -LiteralPath $_ -Recurse -Force -File -ErrorAction SilentlyContinue | Select-Object -First 5 | ForEach-Object { $_.FullName })) -join ', ') + "]" }) -join '; '
      $wv2 = Show-Procs (Get-WebView2Of $SHELLDIR)
      $log = Get-WebView2Log $logFrom
      Assert 'caches gone (Cache, updates, the shell folder, 0.3.4''s profile and updater copy)' ($left.Count -eq 0) "left: $why; WebView2 still running: $wv2; log: $log"
      Assert-ShellGone 'after uninstalling right after the app was ended' $logFrom
      $code = Run-Wait $InstallerNew '/S'
      Assert 'fresh install /S' ($code -eq 0) "exit $code"
      $loc = Get-InstallLocation 'HKCU'
      Assert '  into Programs\SpaceKnife' ($loc -eq $USERDIR) "InstallLocation: $loc"
      $sm = Get-Lnk $SM_USER
      Assert '  Start menu shortcut with AUMID' ($sm.aumid -eq $AUMID) (Show-Lnk $sm)
      # the app running, its window's WebView2 up: the uninstaller closes the app and goes on at
      # once, while WebView2 is still on its way out
      Stop-SpaceKnife
      Start-App $USERDIR
      $wv = @(Wait-WebView2Of $SHELLDIR 90)
      Assert '  the app runs, its window''s WebView2 with its profile in the shell folder' ($wv.Count -gt 0) ("running from ${USERDIR}: " + (Show-Procs (Get-AppProcs $USERDIR)))
      $logFrom = Get-InstallLogCount
      $code = Run-Wait $uninst '/S /currentuser --delete-app-data'
      Assert 'uninstall --delete-app-data (the app running)' ($code -eq 0) "exit $code"
      $ended = Wait-Uninstaller $USERDIR
      Assert '  its copy in %TEMP% has ended' $ended $script:uninstallerBusy
      $app = @(Get-AppProcs $USERDIR)
      Assert '  the app was closed' ($app.Count -eq 0) (Show-Procs $app)
      Assert-ShellGone 'after uninstalling with --delete-app-data' $logFrom
      Assert 'data folder gone' (-not (Test-Path -LiteralPath $DATA)) (Show-Left $DATA)
      $rest = @(Get-Present @((Join-Path $ROAM 'SpaceKnife'), (Join-Path $ROAM $AUMID), (Join-Path $LOCAL 'spaceknife-electron-updater')))
      Assert 'nothing else of SpaceKnife''s left (%APPDATA%\SpaceKnife, the shell''s roaming folder, the updater copy)' ($rest.Count -eq 0) (($rest | ForEach-Object { Show-Left $_ }) -join '; ')
    }
  }

  # ---------------------------------------------------------------- S2 all users
  # On the VM (UAC on, the script not elevated) the update elevates itself; on a runner (elevated,
  # UAC off) it runs elevated from the start: everything else is the same and is checked there too.
  Invoke-Scenario 'S2' 'all users: 0.3.4 /allusers -> 0.3.5 with /allusers (started non-elevated on the VM)' {
    if (-not $uacOn -and -not (Get-Elevated)) { Skip 'S2' 'needs administrator rights: a runner, or a non-elevated admin with UAC on (ConsentPromptBehaviorAdmin=0)'; return }
    Reset-SpaceKnife
    $code = Run-Wait $Installer034 '/S /allusers' -Elevated
    Assert 'installed 0.3.4 /allusers' ($code -eq 0) "exit $code"
    Assert-Layout034 $MACHDIR 'HKLM'
    $seed = Seed-Data
    $lnks = @($SM_ALL, $DT_ALL)
    $before = @{}; foreach ($l in $lnks) { $before[$l] = Get-Lnk $l }
    $setup = Stage-Update $InstallerNew
    $since = Get-Date
    $code = Spawn-Wait $setup '--updated /S --force-run /allusers' -WorkDir $MACHDIR
    if ($uacOn) { Assert 'the update exits 0 (it elevated itself)' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)" }
    else {
      Assert 'the update exits 0' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
      Skip 'the update elevated itself' 'the script runs elevated (a runner): no UAC prompt to go through'
    }
    Assert-Updated $MACHDIR 'HKLM' $NewVersion $before $lnks $since
    $user = @(Get-Entries | Where-Object { $_.hive -eq 'HKCU' })
    Assert 'no per-user entry appeared' ($user.Count -eq 0) (Show-Entries $user)
    Assert-Seeded $seed @('data', 'cache', 'electron') 'after the update'
  }

  # ---------------------------------------------------------------- S4 running app, held files
  Invoke-Scenario 'S4' 'update while 0.3.4 runs; a file held by another program; an elevated SpaceKnife' {
    # 0.3.4 open, as when the user runs the new installer by hand while SpaceKnife is open: its
    # processes closed (asked, then ended: spaceknife.nsh SkCloseIn), then updated
    Reset-SpaceKnife
    $code = Run-Wait $Installer034 '/S /currentuser'
    Assert 'installed 0.3.4 /currentuser' ($code -eq 0) "exit $code"
    $seed = Seed-Data
    $running034 = @(Start-034)
    Assert '0.3.4 runs (its app and its backend)' (@($running034 | Where-Object { $_.path -like '*\SpaceKnife.exe' }).Count -ge 1 -and @($running034 | Where-Object { $_.path -like '*\spaceknife-backend.exe' }).Count -ge 1) (Show-Procs $running034)
    $since = Get-Date
    $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run /currentuser' -WorkDir $USERDIR
    Assert 'the update exits 0' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    $left = @(Get-StillRunning $running034)
    Assert '0.3.4''s processes were ended' ($left.Count -eq 0) (Show-Procs $left)
    Assert-Updated $USERDIR 'HKCU' $NewVersion $null @() $since
    Assert-Seeded $seed @('data', 'cache', 'electron') 'after the update'
    Note ("install log: " + (Get-InstallLog 20))

    # another program holds 0.3.4's app.asar and lets no one delete it: not SpaceKnife's to close;
    # 0.3.4's uninstaller puts every file back, the update stops with nothing changed
    Reset-SpaceKnife
    $code = Run-Wait $Installer034 '/S /currentuser'
    Assert 'installed 0.3.4 /currentuser again' ($code -eq 0) "exit $code"
    $holder = Start-Process powershell -PassThru -WindowStyle Hidden -ArgumentList "-NoProfile -Command `$f=[IO.File]::Open('$USERDIR\resources\app.asar','Open','Read','Read'); Start-Sleep 600"
    try {
      Start-Sleep -Seconds 3
      $since = Get-Date
      $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run /currentuser' -WorkDir $USERDIR
      Assert 'held file: the update stops (non-zero)' ($code -ne 0) "exit $code; install log: $(Get-InstallLog)"
      Assert '  the user is told why (a message box)' (@($script:boxes).Count -ge 1) (Show-Boxes)
      Assert '  the other program was left running' (-not $holder.HasExited) "it ended, exit $(if ($holder.HasExited) { $holder.ExitCode })"
      Assert-Layout034 $USERDIR 'HKCU'
      $tauri = @(Get-Present @('spaceknife-cli.exe', 'uninstall.exe', 'ui') $USERDIR)
      Assert '  no 0.3.5 files' ($tauri.Count -eq 0) ($tauri -join ', ')
      Assert '  no staging folder left' (-not (Test-Path -LiteralPath "$USERDIR.sk-staging")) (Show-Left "$USERDIR.sk-staging")
      $back = @(Wait-App $USERDIR 30 $since)
      Assert '  0.3.4 was started again' ($back.Count -ge 1) ("running from ${USERDIR}: " + (Show-Procs (Get-AppProcs $USERDIR)))
    } finally { try { Stop-Process -Id $holder.Id -Force -ErrorAction Stop } catch {} }
    Note ("install log: " + (Get-InstallLog 20))

    # an elevated 0.3.4 the installer (not elevated) can't end: the update stops before any change
    if ($uacOn) {
      Reset-SpaceKnife
      [void](Run-Wait $Installer034 '/S /currentuser')
      [void](Start-034 -Elevated)
      # nothing else of SpaceKnife's runs after the reset: these are the elevated ones
      $elevated = @(Get-NamedProcs 'SpaceKnife.exe')
      Assert 'an elevated 0.3.4 runs' ($elevated.Count -ge 1) (Show-Procs $elevated)
      $since = Get-Date
      $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run /currentuser' -WorkDir $USERDIR
      Assert 'elevated 0.3.4: the update stops (non-zero)' ($code -ne 0) "exit $code"
      Assert '  the user is told why (a message box)' (@($script:boxes).Count -ge 1) (Show-Boxes)
      Assert-Layout034 $USERDIR 'HKCU'
      Assert '  the elevated 0.3.4 still runs' (@(Get-StillRunning $elevated).Count -ge 1) (Show-Procs (Get-NamedProcs 'SpaceKnife.exe'))
      $log = Get-InstallLog 20
      Assert '  the log says what could not be ended' ($log -match 'could not end: SpaceKnife\.exe') $log
      Assert '  0.3.4 was started again' (@(Wait-App $USERDIR 30 $since).Count -ge 1) (Show-Procs (Get-AppProcs $USERDIR))
      Stop-SpaceKnife
      Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList "-NoProfile -Command Get-Process SpaceKnife, spaceknife-backend -ErrorAction SilentlyContinue | Stop-Process -Force"
    } else { Skip 'an elevated SpaceKnife' 'needs a non-elevated script with UAC on' }
  }

  # ---------------------------------------------------------------- S5 WebView2
  Invoke-Scenario 'S5' 'WebView2 missing (Windows 10)' {
    if ($RemoveWebView2 -and (Get-WebView2) -and ($os.BuildNumber -lt 22000)) {
      $u = Get-RegValue 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Microsoft EdgeWebView' 'UninstallString'
      if ($u -and ("$u" -match '^"([^"]+)"\s*(.*)$')) {
        $wvExe = $Matches[1]; $wvArgs = $Matches[2]
        Note "removing WebView2: $u --force-uninstall"
        Start-Process -FilePath $wvExe -ArgumentList "$wvArgs --force-uninstall" -Verb RunAs -Wait
      }
    }
    if (Get-WebView2) { Skip 'S5' "WebView2 $(Get-WebView2) is installed (Windows 11 always has it; on Windows 10 pass -RemoveWebView2)"; return }
    Reset-SpaceKnife
    [void](Run-Wait $Installer034 '/S /currentuser')
    $seed = Seed-Data
    if ($Offline) {
      Note 'blocking outbound traffic: the WebView2 bootstrapper must fail and 0.3.4 must stay'
      New-NetFirewallRule -DisplayName 'sk-e2e-offline' -Direction Outbound -Action Block | Out-Null
      try {
        $since = Get-Date
        $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run /currentuser' -WorkDir $USERDIR
        Assert 'the update fails' ($code -ne 0) "exit $code"
        Assert '  the user is told why (a message box)' (@($script:boxes).Count -ge 1) (Show-Boxes)
        Assert-Layout034 $USERDIR 'HKCU'
        Assert '0.3.4 was started again' (@(Wait-App $USERDIR 30 $since).Count -ge 1) (Show-Procs (Get-AppProcs $USERDIR))
        Stop-SpaceKnife
      } finally { Remove-NetFirewallRule -DisplayName 'sk-e2e-offline' }
    }
    $since = Get-Date
    $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run /currentuser' -WorkDir $USERDIR
    Assert 'the update exits 0' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    Assert 'WebView2 is installed now' ([bool](Get-WebView2)) "WebView2: $(Get-WebView2)"
    Assert-Updated $USERDIR 'HKCU' $NewVersion $null @($SM_USER) $since
    Assert-Seeded $seed @('data') 'after the update'
  }

  # ---------------------------------------------------------------- S6 downgrade and reinstall
  Invoke-Scenario 'S6' '0.3.4 over 0.3.5 (downgrade), 0.3.5 again, 0.3.5 over itself while it runs' {
    Reset-SpaceKnife
    $code = Run-Wait $InstallerNew '/S'
    Assert 'installed 0.3.5 /S' ($code -eq 0) "exit $code"
    $seed = Seed-Data
    Stop-SpaceKnife
    $code = Run-Wait $Installer034 '/S /currentuser'
    Assert '0.3.4 installer exits 0' ($code -eq 0) "exit $code"
    $e = @(Get-Entries)
    Assert 'one entry, 0.3.4' (($e.Count -eq 1) -and ($e[0].version -eq '0.3.4') -and ($e[0].key -eq $GUID)) (Show-Entries $e)
    Assert-Layout034 $USERDIR 'HKCU'
    $tauriLeft = @(Get-Present @('spaceknife-cli.exe', 'uninstall.exe', 'ui\index.html', 'licenses', 'installed-files.txt') $USERDIR)
    Assert 'no Tauri files left' ($tauriLeft.Count -eq 0) ($tauriLeft -join ', ')
    $sm = Get-Lnk $SM_USER
    Assert '0.3.4 shortcut points at its program' ($sm.target -eq (Join-Path $USERDIR 'SpaceKnife.exe')) (Show-Lnk $sm)
    Assert-Seeded $seed @('data') 'after the downgrade'
    Stop-SpaceKnife
    $since = Get-Date
    $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run /currentuser' -WorkDir $USERDIR
    Assert 'up to 0.3.5 again' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    Assert-Updated $USERDIR 'HKCU' $NewVersion $null @($SM_USER) $since
    # the fork over itself, in place (SkRemoveListed, SkCheckReplaceable), while it runs (the
    # update above started it): closed first
    $running = @(Wait-App $USERDIR 30)
    Note ("running before: " + (Show-Procs $running))
    $code = Run-Wait $InstallerNew '/S'
    Assert '0.3.5 over 0.3.5 exits 0' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    $left = @(Get-StillRunning $running)
    Assert '  the running 0.3.5 was closed' ($left.Count -eq 0) (Show-Procs $left)
    Assert-Updated $USERDIR 'HKCU' $NewVersion $null @($SM_USER) (Get-Date) -NoRestartCheck
    Assert-Seeded $seed @('data') 'after reinstalling'
  }

  # ---------------------------------------------------------------- S7 both scopes
  # The per-machine update elevates itself on the VM and runs elevated from the start on a runner.
  Invoke-Scenario 'S7' 'per-user and per-machine 0.3.4 side by side' {
    if (-not $uacOn -and -not (Get-Elevated)) { Skip 'S7' 'needs administrator rights: a runner, or a non-elevated admin with UAC on'; return }
    Reset-SpaceKnife
    [void](Run-Wait $Installer034 '/S /allusers' -Elevated)
    [void](Run-Wait $Installer034 '/S /currentuser')
    $e = @(Get-Entries)
    Assert 'two 0.3.4 entries' (@($e | Where-Object { $_.version -eq '0.3.4' }).Count -eq 2) (Show-Entries $e)
    $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run /currentuser' -WorkDir $USERDIR
    Assert 'per-user update' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    $e = @(Get-Entries)
    Assert 'HKCU 0.3.5, HKLM still 0.3.4' ((@($e | Where-Object { $_.hive -eq 'HKCU' -and $_.version -eq $NewVersion }).Count -eq 1) -and (@($e | Where-Object { $_.hive -eq 'HKLM' -and $_.version -eq '0.3.4' }).Count -eq 1)) (Show-Entries $e)
    Assert 'Program Files 0.3.4 untouched' (Test-Path -LiteralPath "$MACHDIR\resources\app.asar") "no $MACHDIR\resources\app.asar"
    Stop-SpaceKnife
    $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run /allusers' -WorkDir $MACHDIR
    Assert 'per-machine update' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    if (-not $uacOn) { Skip 'the per-machine update elevated itself' 'the script runs elevated (a runner): no UAC prompt to go through' }
    $e = @(Get-Entries)
    Assert 'both 0.3.5' (@($e | Where-Object { $_.version -eq $NewVersion }).Count -eq 2) (Show-Entries $e)
    Stop-SpaceKnife
  }

  # ---------------------------------------------------------------- S8 fork -> fork
  # Files go by name only (docs/packaging.md §5): an update removes what the earlier build's list
  # (installed-files.txt) names, never a file it does not know. The stale file stands for one of an
  # earlier build's interface files, so it is put on that list; a file of the user's next to it is
  # on no list and stays. Without -NextVersion the candidate updates itself the same way.
  Invoke-Scenario 'S8' 'the fork -> a newer fork (or itself) with the update command every version uses' {
    $next = $InstallerNext; $nextVersion = $NextVersion
    if (-not (Test-Path -LiteralPath $next)) { $next = $InstallerNew; $nextVersion = $NewVersion; Note "no -NextVersion installer in the share: $NewVersion updates itself" }
    Reset-SpaceKnife
    $code = Run-Wait $InstallerNew '/S'
    Assert "installed $NewVersion /S" ($code -eq 0) "exit $code"
    $seed = Seed-Data
    Pin-Copy $SM_USER
    $stale = Join-Path $USERDIR "ui\stale-from-$NewVersion.js"
    $mine = Join-Path $USERDIR 'ui\keep-me.txt'
    Set-Content -LiteralPath $stale -Value '//'
    Set-Content -LiteralPath $mine -Value 'the user''s'
    $list = Join-Path $USERDIR 'installed-files.txt'
    Assert 'the installer wrote its list of files' (Test-Path -LiteralPath $list) "no $list"
    if (Test-Path -LiteralPath $list) {
      $listed = [IO.File]::ReadAllText($list, [Text.Encoding]::Unicode)
      [IO.File]::WriteAllText($list, "ui\stale-from-$NewVersion.js`r`n" + $listed, [Text.Encoding]::Unicode)
    }
    $lnks = @($SM_USER, $DT_USER, $PIN); $before = @{}; foreach ($l in $lnks) { $before[$l] = Get-Lnk $l }
    Stop-SpaceKnife
    $since = Get-Date
    $code = Spawn-Wait (Stage-Update $next) '--updated /S --force-run /currentuser' -WorkDir $USERDIR
    Assert 'exit 0' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    Assert-Updated $USERDIR 'HKCU' $nextVersion $before $lnks $since
    Assert 'the stale ui file its list names is removed' (-not (Test-Path -LiteralPath $stale)) "still there: $stale"
    Assert 'the user''s file in ui\ (on no list) is kept' (Test-Path -LiteralPath $mine) "gone: $mine"
    $now = if (Test-Path -LiteralPath $list) { [IO.File]::ReadAllText($list, [Text.Encoding]::Unicode) } else { '' }
    Assert 'the new list does not name the stale file' ($now -and ($now -notmatch 'stale-from-')) "list: $(Shorten $now)"
    Assert-Seeded $seed @('data', 'cache', 'electron') 'after the update'
  }

  # ---------------------------------------------------------------- S9 custom folder
  Invoke-Scenario 'S9' '0.3.4 in a folder of its own, update without a scope flag' {
    Reset-SpaceKnife
    $dir = 'C:\SK Test\SpaceKnife'
    $code = Run-Wait $Installer034 "/S /currentuser /D=$dir"
    Assert "installed 0.3.4 in $dir" ($code -eq 0) "exit $code"
    Assert-Layout034 $dir 'HKCU'
    $since = Get-Date
    $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run' -WorkDir $dir
    Assert 'exit 0' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    Assert-Updated $dir 'HKCU' $NewVersion $null @($SM_USER) $since
  }

  # ---------------------------------------------------------------- S10 0.3.4 in the data folder
  Invoke-Scenario 'S10' '0.3.4 installed in %LOCALAPPDATA%\SpaceKnife (the data folder)' {
    Reset-SpaceKnife
    $seed = Seed-Data
    $code = Run-Wait $Installer034 "/S /currentuser /D=$DATA"
    Assert "installed 0.3.4 in $DATA" ($code -eq 0) "exit $code"
    $loc = Get-InstallLocation 'HKCU'
    Assert '0.3.4 is in the data folder' ($loc -eq $DATA) "InstallLocation: $loc"
    Assert-Seeded $seed @('data') 'after installing 0.3.4 there'
    $since = Get-Date
    $code = Spawn-Wait (Stage-Update $InstallerNew) '--updated /S --force-run /currentuser' -WorkDir $DATA
    Assert 'exit 0' ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
    Assert-Seeded $seed @('data') 'after the update'
    $loc = Get-InstallLocation 'HKCU'
    Assert 'moved to Programs\SpaceKnife' ($loc -eq $USERDIR) "InstallLocation: $loc"
    $left = @(Get-Present $ELECTRON_FILES $DATA)
    Assert 'no Electron files in the data folder' ($left.Count -eq 0) ($left -join ', ')
    $sm = Get-Lnk $SM_USER
    Assert 'shortcut retargeted' ($sm.target -eq (Join-Path $USERDIR 'SpaceKnife.exe')) (Show-Lnk $sm)
  }

  # ---------------------------------------------------------------- S13 0.3.4 without its keys
  # A 0.3.4 installation whose registry keys are gone (its files are all there is: the state a user's
  # machine was found in). The update finds it by its files (spaceknife.nsh SkFindByFiles,
  # SkDetectScope) and takes it over as with its keys: no Electron file and no 0.3.4 uninstaller
  # left next to 0.3.5 (that one would later delete 0.3.5 with its RMDir /r). Once with 0.3.4's own
  # update command, once without a scope flag (the scope from the files too).
  Invoke-Scenario 'S13' '0.3.4 whose registry keys are gone' {
    foreach ($how in @('--updated /S --force-run /currentuser', '--updated /S --force-run')) {
      Reset-SpaceKnife
      $code = Run-Wait $Installer034 '/S /currentuser'
      Assert 'installed 0.3.4 /currentuser' ($code -eq 0) "exit $code"
      $seed = Seed-Data
      Stop-SpaceKnife
      foreach ($k in @("HKCU:\Software\$GUID", "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$GUID")) { Remove-Item -LiteralPath $k -Recurse -Force -ErrorAction SilentlyContinue }
      $e = @(Get-Entries)
      Assert '0.3.4''s keys are gone' ($e.Count -eq 0) (Show-Entries $e)
      $files = @(Get-Present @('SpaceKnife.exe', 'Uninstall SpaceKnife.exe', 'resources\app.asar') $USERDIR)
      Assert '  its files are there' ($files.Count -eq 3) "in ${USERDIR}: $($files -join ', ')"
      $lnks = @($SM_USER, $DT_USER); $before = @{}; foreach ($l in $lnks) { $before[$l] = Get-Lnk $l }
      $since = Get-Date
      $code = Spawn-Wait (Stage-Update $InstallerNew) $how -WorkDir $USERDIR
      Assert "the update ($how) exits 0" ($code -eq 0) "exit $code; install log: $(Get-InstallLog)"
      Assert-Updated $USERDIR 'HKCU' $NewVersion $before $lnks $since
      Assert-Seeded $seed @('data', 'cache', 'electron') 'after the update'
      $log = Get-InstallLog 60
      Assert '  the log says it was found by its files' ($log -match "0\.3\.4's files in") $log
      Stop-SpaceKnife
    }
  }

  # ---------------------------------------------------------------- S11 console launcher
  Invoke-Scenario 'S11' 'SpaceKnife.com --headless --help' {
    $loc = Get-InstallLocation 'HKCU'; if (-not $loc) { $loc = Get-InstallLocation 'HKLM' }
    if (-not $loc -or -not (Test-Path -LiteralPath (Join-Path $loc 'SpaceKnife.com'))) {
      $code = Run-Wait $InstallerNew '/S'
      Note "installed $NewVersion for this: exit $code"
      $loc = Get-InstallLocation 'HKCU'
    }
    if (-not $loc) { Assert 'an installation to run it from' $false "no InstallLocation in HKCU\Software\$GUID"; return }
    $r = Run-Capture (Join-Path $loc 'SpaceKnife.com') '--headless --help' 60
    $text = "$($r.out) $($r.err)"
    Assert 'exit 0' ($r.code -eq 0) "exit $($r.code): $(Shorten $text)"
    Assert 'prints headless usage' (($text -match '--listen') -or ($text -match 'headless')) (Shorten $text)
  }

  # ---------------------------------------------------------------- S12 Defender
  Invoke-Scenario 'S12' 'Microsoft Defender' {
    $mp = Join-Path $env:ProgramFiles 'Windows Defender\MpCmdRun.exe'
    if (-not (Test-Path -LiteralPath $mp)) { Skip 'S12' 'no MpCmdRun'; return }
    foreach ($f in @(@($InstallerNew, $InstallerNext) | Where-Object { Test-Path -LiteralPath $_ })) {
      $r = Run-Capture $mp "-Scan -ScanType 3 -File `"$f`" -DisableRemediation" 900
      Assert "$(Split-Path $f -Leaf): no threat" ($r.code -eq 0) "exit $($r.code): $(Shorten "$($r.out) $($r.err)" 600)"
    }
    $start = [datetime]::ParseExact($stamp, 'yyyyMMdd-HHmmss', $null)
    try {
      $det = @(Get-WinEvent -FilterHashtable @{ LogName = 'Microsoft-Windows-Windows Defender/Operational'; Id = 1116, 1117; StartTime = $start } -ErrorAction Stop)
      Assert 'no detections during the run' ($det.Count -eq 0) (($det | Select-Object -First 3 | ForEach-Object { $_.Message }) -join ' | ')
    } catch {
      if ($_.Exception.Message -match 'No events were found') { Assert 'no detections during the run' $true '' }
      else { Skip 'detections during the run' "the Defender log could not be read: $($_.Exception.Message)" }
    }
  }
} finally {
  try { Reset-SpaceKnife } catch { Write-Warning "the last Reset-SpaceKnife failed: $($_.Exception.Message)" }
  Save-Results
}

$failed = @($results | Where-Object { $_.status -eq 'fail' })
Write-Host ("done: {0} scenarios, {1} failed; {2}" -f $results.Count, $failed.Count, (Join-Path $OutDir "installer-experiments-$env:COMPUTERNAME-$stamp.json"))
if ($failed.Count) { exit 1 }
