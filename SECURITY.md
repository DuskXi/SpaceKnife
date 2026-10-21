# Security policy

**English** · [简体中文](#安全策略)

## Supported versions

SpaceKnife is in pre-release. Security fixes go into the newest release only; please update before reporting.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Report it privately instead:

- on GitHub: [Security › Report a vulnerability](https://github.com/DuskXi/SpaceKnife/security/advisories/new) (private vulnerability reporting), or
- by e-mail: **spaceknife@kaltsit.dev**

Include what you found, how to reproduce it, and the version and operating system. You will get an answer within a few days; this is a volunteer project, so a fix may take longer, and you will be told what to expect.

## What is in scope

Areas where a security problem matters most:

- **The elevated scan helper** (Windows NTFS quick scan): it runs with administrator rights and must stay read-only and accept only the backend that started it.
- **The local backend**: a WebSocket server on `127.0.0.1` that requires a per-session token; the interface and the backend must not be reachable from other machines or other users.
- **File operations**: the delete basket, the recycle bin and the protected system and program folders; a request to move something to the recycle bin must never delete it permanently.
- **Installers and updates**: the installers are not code-signed yet; verify downloads against the `SHA256SUMS.txt` attached to each release. The in-app update download checks the file against those sums itself and only starts files named like this project's release files. A portable copy's files are swapped by SpaceKnife's own backend executable from the new version (`--apply-update`), not by a script. SpaceKnife never installs an update by itself: every install starts with the user's click in the update card, and no option to install automatically will be added.

SpaceKnife sends no telemetry. Its only network requests of its own are the update check — at most once a day it reads this project's release list from `api.github.com`, sending nothing about you or your files — and, when a newer version is found (or you click to download it), the release file and its checksums from `github.com`. Switch the check off in Settings › About & updates.

---

## 安全策略

### 支持的版本

SpaceKnife 目前是预发布版。安全修复只进入最新版本，报告前请先更新到最新版。

### 报告漏洞

安全问题请**不要提交公开 issue**，请私下报告：

- GitHub：[Security › Report a vulnerability](https://github.com/DuskXi/SpaceKnife/security/advisories/new)（私密漏洞报告）；或
- 邮件：**spaceknife@kaltsit.dev**

请写明发现了什么、如何复现，以及版本和操作系统。几天内会有回复；这是业余维护的项目，修复可能需要更久，届时会告诉你预计的进度。

### 重点范围

- **提权扫描助手**（Windows NTFS 快速扫描）：以管理员权限运行，必须保持只读，并且只接受启动它的那个后端。
- **本地后端**：监听 `127.0.0.1` 的 WebSocket 服务，每次会话需要令牌；不能被其他机器或其他用户访问。
- **文件操作**：删除篮、回收站，以及受保护的系统和程序目录；「移到回收站」的请求绝不能变成永久删除。
- **安装包与更新**：目前还没有代码签名，请用每个版本附带的 `SHA256SUMS.txt` 校验下载的文件。应用内的更新下载会自己按这份校验和核对文件，并且只启动按本项目发布文件命名的文件。便携版的程序文件由新版本自带的 SpaceKnife 后端程序（`--apply-update`）替换，不经过脚本。SpaceKnife 从不自己安装更新：每次安装都从用户在更新卡片里的点击开始，也不会加自动安装的选项。

SpaceKnife 没有遥测。它自己发起的网络请求只有两种：检查更新（每天最多一次，从 `api.github.com` 读取本项目的发布列表，不发送任何关于你或你的文件的信息），以及发现新版本后（或你点击下载时）从 `github.com` 下载发布文件和它的校验值。可以在「设置 › 关于与更新」里关掉检查。
