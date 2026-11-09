#!/usr/bin/env bash
# The file-system matrix (ci.yml, job fs-matrix; docs/manage.md §2.5 「各文件系统」) on Linux and
# macOS: sk-core's ops tests and the ops_bench example run with the temporary folder on a small
# volume of each file system — Linux: ext4, xfs, btrfs, vfat, exfat, ntfs3 (loop images, sudo);
# macOS: APFS, HFS+ (journaled), FAT32, ExFAT (hdiutil images) — mounted so that this user can
# write everywhere on it. Moving to the trash uses the real trash of that volume
# (SPACEKNIFE_FS_MATRIX=1): freedesktop's `$topdir/.Trash-$uid`, macOS's `.Trashes/$uid`.
#
# A test that needs what a file system lacks says `skipped: …` and why; those lines and the
# results go into the job summary. Every file system runs even when one fails; the script fails
# at the end if any did. A file system this runner's kernel cannot mount is a warning.
set -uo pipefail

summary=${GITHUB_STEP_SUMMARY:-/dev/null}
work=${RUNNER_TEMP:-/tmp}/fs-matrix
mkdir -p "$work"
failed=()
tests=(--test ops_trash --test ops_basket --test ops_case_twins --test ops_paths --test ops_locks --test ops_fs)

# The ops tests and two ops_bench runs per mode with TMPDIR in `$1` (a folder on the volume);
# `$2` names the file system.
run_on() {
  local tmp=$1 fs=$2 log="$work/$2.log" ok=1
  echo "::group::$fs ($tmp)"
  if ! TMPDIR=$tmp SPACEKNIFE_FS_MATRIX=1 cargo test -p sk-core --locked "${tests[@]}" -- --test-threads=4 --show-output 2>&1 | tee "$log"; then
    ok=0
  fi
  for mode in delete trash; do
    if ! TMPDIR=$tmp OPS_BENCH_EXPECT=ok cargo run -q -p sk-core --locked --features probe --example ops_bench -- "$tmp" items:19:400 "$mode" "$fs" 2>&1 | tee -a "$log" | grep -E '^RESULT|expected'; then
      ok=0
    fi
  done
  echo "::endgroup::"
  {
    echo "### $fs"
    echo
    echo '```'
    grep -E '^test result|^skipped|FAILED|panicked|trashed into|deletions wait|^ops_bench' "$log"
    python3 -c 'import json, sys
for line in sys.stdin:
    if line.startswith("RESULT "):
        r = json.loads(line[7:])
        print("ops_bench {}: {} in {:.3f} s".format(r["mode"], r["codes"], r["exec_s"]))' < "$log"
    echo '```'
    echo
  } >> "$summary"
  if [ "$ok" = 1 ]; then echo "$fs: passed"; else echo "::error::$fs: failed"; failed+=("$fs"); fi
}

if [ "$(uname)" = Linux ]; then
  uid=$(id -u)
  gid=$(id -g)
  extra_modules=0
  # a kernel module for the file system: the runner's kernel may keep it in linux-modules-extra
  module() {
    grep -qw "$1" /proc/filesystems && return 0
    sudo modprobe "$1" 2> /dev/null && return 0
    if [ "$extra_modules" = 0 ]; then
      extra_modules=1
      sudo apt-get install -y -q "linux-modules-extra-$(uname -r)" > /dev/null 2>&1 || true
    fi
    sudo modprobe "$1" 2> /dev/null
  }
  for fs in ext4 xfs btrfs vfat exfat ntfs3; do
    img="$work/$fs.img"
    mnt="/mnt/sk-$fs"
    if ! module "$fs"; then
      echo "::warning::$fs: this runner's kernel cannot mount it, not tested"
      printf '### %s\n\nnot tested: the kernel cannot mount it\n\n' "$fs" >> "$summary"
      continue
    fi
    truncate -s 512M "$img"
    case $fs in
      ext4) mkfs.ext4 -q -F "$img" ;;
      xfs) mkfs.xfs -q -f "$img" ;;
      btrfs) mkfs.btrfs -q -f "$img" ;;
      vfat) mkfs.vfat -F 32 "$img" > /dev/null ;;
      exfat) mkfs.exfat "$img" > /dev/null ;;
      ntfs3) mkfs.ntfs -Q -F -q "$img" ;;
    esac
    sudo mkdir -p "$mnt"
    case $fs in
      ext4 | xfs) sudo mount -o loop "$img" "$mnt" && sudo chown "$uid:$gid" "$mnt" ;;
      # (an empty subvolume may be removed by its owner: the subvolume test's cleanup)
      btrfs) sudo mount -o loop,user_subvol_rm_allowed "$img" "$mnt" && sudo chown "$uid:$gid" "$mnt" ;;
      vfat) sudo mount -t vfat -o "loop,uid=$uid,gid=$gid,utf8" "$img" "$mnt" ;;
      exfat) sudo mount -t exfat -o "loop,uid=$uid,gid=$gid,iocharset=utf8" "$img" "$mnt" ;;
      ntfs3) sudo mount -t ntfs3 -o "loop,uid=$uid,gid=$gid,iocharset=utf8" "$img" "$mnt" ;;
    esac || { echo "::error::$fs: could not mount"; failed+=("$fs"); continue; }
    findmnt -no FSTYPE,OPTIONS "$mnt"
    mkdir -p "$mnt/tmp"
    run_on "$mnt/tmp" "$fs"
    sudo umount "$mnt" || sudo umount -l "$mnt"
    rm -f "$img"
  done
else
  # macOS: hdiutil's file systems; the volumes appear in /Volumes, owned by this user
  for spec in "APFS:SKAPFS" "HFS+J:SKHFS" "MS-DOS FAT32:SKFAT32" "ExFAT:SKEXFAT"; do
    fs=${spec%%:*}
    vol=${spec##*:}
    img="$work/$vol.dmg"
    mnt="/Volumes/$vol"
    hdiutil create -quiet -size 512m -fs "$fs" -volname "$vol" -ov "$img" \
      && hdiutil attach -quiet "$img" \
      || { echo "::error::$fs: could not make or attach the image"; failed+=("$fs"); continue; }
    diskutil info "$mnt" | grep -E 'File System Personality|Owners' || true
    mkdir -p "$mnt/tmp"
    run_on "$mnt/tmp" "$fs"
    hdiutil detach -quiet "$mnt" || hdiutil detach -quiet -force "$mnt"
    rm -f "$img"
  done
fi

if [ ${#failed[@]} -gt 0 ]; then
  echo "::error::failed on: ${failed[*]}"
  exit 1
fi
