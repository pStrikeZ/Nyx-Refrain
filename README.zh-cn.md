<p align="center">
  <img src="assets/icons/hicolor/256x256/apps/nyx-refrain.png" width="200" alt="Nyx Refrain">
</p>

<h1 align="center">Nyx Refrain</h1>

<p align="center">把 Windows / Linux 电脑的系统声音，以低延迟推送到 HomePod 等 AirPlay 2 音箱。</p>

<p align="center"><a href="README.md">English</a> | 简体中文</p>

Nyx Refrain 是用纯 Rust 实现的 AirPlay 2 realtime 发送端：自带配对、加密、授时和重传，不依赖 OpenSSL、Bonjour for Windows 或其他 C 库。一次推流连接一台接收端，默认端到端延迟约 140 ms，看视频、打游戏也基本对得上嘴型。

> 当前版本 0.9.x，主要在 HomePod mini（tvOS 27）上测试。欢迎在其他 AirPlay 2 接收端上试用并反馈。

> [!NOTE]
> **LLM Disclaimer**
>
> 本项目的全部代码 100% 由 AI 生成，维护者可能会保证项目的易用性，但无法保证代码质量。维护者也有可能使用 100% 复制粘贴的形式回答用户的问题，这是正常现象，还请知悉。

---

## 功能

- **低延迟推流**：AirPlay 2 realtime 模式，三档延迟可选（约 97 / 137 / 237 ms），自动补偿本机与音箱之间的时钟漂移，长时间推流不跑调、不断音。
- **Windows 托盘程序**：Windows 11 风格的托盘弹出面板，右键菜单可完成所有常用操作。界面用 CPU 渲染，不需要显卡驱动，没有 GPU 的虚拟机里也能用；空闲时内存占用只有几 MiB。
- **按进程采集（Windows）**：默认直接采集各应用的声音，笔记本扬声器的音效不会混进推流，本机外放静音也不影响音箱出声。也可以在设置里切回传统的「设备回环」采集。
- **Linux 托盘与虚拟声卡**：通过 StatusNotifierItem 托盘（KDE 原生支持，GNOME 需扩展）控制；推流时创建一个名为「Nyx Refrain (AirPlay)」的 PipeWire 虚拟输出设备并设为默认输出，停止后自动恢复。
- **曲目信息与封面**：把系统正在播放的曲名、艺人、专辑和封面推送给音箱（Windows SMTC / Linux MPRIS）。
- **音箱反向控制**：在 HomePod 或 Home Assistant 上按播放、暂停、上一首、下一首，会转给电脑上的播放器；在音箱上调的音量也会同步回来。
- **开机自启与自动续推**：可以开机静默启动到托盘，上次退出时还在推流的话，启动后自动连回。
- **命令行工具 `nyxr`**：推流、设备发现、网卡诊断、测试音，适合脚本和无界面环境。
- **代理 / TUN 友好**：自动识别并避开 Clash、sing-box、WireGuard、Tailscale 等虚拟网卡，连接固定走和音箱同网段的物理网卡。

---

## 安装

Windows（x64 / ARM64）与 Linux（x86_64 / aarch64 / loongarch64）同等对待：每个版本都为全部架构构建安装包，功能一致。

从 [Releases](https://github.com/pStrikeZ/Nyx-Refrain/releases) 下载对应系统和架构的安装包。

### Windows

| 架构 | 安装包 |
|---|---|
| x64（Intel / AMD） | `nyx-refrain-<版本>-windows-x86_64-setup.exe` |
| ARM64（骁龙等） | `nyx-refrain-<版本>-windows-arm64-setup.exe` |

- 安装到 `C:\Program Files\Nyx Refrain`，可选开始菜单 / 桌面快捷方式；默认把安装目录加入 `PATH`，新开的终端里可以直接用 `nyxr`。
- 卸载时会一并清理开机自启项、`PATH` 和 Nyx Refrain 添加的防火墙规则；设置保留在 `%APPDATA%\nyx-refrain`。
- 按进程采集需要 Windows 10 2004 或更新的版本，更早的系统会自动退回设备回环采集。
- 建议始终通过安装包安装，不要把 exe 复制到别处运行：防火墙规则是按程序路径添加的，换了位置就需要重新放行。

### Linux

软件包提供 x86_64、aarch64、loongarch64 三种架构，每个包都包含托盘程序 `nyx-refrain` 和命令行工具 `nyxr`，均为静态链接，没有硬性库依赖。

```bash
sudo apt install ./nyx-refrain_*.deb          # Debian / Ubuntu
sudo dnf install ./nyx-refrain-*.rpm          # Fedora
sudo pacman -U ./nyx-refrain-*.pkg.tar.zst    # Arch Linux
```

- 系统音频采集需要运行中的 **PipeWire** 与 **WirePlumber**。
- 托盘需要桌面支持 StatusNotifierItem：
  - **KDE Plasma**：原生支持。
  - **GNOME**：需要安装并启用 [AppIndicator and KStatusNotifierItem Support](https://extensions.gnome.org/extension/615/appindicator-support/) 扩展（Ubuntu 通常已预装）。缺少托盘时，程序会弹出桌面通知提醒。
  - 其他桌面 / 窗口管理器：使用支持 SNI 的托盘模块（如 Waybar、Polybar 配合 `snixembed`）。
- Linux 版只有托盘菜单，没有窗口界面。

---

## 使用

### 托盘程序

1. 启动 **Nyx Refrain**。Windows 上点击托盘图标打开面板；Linux 上右键托盘图标打开菜单。
2. 选择同一局域网里的音箱。发现不到时，可以手动输入地址（如 `192.0.2.10` 或 `192.0.2.10:7000`）。
3. 点「开始推流」。之后电脑播放的声音都会从音箱出来。

**Windows 防火墙**：AirPlay 2 需要音箱主动连回电脑（授时和控制通道），所以必须允许 Nyx Refrain 的入站连接。首次使用时，如果面板提示防火墙拦截，按提示点一下放行即可（会请求管理员权限）。

**设置**（Windows 在面板右上角 ⚙，Linux 在托盘菜单中）：界面语言、推送曲目信息、允许音箱控制播放器、启动时恢复上次推流、开机自启；Windows 另有「使用设备回环采集」。

### 命令行 `nyxr`

```bash
# 发现局域网里的 AirPlay 设备
nyxr discover

# 推流本机系统声音（设备名或 IP 均可）
nyxr stream --target "客厅" --audible
nyxr stream --target 192.0.2.10 --audible --now-playing

# 选择延迟档位，每 5 秒打印一次推流状态
nyxr stream --target 192.0.2.10 --audible --latency-profile stable --stats

# 推送 WAV 文件，或经标准输入推送任意音频
nyxr stream --target 192.0.2.10 --source wav --device music.wav --audible
ffmpeg -i input.flac -f s16le -ar 44100 -ac 2 - | nyxr stream --target 192.0.2.10 --source stdin --audible

# 查看网卡识别结果（物理网卡 / 虚拟网卡）
nyxr list-interfaces
```

- `--volume`：0 为静音，1–100 为百分比（映射到 −30..0 dB），也可以直接写负的分贝值，如 `-15`。
- `--source`：Windows 默认 `wasapi`（设备回环），`process` 为按进程采集；Linux 默认 `pipewire`。另有 `sine`、`wav`、`stdin`。
- 完整参数见 `nyxr --help` 与 `nyxr <子命令> --help`。

常用参数可以写进配置文件，之后直接 `nyxr stream --audible` 即可：

- Windows：`%APPDATA%\nyx-refrain\config.toml`
- Linux：`~/.config/nyx-refrain/config.toml`

```toml
target = "192.0.2.10"   # 设备 IP 或名称
interface = "Wi-Fi"     # 可选：固定使用的网卡
volume = 40.0           # 初始音量（百分比）
log_level = "info"
```

### 延迟档位

| 档位 | 端到端延迟（HomePod 实测） | 说明 |
|---|---|---|
| `low` | 约 97 ms | Wi-Fi 抖动时余量较小 |
| `balanced`（默认） | 约 137 ms | 大多数情况下的推荐值 |
| `stable` | 约 237 ms | 接收端原生缓冲，网络较差时使用 |

也可以用 `--ap2-sync-latency-ms` 直接指定（−165 至 3000 ms）。

---

## 代理与 TUN 环境

Nyx Refrain 会自动排除 TUN / TAP 等虚拟网卡，只从和音箱同网段的物理网卡发起连接。如果代理软件接管了局域网流量，还需要在代理规则里让局域网和 mDNS 直连，例如（Clash / mihomo）：

```yaml
rules:
  - IP-CIDR,192.168.0.0/16,DIRECT   # 按你的局域网网段修改
  - IP-CIDR,224.0.0.251/32,DIRECT   # mDNS
  - DOMAIN-SUFFIX,local,DIRECT
```

**不支持经 VPN / TUN 从外网推流。** AirPlay 2 要求音箱能直接连回发送端，代理型 TUN 和做地址转换的 VPN 都做不到。此时程序会提示「无法直连接收端」。

---

## 已知限制

- 仅支持 AirPlay 2，不支持 AirPlay 1；一次只推送到一台接收端，暂不支持 HomePod 立体声对和多房间。
- 暂无 macOS 版本。
- 在音箱上按暂停时，电脑上的播放器会暂停，但音频流本身不中断；音箱不显示进度条，也不支持在音箱上拖动进度。
- 第三方发送端的曲目信息不会显示在 Apple「家庭」App 和控制中心里，可以在 Home Assistant 等基于 pyatv 的工具中看到。

---

## 路线图

下面两项是计划中的功能，目前卡在缺少测试设备：

- **HomePod 立体声对**：两台组成立体声对的 HomePod 需要分别建立连接、共用同一条播放时间线，同步效果必须用两台实机调。
- **macOS 版**：需要一台 Mac 来开发和测试系统音频采集。

如果你有这些设备并愿意帮忙测试，欢迎在 Issue 里说一声。

---

## 从源码构建

所有目标都在 Linux 上交叉编译，需要：

- Rust（stable，CI 使用 1.95.0）及目标 `x86_64-pc-windows-gnu`、`aarch64-pc-windows-gnullvm`、`x86_64-unknown-linux-musl`、`aarch64-unknown-linux-musl`、`loongarch64-unknown-linux-musl`
- [zig](https://ziglang.org/) 0.16 与 [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)
- 打包：`makensis`（Windows 安装包）、Python 3.11+、`bsdtar`（Debian / Ubuntu：`libarchive-tools`）；nfpm 由脚本自动下载并校验
- 检查 Windows 产物需要 `llvm-readobj`（Debian / Ubuntu：`llvm`）

```bash
./scripts/setup-toolchain.sh          # 检查并补齐 Rust 目标与工具
./scripts/build.sh                    # 构建全部 5 个目标并打包到 dist/
TARGETS="win-x64 linux-x64" ./scripts/build.sh   # 只构建部分目标
NO_PACKAGES=1 ./scripts/build.sh      # 只生成二进制，不打包
```

开发时的检查：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib --bins
```

---

## 参与贡献

欢迎提交 Issue 和 PR，尤其是 HomePod 立体声对、多房间和 macOS 支持。

---

## 项目命名由来

名字取自两首音游曲：

- [Nýx](https://music-am.sega.jp/songs/song-01340/)：7mai（オンゲキ）
- [Ref:rain (for 7th Heaven)](https://music-am.sega.jp/songs/song-02462/)：カモメサノエレクトリックオーケストラ include Limonène（maimai でらっくす）

---

## 许可证

[MIT](LICENSE) © 2026 pStrikeZ and contributors

图标与默认封面来自 [Microsoft Fluent Emoji](https://github.com/microsoft/fluentui-emoji)（MIT），见 [NOTICE](NOTICE) 与 [LICENSE-fluentui-emoji](LICENSE-fluentui-emoji)。
