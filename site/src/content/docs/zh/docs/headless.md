---
title: 无头模式
description: 不开窗口运行 SpaceKnife，在浏览器里使用，本机或别的设备都行；在网络上开放的风险，以及怎样保证安全。
---

无头模式下 SpaceKnife 不开窗口：后端照常扫描，应用把界面通过 HTTP 提供出来，你在浏览器里使用 SpaceKnife。适合没有显示器的服务器或 NAS，或者从笔记本、平板访问家里的台式机。在浏览器里看到和做的一切，都发生在运行无头模式的那台电脑上。

## 启动

给应用的可执行文件加上下面任意一个选项：

```bash
SpaceKnife --headless                  # 只有本机能访问（127.0.0.1，端口 8375）
SpaceKnife --listen 192.168.1.20       # 这台电脑的某个地址，端口 8375
SpaceKnife --listen 0.0.0.0:9000       # 所有网络接口，端口 9000
```

它会打印要在浏览器里打开的地址，末尾带一个令牌，然后一直运行，直到你按 <kbd>Ctrl</kbd>+<kbd>C</kbd>：

```text
SpaceKnife 无头模式 · 0.3.3
在浏览器里打开：
  http://192.168.1.20:8375/#token=n3Vq…
按 Ctrl+C 停止。
```

运行期间，终端里会显示浏览器的连接和断开，以及后端的警告和错误。完整日志照常写进日志文件（[数据位置](../data-locations/)）。

| 安装方式 | 命令 |
|---|---|
| Windows 安装包，当前用户 | `%LOCALAPPDATA%\Programs\SpaceKnife\SpaceKnife --headless`（默认文件夹） |
| Windows 安装包，所有用户 | `"C:\Program Files\SpaceKnife\SpaceKnife" --headless`（默认文件夹） |
| Windows 便携版 | 在它的文件夹里 `SpaceKnife --headless` |
| deb / rpm | `spaceknife --headless` |
| Linux 便携版 | 在它的文件夹里 `./spaceknife --headless` |
| AppImage | `./SpaceKnife-<版本>-x86_64.AppImage --headless` |
| macOS | `/Applications/SpaceKnife.app/Contents/MacOS/SpaceKnife --headless` |

:::note[Windows：不要写 .exe]
`SpaceKnife.exe` 是窗口程序，终端不会等它，它就没法提问，也没法用 <kbd>Ctrl</kbd>+<kbd>C</kbd> 停止。它旁边有一个 `SpaceKnife.com`，这个小小的控制台程序负责启动它并等待它结束。命令提示符和 PowerShell 会先找 `.com` 再找 `.exe`，所以写 `SpaceKnife`，不加扩展名。
:::

## 选项

| 选项 | 作用 |
|---|---|
| `--headless` | 无头模式，只监听本机：`127.0.0.1:8375` |
| `--listen <地址>` | 监听这个地址，同时启动无头模式 |
| `--yes` | 在网络上开放前不再询问。没有终端可以回答时必须加 |
| `--tls-cert <文件>` | 用这个证书（PEM）提供 HTTPS，与 `--tls-key` 一起用 |
| `--tls-key <文件>` | 证书的私钥（PEM） |
| `--help` | 列出这些选项 |

地址可以这样写：

| 地址 | 含义 |
|---|---|
| `192.168.1.20` | 这个地址，端口 8375 |
| `192.168.1.20:9000` | 这个地址和端口 |
| `:9000` 或 `9000` | 只有本机，这个端口 |
| `0.0.0.0` | 所有 IPv4 接口 |
| `::` 或 `[::]:9000` | 所有接口，IPv6 和 IPv4 |
| `[fd00::5]:9000` | 一个 IPv6 地址和端口 |
| `nas.lan` | 一个名字，解析到哪里就是哪里 |

端口写 `0` 由系统挑一个空闲端口，打印出的地址里会带上它。

退出码：回答了「否」、地址用不了、后端退出时是 `1`；选项写错，或者没有终端可以回答时是 `2`。用 <kbd>Ctrl</kbd>+<kbd>C</kbd> 或被系统停止时，Linux 和 macOS 上是 `0`；Windows 上 <kbd>Ctrl</kbd>+<kbd>C</kbd> 会立刻结束它，后端随之自行退出。

## 在网络上开放

能访问这个地址、又拿到链接的人，可以用**你的账户的权限**浏览这台电脑上的文件、移到回收站，或者永久删除。所以只要监听的不只是本机（`127.x.x.x`、`::1`、`localhost` 以外的任何地址），SpaceKnife 都会先用红色显示警告，然后询问：

```text
 ⚠ 危险：SpaceKnife 将在网络上开放
  监听 0.0.0.0:8375：这台电脑的所有网络接口，…
确定要开放吗？输入 Y 继续，N 退出 [y/N]：
```

只有 <kbd>Y</kbd>（或 `yes`）会继续。<kbd>N</kbd>、别的任何输入、空行或者输入结束，都会退出，不监听任何地址。

没有终端时（作为服务、计划任务运行，或输入来自文件、`/dev/null`），没有人可以回答：SpaceKnife 拒绝启动，除非加上 `--yes`。警告仍然会打印到日志里。

它怎样保护你：

- **链接里的令牌**：每次启动都生成一个新的随机令牌，放在 `#` 后面，浏览器不会把这部分发给服务器。页面随后把它从地址栏里拿掉，保存在这个浏览器标签页里：刷新照样能连，分享或收藏的地址里也没有令牌。每个连接到后端的会话都要用它，所以重新启动之后旧链接就失效了，请打开新的地址。
- **只认本站的页面**：浏览器发起的 WebSocket 必须来自这个服务器提供的页面。同一个浏览器里打开的其他网站，就算拿到令牌也连不上。
- **后端不对外**：后端照旧只监听 `127.0.0.1`，由无头模式把浏览器的连接转发给它。页面本身的文件是公开的，里面没有任何关于你的文件的信息。

:::caution[明文 HTTP]
不提供证书时，传输的是明文 HTTP。同一网络上、或者传输途中的任何人，都能看到令牌和所有路径、文件名。在你管不了的网络上，请用 HTTPS 或 SSH 隧道。
:::

**HTTPS**：用 PEM 文件提供证书和私钥，比如用 [mkcert](https://github.com/FiloSottile/mkcert) 为这台电脑的名字或地址签发：

```bash
spaceknife --listen 0.0.0.0 --tls-cert nas.lan.pem --tls-key nas.lan-key.pem
```

这时打印出的地址以 `https://` 开头。浏览器对不信任的证书（自签名）会给出警告：请在用来访问的设备上安装你自己的证书颁发机构，不要直接点过警告。

**SSH 隧道**完全不需要开放端口。在服务器上只监听本机：

```bash
spaceknife --headless
```

在你的电脑上把一个本地端口转发过去，然后在这台电脑上打开打印出的地址（`http://localhost:8375/#token=…`）：

```bash
ssh -L 8375:localhost:8375 lenny@nas.lan
```

**防火墙**：SpaceKnife 第一次在网络上监听时，Windows 会询问是否允许它通过防火墙：只允许「专用网络」。Linux 上开着防火墙的话请放行端口，比如 `sudo firewall-cmd --add-port=8375/tcp` 或 `sudo ufw allow 8375/tcp`。

## 在浏览器里

界面和应用里一样，只是因为文件在另一台电脑上，有几处不同：

- **标题栏**显示那台电脑的名字，没有窗口按钮；连接没有加密时（不经 `localhost`）标上 **HTTP**。鼠标悬停会说明扫描和删除都作用于那台电脑。浏览器标签页的标题里也有这个名字。
- **路径是那台电脑的**：在开始页的路径框里输入（占位文字按那台电脑的路径格式显示）。没有「浏览…」按钮，把文件夹拖到页面上也不行：那是你正在用的这台电脑上的文件夹。打开快照、导出报告、导入导出设置时，会请你输入那台电脑上的路径，从它的主文件夹开始。
- **没有文件管理器、终端和系统菜单**：菜单里没有「打开」「在资源管理器中显示」（访达、文件管理器）「在此打开终端」和系统菜单。对应的按键（<kbd>Ctrl</kbd>+<kbd>E</kbd>、<kbd>Ctrl</kbd>+<kbd>&#96;</kbd>）改为复制路径，并给出提示。macOS 上的「完全磁盘访问权限」要在那台 Mac 上授予。
- **删除发生在那台电脑上**：「移到回收站」用的是那台电脑的回收站（废纸篓），「永久删除」也是在那里删除。删除篮和确认框都会写明，撤销的 5 秒和应用里一样（[清理空间](../cleanup/)）。
- **更新**：更新卡片只给出发布页链接；下载和安装要在那台电脑上进行，在应用里或用新的安装包。
- **浏览器的按键优先**：<kbd>Ctrl</kbd>+<kbd>W</kbd>、<kbd>Ctrl</kbd>+<kbd>T</kbd>、<kbd>Ctrl</kbd>+<kbd>N</kbd> 关闭和打开的是浏览器的标签页，不是 SpaceKnife 的：关闭 SpaceKnife 的标签请点它的 ×，或者用鼠标中键。有扫描或删除正在进行、或者开着扫描标签时，离开页面前浏览器会先问一句。
- **多个浏览器同时使用**：每个浏览器标签页都有自己的 SpaceKnife 标签和视图；设置、快照和删除篮属于那台电脑，大家共用。
- **复制**：经 HTTPS 和 `localhost` 时用剪贴板接口；明文 HTTP 下用浏览器较旧的复制命令。

标签页在后台时扫描结束，标签页标题前会出现一个圆点，代替任务栏闪烁。

## 没有显示器的 Linux 服务器

无头模式不需要 X11 或 Wayland：`--headless` 同时也是 Chromium 不连接显示器运行的开关，只写 `--listen` 时会自动加上。程序仍然需要它编译时链接的库（GTK 3、NSS、GBM、ALSA 和几个 X11 库，但不需要 X 服务器）：deb 和 rpm 包会自动装上；在精简系统上用 AppImage 或便携版时请自己安装（Ubuntu 上比如 `libgtk-3-0 libnss3 libgbm1 libasound2`）。

Chromium 不加 `--no-sandbox` 就不能以 root 运行。请用你想查看其文件的那个用户运行 SpaceKnife；必须用 root 时再加 `--no-sandbox`。

作为 systemd 服务运行（`~/.config/systemd/user/spaceknife-headless.service`，然后 `systemctl --user enable --now spaceknife-headless`）：

```ini
[Unit]
Description=SpaceKnife headless mode

[Service]
ExecStart=/usr/bin/spaceknife --listen 192.168.1.20:8375 --yes
Restart=on-failure

[Install]
WantedBy=default.target
```

令牌每次启动都会变：用 `journalctl --user -u spaceknife-headless` 查看地址。

## macOS 上

运行应用包里的可执行文件（见上面的表格）。无头模式不在程序坞里显示图标。SpaceKnife 能读取哪些东西和应用里一样：要看到全部，请在那台 Mac 上给 SpaceKnife 授予「完全磁盘访问权限」（[安装与升级](../install/#完全磁盘访问权限)）。
