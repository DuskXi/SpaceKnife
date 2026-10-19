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
- **Installers and updates**: the installers are not code-signed yet; verify downloads against the `SHA256SUMS.txt` attached to each release.

SpaceKnife sends no telemetry and makes no network requests of its own.

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
- **安装包**：目前还没有代码签名，请用每个版本附带的 `SHA256SUMS.txt` 校验下载的文件。

SpaceKnife 没有遥测，自身不发起任何网络请求。
