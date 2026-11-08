---
title: 无头模式
description: 不开窗口运行 SpaceKnife，在浏览器里使用，本机或别的设备都行；在网络上开放的风险，以及怎样保证安全。
---

无头模式下 SpaceKnife 不开窗口：命令行程序 `spaceknife-cli` 像应用一样扫描，并把应用的界面通过 HTTP 提供出来，你在浏览器里使用 SpaceKnife。适合没有显示器的服务器或 NAS，或者从笔记本、平板访问家里的台式机。在浏览器里看到和做的一切，都发生在运行无头模式的那台电脑上。

## 启动

运行 `spaceknife-cli headless`；要让别的设备访问，再加上监听的地址：

```bash
spaceknife-cli headless                          # 只有本机能访问（127.0.0.1，端口 8375）
spaceknife-cli headless --listen 192.168.1.20    # 这台电脑的某个地址，端口 8375
spaceknife-cli headless --listen 0.0.0.0:9000    # 所有网络接口，端口 9000
```

它会打印要在浏览器里打开的地址，末尾带一个令牌，然后一直运行，直到你按 <kbd>Ctrl</kbd>+<kbd>C</kbd>：

```text
SpaceKnife 无头模式 · 0.3.5
在浏览器里打开：
  http://192.168.1.20:8375/#token=n3Vq…
按 Ctrl+C 停止。
```

运行期间，终端里会显示浏览器的连接和断开（`14:02:11 192.168.1.31 已连接`），以及后端的警告和错误。完整日志照常写进日志文件（[数据位置](../data-locations/)）。

`spaceknife-cli` 装在应用旁边（[在哪里](../command-line/#程序在哪里)）：

| 安装方式 | 命令 |
|---|---|
| Windows 安装包 | `%LOCALAPPDATA%\SpaceKnife\spaceknife-cli headless`（默认文件夹） |
| Windows 便携版 | 在它的文件夹里 `.\spaceknife-cli headless` |
| deb / rpm | `spaceknife-cli headless` |
| Linux 便携版 | 在它的文件夹里 `./spaceknife-cli headless` |
| macOS | `/Applications/SpaceKnife.app/Contents/MacOS/spaceknife-cli headless` |

它是控制台程序：终端会等它结束，<kbd>Ctrl</kbd>+<kbd>C</kbd> 就能停止。它提供的页面是 `spaceknife-cli` 旁边 `ui` 文件夹里构建好的界面；用 `--ui` 可以指定别的文件夹。

在便携版里，无头模式和应用一样把数据存在便携版的 `data` 文件夹里（[数据位置](../data-locations/)）；设了 `SPACEKNIFE_HOME` 时仍以它为准。

<!-- TODO: confirm — default_ui_dir (crates/sk-server/src/cli/headless.rs) looks for `ui` next to spaceknife-cli, `resources/ui`, `../lib/SpaceKnife/ui` (deb / rpm) and `Contents/Resources/ui` (macOS); not yet checked on a real deb, rpm or macOS install. The AppImage runs spaceknife-cli only once the app's shells hand `--headless` on to it (merge plan §4, the shells' work). -->

## 选项

| 选项 | 作用 |
|---|---|
| `--listen <地址>` | 监听这个地址。不写时只监听本机：`127.0.0.1:8375` |
| `--yes` | 在网络上开放前不再询问。没有终端可以回答时必须加 |
| `--tls-cert <文件>` | 用这个证书（PEM）提供 HTTPS，与 `--tls-key` 一起用 |
| `--tls-key <文件>` | 证书的私钥（PEM，不加密：见[在网络上开放](#在网络上开放)里的 **HTTPS**） |
| `--ui <文件夹>` | 要提供的构建好的界面（默认：程序旁边的 `ui` 文件夹） |
| `--help` | 列出这些选项 |

选项的顺序随意，也可以写成 `--listen=0.0.0.0`。帮助和选项写错时的提示使用界面语言（「设置 › 常规 › 界面语言」），和命令行的其他输出一样。

SpaceKnife 0.3.x 启动无头模式的写法仍然可用：`--headless` 和 `--listen <地址>` 放在参数里的任何位置，例如 `spaceknife-cli --yes --listen 0.0.0.0`（0.3.x 命令行里的其他东西，比如 Chromium 的开关，会被跳过）。Windows 上在应用的文件夹里打开终端，输入 `SpaceKnife --headless …`，运行的是那里的 `SpaceKnife.com`：它用同样的参数启动 `spaceknife-cli` 并等它结束，和 0.3.x 一样；不带这些选项时它打开窗口。

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

退出码：回答了「否」、地址用不了、后端没有启动时是 `1`；选项写错、没有终端可以回答、证书或私钥用不了，或者 `ui` 文件夹里没有构建好的界面时是 `2`。用 <kbd>Ctrl</kbd>+<kbd>C</kbd> 或被系统停止时是 `0`，`--help` 也是 `0`。

## 在网络上开放

能访问这个地址、又拿到链接的人，可以用**你的账户的权限**使用这台电脑：浏览文件、移到回收站或者永久删除，把文件写到你的账户能写的任何地方（导出报告、快照另存为、导出设置；写对了地方，这样一个文件就能让这台电脑运行程序），还能在这台电脑上弹出管理员授权的请求。所以只要监听的不只是本机（`127.x.x.x`、`::1`、`localhost` 以外的任何地址），SpaceKnife 都会先用红色显示警告，然后询问：

```text
 ⚠ 危险：SpaceKnife 将在网络上开放
  监听 0.0.0.0:8375：这台电脑的所有网络接口，…
确定要开放吗？输入 Y 继续，N 退出 [y/N]：
```

只有 <kbd>Y</kbd>（或 `yes`）会继续。<kbd>N</kbd>、别的任何输入、空行或者输入结束，都会退出，不监听任何地址。

没有终端时（作为服务、计划任务运行，或输入来自文件、`/dev/null`），没有人可以回答：SpaceKnife 拒绝启动，除非加上 `--yes`。警告仍然会打印到日志里。

它怎样保护你：

- **链接里的令牌**：每次启动都生成一个新的随机令牌，放在 `#` 后面，浏览器不会把这部分发给服务器。页面随后把它从地址栏里拿掉，保存在这个浏览器标签页里：刷新照样能连，分享或收藏的地址里也没有令牌。每个连接到后端的会话都要用它，所以重新启动之后旧链接就失效了，请打开新的地址。
- **只认本站的页面**：浏览器发起的 WebSocket 必须来自这个服务器提供的页面。同一个浏览器里打开的其他网站，就算拿到令牌也连不上；不说明自己来自哪里的程序也连不上。页面只运行自己的脚本（内容安全策略），也不能被嵌进别的网站。
- **只有一个地址**：页面和后端的 WebSocket 一起在你指定的地址上提供，没有别的端口在监听。页面本身的文件是公开的，里面没有任何关于你的文件的信息。
- **限制**：同时最多 64 个连接（页面和 WebSocket 一起算）；请求要在 20 秒内发完、30 秒内得到回应，空闲 20 秒的连接会被关掉，慢吞吞的客户端占不住名额。

:::caution[明文 HTTP]
不提供证书时，传输的是明文 HTTP。同一网络上、或者传输途中的任何人，都能看到令牌和所有路径、文件名。在你管不了的网络上，请用 HTTPS 或 SSH 隧道。
:::

**HTTPS**：用 PEM 文件提供证书和私钥，比如用 [mkcert](https://github.com/FiloSottile/mkcert) 为这台电脑的名字或地址签发：

```bash
spaceknife-cli headless --listen 0.0.0.0 --tls-cert nas.lan.pem --tls-key nas.lan-key.pem
```

这时打印出的地址以 `https://` 开头。浏览器对不信任的证书（自签名）会给出警告：请在用来访问的设备上安装你自己的证书颁发机构，不要直接点过警告。

文件要求：

- **证书**：PEM 格式（`-----BEGIN CERTIFICATE-----`），这台电脑自己的证书在前，有中间证书的话跟在后面。
- **私钥**：PEM 格式且不加密，是 `-----BEGIN PRIVATE KEY-----`（PKCS #8，mkcert 和 `openssl genpkey` 写的就是它）、`-----BEGIN RSA PRIVATE KEY-----`（PKCS #1）或 `-----BEGIN EC PRIVATE KEY-----`（SEC1）之一。

带密码的私钥、二进制（DER）文件和 `.pfx` / `.p12` 读不了，SpaceKnife 会在监听之前说明是哪一种。先用 OpenSSL 转换：

```bash
openssl pkey -in key-with-password.pem -out key.pem              # 会问一次密码
openssl pkcs12 -in nas.pfx -nocerts -nodes -out key.pem          # .pfx / .p12 里的私钥
openssl pkcs12 -in nas.pfx -nokeys -out cert.pem                 # 里面的证书
openssl x509 -inform der -in cert.cer -out cert.pem              # DER 格式的证书
```

**SSH 隧道**完全不需要开放端口。在服务器上只监听本机：

```bash
spaceknife-cli headless
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

`spaceknife-cli` 是控制台程序：不需要 X11 或 Wayland，也不需要窗口用的库（GTK、WebKitGTK）。在服务器上用便携版 tar.gz 就够了：解压后运行 `./spaceknife-cli headless`。

请用你想查看其文件的那个用户运行它。以 root 运行也可以，但浏览器里的一切操作都带着 root 的权限。

作为 systemd 服务运行（`~/.config/systemd/user/spaceknife-headless.service`，然后 `systemctl --user enable --now spaceknife-headless`）：

```ini
[Unit]
Description=SpaceKnife headless mode

[Service]
ExecStart=/usr/bin/spaceknife-cli headless --listen 192.168.1.20:8375 --yes
Restart=on-failure

[Install]
WantedBy=default.target
```

令牌每次启动都会变：用 `journalctl --user -u spaceknife-headless` 查看地址。

## macOS 上

运行应用包里的 `spaceknife-cli`（见上面的表格）。无头模式不在程序坞里显示图标。访问受保护的位置时，macOS 以启动它的程序（通常是「终端」）的名义询问：要看到全部，请给那个程序授予「完全磁盘访问权限」（[安装与升级](../install/#完全磁盘访问权限)）。
