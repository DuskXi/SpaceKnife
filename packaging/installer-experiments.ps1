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
  script started from a NON-elevated Windows PowerShell 5.1. GitHub runners are elevated with UAC
  off: the UAC-dependent assertions are recorded as "skip" there.

  Scenarios (-Only to pick):
    S0  launchability: CreateProcess (what 0.3.4's Node spawn and 0.3.5's Rust Command do) of each
        installer, suspended and killed at once: the fork must start, a highestAvailable /
        requireAdministrator build fails with 740
    S1  per-user: 0.3.4 /currentuser, data + shortcuts + a pin copy, 0.3.4's exact update command
        (--updated /S --force-run /currentuser, started like 0.3.4 starts it, from the updates
        folder) -> one entry, same folder, no Electron files, SpaceKnife.com kept, shortcuts are the
        same files with the AUMID, data and 0.3.4's theme kept, the app restarted with --updated,
        not elevated
    S1U uninstall of S1's result: program, entry and shortcuts gone, caches gone, data kept;
        then a fresh install + uninstall with --delete-app-data: data gone
    S2  all users: 0.3.4 /allusers, update with /allusers started NON-elevated (UAC legs) -> the
        fork elevates itself, HKLM entry, Program Files, all-users shortcuts, app restarted as the
        user (not elevated)
    S4  running app: 0.3.4 running (and a second process holding a file) while updating -> closed,
        updated; an ELEVATED lock holder the installer can't close -> update refused, 0.3.4 intact
        and started again
    S5  WebView2: on a machine without the WebView2 Runtime, the update installs it before
        touching 0.3.4 (-RemoveWebView2 on Windows 10 removes it first; -Offline blocks the network
        to check that a failed WebView2 install leaves 0.3.4 intact and running)
    S6  downgrade / reinstall: 0.3.4's installer over 0.3.5 (electron-builder runs the fork's
        uninstaller with its own flags) -> one entry 0.3.4, no Tauri files, data kept; then 0.3.5
        again; then 0.3.5 over 0.3.5
    S7  both scopes installed: per-user and per-machine 0.3.4, each updated by its own flag, the
        other untouched
    S8  fork -> fork (needs -NextVersion): the update command every version uses (0.3.4's,
        install.rs nsis_update_args: --updated /S --force-run /currentuser) -> in place, a stale
        ui\ file removed, shortcuts kept, restarted
    S9  custom folder: 0.3.4 in C:\SK Test\SpaceKnife, update without a scope flag (0.3.4 passes
        none there) -> same folder
    S10 0.3.4 installed IN the data folder (pathological) -> data kept, program moved to
        Programs\SpaceKnife, the Electron files gone from the data folder
    S11 console: SpaceKnife.com --headless --help from the install folder
    S12 Defender: MpCmdRun custom scan of the installers, detections during the run

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

$GUID = '3cb3f1fc-eef4-5813-9008-c29b5510458c'
$AUMID = 'io.github.duskxi.spaceknife'
$OLD = Join-Path $Share 'SpaceKnife-0.3.4-Setup-x64.exe'
$NEW = Join-Path $Share "SpaceKnife-$NewVersion-Setup-x64.exe"
$NEXT = if ($NextVersion) { Join-Path $Share "SpaceKnife-$NextVersion-Setup-x64.exe" } else { Join-Path $Share 'no-next-version' }
$STOCK = Join-Path $Share 'stock-both-Setup.exe'
$OLD_SHA = '383f640a26e81948496594abf3ddbc7a0242bd5e2e7a2b155832b6c33d8acb40'
if (-not $OutDir) { $OutDir = $Share }
$work = Join-Path $env:TEMP ('sk-installer-e2e-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force $work | Out-Null

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
function Start-Scenario([string]$Id, [string]$Name) {
  $script:current = [ordered]@{ id = $Id; name = $Name; status = 'pass'; startedUtc = (Get-Date).ToUniversalTime().ToString('o'); durationMs = 0; assertions = New-Object System.Collections.ArrayList; notes = New-Object System.Collections.ArrayList }
  $script:clock = [Diagnostics.Stopwatch]::StartNew()
  Write-Host "== $Id $Name"
}
function Assert([string]$Name, [bool]$Ok, $Detail = '') {
  [void]$script:current.assertions.Add([ordered]@{ name = $Name; ok = $Ok; detail = "$Detail" })
  if (-not $Ok) { $script:current.status = 'fail'; Write-Host "   FAIL $Name : $Detail" -ForegroundColor Red } else { Write-Host "   ok   $Name" }
}
function Skip([string]$Name, [string]$Why) {
  [void]$script:current.assertions.Add([ordered]@{ name = $Name; ok = $null; detail = "skip: $Why" })
  Write-Host "   skip $Name : $Why" -ForegroundColor Yellow
}
function Note([string]$Text) { [void]$script:current.notes.Add($Text); Write-Host "   .. $Text" }
function End-Scenario {
  $script:current.durationMs = $script:clock.ElapsedMilliseconds
  [void]$script:results.Add($script:current)
  Save-Results
}
function Save-Results {
  $doc = [ordered]@{ tool = 'installer-experiments.ps1'; machine = $env:COMPUTERNAME; environment = $script:envInfo; scenarios = $script:results }
  $path = Join-Path $OutDir ("installer-experiments-{0}-{1}.json" -f $env:COMPUTERNAME, $script:stamp)
  $doc | ConvertTo-Json -Depth 8 | Set-Content -Path $path -Encoding UTF8
}
function Want([string]$Id) { return ($Only.Count -eq 0) -or ($Only -contains $Id) }

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
    foreach ($k in Get-ChildItem $root) {
      $name = Get-RegValue $k.PSPath 'DisplayName'
      if ($k.PSChildName -eq $GUID -or $k.PSChildName -eq 'SpaceKnife' -or ("$name" -like 'SpaceKnife*')) {
        $out += [pscustomobject]@{ hive = ($root -split ':')[0]; key = $k.PSChildName; version = (Get-RegValue $k.PSPath 'DisplayVersion'); uninstall = (Get-RegValue $k.PSPath 'UninstallString'); quiet = (Get-RegValue $k.PSPath 'QuietUninstallString'); location = (Get-RegValue $k.PSPath 'InstallLocation') }
      }
    }
  }
  return ,$out
}
function Get-InstallLocation([string]$Hive) { return Get-RegValue "$($Hive):\Software\$GUID" 'InstallLocation' }
function Get-Lnk([string]$Path) {
  if (-not (Test-Path -LiteralPath $Path)) { return [pscustomobject]@{ exists = $false; target = $null; aumid = $null; id = $null } }
  $t = (New-Object -ComObject WScript.Shell).CreateShortcut($Path).TargetPath
  return [pscustomobject]@{ exists = $true; target = $t; aumid = [SkNative]::Aumid($Path); id = [SkNative]::FileId($Path) }
}
function Get-AppProcs([string]$Dir) {
  $d = $Dir.TrimEnd('\') + '\'
  return @(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($d, [StringComparison]::OrdinalIgnoreCase) } |
    ForEach-Object { [pscustomobject]@{ pid = $_.ProcessId; path = $_.ExecutablePath; cmd = $_.CommandLine; elevated = [SkNative]::Elevated([int]$_.ProcessId); started = $_.CreationDate } })
}
function Wait-App([string]$Dir, [int]$Seconds = 60) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
    $p = Get-AppProcs $Dir | Where-Object { $_.path -like '*\SpaceKnife.exe' }
    if ($p) { return $p }
    Start-Sleep -Milliseconds 500
  }
  return @()
}
# an NSIS uninstaller started without _?= (as Windows starts it) copies itself to
# %TEMP%\~nsu*.tmp\Au_.exe, starts the copy with _?=<its folder>\ and exits before the copy has done
# anything: wait (at most $Seconds) until no process runs from $Dir or has it on its command line
# (the copy), as packaging/lib.mjs untilUninstalled does; a copy whose command line cannot be read
# (elevated, seen from a script that is not) counts by its name. $true when it has ended.
function Wait-Uninstaller([string]$Dir, [int]$Seconds = 180) {
  $d = [IO.Path]::GetFullPath($Dir).TrimEnd('\')
  $sw = [Diagnostics.Stopwatch]::StartNew()
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
  Note ("still at work: " + (($busy | ForEach-Object { "$($_.ProcessId) $($_.Name) $($_.CommandLine)" }) -join '; '))
  return $false
}
function Sha([string]$Path) { return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLower() }
function Is-Under([string]$Path, [string]$Dir) { $p = [IO.Path]::GetFullPath($Path).TrimEnd('\'); $d = [IO.Path]::GetFullPath($Dir).TrimEnd('\'); return $p.Equals($d, 'OrdinalIgnoreCase') -or $p.StartsWith($d + '\', 'OrdinalIgnoreCase') }

# ------------------------------------------------------------------ actions
function Run-Wait([string]$Exe, [string]$Arguments, [switch]$Elevated, [int]$Timeout = 900) {
  Note "run $(Split-Path $Exe -Leaf) $Arguments$(if ($Elevated) { ' (elevated)' })"
  if ($Elevated -and -not (Get-Elevated)) { $p = Start-Process -FilePath $Exe -ArgumentList $Arguments -Verb RunAs -PassThru }
  else { $p = Start-Process -FilePath $Exe -ArgumentList $Arguments -PassThru }
  if (-not $p.WaitForExit($Timeout * 1000)) { try { $p.Kill() } catch {} ; return -999 }
  return $p.ExitCode
}
# what 0.3.4 (Node spawn) and 0.3.5 (Rust Command) do: CreateProcess, no shell, not elevated by itself
function Spawn-Wait([string]$Exe, [string]$Arguments, [int]$Timeout = 900) {
  Note "spawn (CreateProcess) $(Split-Path $Exe -Leaf) $Arguments"
  $psi = New-Object Diagnostics.ProcessStartInfo $Exe, $Arguments
  $psi.UseShellExecute = $false
  try { $p = [Diagnostics.Process]::Start($psi) } catch [ComponentModel.Win32Exception] { return (-1 * $_.Exception.NativeErrorCode) }
  if (-not $p.WaitForExit($Timeout * 1000)) { try { $p.Kill() } catch {} ; return -999 }
  return $p.ExitCode
}
function Stop-SpaceKnife {
  Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('SpaceKnife.exe', 'spaceknife-backend.exe', 'spaceknife-cli.exe', 'SpaceKnife.com') } | ForEach-Object { try { Stop-Process -Id $_.ProcessId -Force -ErrorAction Stop } catch {} }
  Start-Sleep -Seconds 1
}
function Reset-SpaceKnife {
  Stop-SpaceKnife
  foreach ($e in Get-Entries) {
    if ($e.quiet -or $e.uninstall) {
      $cmd = if ($e.quiet) { $e.quiet } else { "$($e.uninstall) /S" }
      if ($cmd -match '^"([^"]+)"\s*(.*)$') { $exe = $Matches[1]; $rest = $Matches[2] } else { $exe = $cmd; $rest = '/S' }
      if (Test-Path -LiteralPath $exe) {
        [void](Run-Wait $exe "$rest --delete-app-data" -Elevated:($e.hive -eq 'HKLM') -Timeout 300)
        [void](Wait-Uninstaller (Split-Path $exe) 300)
      }
    }
  }
  Start-Sleep -Seconds 3
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
    $leftovers = @($MACHDIR, $SM_ALL, $DT_ALL) | Where-Object { Test-Path -LiteralPath $_ }
    if ($leftovers) { $ps = "Remove-Item -LiteralPath '$($leftovers -join "','")' -Recurse -Force; Remove-Item -LiteralPath 'HKLM:\SOFTWARE\$GUID','HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$GUID' -Recurse -Force -ErrorAction SilentlyContinue"; Start-Process powershell -Verb RunAs -Wait -ArgumentList "-NoProfile -Command $ps" }
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
    Assert "$When : $k marker kept" ((Test-Path -LiteralPath $p) -and ((Sha $p) -eq $Seed[$k].sha)) $p
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
function Assert-Layout034([string]$Dir, [string]$Hive) {
  Assert '0.3.4 entry' (@(Get-Entries | Where-Object { $_.key -eq $GUID -and $_.hive -eq $Hive -and $_.version -eq '0.3.4' }).Count -eq 1) (Get-Entries | ConvertTo-Json -Compress)
  Assert '0.3.4 InstallLocation' ((Get-InstallLocation $Hive) -eq $Dir) (Get-InstallLocation $Hive)
  Assert '0.3.4 files' ((Test-Path "$Dir\SpaceKnife.exe") -and (Test-Path "$Dir\SpaceKnife.com") -and (Test-Path "$Dir\Uninstall SpaceKnife.exe") -and (Test-Path "$Dir\resources\app.asar")) $Dir
}
function Assert-Updated([string]$Dir, [string]$Hive, [string]$Version, [hashtable]$LnkBefore, [string[]]$Lnks, [datetime]$Since, [switch]$NoRestartCheck) {
  $entries = Get-Entries
  Assert 'one SpaceKnife entry in this hive' (@($entries | Where-Object { $_.hive -eq $Hive }).Count -eq 1) ($entries | ConvertTo-Json -Compress)
  Assert "the entry is the GUID key, version $Version" (@($entries | Where-Object { $_.hive -eq $Hive -and $_.key -eq $GUID -and $_.version -eq $Version }).Count -eq 1) ($entries | ConvertTo-Json -Compress)
  Assert 'no stock Tauri keys' (-not (Test-Path "$($Hive):\Software\Microsoft\Windows\CurrentVersion\Uninstall\SpaceKnife") -and -not (Test-Path "$($Hive):\Software\DuskXi\SpaceKnife")) ''
  Assert 'same folder' ((Get-InstallLocation $Hive) -eq $Dir) (Get-InstallLocation $Hive)
  Assert 'not in the data folder' (-not (Is-Under $Dir $DATA) -and -not (Is-Under $DATA $Dir)) $Dir
  foreach ($f in $TAURI_FILES) { Assert "has $f" (Test-Path -LiteralPath (Join-Path $Dir $f)) '' }
  Assert 'has ui\' (Test-Path -LiteralPath (Join-Path $Dir 'ui')) ''
  $v = (Get-Item -LiteralPath (Join-Path $Dir 'SpaceKnife.exe')).VersionInfo.ProductVersion
  Assert "SpaceKnife.exe is $Version" ("$v" -like "$Version*") $v
  $left = @($ELECTRON_FILES | Where-Object { Test-Path -LiteralPath (Join-Path $Dir $_) })
  Assert 'no Electron files left' ($left.Count -eq 0) ($left -join ', ')
  Assert 'no staging folder left' (-not (Test-Path -LiteralPath "$Dir.sk-staging")) "$Dir.sk-staging"
  foreach ($l in $Lnks) {
    $now = Get-Lnk $l
    if ($LnkBefore -and $LnkBefore.ContainsKey($l)) {
      $b = $LnkBefore[$l]
      if ($b.exists) {
        Assert "$(Split-Path $l -Leaf) [$l] still there" $now.exists ''
        Assert "  the same file (pins keep working)" ($now.id -eq $b.id) "$($b.id) -> $($now.id)"
      } else { Assert "$l not made again (the user removed it)" (-not $now.exists) '' }
    }
    if ($now.exists) {
      Assert "  points at $Dir\SpaceKnife.exe" ($now.target -eq (Join-Path $Dir 'SpaceKnife.exe')) $now.target
      Assert "  AUMID $AUMID" ($now.aumid -eq $AUMID) "$($now.aumid)"
    }
  }
  if (-not $NoRestartCheck) {
    $app = @(Wait-App $Dir 60 | Where-Object { $_.started -ge $Since })
    Assert 'the app was started again' ($app.Count -ge 1) ''
    if ($app.Count -ge 1) {
      Assert '  with --updated' ($app[0].cmd -like '*--updated*') $app[0].cmd
      if ($script:uacOn) { Assert '  not elevated' ($app[0].elevated -eq 0) "elevated=$($app[0].elevated)" } else { Skip '  not elevated' 'UAC is off: every process is elevated' }
    }
  }
}

# ------------------------------------------------------------------ environment
$stamp = (Get-Date).ToString('yyyyMMdd-HHmmss')
$os = Get-CimInstance Win32_OperatingSystem
$uac = Get-RegValue 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System' 'EnableLUA'
$consent = Get-RegValue 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System' 'ConsentPromptBehaviorAdmin'
$uacOn = ($uac -eq 1) -and -not (Get-Elevated)
$envInfo = [ordered]@{
  os = "$($os.Caption) $($os.Version) build $($os.BuildNumber)"; elevated = (Get-Elevated); enableLUA = $uac; consentPromptBehaviorAdmin = $consent
  webview2 = (Get-WebView2); github = ($env:GITHUB_ACTIONS -eq 'true'); user = $env:USERNAME
  installers = [ordered]@{}
}
foreach ($f in @($OLD, $NEW, $NEXT, $STOCK)) { if (Test-Path -LiteralPath $f) { $envInfo.installers[(Split-Path $f -Leaf)] = (Sha $f) } }
if (-not (Test-Path -LiteralPath $OLD)) { throw "missing $OLD" }
if ((Sha $OLD) -ne $OLD_SHA) { throw "$OLD is not the v0.3.4 release file" }
if (-not (Test-Path -LiteralPath $NEW)) { throw "missing $NEW" }
Write-Host "environment: $($envInfo | ConvertTo-Json -Compress)"

# ------------------------------------------------------------------ S0 launchability
if (Want 'S0') {
  Start-Scenario 'S0' 'CreateProcess can start the installer (0.3.4 and 0.3.5 use CreateProcess)'
  if ($uacOn) {
    Assert 'the 0.3.4 installer starts (asInvoker)' ([SkNative]::Launchable($OLD) -eq 0) ''
    Assert 'the fork starts (asInvoker)' ([SkNative]::Launchable($NEW) -eq 0) ''
    if (Test-Path -LiteralPath $STOCK) { $e = [SkNative]::Launchable($STOCK); Assert 'a stock "both" build does NOT start (740)' ($e -eq 740) "error $e" } else { Skip 'stock "both" build' 'no stock-both-Setup.exe in the share' }
  } else { Skip 'launchability' 'needs a non-elevated admin with UAC on' }
  End-Scenario
}

# ------------------------------------------------------------------ S1 per user
if (Want 'S1') {
  Start-Scenario 'S1' 'per user: 0.3.4 -> 0.3.5 with 0.3.4''s seamless-update command'
  Reset-SpaceKnife
  $wv = Get-WebView2
  Assert 'installed 0.3.4 /currentuser' ((Run-Wait $OLD '/S /currentuser') -eq 0) ''
  Assert-Layout034 $USERDIR 'HKCU'
  $seed = Seed-Data
  Pin-Copy $SM_USER
  $lnks = @($SM_USER, $DT_USER, $PIN)
  $before = @{}; foreach ($l in $lnks) { $before[$l] = Get-Lnk $l }
  Assert '0.3.4 shortcuts carry the AUMID' (($before[$SM_USER].aumid -eq $AUMID) -and ($before[$DT_USER].aumid -eq $AUMID)) "$($before[$SM_USER].aumid) / $($before[$DT_USER].aumid)"
  $setup = Stage-Update $NEW
  $since = Get-Date
  $code = Spawn-Wait $setup '--updated /S --force-run /currentuser'
  Assert 'the update exits 0' ($code -eq 0) "exit $code"
  Assert-Updated $USERDIR 'HKCU' $NewVersion $before $lnks $since
  Assert-Seeded $seed @('data', 'cache', 'electron') 'after the update'
  if (-not $wv) { Assert 'WebView2 installed by the update' ([bool](Get-WebView2)) "$(Get-WebView2)" }
  $log = Join-Path $env:TEMP 'SpaceKnife-install.log'
  if (Test-Path $log) { Note ("install log: " + ((Get-Content $log -Encoding Unicode -Tail 30) -join ' | ')) }
  End-Scenario

  if (Want 'S1U') {
    Start-Scenario 'S1U' 'uninstall: data kept by default, gone with --delete-app-data'
    Stop-SpaceKnife
    Assert 'uninstall /S /currentuser exits 0' ((Run-Wait (Join-Path $USERDIR 'uninstall.exe') '/S /currentuser') -eq 0) ''
    Assert '  its copy in %TEMP% has ended' (Wait-Uninstaller $USERDIR) ''
    Assert 'program gone' (-not (Test-Path -LiteralPath (Join-Path $USERDIR 'SpaceKnife.exe'))) ''
    Assert 'entry gone' (@(Get-Entries).Count -eq 0) (Get-Entries | ConvertTo-Json -Compress)
    Assert 'Start menu and desktop shortcuts gone' (-not (Test-Path $SM_USER) -and -not (Test-Path $DT_USER)) ''
    Assert-Seeded $seed @('data') 'after uninstalling'
    Assert 'caches gone (Cache, updates, the shell folder, 0.3.4''s profile)' (-not (Test-Path (Join-Path $DATA 'Cache')) -and -not (Test-Path (Join-Path $DATA 'updates')) -and -not (Test-Path $SHELLDIR) -and -not (Test-Path (Join-Path $ROAM 'SpaceKnife\electron'))) ''
    Assert 'fresh install /S' ((Run-Wait $NEW '/S') -eq 0) ''
    Assert '  into Programs\SpaceKnife' ((Get-InstallLocation 'HKCU') -eq $USERDIR) (Get-InstallLocation 'HKCU')
    Assert '  Start menu shortcut with AUMID' ((Get-Lnk $SM_USER).aumid -eq $AUMID) ''
    Stop-SpaceKnife
    Assert 'uninstall --delete-app-data' ((Run-Wait (Join-Path $USERDIR 'uninstall.exe') '/S /currentuser --delete-app-data') -eq 0) ''
    Assert '  its copy in %TEMP% has ended' (Wait-Uninstaller $USERDIR) ''
    Assert 'data folder gone' (-not (Test-Path -LiteralPath $DATA)) ''
    End-Scenario
  }
}

# ------------------------------------------------------------------ S2 all users
if (Want 'S2') {
  Start-Scenario 'S2' 'all users: 0.3.4 /allusers -> 0.3.5 started non-elevated with /allusers'
  if (-not $uacOn) { Skip 'S2' 'needs a non-elevated admin with UAC on (ConsentPromptBehaviorAdmin=0)' }
  else {
    Reset-SpaceKnife
    Assert 'installed 0.3.4 /allusers' ((Run-Wait $OLD '/S /allusers' -Elevated) -eq 0) ''
    Assert-Layout034 $MACHDIR 'HKLM'
    $seed = Seed-Data
    $lnks = @($SM_ALL, $DT_ALL)
    $before = @{}; foreach ($l in $lnks) { $before[$l] = Get-Lnk $l }
    $setup = Stage-Update $NEW
    $since = Get-Date
    $code = Spawn-Wait $setup '--updated /S --force-run /allusers'
    Assert 'the update exits 0 (it elevated itself)' ($code -eq 0) "exit $code"
    Assert-Updated $MACHDIR 'HKLM' $NewVersion $before $lnks $since
    Assert 'no per-user entry appeared' (@(Get-Entries | Where-Object { $_.hive -eq 'HKCU' }).Count -eq 0) ''
    Assert-Seeded $seed @('data', 'cache', 'electron') 'after the update'
  }
  End-Scenario
}

# ------------------------------------------------------------------ S4 running app, locked files
if (Want 'S4') {
  Start-Scenario 'S4' 'update while 0.3.4 runs; a lock the installer cannot lift'
  Reset-SpaceKnife
  [void](Run-Wait $OLD '/S /currentuser')
  $seed = Seed-Data
  $env:SPACEKNIFE_START_MINIMIZED = '1'
  $old = Start-Process -FilePath (Join-Path $USERDIR 'SpaceKnife.exe') -PassThru
  Start-Sleep -Seconds 15
  Note ("running before: " + ((Get-AppProcs $USERDIR | ForEach-Object { Split-Path $_.path -Leaf }) -join ', '))
  # a second, unrelated holder of one of 0.3.4's files (the installer may close it)
  $holder = Start-Process powershell -PassThru -WindowStyle Hidden -ArgumentList "-NoProfile -Command `$f=[IO.File]::Open('$USERDIR\resources\app.asar','Open','Read','Read'); Start-Sleep 600"
  Start-Sleep -Seconds 2
  $since = Get-Date
  $code = Spawn-Wait (Stage-Update $NEW) '--updated /S --force-run /currentuser'
  Assert 'the update exits 0' ($code -eq 0) "exit $code"
  Assert 'no 0.3.4 process left' (@(Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'spaceknife-backend.exe' }).Count -eq 0) ''
  Assert-Updated $USERDIR 'HKCU' $NewVersion $null @() $since
  Assert-Seeded $seed @('data', 'cache', 'electron') 'after the update'
  try { Stop-Process -Id $holder.Id -Force -ErrorAction SilentlyContinue } catch {}
  if ($uacOn) {
    Note 'elevated lock holder: the installer (not elevated) cannot close it'
    Reset-SpaceKnife
    [void](Run-Wait $OLD '/S /currentuser')
    $h2 = Start-Process powershell -Verb RunAs -PassThru -WindowStyle Hidden -ArgumentList "-NoProfile -Command `$f=[IO.File]::Open('$USERDIR\resources\app.asar','Open','Read','Read'); Start-Sleep 300"
    Start-Sleep -Seconds 3
    $since = Get-Date
    $code = Spawn-Wait (Stage-Update $NEW) '--updated /S --force-run /currentuser'
    Assert 'the update fails (non-zero)' ($code -ne 0) "exit $code"
    Assert-Layout034 $USERDIR 'HKCU'
    $back = @(Wait-App $USERDIR 30 | Where-Object { $_.started -ge $since })
    Assert '0.3.4 was started again' ($back.Count -ge 1) ''
    Stop-SpaceKnife
    Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList "-NoProfile -Command Stop-Process -Id $($h2.Id) -Force"
  } else { Skip 'elevated lock holder' 'needs UAC on' }
  End-Scenario
}

# ------------------------------------------------------------------ S5 WebView2
if (Want 'S5') {
  Start-Scenario 'S5' 'WebView2 missing (Windows 10)'
  if ($RemoveWebView2 -and (Get-WebView2) -and ($os.BuildNumber -lt 22000)) {
    $u = Get-RegValue 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Microsoft EdgeWebView' 'UninstallString'
    if ($u -and $u -match '^"([^"]+)"\s*(.*)$') { Note "removing WebView2: $u --force-uninstall"; Start-Process -FilePath $Matches[1] -ArgumentList "$($Matches[2]) --force-uninstall" -Verb RunAs -Wait }
  }
  if (Get-WebView2) { Skip 'S5' "WebView2 $(Get-WebView2) is installed (Windows 11 always has it; on Windows 10 pass -RemoveWebView2)" }
  else {
    Reset-SpaceKnife
    [void](Run-Wait $OLD '/S /currentuser')
    $seed = Seed-Data
    if ($Offline) {
      Note 'blocking outbound traffic: the WebView2 bootstrapper must fail and 0.3.4 must stay'
      New-NetFirewallRule -DisplayName 'sk-e2e-offline' -Direction Outbound -Action Block | Out-Null
      try {
        $since = Get-Date
        $code = Spawn-Wait (Stage-Update $NEW) '--updated /S --force-run /currentuser'
        Assert 'the update fails' ($code -ne 0) "exit $code"
        Assert-Layout034 $USERDIR 'HKCU'
        Assert '0.3.4 was started again' (@(Wait-App $USERDIR 30 | Where-Object { $_.started -ge $since }).Count -ge 1) ''
        Stop-SpaceKnife
      } finally { Remove-NetFirewallRule -DisplayName 'sk-e2e-offline' }
    }
    $since = Get-Date
    $code = Spawn-Wait (Stage-Update $NEW) '--updated /S --force-run /currentuser'
    Assert 'the update exits 0' ($code -eq 0) "exit $code"
    Assert 'WebView2 is installed now' ([bool](Get-WebView2)) "$(Get-WebView2)"
    Assert-Updated $USERDIR 'HKCU' $NewVersion $null @($SM_USER) $since
    Assert-Seeded $seed @('data') 'after the update'
  }
  End-Scenario
}

# ------------------------------------------------------------------ S6 downgrade and reinstall
if (Want 'S6') {
  Start-Scenario 'S6' '0.3.4 over 0.3.5 (downgrade), 0.3.5 again, 0.3.5 over itself'
  Reset-SpaceKnife
  [void](Run-Wait $NEW '/S')
  $seed = Seed-Data
  Stop-SpaceKnife
  $code = Run-Wait $OLD '/S /currentuser'
  Assert '0.3.4 installer exits 0' ($code -eq 0) "exit $code"
  $e = Get-Entries
  Assert 'one entry, 0.3.4' ((@($e).Count -eq 1) -and ($e[0].version -eq '0.3.4') -and ($e[0].key -eq $GUID)) ($e | ConvertTo-Json -Compress)
  Assert-Layout034 $USERDIR 'HKCU'
  $tauriLeft = @('spaceknife-cli.exe', 'uninstall.exe', 'ui\index.html', 'licenses') | Where-Object { Test-Path -LiteralPath (Join-Path $USERDIR $_) }
  Assert 'no Tauri files left' (@($tauriLeft).Count -eq 0) ($tauriLeft -join ', ')
  Assert '0.3.4 shortcut points at its program' ((Get-Lnk $SM_USER).target -eq (Join-Path $USERDIR 'SpaceKnife.exe')) ''
  Assert-Seeded $seed @('data') 'after the downgrade'
  Stop-SpaceKnife
  $since = Get-Date
  $code = Spawn-Wait (Stage-Update $NEW) '--updated /S --force-run /currentuser'
  Assert 'up to 0.3.5 again' ($code -eq 0) "exit $code"
  Assert-Updated $USERDIR 'HKCU' $NewVersion $null @($SM_USER) $since
  Stop-SpaceKnife
  $code = Run-Wait $NEW '/S'
  Assert '0.3.5 over 0.3.5 exits 0' ($code -eq 0) "exit $code"
  Assert-Updated $USERDIR 'HKCU' $NewVersion $null @($SM_USER) (Get-Date) -NoRestartCheck
  End-Scenario
}

# ------------------------------------------------------------------ S7 both scopes
if (Want 'S7') {
  Start-Scenario 'S7' 'per-user and per-machine 0.3.4 side by side'
  if (-not $uacOn) { Skip 'S7' 'needs a non-elevated admin with UAC on' }
  else {
    Reset-SpaceKnife
    [void](Run-Wait $OLD '/S /allusers' -Elevated)
    [void](Run-Wait $OLD '/S /currentuser')
    Assert 'two 0.3.4 entries' (@(Get-Entries | Where-Object { $_.version -eq '0.3.4' }).Count -eq 2) (Get-Entries | ConvertTo-Json -Compress)
    $since = Get-Date
    Assert 'per-user update' ((Spawn-Wait (Stage-Update $NEW) '--updated /S --force-run /currentuser') -eq 0) ''
    $e = Get-Entries
    Assert 'HKCU 0.3.5, HKLM still 0.3.4' ((@($e | Where-Object { $_.hive -eq 'HKCU' -and $_.version -eq $NewVersion }).Count -eq 1) -and (@($e | Where-Object { $_.hive -eq 'HKLM' -and $_.version -eq '0.3.4' }).Count -eq 1)) ($e | ConvertTo-Json -Compress)
    Assert 'Program Files 0.3.4 untouched' (Test-Path "$MACHDIR\resources\app.asar") ''
    Stop-SpaceKnife
    Assert 'per-machine update' ((Spawn-Wait (Stage-Update $NEW) '--updated /S --force-run /allusers') -eq 0) ''
    $e = Get-Entries
    Assert 'both 0.3.5' (@($e | Where-Object { $_.version -eq $NewVersion }).Count -eq 2) ($e | ConvertTo-Json -Compress)
    Stop-SpaceKnife
  }
  End-Scenario
}

# ------------------------------------------------------------------ S8 fork -> fork
if (Want 'S8') {
  Start-Scenario 'S8' 'the fork -> a newer fork with the update command every version uses'
  if (-not (Test-Path -LiteralPath $NEXT)) { Skip 'S8' 'no -NextVersion installer in the share' }
  else {
    Reset-SpaceKnife
    [void](Run-Wait $NEW '/S')
    $seed = Seed-Data
    Pin-Copy $SM_USER
    Set-Content -LiteralPath (Join-Path $USERDIR 'ui\stale-from-0.3.5.js') -Value '//'
    $lnks = @($SM_USER, $DT_USER, $PIN); $before = @{}; foreach ($l in $lnks) { $before[$l] = Get-Lnk $l }
    Stop-SpaceKnife
    $since = Get-Date
    $code = Spawn-Wait (Stage-Update $NEXT) '--updated /S --force-run /currentuser'
    Assert 'exit 0' ($code -eq 0) "exit $code"
    Assert-Updated $USERDIR 'HKCU' $NextVersion $before $lnks $since
    Assert 'stale ui file removed' (-not (Test-Path (Join-Path $USERDIR 'ui\stale-from-0.3.5.js'))) ''
    Assert-Seeded $seed @('data', 'cache', 'electron') 'after the update'
  }
  End-Scenario
}

# ------------------------------------------------------------------ S9 custom folder
if (Want 'S9') {
  Start-Scenario 'S9' '0.3.4 in a folder of its own, update without a scope flag'
  Reset-SpaceKnife
  $dir = 'C:\SK Test\SpaceKnife'
  [void](Run-Wait $OLD "/S /currentuser /D=$dir")
  Assert-Layout034 $dir 'HKCU'
  $since = Get-Date
  $code = Spawn-Wait (Stage-Update $NEW) '--updated /S --force-run'
  Assert 'exit 0' ($code -eq 0) "exit $code"
  Assert-Updated $dir 'HKCU' $NewVersion $null @($SM_USER) $since
  End-Scenario
}

# ------------------------------------------------------------------ S10 0.3.4 in the data folder
if (Want 'S10') {
  Start-Scenario 'S10' '0.3.4 installed in %LOCALAPPDATA%\SpaceKnife (the data folder)'
  Reset-SpaceKnife
  $seed = Seed-Data
  [void](Run-Wait $OLD "/S /currentuser /D=$DATA")
  Assert '0.3.4 is in the data folder' ((Get-InstallLocation 'HKCU') -eq $DATA) (Get-InstallLocation 'HKCU')
  Assert-Seeded $seed @('data') 'after installing 0.3.4 there'
  $since = Get-Date
  $code = Spawn-Wait (Stage-Update $NEW) '--updated /S --force-run /currentuser'
  Assert 'exit 0' ($code -eq 0) "exit $code"
  Assert-Seeded $seed @('data') 'after the update'
  Assert 'moved to Programs\SpaceKnife' ((Get-InstallLocation 'HKCU') -eq $USERDIR) (Get-InstallLocation 'HKCU')
  $left = @($ELECTRON_FILES | Where-Object { Test-Path -LiteralPath (Join-Path $DATA $_) })
  Assert 'no Electron files in the data folder' ($left.Count -eq 0) ($left -join ', ')
  Assert 'shortcut retargeted' ((Get-Lnk $SM_USER).target -eq (Join-Path $USERDIR 'SpaceKnife.exe')) (Get-Lnk $SM_USER).target
  End-Scenario
}

# ------------------------------------------------------------------ S11 console launcher
if (Want 'S11') {
  Start-Scenario 'S11' 'SpaceKnife.com --headless --help'
  $loc = Get-InstallLocation 'HKCU'; if (-not $loc) { $loc = Get-InstallLocation 'HKLM' }
  if (-not $loc -or -not (Test-Path (Join-Path $loc 'SpaceKnife.com'))) { [void](Run-Wait $NEW '/S'); $loc = Get-InstallLocation 'HKCU' }
  $out = & (Join-Path $loc 'SpaceKnife.com') --headless --help 2>&1 | Out-String
  Assert 'exit 0' ($LASTEXITCODE -eq 0) "exit $LASTEXITCODE"
  Assert 'prints headless usage' ($out -match '--listen' -or $out -match 'headless') ($out.Substring(0, [Math]::Min(300, $out.Length)))
  End-Scenario
}

# ------------------------------------------------------------------ S12 Defender
if (Want 'S12') {
  Start-Scenario 'S12' 'Microsoft Defender'
  $mp = Join-Path $env:ProgramFiles 'Windows Defender\MpCmdRun.exe'
  if (-not (Test-Path $mp)) { Skip 'S12' 'no MpCmdRun' }
  else {
    foreach ($f in @($NEW, $NEXT) | Where-Object { Test-Path -LiteralPath $_ }) {
      $o = & $mp -Scan -ScanType 3 -File $f -DisableRemediation 2>&1 | Out-String
      Assert "$(Split-Path $f -Leaf): no threat" ($LASTEXITCODE -eq 0) ($o.Trim() -replace '\s+', ' ')
    }
    try {
      $det = Get-WinEvent -FilterHashtable @{ LogName = 'Microsoft-Windows-Windows Defender/Operational'; Id = 1116, 1117; StartTime = [datetime]::ParseExact($stamp, 'yyyyMMdd-HHmmss', $null) } -ErrorAction Stop
      Assert 'no detections during the run' (@($det).Count -eq 0) (($det | Select-Object -First 3 | ForEach-Object { $_.Message }) -join ' | ')
    } catch { Assert 'no detections during the run' $true 'no events' }
  }
  End-Scenario
}

Reset-SpaceKnife
Save-Results
$failed = @($results | Where-Object { $_.status -eq 'fail' })
Write-Host ("done: {0} scenarios, {1} failed; {2}" -f $results.Count, $failed.Count, (Join-Path $OutDir "installer-experiments-$env:COMPUTERNAME-$stamp.json"))
if ($failed.Count) { exit 1 }
