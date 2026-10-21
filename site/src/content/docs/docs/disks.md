---
title: Disks
description: Every disk in this computer, scanned or not, with its type, interface and link speed, its reads, writes and health record, the volumes on it, and what SpaceKnife knows of each volume.
---

The **Disks** page lists the physical disks of this computer: SSDs and hard disks inside it, USB drives, card readers and memory cards. For each one it shows what kind of disk it is, how it is connected and how fast that connection runs, how much has been read from and written to it, what it records about its own health, and which volumes live on it. Disks you never scanned are there too.

Open it with the **Disks** button in the title bar (left of Help and Settings, a gauge over a disk), the **Disks** card at the top of the start page's right column, or **Disks** in the [command palette](../command-palette/) (it also finds `disks`, `ssd`, `nvme`, `sata` or `usb`). It opens in its own tab and reads the disks when you open it and when you click **Refresh**; nothing on it changes or pops up by itself.

## The card on the start page

The card sums the page up: how many disks there are, their total size and the free space, a bar of the capacity by storage type, the **Fullest** volume and the **Fastest** link. When there are findings, **N findings** shows at its top right (in the warning color with a warning icon when one needs a look). Click anywhere on the card to open the page.

## The summary

The top of the page is a dashboard:

- **Physical disks**: how many, by interface. An empty card reader slot doesn't count.
- **Capacity**: the disks' total size; the ring shows how much of your volumes' space is used, and the line below says how much is used and free.
- **Volumes**: how many local volumes there are, and how many SpaceKnife knows from a scan or a snapshot.
- **Fastest link**: the fastest connection of any disk, what it carries and which disk it is.
- **Capacity by type**: a bar of the capacity by storage type, from the fastest (NVMe SSD, brightest) to the slowest (external drives and cards); the table under it gives each type's disks, size and share.
- **Findings**: what is worth knowing, the ones to look at first:

| Finding | When |
|---|---|
| **E:\ is 92% full, 38 GB left** (warning) | A volume is 90% full or more. From 80% it is listed without the warning. |
| A link below what the disk supports | See [When a link runs slower than it could](#when-a-link-runs-slower-than-it-could). A controller that carries less than three quarters of the disk's SATA link is a warning. |
| **… doesn't take TRIM** | An SSD that doesn't take TRIM: it may slow down after much writing. |
| **D:\ holds 18 GB more than its snapshot of 3 days ago** | A volume grew (or shrank) by 5 GB or more, and by 2% of its size or more, between its two newest complete snapshots. |
| **… reports a critical warning**, **… has recorded 3 media errors** (warning) | The disk's own health record holds a critical warning or media errors. See [Reads, writes and health](#reads-writes-and-health). |
| **… has used 75% of its write endurance** | An SSD estimates that 70% or more of its write endurance is used; from 90% it is a warning. |
| **… has only 8% of its spare blocks left** (warning) | An SSD's spare blocks fell to its own warning level or below. |
| **… is at 72 °C, on the hot side** | The disk reports 70 °C or more. |

Nothing tells you to scan a volume: volumes you never scanned are simply shown as such.

## What a disk shows

Under **Disks**, each disk is a card; the cards fill the window in as many columns as fit, two in a narrow window and three or four in a wide one. The cards of a row line up section by section, so the links, reads and writes, health and volumes start at the same height and read across; what isn't known is **—**, with the reason on hover.

- **Name and size**: the model the disk reports (with its vendor when the model doesn't name it), the storage type (**NVMe SSD**, **SATA SSD**, **HDD**, **USB drive**, **Memory card**, …), the size, and how the system numbers it (**Disk 3** on Windows, as in Disk Management).
- **System disk** marks the disk holding the system volume; **Removable** marks drives you can unplug or eject.
- **How full**: the ring at its left shows how much of the disk its volumes use, with the share in the middle.
- **Interface** and **current link**: **NVMe** with its PCIe link (for example **PCIe 4.0 ×4 ≈ 7.9 GB/s**), **SATA** with its rate (**SATA 6 Gb/s**), **USB** with the rate it is connected at (**USB 480 Mb/s**, **USB 5 Gb/s**). When the system also tells the most the disk supports, a meter shows the link against it: **Full speed**, or for example **50%** (of its top rate) for a PCIe 5.0 drive in a 4.0 slot.
- **Reads and writes**: a small table. The **Lifetime** row is how much was read and written over the disk's life, as the disk records it; the **Since startup** row is what the system read and wrote since it last started. Sizes pick their unit by size, as the **Size units** setting says.
- **Health**: the temperature, the power-on time and, for an SSD, **N% of its endurance used**. See [Reads, writes and health](#reads-writes-and-health).
- **Details**: the model, vendor, serial number, firmware, whether it takes TRIM, the health record (its source, power-on time, power cycles, unsafe shutdowns, media errors, spare blocks), and the system's own name for it (`PhysicalDrive3`, `nvme0n1`, `disk2`). The serial number stays on your computer and shows only here, folded away.

A disk with no medium in it, such as an empty card reader slot, shows as one line with its drive letter.

### When a link runs slower than it could

A line under the link says so when the system tells both what the disk supports and what it runs at:

| You see | What it means |
|---|---|
| The disk supports PCIe 5.0 ×4; its slot gives it PCIe 4.0 ×4 | The drive is faster than the slot it is in. Another slot, or the board, can give it more. |
| The device supports 5 Gb/s but is connected at 480 Mb/s | A USB 3 drive or reader running at USB 2 speed: a USB 2 port, a hub or a cable is in between. |
| The disk supports 6 Gb/s but is connected at 3 Gb/s | The SATA port or cable holds the disk back. |
| Through a PCIe 2.0 ×1 controller (≈ 500 MB/s) | The SATA disk hangs on an add-in controller card. When the controller carries less than the disk's SATA link, long reads and writes are held back. |

These are facts the system reports, not advice: nothing needs doing if the disk is fast enough for you.

### Reads, writes and health

SSDs and most hard disks keep their own record: how long they have been powered on, how often they were switched on, how much was read and written in all, how warm they are. SpaceKnife reads that record through the system, without administrator rights:

| You see | What it means |
|---|---|
| **Lifetime** Read 96.3 TB · Written 229.6 TB | What was read and written over the disk's whole life. Moving the disk or reinstalling the system doesn't reset it. |
| **Since startup** Read 1.2 TB · Written 310 GB | What the system read and wrote on this disk since it last started. Hover the row to see when that was. |
| **47 °C** | The temperature the disk reports, in the warning color from 70 °C. |
| **On for 336 days** | The power-on time, in hours under two days. Hover it for the exact hours. |
| **9% of its endurance used** | An NVMe SSD's estimate of the write endurance it has used. It can pass 100%: the disk still works, it has just been written more than its vendor promised. |

When the record can't be read, the card says why instead of leaving a gap:

| You see | What it means |
|---|---|
| The USB bridge doesn't pass on the disk's own record | The bridge chips in drive enclosures, USB sticks and card readers usually don't forward these queries. |
| This disk didn't answer the query for its health record | Older SATA disks often have no Device Statistics log (they report their health only through each vendor's own SMART attributes, which SpaceKnife doesn't read); some RAID drivers don't pass such queries on either. |
| The disk's own lifetime read and write totals need administrator rights here | On Linux only root can read the record. **Read with administrator rights…** on the card reads it, see [Reading with administrator rights on Linux](#reading-with-administrator-rights-on-linux); the temperature and the reads and writes since startup still show. |
| This system has no way to read the disk's own health record | On macOS only NVMe disks give their record (without administrator rights); SATA disks don't. While another app (a disk monitor, say) holds an NVMe disk's interface for this, it can't be read that time either. The reads and writes since startup still show. |

### Reading with administrator rights on Linux

Linux lets only root send a disk the commands that read this record, so the cards of NVMe disks, SATA disks and drives in enclosures have a **Read with administrator rights…** button. Only clicking it asks for rights: opening the page or refreshing it never does.

- The system's authentication dialog (polkit, through `pkexec`) asks for an administrator password. Then SpaceKnife starts its read-only helper as root; the app itself and its window never run elevated.
- One prompt reads every disk that needs it. The helper opens the disk devices read-only and sends fixed read-only commands only: NVMe's Get Log Page 02h, or SATA's IDENTIFY DEVICE and READ LOG EXT of the Device Statistics log 04h (through ATA pass-through). It reads and writes none of the data on the disk.
- The records are kept while SpaceKnife runs; hover the **Lifetime** row to see when they were read. The helper exits after 10 idle minutes; until then **Refresh** reads them again without asking.
- A disk that doesn't answer says why on its card, and the button goes away.

When rights can't be asked for, the card says why:

| You see | What it means |
|---|---|
| The user declined to grant administrator rights | You closed the password dialog. The button stays; click it again any time. |
| No authentication agent can ask for the administrator password (in an SSH session, say, or without a graphical desktop) | Desktop environments come with a polkit authentication agent; SSH sessions and systems without a desktop have none. |
| The AppImage can't run the helper as administrator; use the deb or rpm package instead | An AppImage runs from a mount only you can read, which root can't. The deb and rpm packages install to `/opt/SpaceKnife`. |
| pkexec not found: asking for administrator rights needs polkit | The system has no polkit (`pkexec`). |

### Volumes and the capacity bar

Under each disk are its volumes, in the order they sit on the disk: the drive letter or mount point, the label, the file system and how full it is (a meter in the warning color from 90%). When a disk holds more than one volume, or space outside its volumes (a recovery partition, unallocated space), a bar shows how the disk is divided; the hatched part is space no volume of yours takes.

A volume spread over several disks (Storage Spaces, LVM, RAID) is listed under each of them. Volumes on no known disk, such as network places and virtual volumes, are under **Other places**.

## What SpaceKnife knows of a volume

Under each volume, a line says what SpaceKnife knows of it from a scan of the whole volume, with a button on the volume's first line:

| You see | From |
|---|---|
| **Scanned 2 hours ago · 1,234,567 files · 1.9 TB**, the largest file classes and the largest top-level folders | A scan of the volume open in a tab. **Show** goes to that tab. |
| **Scanning · 120,000 files** | A scan of the volume still running. |
| **Snapshot 3 days ago · … files · …** | The newest [snapshot](../snapshots-compare/) of the volume, when no scan of it is open. **Open snapshot** opens it. |
| **Not scanned yet** | Neither. **Scan…** opens a start page with the volume picked, so you can check the options first. |

Only scans of a whole volume count here; a scan of a folder on it doesn't.

Once a scan of the volume is done, a bar under its line shows the mix of its largest [file classes](../tags-classes/) with their sizes, and the line after names its largest top-level folders. When a volume has two or more complete snapshots, a small line chart shows how much it held at each of the last 12, and the text next to it how much it changed since the one before the newest, for example **+18.0 GB since the snapshot of 3 days ago**.

## Where the facts come from

SpaceKnife asks the system (including for the disk's own health record, which the system passes on) and reads no files or data from the disks. None of this needs administrator rights, except a disk's own lifetime record on Linux, and only after you click the button (see above).

| System | Source |
|---|---|
| Windows | The storage queries of each disk (model, serial, bus, TRIM, the SATA speeds from the disk's identify data, NVMe identify), the device tree above it (the PCIe link of the NVMe or SATA controller, the USB hub's connection speed) and the disk extents of each volume; the disk's own record is the NVMe SMART / Health log (log page 02h) or the ATA Device Statistics log (04h) passed on by the same storage query, and the reads and writes since startup come from the disk performance counters |
| Linux | sysfs: `/sys/block`, the PCI device's `current_link_speed` and `current_link_width`, `/sys/class/ata_link/*/sata_spd`, the USB device's `speed` and `version`, and `/dev/disk/by-id`; the reads and writes since startup come from `/sys/block/*/stat`, the temperature from hwmon (the NVMe driver, or `drivetemp` for SATA disks); the disk's own lifetime record is read by the elevated helper (the NVMe admin command `NVME_IOCTL_ADMIN_CMD`, or ATA pass-through through `SG_IO` for SATA) |
| macOS | `diskutil` and `system_profiler` (NVMe, SATA and USB); the reads and writes since startup come from each disk's statistics in the I/O Registry; an NVMe disk's own record comes from IOKit's NVMe SMART interface |

What a system doesn't report stays empty, never guessed. In particular:

- Windows reports USB 3.2 Gen 2×2 (20 Gb/s) as 10 Gb/s.
- Linux doesn't say the most a SATA disk supports, only the rate it runs at; for a USB 3 device on a slower port it says it supports at least 5 Gb/s.
- On a Mac with Apple silicon, the internal SSD has no PCIe link to report.
