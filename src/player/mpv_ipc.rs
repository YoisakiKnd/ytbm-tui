//! mpv subprocess + JSON IPC transport.
//!
//! Protocol: newline-delimited JSON over a named pipe (Windows) or unix
//! socket. Requests carry `request_id` and are matched to responses; async
//! `event` messages are translated into [`PlayerEvent`]s.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, warn};

use super::PlayerEvent;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

pub struct Mpv {
    child: Child,
    line_tx: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicU64,
    shutting_down: Arc<AtomicBool>,
}

impl Mpv {
    /// Spawn mpv, connect to its IPC endpoint and start reader/writer tasks.
    /// mpv resolves YouTube URLs itself through its built-in yt-dlp hook.
    pub async fn spawn(
        mpv_path: &str,
        ytdlp_path: Option<&str>,
        initial_volume: i64,
        ev_tx: mpsc::UnboundedSender<PlayerEvent>,
    ) -> Result<Self> {
        let ipc_addr = ipc_addr();

        let mut cmd = Command::new(mpv_path);
        cmd.args([
            "--no-video",
            "--idle=yes",
            "--no-terminal",
            "--really-quiet",
            "--no-config",
            "--keep-open=no",
            "--ytdl-format=bestaudio/best",
        ])
        .arg(format!("--volume={initial_volume}"))
        .arg(format!("--input-ipc-server={ipc_addr}"));
        // Point mpv's ytdl hook at the resolved yt-dlp binary so playback
        // does not depend on the PATH of whatever terminal launched us.
        if let Some(ytdlp) = ytdlp_path {
            cmd.arg(format!("--script-opts=ytdl_hook-ytdl_path={ytdlp}"));
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let child = cmd
            .spawn()
            .with_context(|| format!("无法启动 mpv（{mpv_path}）"))?;

        let (reader, writer) = connect_with_retry(&ipc_addr).await?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let shutting_down = Arc::new(AtomicBool::new(false));

        // Writer task: serialize one JSON line per command.
        let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut writer = writer;
            while let Some(line) = line_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
            }
        });

        // Reader task: route responses by request_id, translate events.
        {
            let pending = pending.clone();
            let shutting_down = shutting_down.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(reader).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => handle_line(&line, &pending, &ev_tx).await,
                        Ok(None) => break,
                        Err(e) => {
                            warn!("mpv ipc read error: {e}");
                            break;
                        }
                    }
                }
                // Connection gone: fail all in-flight commands.
                pending.lock().await.clear();
                if !shutting_down.load(Ordering::SeqCst) {
                    let _ = ev_tx.send(PlayerEvent::Died);
                }
            });
        }

        Ok(Self {
            child,
            line_tx,
            pending,
            next_id: AtomicU64::new(1),
            shutting_down,
        })
    }

    /// Send a command and await mpv's reply. Returns the `data` field.
    pub async fn command(&self, args: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let msg = json!({ "command": args, "request_id": id }).to_string();
        self.line_tx
            .send(msg)
            .map_err(|_| anyhow!("mpv 连接已断开"))?;

        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .map_err(|_| anyhow!("mpv 响应超时"))?
            .map_err(|_| anyhow!("mpv 连接已断开"))?;

        match reply.get("error").and_then(Value::as_str) {
            Some("success") => Ok(reply.get("data").cloned().unwrap_or(Value::Null)),
            Some(err) => bail!("mpv 命令失败: {err}"),
            None => Ok(Value::Null),
        }
    }

    pub async fn observe_default_properties(&self) -> Result<()> {
        for (i, prop) in ["time-pos", "duration", "pause", "volume", "mute"]
            .iter()
            .enumerate()
        {
            self.command(json!(["observe_property", i as u64 + 1, prop]))
                .await?;
        }
        Ok(())
    }

    /// Graceful quit; falls back to kill after a short grace period.
    pub async fn shutdown(&mut self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let _ = self.command(json!(["quit"])).await;
        let waited =
            tokio::time::timeout(std::time::Duration::from_secs(2), self.child.wait()).await;
        if waited.is_err() {
            let _ = self.child.kill().await;
        }
    }
}

async fn handle_line(line: &str, pending: &Pending, ev_tx: &mpsc::UnboundedSender<PlayerEvent>) {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            warn!("bad ipc json: {e}: {line}");
            return;
        }
    };

    if let Some(id) = msg.get("request_id").and_then(Value::as_u64) {
        if let Some(tx) = pending.lock().await.remove(&id) {
            let _ = tx.send(msg);
        }
        return;
    }

    let Some(event) = msg.get("event").and_then(Value::as_str) else {
        return;
    };
    let ev = match event {
        "property-change" => {
            let name = msg.get("name").and_then(Value::as_str).unwrap_or("");
            let data = msg.get("data");
            match (name, data) {
                ("time-pos", Some(v)) => v.as_f64().map(PlayerEvent::TimePos),
                ("duration", Some(v)) => v.as_f64().map(PlayerEvent::Duration),
                ("pause", Some(v)) => v.as_bool().map(PlayerEvent::Paused),
                ("mute", Some(v)) => v.as_bool().map(PlayerEvent::Muted),
                ("volume", Some(v)) => v.as_f64().map(|f| PlayerEvent::Volume(f.round() as i64)),
                _ => None,
            }
        }
        "file-loaded" => Some(PlayerEvent::FileLoaded),
        "end-file" => {
            // Reasons: eof = natural end; error = load/decode failure;
            // stop/quit/redirect = triggered by us - not track transitions.
            let reason = msg.get("reason").and_then(Value::as_str).unwrap_or("");
            match reason {
                "eof" => Some(PlayerEvent::TrackEnded),
                "error" => {
                    let err = msg
                        .get("file_error")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string();
                    Some(PlayerEvent::LoadFailed(err))
                }
                _ => None,
            }
        }
        other => {
            debug!("mpv event ignored: {other}");
            None
        }
    };
    if let Some(ev) = ev {
        let _ = ev_tx.send(ev);
    }
}

fn ipc_addr() -> String {
    let pid = std::process::id();
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\ytbm-mpv-{pid}")
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir()
            .join(format!("ytbm-mpv-{pid}.sock"))
            .to_string_lossy()
            .into_owned()
    }
}

type IpcReader = Box<dyn AsyncRead + Send + Unpin>;
type IpcWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// mpv needs a moment to create the IPC endpoint after spawning.
async fn connect_with_retry(addr: &str) -> Result<(IpcReader, IpcWriter)> {
    for _ in 0..40 {
        match try_connect(addr).await {
            Ok(pair) => return Ok(pair),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
    bail!("连接 mpv IPC 超时（{addr}）");
}

#[cfg(windows)]
async fn try_connect(addr: &str) -> Result<(IpcReader, IpcWriter)> {
    use tokio::net::windows::named_pipe::ClientOptions;
    let client = ClientOptions::new().open(addr)?;
    let (r, w) = tokio::io::split(client);
    Ok((Box::new(r), Box::new(w)))
}

#[cfg(not(windows))]
async fn try_connect(addr: &str) -> Result<(IpcReader, IpcWriter)> {
    let stream = tokio::net::UnixStream::connect(addr).await?;
    let (r, w) = tokio::io::split(stream);
    Ok((Box::new(r), Box::new(w)))
}
