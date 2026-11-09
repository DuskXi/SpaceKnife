# The file-system matrix (ci.yml, job fs-matrix; docs/manage.md §2.5 「各文件系统」) on Windows:
# sk-core's ops tests and the ops_bench example run with the temporary folder on a small virtual
# disk of each file system — NTFS, ReFS (formatting it needs a Server edition: the runner's),
# exFAT, FAT32 — made with diskpart and given a drive letter, and on the runner's own C: drive
# reached over SMB (\\localhost\C$: a network location, which has no recycle bin). Moving to the
# trash uses the recycle bin of that volume; the tests and ops_bench take back what they put there.
#
# A test that needs what a file system lacks says `skipped: …` and why; those lines and the
# results go into the job summary. Every file system runs even when one fails; the script fails at
# the end if any did.
$ErrorActionPreference = 'Continue'
$summary = if ($env:GITHUB_STEP_SUMMARY) { $env:GITHUB_STEP_SUMMARY } else { 'NUL' }
if (-not $env:RUNNER_TEMP) { $env:RUNNER_TEMP = $env:TEMP }
$work = Join-Path $env:RUNNER_TEMP 'fs-matrix'
New-Item -ItemType Directory -Force $work | Out-Null
$failed = @()
$tests = @('--test', 'ops_trash', '--test', 'ops_basket', '--test', 'ops_case_twins', '--test', 'ops_paths', '--test', 'ops_locks', '--test', 'ops_fs')

# The ops tests and ops_bench (both modes) with TMP/TEMP at $Tmp; $Trash: what moving there to
# the trash must come to (ok, or no_trash where there is no recycle bin).
function Run-On([string]$Tmp, [string]$Fs, [string]$Trash) {
  $log = Join-Path $work "$Fs.log"
  $ok = $true
  Write-Host "::group::$Fs ($Tmp)"
  $env:TMP = $Tmp; $env:TEMP = $Tmp
  cargo test -p sk-core --locked @tests -- --test-threads=4 --show-output 2>&1 | Tee-Object -FilePath $log
  if ($LASTEXITCODE -ne 0) { $ok = $false }
  foreach ($mode in 'delete', 'trash') {
    $env:OPS_BENCH_EXPECT = if ($mode -eq 'trash') { $Trash } else { 'ok' }
    cargo run -q -p sk-core --locked --features probe --example ops_bench -- $Tmp items:19:400 $mode $Fs 2>&1 | Tee-Object -FilePath $log -Append | Select-String -Pattern '^RESULT|expected'
    if ($LASTEXITCODE -ne 0) { $ok = $false }
  }
  Remove-Item Env:OPS_BENCH_EXPECT
  $env:TMP = $env:RUNNER_TEMP; $env:TEMP = $env:RUNNER_TEMP
  Write-Host '::endgroup::'
  $lines = @("### $Fs", '', '```')
  $lines += Get-Content $log | Select-String -Pattern '^test result|^skipped|FAILED|panicked|trashed into|deletions wait|^ops_bench' | ForEach-Object { $_.Line }
  foreach ($r in Get-Content $log | Where-Object { $_ -like 'RESULT *' }) {
    $j = $r.Substring(7) | ConvertFrom-Json
    $lines += 'ops_bench {0}: {1} in {2:N3} s' -f $j.mode, ($j.codes | ConvertTo-Json -Compress), $j.exec_s
  }
  $lines += @('```', '')
  $lines | Add-Content -Encoding utf8 $summary
  if ($ok) { Write-Host "${Fs}: passed" } else { Write-Host "::error::${Fs}: failed"; $script:failed += $Fs }
}

# a virtual disk of $SizeMb with one volume of $Fs at drive $Letter
function New-TestVolume([string]$Fs, [string]$Letter, [int]$SizeMb) {
  $vhd = Join-Path $work "$Fs.vhdx"
  $script = Join-Path $work "$Fs.diskpart"
  @(
    "create vdisk file=`"$vhd`" maximum=$SizeMb type=expandable",
    "select vdisk file=`"$vhd`"",
    'attach vdisk',
    'create partition primary',
    "format fs=$($Fs.ToLower()) quick label=SK$($Fs.ToUpper())",
    "assign letter=$Letter"
  ) | Set-Content -Encoding ascii $script
  diskpart /s $script | Out-Host
  return (Test-Path "${Letter}:\")
}

$volumes = @(
  @{ Fs = 'NTFS'; Letter = 'M'; Size = 1024 },
  @{ Fs = 'ReFS'; Letter = 'N'; Size = 4096 },
  @{ Fs = 'exFAT'; Letter = 'O'; Size = 1024 },
  @{ Fs = 'FAT32'; Letter = 'P'; Size = 1024 }
)
foreach ($v in $volumes) {
  if (-not (New-TestVolume $v.Fs $v.Letter $v.Size)) {
    Write-Host "::error::$($v.Fs): could not make the volume"
    $failed += $v.Fs
    continue
  }
  Get-Volume -DriveLetter $v.Letter | Format-Table DriveLetter, FileSystemType, DriveType, Size -AutoSize | Out-Host
  $tmp = "$($v.Letter):\tmp"
  New-Item -ItemType Directory -Force $tmp | Out-Null
  Run-On $tmp $v.Fs 'ok'
}

# a network location: the runner's own C: over SMB
$share = '\\localhost\C$' + (Join-Path $env:RUNNER_TEMP 'fs-smb').Substring(2)
New-Item -ItemType Directory -Force (Join-Path $env:RUNNER_TEMP 'fs-smb') | Out-Null
if (Test-Path $share) {
  Run-On $share 'SMB' 'no_trash'
} else {
  Write-Host "::warning::SMB: $share cannot be reached, not tested"
  @('### SMB', '', "not tested: $share cannot be reached", '') | Add-Content -Encoding utf8 $summary
}

if ($failed.Count -gt 0) {
  Write-Host "::error::failed on: $($failed -join ', ')"
  exit 1
}
