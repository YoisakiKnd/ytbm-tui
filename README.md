# ytbm-tui

[![CI](https://github.com/YoisakiKnd/ytbm-tui/actions/workflows/ci.yml/badge.svg)](https://github.com/YoisakiKnd/ytbm-tui/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/YoisakiKnd/ytbm-tui)](https://github.com/YoisakiKnd/ytbm-tui/releases/latest)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue)](LICENSE)

轻量级 YouTube Music 终端客户端（TUI）。单个可执行文件，运行内存 ~15MB，
替代内存动辄数百 MB 的官方 Electron/浏览器方案。

```
 首页 Esc  搜索 /  音乐库 L  正在播放 l              ? 帮助  q 退出
┌ 首页 ───────────────────────────────────────┬ 队列 12 ─────────┐
│ 热门歌曲 ----------------------------------- │ > 晴天    周杰伦  │
│ >  1 Golden              HUNTR/X       3:20 │   七里香  周杰伦  │
│    2 Soda Pop            Saja Boys     2:46 │   稻香    周杰伦  │
│ 新专辑 ------------------------------------- │                  │
│    ALB The Life of a Sh.. Taylor Swift 2025 │                  │
├─────────────────────────────────────────────┴──────────────────┤
│ >  晴天 - 周杰伦    1:23 ----●--------- 4:29  vol 70%  RPT RADIO│
└────────────────────────────────────────────────────────────────┘
```

顶部导航栏显示当前位置与到达各页面的按键，可直接点击切换。

## 下载

到 [Releases](https://github.com/YoisakiKnd/ytbm-tui/releases/latest) 下载对应平台的
压缩包（Windows / Linux / macOS Intel / macOS Apple Silicon），解压即用。

## 特性

- **无广告播放**：音频流经 yt-dlp 直接提取，流中天然不含广告（广告由官方客户端注入）
- **浏览器一键登录**：自动检测已装浏览器，选中即导入登录身份，无需复制粘贴；登录后可访问 喜欢的音乐 / 我的歌单 / 收藏专辑 / 关注歌手 / 播放历史，凭据只存本机
- **首页推荐**：打开即见排行榜热门歌曲与新发行专辑
- **SponsorBlock**：自动跳过 MV 音源中的非音乐片段（口播/片头/片尾），社区数据驱动
- **播放详情页**（按 `l`）：专辑封面 + 曲目信息 + 同步歌词逐行滚动。封面自动选用
  终端支持的最佳图形协议（kitty / iTerm2 / Sixel），都不支持时降级为半块字符——
  任何终端都能看到图
- **同步歌词**：LRCLIB 逐行滚动高亮，无同步歌词时回退 YT Music 文本歌词
- **电台续播**：队列快播完时自动追加相似歌曲（可开关）
- **完整队列**：插播/下一首/移除/排序/打乱，循环三模式（关/全部/单曲）
- **鼠标支持**：点击选中、双击播放、滚轮滚动、点击进度条跳转、点击分类页签

## 安装

依赖两个外部程序（音频引擎 + 流解析），scoop 或 winget 任选：

```powershell
scoop install mpv yt-dlp
```

> 程序会自动在 PATH、scoop、Program Files 中寻找 mpv/yt-dlp，
> 装完不必重开终端。新版 yt-dlp 需要 JS 运行时（deno 或 node）解 YouTube
> 挑战：已有 node 的话在 yt-dlp.conf 加一行 `--js-runtimes node` 即可。

从源码构建（需要 [Rust 工具链](https://rustup.rs/)）：

```powershell
cargo build --release
# 产物: target\release\ytbm-tui.exe，单文件可任意拷贝
```

## 开发

```powershell
cargo test                 # 纯逻辑 + 渲染测试，不联网
cargo test -- --ignored    # 冒烟测试，会真实请求 YouTube/LRCLIB/SponsorBlock
cargo clippy --all-targets -- -D warnings
```

CI（`.github/workflows/ci.yml`）在三大平台跑 fmt + clippy + test + release 构建；
推送 `v*` 标签会触发 `release.yml` 交叉构建四个平台产物并自动创建 GitHub Release。
项目约定见 [CLAUDE.md](CLAUDE.md)。

## 登录（访问个人音乐库）

按 `L` 打开登录页，程序会自动列出本机已安装的浏览器：

```
[导入] 从 Firefox 导入登录
[导入] 从 Vivaldi (…\persist\vivaldi\User Data) 导入登录
[网页] 先在浏览器登录 music.youtube.com（打开网页）
[手动] 手动粘贴 Cookie 或 cookies.txt 路径
```

**一键登录**：确保该浏览器里已登录 YouTube，选中它按 Enter 即可——底层用 yt-dlp
读取浏览器中已有的登录身份，全程离线、无需复制粘贴。浏览器还没登录的话，
先选「打开网页」登录，回来再导入。

失败时的常见原因：Chrome 系浏览器需先**完全关闭**才能解密 Cookie 数据库
（Firefox 无此限制）；实在读不出来就用「手动粘贴」兜底（浏览器扩展
「Get cookies.txt LOCALLY」导出后粘贴文件路径即可）。

登录信息保存在本机数据目录，重启免登录；音乐库页按 `x` 登出。
注意 YouTube 会轮换 Cookie，若长时间后接口报错，重新导入一次即可。

## 快捷键

| 键 | 功能 |
|---|---|
| `/` | 搜索 |
| `L` | 音乐库（登录入口） |
| `1-4` / `[` `]` | 搜索分类切换（歌曲/专辑/歌手/歌单） |
| `j/k` `↑/↓` `PgUp/PgDn` `g/G` | 列表导航 |
| `Enter` | 播放（当前列表从这首起顺序播放）/ 打开 |
| `a` / `A` | 加入队列 / 作为下一首 |
| `P` | 整页（专辑/歌单/列表）从头播放 |
| `Tab` | 主面板 ⇄ 队列 |
| `x` | 队列：移除所选 · 音乐库：登出 |
| `J/K` | 队列内下移 / 上移 |
| `Space` / `m` | 暂停 / 静音 |
| `n` / `p` | 下一首 / 上一首 |
| `←` / `→` | 快退 / 快进 5s |
| `-` / `=` | 音量 ∓5% |
| `r` / `t` / `s` | 循环模式 / 电台续播 / 打乱 |
| `l` | 播放详情页（封面 + 歌词） |
| `?` | 帮助 |
| `q` | 退出 |

鼠标：单击选中 · 双击播放/打开 · 滚轮滚动 · 点击进度条跳转 · 点击搜索页签切分类。
（终端内选择文本请用 Shift+拖动）

## 配置

首次运行自动生成 `%APPDATA%\ytbm-tui\config\config.toml`：

```toml
[playback]
mpv_path = "mpv"     # 自动发现失败时可写绝对路径
volume = 70
radio_auto = true

[sponsorblock]
enabled = true
categories = ["sponsor", "selfpromo", "interaction", "music_offtopic"]

[lyrics]
enabled = true

[keys]           # 全局键位可重映射（列表内导航键固定）
# next = "b"     # 动作名: quit/search/library/help/focus/play_pause/mute/next/
# vol_up = "]"   #   prev/seek_back/seek_fwd/vol_down/vol_up/repeat/radio/
                 #   shuffle/lyrics/restart_player
```

日志：`%APPDATA%\ytbm-tui\data\ytbm-tui.log`（TUI 模式下所有诊断信息写入文件）。

## 故障排查

- **播放一直"解析中"**：多半是 yt-dlp 过旧或网络不通 YouTube，
  运行 `scoop update yt-dlp`（或 `winget upgrade yt-dlp`）；播放条会显示已等待秒数
- **yt-dlp 警告缺少 JS 运行时**：安装 deno，或已有 node 时在 `yt-dlp.conf`
  中加一行 `--js-runtimes node`
- **提示未检测到 mpv**：`scoop install mpv`；程序会自动搜 scoop/winget 安装位置
- **mpv 崩溃**：界面会提示，按 `R` 重启播放器进程，队列不丢失
- **浏览器导入失败**：Chrome 系需完全关闭浏览器后重试；Firefox 无此限制；
  或改用「手动粘贴 Cookie」
- **登录后接口报错**：Cookie 已被 YouTube 轮换失效，重新导入一次即可

## 合规说明

本项目为个人学习/研究用途，使用非官方公开接口；不绕过任何 DRM，
不缓存、不内置、不分发任何 YouTube 内容。登录 Cookie 仅保存在本机、
仅用于向 YouTube 发起请求。使用产生的流量及 YouTube 服务条款相关风险由使用者自行承担。

## License

GPL-3.0-only（依赖 [rustypipe](https://codeberg.org/ThetaDev/rustypipe)，其为 GPL-3.0 协议）
