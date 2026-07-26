//! Player facade: the app talks to a [`PlayerHandle`] with [`PlayerCmd`]s
//! and receives [`PlayerEvent`]s; the mpv process/IPC lives in `mpv_ipc`.

mod mpv_ipc;
pub mod queue;

use serde_json::json;
use tokio::sync::mpsc;
use tracing::{error, info};

#[derive(Debug, Clone)]
pub enum PlayerCmd {
    /// Load and play a YouTube Music track by videoId.
    Load(String),
    TogglePause,
    ToggleMute,
    SeekRel(f64),
    SeekAbs(f64),
    AddVolume(i64),
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Ready,
    InitFailed(String),
    FileLoaded,
    TimePos(f64),
    Duration(f64),
    Paused(bool),
    Volume(i64),
    Muted(bool),
    /// end-file reason=eof - natural track end, advance the queue.
    TrackEnded,
    /// end-file reason=error - extraction/decode failed for this track.
    LoadFailed(String),
    /// mpv process/IPC gone unexpectedly.
    Died,
}

#[derive(Clone)]
pub struct PlayerHandle {
    cmd_tx: mpsc::UnboundedSender<PlayerCmd>,
}

impl PlayerHandle {
    pub fn send(&self, cmd: PlayerCmd) {
        // A dead player surfaces as PlayerEvent::Died; dropping the command
        // here is fine.
        let _ = self.cmd_tx.send(cmd);
    }
}

pub fn spawn_player(
    mpv_path: String,
    ytdlp_path: Option<String>,
    initial_volume: i64,
    ev_tx: mpsc::UnboundedSender<PlayerEvent>,
) -> PlayerHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(player_task(
        mpv_path,
        ytdlp_path,
        initial_volume,
        cmd_rx,
        ev_tx,
    ));
    PlayerHandle { cmd_tx }
}

async fn player_task(
    mpv_path: String,
    ytdlp_path: Option<String>,
    initial_volume: i64,
    mut cmd_rx: mpsc::UnboundedReceiver<PlayerCmd>,
    ev_tx: mpsc::UnboundedSender<PlayerEvent>,
) {
    let mut mpv = match mpv_ipc::Mpv::spawn(
        &mpv_path,
        ytdlp_path.as_deref(),
        initial_volume,
        ev_tx.clone(),
    )
    .await
    {
        Ok(m) => m,
        Err(e) => {
            error!("player init failed: {e:#}");
            let _ = ev_tx.send(PlayerEvent::InitFailed(format!("{e:#}")));
            return;
        }
    };
    if let Err(e) = mpv.observe_default_properties().await {
        error!("observe_property failed: {e:#}");
        let _ = ev_tx.send(PlayerEvent::InitFailed(format!("{e:#}")));
        return;
    }
    info!("mpv ready");
    let _ = ev_tx.send(PlayerEvent::Ready);

    while let Some(cmd) = cmd_rx.recv().await {
        let result = match cmd {
            PlayerCmd::Load(video_id) => {
                let url = format!("https://music.youtube.com/watch?v={video_id}");
                info!("loadfile {url}");
                mpv.command(json!(["loadfile", url, "replace"])).await
            }
            PlayerCmd::TogglePause => mpv.command(json!(["cycle", "pause"])).await,
            PlayerCmd::ToggleMute => mpv.command(json!(["cycle", "mute"])).await,
            PlayerCmd::SeekRel(secs) => mpv.command(json!(["seek", secs, "relative"])).await,
            PlayerCmd::SeekAbs(secs) => mpv.command(json!(["seek", secs, "absolute"])).await,
            PlayerCmd::AddVolume(delta) => mpv.command(json!(["add", "volume", delta])).await,
            PlayerCmd::Shutdown => {
                mpv.shutdown().await;
                break;
            }
        };
        if let Err(e) = result {
            // Seek before anything is loaded etc. - not fatal, just log.
            tracing::warn!("player command failed: {e:#}");
        }
    }
    info!("player task ended");
}
