# ytbm-tui — 项目约定

轻量级 YouTube Music TUI 客户端（Rust + ratatui + mpv）。完整设计与里程碑见
`C:\Users\TenonSuzu\.claude\plans\youtubemusic-tui-curried-rain.md`。

## 构建与测试

```powershell
cargo build            # 调试构建
cargo test             # 纯逻辑 + TestBackend 渲染测试，不联网
cargo test -- --ignored # 冒烟测试，真实请求 YouTube/LRCLIB/SponsorBlock
cargo build --release  # 发布（lto+strip，单 exe）
```

**提交前必须过 CI 的三道关**（`.github/workflows/ci.yml` 用 `-D warnings`）：

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

⚠️ **先 `rustup update stable`**：CI 装的是最新 stable，新版 clippy 的 lint 更严。
本地工具链落后就会出现"本地全绿、CI 全红"（曾因落后 7 个小版本踩过）。
另外 PowerShell 里 `cargo ... | Select-String` 之后的 `$LASTEXITCODE` 是
**管道最后一个命令**的退出码，会掩盖 cargo 的失败——判断成败要用 `*> $null` 重定向。

CI 在 Windows/Linux/macOS 三平台跑。发布：推 `v*` 标签即触发 `release.yml`
交叉构建四个产物并自动建 GitHub Release，**不要手工传附件**。

运行依赖外部程序：`winget install mpv yt-dlp`（mpv 必需，yt-dlp 用于解析播放地址）。

## 架构要点

- **单一事件循环**：main.rs 拥有唯一的 `App`；一切输入（按键/tick/mpv 事件/API 结果）
  汇入 `AppEvent` mpsc 通道，主循环逐个应用后重绘。任何网络/IPC 都必须
  `tokio::spawn` 后经通道回流，**严禁阻塞主循环**。
- **播放**：`player/mpv_ipc.rs` spawn `mpv --no-video --idle` 子进程，JSON IPC
  （Windows 命名管道 `\\.\pipe\ytbm-mpv-{pid}`）。播放地址不自己解析——把
  `https://music.youtube.com/watch?v=<id>` 直接交给 mpv 的 yt-dlp hook。
  曲目结束只认 `end-file` 事件的 `reason=eof`；`stop/quit` 是我们自己触发的，忽略。
- **队列权威在 Rust 侧**（`player/queue.rs`，纯逻辑有单测）：一次只给 mpv 一个
  loadfile，结束后由 App 决定下一首。
- **元数据**：`api/` 层 `MusicApi` trait 隔离后端；实现用 rustypipe。
  **改 rustypipe 相关代码前先对照本机 registry 缓存里的实际源码校对签名，
  不要凭记忆写。**
- **登录**：音乐库接口走 `ClientType::DesktopMusic`（web 类），rustypipe 只对
  web 类客户端使用 Cookie 鉴权、仅对 `ClientType::Tv` 用 OAuth——**所以 OAuth
  设备码登录拿不到音乐库数据，必须用浏览器 Cookie**。`browser_login.rs` 用
  `yt-dlp --cookies-from-browser`（不带 URL、纯离线）导出，自己解析 Netscape
  格式成 Cookie 头（rustypipe 自带的解析器只认精确 `.youtube.com` 域名，会丢
  host-only 行）。yt-dlp 无 URL 时**退出码非零但文件已写入**，判断成败要看文件
  内容而非退出码。
- **凭据处理红线**：Cookie 内容绝不能进 tracing 日志、UI 或错误消息；导出的临时
  文件读取后立即删除；只记录条数。
- **歌词**：LRCLIB 优先（同步 LRC），回退 YT 纯文本；`lyrics.rs` 有 LRC 解析单测。
- **SponsorBlock**：`sponsorblock.rs`；每片段每曲只自动跳一次（防 seek 死循环），
  判定逻辑有单测。

## 播放详情页与图片

- `ui/now_playing.rs`（按 `l` 进入）：封面 + 元数据 + 进度 + 同步歌词。
- 图片走 **ratatui-image**：`Picker::from_query_stdio()` 探测终端协议
  （kitty/iTerm2/Sixel），失败回退 `Picker::halfblocks()`——所以任何终端都有图。
  **必须 `default-features = false`**：默认的 `chafa-dyn` 需要 pkg-config +
  chafa 原生库，Windows 上直接构建失败。
- ratatui-image 11 要求 **ratatui 0.30**；两者版本必须一致，否则
  `StatefulWidget` 是两个不同 trait，报"未实现"的怪错。
- 封面 URL 要用 `upscale_cover_url` 改写尺寸参数（搜索结果常只给 120px，
  Google 图床支持 `=w544-h544` 这类尺寸提示）。
- `ui::draw` 取 `&mut App`，因为图片协议对象在区域变化时要重新编码。

## UI 约定

- **禁止 emoji**：各终端对 emoji 的显示宽度判定不一致（1 列/2 列），会破坏列
  对齐。类型标识统一用 `ui/mod.rs` 的定宽 ASCII 徽标（`ALB `/`ART `/`LST `，
  各 4 列 + 颜色区分），播放状态用 `state_marker()`（`> `/`||`/`..`）。
- **同样禁止 East Asian Ambiguous 宽度字符**：`…` `—` `·` `→` `─` `✓` `▶` 等在
  中文环境下终端按 2 列渲染，而 `unicode-width` 按 1 列算，必然错位（还可能缺
  字形）。截断标记用 `ELLIPSIS`（`..`），分隔线用 `-`。`ui/mod.rs` 有守卫测试。
  例外：ratatui Block 边框自带的制表符是它内部处理的，不受此限。
- **列表按列排版**：用 `TrackCols::new(width)` 分配标题/歌手/时长宽度，
  文本一律走 `fit_w`/`truncate_w`（unicode-width 感知，中文按 2 列算）。
- 布局改动要用 `TestBackend` 渲染进缓冲区做断言（见 `ui/mod.rs` 的测试），
  注意宽字符在缓冲区里占两格、第二格读出来是空格。

## 代码风格

- 注释/UI 文案用中文，标识符/日志用英文。
- 错误提示要可执行（告诉用户运行什么命令），toast 走 `app.toast()`。
- Windows 专项：spawn 子进程一律加 `CREATE_NO_WINDOW`；按键只处理
  `KeyEventKind::Press`（Windows 会送 Release）。
- TUI 期间禁止任何 stdout/stderr 输出，诊断走 tracing 文件日志
  （`%APPDATA%\ytbm-tui\ytbm-tui.log`）。
