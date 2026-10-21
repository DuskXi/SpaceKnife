---
title: 安装与升级
description: Windows、macOS、Linux 各下载哪个文件，第一次打开要放行什么，怎么升级到新版本，怎么卸载。
---

SpaceKnife 发布在 [Releases 页面](https://github.com/DuskXi/SpaceKnife/releases)。每个版本都有 Windows、Linux、macOS 的安装包和便携包，以及它们的 SHA-256 校验文件。界面语言默认跟随系统（简体中文或英文），随时可以在「设置 › 常规 › 界面语言」里切换。

## 下载哪个文件

| 系统 | 文件 | 说明 |
|---|---|---|
| Windows 10 及以上，x64 | `SpaceKnife-<版本>-Setup-x64.exe` | 安装包，可装给当前用户或所有用户 |
| Windows 10 及以上，x64 | `SpaceKnife-<版本>-win-x64-portable.zip` | 便携版：解压即用，不用安装 |
| macOS 14 及以上，Apple Silicon | `SpaceKnife-<版本>-mac-arm64.dmg` | 磁盘映像 |
| macOS 14 及以上，Intel | `SpaceKnife-<版本>-mac-x64.dmg` | 磁盘映像 |
| Ubuntu 22.04 及以上 / Debian 系 | `spaceknife_<版本>_amd64.deb` | 软件包 |
| RHEL 9 及以上 / Fedora 系 | `spaceknife-<版本>.x86_64.rpm` | 软件包 |
| 其他 Linux，x64 | `SpaceKnife-<版本>-x86_64.AppImage` | 单个文件，需要 FUSE 2 |
| Linux，x64 | `SpaceKnife-<版本>-linux-x64-portable.tar.gz` | 便携版：解压即用，不需要 FUSE |

Windows 和 Linux 只有 x64 版本，没有 32 位 Windows 版。Linux 版需要 glibc 2.34 及以上（RHEL 9 的水平；Ubuntu 22.04 是 2.35）。

### 校验下载的文件

软件内的更新会自动校验（见[更新与隐私](../updates-privacy/)）。自己下载的文件，可以算出 SHA-256，和发布里校验文件（`SHA256SUMS.txt`，以及旁边的 `SHA256SUMS-portable-…`、`SHA256SUMS-mac-…`）中对应的那一行比较：

```powershell
Get-FileHash .\SpaceKnife-<版本>-Setup-x64.exe -Algorithm SHA256
```

```bash
sha256sum -c --ignore-missing SHA256SUMS.txt
```

```bash
shasum -a 256 SpaceKnife-<版本>-mac-arm64.dmg
```

## Windows

### 安装包

1. 运行 `SpaceKnife-<版本>-Setup-x64.exe`。
2. 安装包还没有代码签名，第一次运行时 SmartScreen 会提示「未知发布者」：点「更多信息」，再点「仍要运行」。
3. 选择装给谁：仅当前用户（默认，不需要管理员权限），或所有用户（需要管理员权限）。安装目录可以改。

安装向导跟随系统语言（中文或英文），会创建开始菜单和桌面快捷方式，装完可以直接启动 SpaceKnife。

:::note[仅当前用户还是所有用户]
「仅当前用户」安装在你自己的账户能写入的目录里，而 [MFT 快速扫描](../first-scan/#扫描方式)的提权只读扫描助手就从安装目录启动。希望这个程序放在只有管理员能改动的位置，就选「所有用户」安装（Program Files）。
:::

### 静默安装

脚本部署时，安装包支持常用的参数：

```powershell
.\SpaceKnife-<版本>-Setup-x64.exe /S /currentuser
```

装给整台电脑就把 `/currentuser` 换成 `/allusers`（以管理员身份运行）。静默卸载用注册表里 SpaceKnife 卸载项的 `QuietUninstallString`，也就是安装目录里的 `"Uninstall SpaceKnife.exe" /currentuser /S`。卸载程序会先把自己复制到临时目录再运行，所以命令会立即返回。静默卸载会保留快照、设置和日志；加上 `--delete-app-data` 就连它们一起删除。

### 便携版 zip

把 `SpaceKnife-<版本>-win-x64-portable.zip` 解压到自己能写入的位置（用户目录、U 盘；不要放进 Program Files），运行 `SpaceKnife.exe`。解压后第一次启动会慢几秒，Windows 要先检查这些新文件。

`SpaceKnife.exe` 旁边有一个 `data` 文件夹。只要它在，SpaceKnife 就把所有东西（快照、设置、日志、缓存）都存在里面，不写进你的用户目录。见[便携模式](../data-locations/#便携模式)。

## macOS

### 安装

打开 dmg，把 SpaceKnife 拖进「应用程序」。需要 macOS 14 及以上；Apple Silicon 用 `arm64` 的 dmg，Intel 的 Mac 用 `x64` 的 dmg。

### 第一次打开：放行一次

macOS 版还没有经过苹果公证，第一次打开会被系统拦下：

1. 打开 SpaceKnife，系统提示无法验证它是否包含恶意软件，点「完成」。
2. 打开「系统设置 › 隐私与安全性」，往下找到 SpaceKnife 已被阻止的提示，点「仍要打开」；再点一次「仍要打开」并输入密码。

之后就能正常打开。安装新版本后需要再放行一次。

也可以这样放行：

- macOS 14 上，在「应用程序」里右键 SpaceKnife，选「打开」。
- 在终端里：`xattr -dr com.apple.quarantine /Applications/SpaceKnife.app`

### 完全磁盘访问权限

没有「完全磁盘访问权限」时，SpaceKnife 读桌面、文稿、下载、iCloud 云盘和其他云盘、照片图库、其他应用的数据、外接盘和网络盘之前，macOS 都要分别询问。扫描时会一个接一个弹窗，而邮件、信息等位置根本读不到，计入[未知空间](../first-scan/#可用空间与未知空间)。

每次扫描前（「受保护的位置」为默认的「扫描前询问」时），SpaceKnife 会先检查有没有这个权限，检查本身不会弹窗。没有的话，弹出「让 SpaceKnife 一次读到全部文件」，有三种选择：

| 按钮 | 结果 |
|---|---|
| 「打开系统设置」 | 打开「完全磁盘访问权限」列表。打开 SpaceKnife 的开关（列表里没有就点 + 添加），再按提示重新打开 SpaceKnife。这次不开始扫描。 |
| 「跳过受保护的位置」 | 照常扫描，不弹窗；这些位置显示为无法访问。 |
| 「照常扫描，逐个询问」 | 照常扫描，由 macOS 逐个询问。 |

勾选「记住「跳过」或「逐个询问」的选择」再点后两个按钮之一，之后就不再弹出这个对话框。这个选择就是设置「设置 › 扫描与性能 › 受保护的位置」（「扫描前询问」「跳过，不弹窗」「照常访问」）。

:::caution[每次更新之后]
macOS 版是 ad-hoc 签名，系统把授权绑定在具体的那个构建上。装好新版本后，打开「完全磁盘访问权限」列表，把 SpaceKnife 的开关关掉再打开一次。
:::

扫描在 SpaceKnife 的后台进程里进行；macOS 仍把它的授权提示和权限记在 SpaceKnife 名下。

## Linux

### deb 与 rpm

```bash
sudo apt install ./spaceknife_<版本>_amd64.deb
```

```bash
sudo dnf install ./spaceknife-<版本>.x86_64.rpm
```

两者都把程序装在 `/opt/SpaceKnife/`，在 `/usr/bin` 里放一个 `spaceknife` 命令，并把 SpaceKnife 和它的图标加进应用菜单。依赖（桌面应用常见的那些：GTK 3、NSS 等）由包管理器自动安装。

### AppImage

```bash
chmod +x SpaceKnife-<版本>-x86_64.AppImage
./SpaceKnife-<版本>-x86_64.AppImage
```

AppImage 需要 FUSE 2（Ubuntu 22.04 上是 `libfuse2` 包）。没有的话，加 `--appimage-extract-and-run` 运行。

从 0.3.2 起，AppImage 里写有更新信息，`./SpaceKnife-<版本>-x86_64.AppImage --appimage-updateinformation` 可以打印出来。AppImage 的更新工具会读取它，配合发布里放在旁边的 `.zsync` 文件，只下载变化的部分。

### 便携版 tar.gz

```bash
tar xzf SpaceKnife-<版本>-linux-x64-portable.tar.gz
cd SpaceKnife-<版本>-linux-x64-portable
./spaceknife
```

不需要 FUSE。和 Windows 的 zip 一样，所有数据都存在程序旁边的 `data` 文件夹里（[便携模式](../data-locations/#便携模式)）。

:::caution[较新的 Ubuntu]
AppImage 和 tar.gz 里 Chromium 的 `chrome-sandbox` 没有 setuid。在限制非特权用户命名空间的发行版上（Ubuntu 23.10 起的 AppArmor 规则），它们可能启动不了，需要加 `--no-sandbox` 运行。Ubuntu 22.04 和 RHEL 9 不受影响。
:::

## 升级

SpaceKnife 每天最多检查一次新版本，有新版本时在标题栏提示，并在后台下载、校验新版本。下载好以后，更新卡片里有两个按钮：「无缝更新」（Windows 安装版、便携版和 AppImage）由 SpaceKnife 自己换上新版本并重新打开；经典方式交给安装程序、磁盘映像或安装包，或者由你自己替换。只有你点了才会安装。具体怎么提示、怎么跳过一个系列或关掉检查，见[更新与隐私](../updates-privacy/)。

不管用哪种方式升级，扫描、快照和设置都会保留：它们不在程序目录里（便携版则在 `data` 文件夹里）。

| 安装方式 | 新版本怎么替换旧版本 |
|---|---|
| Windows 安装包 | 在更新卡片里点「无缝更新」：SpaceKnife 关闭，安装程序在后台替换旧版本（不显示安装向导），然后 SpaceKnife 自动重新打开。点「运行安装程序」则是 SpaceKnife 退出、打开安装向导，和手动运行新的安装包一样。 |
| Windows / Linux 便携版 | 点「无缝更新」：SpaceKnife 自己换上新的程序文件，`data` 文件夹保留。点「自己替换」或手动升级：退出 SpaceKnife，解压新的压缩包，用新文件替换旧文件夹里的内容，但留下其中的 `data` 文件夹。 |
| macOS dmg | 点「打开磁盘映像」：SpaceKnife 退出并打开磁盘映像，把 SpaceKnife 拖进「应用程序」替换旧版本。然后再放行一次，并把「完全磁盘访问权限」关掉再打开（见上文）。 |
| deb / rpm | 点「打开安装包」：SpaceKnife 退出，并用系统的软件安装器打开安装包。也可以照上面的方法用 `apt` 或 `dnf` 安装新包。 |
| AppImage | 点「无缝更新」：新的 AppImage 替换旧文件。点「自己替换」或手动升级：退出 SpaceKnife，用新的 AppImage 文件替换旧文件。 |

## 卸载

| 安装方式 | 怎么卸载 |
|---|---|
| Windows 安装包 | 在 Windows 设置的应用列表里卸载 SpaceKnife。程序、界面缓存、缓存和下载的更新都会删除。卸载程序的第二页「你的数据」问其余的：勾上「同时删除快照、设置和日志」（默认不勾）就连 `%LOCALAPPDATA%\SpaceKnife` 一起删掉。 |
| 便携版 | 删除整个文件夹，`data` 文件夹也一起删掉。 |
| macOS | 退出 SpaceKnife，把它从「应用程序」移到废纸篓。 |
| deb | `sudo apt remove spaceknife` |
| rpm | `sudo dnf remove spaceknife` |
| AppImage | 删除这个文件。 |

除非勾上那一项，或者删的是便携版，扫描、快照和设置都会保留，以后重新安装还能接着用。要手动删掉它们，删除[数据位置](../data-locations/)里列出的文件夹。用 `SPACEKNIFE_HOME` 指定的文件夹，卸载程序从不碰。

## 命令行

每个安装好的 SpaceKnife 都带着后端的命令行，可以在终端里扫描、管理快照：

| 安装方式 | 路径 |
|---|---|
| Windows | `<安装目录>\resources\backend\spaceknife-backend.exe` |
| Linux deb / rpm | `/opt/SpaceKnife/resources/backend/spaceknife-backend` |
| macOS | `/Applications/SpaceKnife.app/Contents/Resources/backend/spaceknife-backend` |

能做什么，见[命令行](../command-line/)。
