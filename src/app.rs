use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;
use tokio::sync::mpsc;

use crate::api::models::{
    AlbumDetail, AlbumSummary, ArtistDetail, ArtistSummary, PlaylistDetail, PlaylistSummary,
    SearchKind, SearchResults, Track,
};
use crate::api::MusicApi;
use crate::config::Config;
use crate::lyrics::{self, LyricsData};
use crate::player::queue::{Advance, Queue};
use crate::player::{PlayerCmd, PlayerEvent, PlayerHandle};
use crate::sponsorblock::{self, Segment};

/// All inputs to the app converge into this enum; the main loop owns the
/// only mutable reference to `App` and applies events sequentially.
#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Bracketed paste - needed for multi-line cookie input.
    Paste(String),
    Resize,
    Tick,
    Player(PlayerEvent),
    Api(ApiMsg),
}

/// Results of spawned async work. `seq` guards against stale responses
/// overwriting newer state.
#[derive(Debug)]
pub enum ApiMsg {
    SearchDone {
        seq: u64,
        kind: SearchKind,
        result: Result<SearchResults, String>,
    },
    AlbumDone {
        seq: u64,
        result: Result<AlbumDetail, String>,
    },
    ArtistDone {
        seq: u64,
        result: Result<ArtistDetail, String>,
    },
    PlaylistDone {
        seq: u64,
        result: Result<PlaylistDetail, String>,
    },
    RadioDone {
        result: Result<Vec<Track>, String>,
    },
    /// Audio stream URL resolved for a track (see [`MusicApi::stream_url`]).
    StreamResolved {
        video_id: String,
        attempt: u8,
        result: Result<String, String>,
    },
    LyricsDone {
        video_id: String,
        data: LyricsData,
    },
    SponsorDone {
        video_id: String,
        segments: Vec<Segment>,
    },
    CoverLoaded {
        video_id: String,
        image: Box<image::DynamicImage>,
    },
    HomeDone {
        result: Result<(Vec<Track>, Vec<AlbumSummary>), String>,
    },
    LibraryTracks {
        title: String,
        result: Result<Vec<Track>, String>,
    },
    LibraryPlaylists {
        result: Result<Vec<PlaylistSummary>, String>,
    },
    LibraryAlbums {
        result: Result<Vec<AlbumSummary>, String>,
    },
    LibraryArtists {
        result: Result<Vec<ArtistSummary>, String>,
    },
    LoginDone {
        result: Result<(), String>,
    },
    LogoutDone {
        result: Result<(), String>,
    },
}

/// Remappable global actions. List-navigation keys (j/k/Enter/a/x/..) are
/// intentionally fixed - only playback/mode keys can be overridden in
/// `[keys]` of config.toml, keyed by the name in [`DEFAULT_KEYS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Search,
    Library,
    Help,
    FocusToggle,
    PlayPause,
    Mute,
    NextTrack,
    PrevTrack,
    SeekBack,
    SeekFwd,
    VolDown,
    VolUp,
    RepeatCycle,
    RadioToggle,
    Shuffle,
    LyricsToggle,
    RestartPlayer,
    History,
}

/// (action, config name, default key)
const DEFAULT_KEYS: &[(Action, &str, KeyCode)] = &[
    (Action::Quit, "quit", KeyCode::Char('q')),
    (Action::Search, "search", KeyCode::Char('/')),
    (Action::Library, "library", KeyCode::Char('L')),
    (Action::Help, "help", KeyCode::Char('?')),
    (Action::FocusToggle, "focus", KeyCode::Tab),
    (Action::PlayPause, "play_pause", KeyCode::Char(' ')),
    (Action::Mute, "mute", KeyCode::Char('m')),
    (Action::NextTrack, "next", KeyCode::Char('n')),
    (Action::PrevTrack, "prev", KeyCode::Char('p')),
    (Action::SeekBack, "seek_back", KeyCode::Left),
    (Action::SeekFwd, "seek_fwd", KeyCode::Right),
    (Action::VolDown, "vol_down", KeyCode::Char('-')),
    (Action::VolUp, "vol_up", KeyCode::Char('=')),
    (Action::RepeatCycle, "repeat", KeyCode::Char('r')),
    (Action::RadioToggle, "radio", KeyCode::Char('t')),
    (Action::Shuffle, "shuffle", KeyCode::Char('s')),
    (Action::LyricsToggle, "lyrics", KeyCode::Char('l')),
    (Action::RestartPlayer, "restart_player", KeyCode::Char('R')),
    (Action::History, "history", KeyCode::Char('H')),
];

/// Download and decode album art. Failures are silent - a missing cover
/// must never interrupt playback.
async fn fetch_cover(http: reqwest::Client, url: String) -> Option<image::DynamicImage> {
    let bytes = http.get(&url).send().await.ok()?.bytes().await.ok()?;
    match image::load_from_memory(&bytes) {
        Ok(img) => Some(img),
        Err(e) => {
            tracing::debug!("cover decode failed: {e}");
            None
        }
    }
}

/// Detected browsers first (the one-keystroke path), then the fallbacks.
fn login_methods() -> Vec<LoginMethod> {
    let mut methods: Vec<LoginMethod> = crate::browser_cookies::detect()
        .into_iter()
        .map(|profile| LoginMethod::Browser {
            display: profile.display.clone(),
            profile: Box::new(profile),
        })
        .collect();
    methods.push(LoginMethod::OpenBrowser);
    methods.push(LoginMethod::Manual);
    methods
}

/// Shared list navigation: j/k, arrows, PageUp/Down, g/G.
/// Returns true if the key was a navigation key (even on an empty list).
fn nav_list(selected: &mut usize, len: usize, key: KeyCode) -> bool {
    let last = len.saturating_sub(1);
    match key {
        KeyCode::Up | KeyCode::Char('k') => {
            *selected = selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            *selected = (*selected + 1).min(last);
        }
        KeyCode::PageUp => *selected = selected.saturating_sub(10),
        KeyCode::PageDown => *selected = (*selected + 10).min(last),
        KeyCode::Char('g') => *selected = 0,
        KeyCode::Char('G') => *selected = last,
        _ => return false,
    }
    true
}

fn build_keymap(overrides: &HashMap<String, String>) -> HashMap<KeyCode, Action> {
    let mut map = HashMap::new();
    for (action, name, default) in DEFAULT_KEYS {
        let key = overrides
            .get(*name)
            .and_then(|s| crate::config::parse_key(s))
            .unwrap_or(*default);
        map.insert(key, *action);
    }
    map
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Main,
    Queue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainView {
    Home,
    Search,
    Browse,
    /// Full-page player: cover art, metadata and synced lyrics.
    NowPlaying,
    Library,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Search,
    Login,
}

/// Charts + new releases shown on the home screen.
pub struct HomeState {
    pub tracks: Vec<Track>,
    pub albums: Vec<AlbumSummary>,
    pub selected: usize,
    pub loading: bool,
}

impl HomeState {
    pub fn row_count(&self) -> usize {
        self.tracks.len() + self.albums.len()
    }
}

pub const LIBRARY_MENU: [&str; 5] = [
    "喜欢的音乐",
    "我的歌单",
    "收藏的专辑",
    "关注的歌手",
    "播放历史",
];

pub struct LibraryState {
    pub selected: usize,
    pub loading: bool,
}

/// One row on the login screen.
#[derive(Debug, Clone)]
pub enum LoginMethod {
    /// Import cookies by reading an installed browser's own cookie database.
    /// Boxed to keep the enum small - the other variants carry nothing.
    Browser {
        display: String,
        profile: Box<crate::browser_cookies::BrowserProfile>,
    },
    /// Open music.youtube.com so the user can sign in first.
    OpenBrowser,
    /// Fall back to pasting a cookie / cookies.txt path.
    Manual,
}

impl LoginMethod {
    pub fn label(&self) -> String {
        match self {
            LoginMethod::Browser { display, .. } => format!("从 {display} 导入登录"),
            LoginMethod::OpenBrowser => "先在浏览器登录 music.youtube.com（打开网页）".into(),
            LoginMethod::Manual => "手动粘贴 Cookie 或 cookies.txt 路径".into(),
        }
    }
}

pub struct LoginState {
    pub methods: Vec<LoginMethod>,
    pub selected: usize,
    pub busy: bool,
}

pub struct SearchState {
    pub query: String,
    pub kind_idx: usize,
    pub results: SearchResults,
    pub selected: usize,
    pub loading: bool,
}

impl SearchState {
    pub fn kind(&self) -> SearchKind {
        SearchKind::ALL[self.kind_idx]
    }

    pub fn list_len(&self) -> usize {
        match self.kind() {
            SearchKind::Songs => self.results.tracks.len(),
            SearchKind::Albums => self.results.albums.len(),
            SearchKind::Artists => self.results.artists.len(),
            SearchKind::Playlists => self.results.playlists.len(),
        }
    }
}

pub enum BrowsePage {
    Album {
        title: String,
        data: Option<AlbumDetail>,
        selected: usize,
    },
    Artist {
        name: String,
        data: Option<ArtistDetail>,
        selected: usize,
    },
    Playlist {
        title: String,
        data: Option<PlaylistDetail>,
        selected: usize,
    },
    /// Pre-fetched flat pages (library lists, history, liked music).
    Tracks {
        title: String,
        tracks: Vec<Track>,
        selected: usize,
    },
    Albums {
        title: String,
        items: Vec<AlbumSummary>,
        selected: usize,
    },
    Artists {
        title: String,
        items: Vec<ArtistSummary>,
        selected: usize,
    },
    Playlists {
        title: String,
        items: Vec<PlaylistSummary>,
        selected: usize,
    },
}

impl BrowsePage {
    /// Number of selectable rows (artist pages: top tracks then albums).
    pub fn row_count(&self) -> usize {
        match self {
            BrowsePage::Album { data, .. } => data.as_ref().map_or(0, |d| d.tracks.len()),
            BrowsePage::Playlist { data, .. } => data.as_ref().map_or(0, |d| d.tracks.len()),
            BrowsePage::Artist { data, .. } => data
                .as_ref()
                .map_or(0, |d| d.top_tracks.len() + d.albums.len()),
            BrowsePage::Tracks { tracks, .. } => tracks.len(),
            BrowsePage::Albums { items, .. } => items.len(),
            BrowsePage::Artists { items, .. } => items.len(),
            BrowsePage::Playlists { items, .. } => items.len(),
        }
    }

    fn selected_mut(&mut self) -> &mut usize {
        match self {
            BrowsePage::Album { selected, .. }
            | BrowsePage::Artist { selected, .. }
            | BrowsePage::Playlist { selected, .. }
            | BrowsePage::Tracks { selected, .. }
            | BrowsePage::Albums { selected, .. }
            | BrowsePage::Artists { selected, .. }
            | BrowsePage::Playlists { selected, .. } => selected,
        }
    }
}

pub struct LyricsState {
    pub video_id: String,
    pub data: LyricsData,
    pub loading: bool,
    /// Manual scroll offset for plain lyrics.
    pub scroll: u16,
}

/// Album art for the now-playing page. The protocol object is built once
/// per track and re-encoded by the widget whenever the area changes.
pub struct CoverState {
    pub video_id: String,
    pub protocol: Option<ratatui_image::protocol::StatefulProtocol>,
    pub loading: bool,
}

/// Mirror of the mpv-side playback state for rendering.
pub struct PlaybackState {
    pub current_title: Option<String>,
    pub loading: bool,
    /// Seconds spent in the loading state (stream-resolution feedback).
    pub loading_secs: f32,
    pub time_pos: f64,
    pub duration: f64,
    pub paused: bool,
    pub volume: i64,
    pub muted: bool,
    pub alive: bool,
}

pub struct App {
    pub config: Config,
    pub should_quit: bool,
    pub player: PlayerHandle,
    pub pb: PlaybackState,
    /// Set when the user asks to restart a dead player; main re-wires it.
    pub player_restart_requested: bool,
    pub status: Option<String>,
    status_ttl: u8,

    pub api: Arc<dyn MusicApi>,
    pub http: reqwest::Client,
    /// Scratch space for the browser cookie export.
    data_dir: std::path::PathBuf,
    tx: mpsc::UnboundedSender<AppEvent>,

    pub focus: Focus,
    pub main_view: MainView,
    prev_main_view: MainView,
    /// Some((mode, buffer)) while the user is typing (search query / cookie).
    pub input: Option<(InputMode, String)>,
    pub search: SearchState,
    pub home: HomeState,
    pub library: LibraryState,
    pub login: LoginState,
    pub browse_stack: Vec<BrowsePage>,
    pub queue: Queue,
    pub queue_selected: usize,
    pub radio_on: bool,
    radio_inflight: bool,
    pub lyrics: LyricsState,
    pub cover: CoverState,
    /// None when the terminal cannot show images at all.
    picker: Option<ratatui_image::picker::Picker>,
    /// The track being shown on the now-playing page.
    pub now_playing: Option<Track>,
    pub history: crate::history::PlaybackHistory,
    sponsor_video_id: String,
    sponsor_segments: Vec<Segment>,
    current_video_id: Option<String>,
    stream_attempt: u8,
    resume_position: Option<f64>,
    pub help_visible: bool,
    search_seq: u64,
    browse_seq: u64,
    keymap: HashMap<KeyCode, Action>,
    /// Filled by main after each draw; consumed by the mouse handler.
    pub ui_layout: crate::ui::UiLayout,
    last_click: Option<(Instant, u16, u16)>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        player: PlayerHandle,
        api: Arc<dyn MusicApi>,
        http: reqwest::Client,
        data_dir: std::path::PathBuf,
        picker: Option<ratatui_image::picker::Picker>,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        let volume = config.playback.volume;
        let radio_on = config.playback.radio_auto;
        let keymap = build_keymap(&config.keys);
        Self {
            config,
            should_quit: false,
            player,
            pb: PlaybackState {
                current_title: None,
                loading: false,
                loading_secs: 0.0,
                time_pos: 0.0,
                duration: 0.0,
                paused: false,
                volume,
                muted: false,
                alive: false,
            },
            player_restart_requested: false,
            status: None,
            status_ttl: 0,
            api,
            http,
            data_dir,
            tx,
            focus: Focus::Main,
            main_view: MainView::Home,
            prev_main_view: MainView::Home,
            input: None,
            search: SearchState {
                query: String::new(),
                kind_idx: 0,
                results: SearchResults::default(),
                selected: 0,
                loading: false,
            },
            home: HomeState {
                tracks: Vec::new(),
                albums: Vec::new(),
                selected: 0,
                loading: false,
            },
            library: LibraryState {
                selected: 0,
                loading: false,
            },
            login: LoginState {
                methods: login_methods(),
                selected: 0,
                busy: false,
            },
            browse_stack: Vec::new(),
            queue: Queue::default(),
            queue_selected: 0,
            radio_on,
            radio_inflight: false,
            lyrics: LyricsState {
                video_id: String::new(),
                data: LyricsData::None,
                loading: false,
                scroll: 0,
            },
            cover: CoverState {
                video_id: String::new(),
                protocol: None,
                loading: false,
            },
            picker,
            now_playing: None,
            history: crate::history::PlaybackHistory::default(),
            sponsor_video_id: String::new(),
            sponsor_segments: Vec::new(),
            current_video_id: None,
            stream_attempt: 0,
            resume_position: None,
            help_visible: false,
            search_seq: 0,
            browse_seq: 0,
            keymap,
            ui_layout: crate::ui::UiLayout::default(),
            last_click: None,
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
        self.status_ttl = 12; // ~3s at 250ms ticks
    }

    /// How many rows a `cols`-wide square cover needs.
    ///
    /// Terminal cells are taller than they are wide, and the exact ratio
    /// varies by font, so ask the picker rather than assuming 1:2 — getting
    /// this wrong is what makes the art come out stretched.
    pub fn cover_rows(&self, cols: u16) -> u16 {
        let (fw, fh) = match &self.picker {
            Some(p) => {
                let fs = p.font_size();
                (fs.width.max(1) as u32, fs.height.max(1) as u32)
            }
            None => (1, 2),
        };
        ((cols as u32 * fw + fh / 2) / fh).max(1) as u16
    }

    pub fn restore_session(&mut self) {
        let path = self.session_path();
        let Ok(Some(saved)) = crate::session::SavedSession::load(&path) else {
            return;
        };
        let Some(current) = saved.current else {
            return;
        };
        if self.queue.restore(saved.tracks, current, saved.repeat) {
            self.radio_on = saved.radio_on;
            self.queue_selected = current;
            self.resume_position = (saved.position > 3.0).then_some(saved.position);
            self.toast("已恢复上次队列，按 Enter 继续播放");
        }
    }

    fn session_path(&self) -> std::path::PathBuf {
        self.data_dir.join("playback-session.json")
    }

    pub fn save_session(&self) {
        let Some(saved) =
            crate::session::SavedSession::from_queue(&self.queue, self.pb.time_pos, self.radio_on)
        else {
            return;
        };
        if let Err(e) = saved.save(&self.session_path()) {
            tracing::warn!("{e:#}");
        }
    }

    pub fn load_history(&mut self) {
        match crate::history::PlaybackHistory::load(&self.data_dir.join("playback-history.json")) {
            Ok(history) => self.history = history,
            Err(e) => tracing::warn!("{e:#}"),
        }
    }

    pub fn save_history(&self) {
        if let Err(e) = self
            .history
            .save(&self.data_dir.join("playback-history.json"))
        {
            tracing::warn!("{e:#}");
        }
    }

    /// Kick off startup fetches (home recommendations).
    pub fn on_start(&mut self) {
        self.load_home();
    }

    fn load_home(&mut self) {
        if self.home.loading {
            return;
        }
        self.home.loading = true;
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = api.home().await.map_err(|e| format!("{e}"));
            let _ = tx.send(AppEvent::Api(ApiMsg::HomeDone { result }));
        });
    }

    pub fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Key(key) => self.on_key(key),
            AppEvent::Mouse(me) => self.on_mouse(me),
            AppEvent::Paste(text) => {
                if let Some((_, buf)) = self.input.as_mut() {
                    buf.push_str(&text);
                }
            }
            AppEvent::Resize => {}
            AppEvent::Tick => {
                if self.status_ttl > 0 {
                    self.status_ttl -= 1;
                    if self.status_ttl == 0 {
                        self.status = None;
                    }
                }
                if self.pb.loading {
                    self.pb.loading_secs += 0.25;
                }
            }
            AppEvent::Player(pe) => self.on_player_event(pe),
            AppEvent::Api(msg) => self.on_api_msg(msg),
        }
    }

    // ---------- playback ----------

    fn start_track_at(&mut self, index: usize) {
        let Some(track) = self.queue.track_at(index).cloned() else {
            return;
        };
        self.start_track(track);
    }

    fn start_track(&mut self, track: Track) {
        self.now_playing = Some(track.clone());
        self.history.record(track.clone());
        self.pb.current_title = Some(format!("{} - {}", track.title, track.artists));
        self.pb.loading = true;
        self.pb.loading_secs = 0.0;
        self.pb.time_pos = 0.0;
        self.pb.duration = track.duration_secs.map(f64::from).unwrap_or(0.0);
        self.resume_position = None;
        self.current_video_id = Some(track.video_id.clone());
        self.stream_attempt = 0;

        // Resolve the stream URL in-process rather than letting mpv shell out
        // to yt-dlp. The result is applied in `ApiMsg::StreamResolved`, which
        // drops it if the user has moved on to another track meanwhile.
        {
            let api = self.api.clone();
            let tx = self.tx.clone();
            let vid = track.video_id.clone();
            tokio::spawn(async move {
                let result = tokio::time::timeout(Duration::from_secs(20), api.stream_url(&vid))
                    .await
                    .map_err(|_| "解析播放地址超时".to_string())
                    .and_then(|result| result.map_err(|e| format!("{e:#}")));
                let _ = tx.send(AppEvent::Api(ApiMsg::StreamResolved {
                    video_id: vid,
                    attempt: 0,
                    result,
                }));
            });
        }

        // Lyrics for the new track.
        self.lyrics = LyricsState {
            video_id: track.video_id.clone(),
            data: LyricsData::None,
            loading: self.config.lyrics.enabled,
            scroll: 0,
        };
        if self.config.lyrics.enabled {
            let http = self.http.clone();
            let api = self.api.clone();
            let tx = self.tx.clone();
            let t = track.clone();
            tokio::spawn(async move {
                let data = lyrics::fetch(http, api, t.clone()).await;
                let _ = tx.send(AppEvent::Api(ApiMsg::LyricsDone {
                    video_id: t.video_id,
                    data,
                }));
            });
        }

        // SponsorBlock segments for the new track.
        // Album art - only fetched when the terminal can display it.
        self.cover = CoverState {
            video_id: track.video_id.clone(),
            protocol: None,
            loading: false,
        };
        if let (Some(url), true) = (track.cover_url.clone(), self.picker.is_some()) {
            self.cover.loading = true;
            let http = self.http.clone();
            let tx = self.tx.clone();
            let vid = track.video_id.clone();
            tokio::spawn(async move {
                if let Some(image) = fetch_cover(http, url).await {
                    let _ = tx.send(AppEvent::Api(ApiMsg::CoverLoaded {
                        video_id: vid,
                        image: Box::new(image),
                    }));
                }
            });
        }

        self.sponsor_video_id = track.video_id.clone();
        self.sponsor_segments.clear();
        if self.config.sponsorblock.enabled {
            let http = self.http.clone();
            let tx = self.tx.clone();
            let vid = track.video_id.clone();
            let cats = self.config.sponsorblock.categories.clone();
            tokio::spawn(async move {
                let segments = sponsorblock::fetch(http, vid.clone(), cats).await;
                let _ = tx.send(AppEvent::Api(ApiMsg::SponsorDone {
                    video_id: vid,
                    segments,
                }));
            });
        }

        self.maybe_refill_radio();
    }

    /// Play `start` within `tracks`, making that list the queue so playback
    /// continues in list order (an album keeps playing as an album).
    fn play_context(&mut self, tracks: Vec<Track>, start: usize) {
        let count = tracks.len();
        match self.queue.set_context(tracks, start) {
            Some(idx) => {
                self.start_track_at(idx);
                if count > 1 {
                    self.toast(format!("播放中 - 队列 {} 首（第 {} 首）", count, idx + 1));
                }
            }
            None => self.toast("列表为空"),
        }
    }

    fn stop_playback_ui(&mut self) {
        self.pb.current_title = None;
        self.pb.loading = false;
        self.pb.time_pos = 0.0;
        self.pb.duration = 0.0;
        self.current_video_id = None;
    }

    fn advance(&mut self, advance: Advance) {
        match advance {
            Advance::Play(i) => self.start_track_at(i),
            Advance::Stop => {
                self.stop_playback_ui();
                self.toast("队列播放完毕");
            }
        }
    }

    fn on_player_event(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Ready => {
                self.pb.alive = true;
            }
            PlayerEvent::InitFailed(e) => {
                self.pb.alive = false;
                self.toast(format!("播放器启动失败: {e}"));
            }
            PlayerEvent::FileLoaded => {
                self.pb.loading = false;
                self.pb.loading_secs = 0.0;
                self.pb.time_pos = 0.0;
                if let Some(position) = self.resume_position.take() {
                    self.player.send(PlayerCmd::SeekAbs(position));
                    self.pb.time_pos = position;
                }
            }
            PlayerEvent::TimePos(t) => {
                self.pb.time_pos = t;
                self.check_sponsor_skip(t);
            }
            PlayerEvent::Duration(d) => self.pb.duration = d,
            PlayerEvent::Paused(p) => self.pb.paused = p,
            PlayerEvent::Volume(v) => self.pb.volume = v,
            PlayerEvent::Muted(m) => self.pb.muted = m,
            PlayerEvent::TrackEnded => {
                let adv = self.queue.advance_on_end();
                self.advance(adv);
            }
            PlayerEvent::LoadFailed(e) => {
                self.pb.loading = false;
                self.toast(format!("加载失败，跳到下一首: {e}"));
                let adv = self.queue.next_manual();
                self.advance(adv);
            }
            PlayerEvent::Died => {
                self.pb.alive = false;
                self.toast("mpv 已退出 - 按 R 重启播放器");
            }
        }
    }

    fn check_sponsor_skip(&mut self, t: f64) {
        if !self.config.sponsorblock.enabled {
            return;
        }
        if self.current_video_id.as_deref() != Some(self.sponsor_video_id.as_str()) {
            return;
        }
        if let Some((to, category, len)) = sponsorblock::check_skip(&mut self.sponsor_segments, t) {
            self.player.send(PlayerCmd::SeekAbs(to));
            self.toast(format!(
                "已跳过{} {:.0}s",
                sponsorblock::category_label(&category),
                len
            ));
        }
    }

    // ---------- async task launchers ----------

    fn fire_kind_search(&mut self) {
        if self.search.query.is_empty() {
            return;
        }
        self.search.loading = true;
        let seq = self.search_seq;
        let kind = self.search.kind();
        let query = self.search.query.clone();
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = api.search(&query, kind).await.map_err(|e| format!("{e}"));
            let _ = tx.send(AppEvent::Api(ApiMsg::SearchDone { seq, kind, result }));
        });
    }

    fn open_album(&mut self, id: String, title: String) {
        self.browse_stack.push(BrowsePage::Album {
            title,
            data: None,
            selected: 0,
        });
        self.main_view = MainView::Browse;
        self.browse_seq += 1;
        let seq = self.browse_seq;
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = api.album(&id).await.map_err(|e| format!("{e}"));
            let _ = tx.send(AppEvent::Api(ApiMsg::AlbumDone { seq, result }));
        });
    }

    fn open_artist(&mut self, id: String, name: String) {
        self.browse_stack.push(BrowsePage::Artist {
            name,
            data: None,
            selected: 0,
        });
        self.main_view = MainView::Browse;
        self.browse_seq += 1;
        let seq = self.browse_seq;
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = api.artist(&id).await.map_err(|e| format!("{e}"));
            let _ = tx.send(AppEvent::Api(ApiMsg::ArtistDone { seq, result }));
        });
    }

    fn open_playlist(&mut self, id: String, title: String) {
        self.browse_stack.push(BrowsePage::Playlist {
            title,
            data: None,
            selected: 0,
        });
        self.main_view = MainView::Browse;
        self.browse_seq += 1;
        let seq = self.browse_seq;
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = api.playlist(&id).await.map_err(|e| format!("{e}"));
            let _ = tx.send(AppEvent::Api(ApiMsg::PlaylistDone { seq, result }));
        });
    }

    fn maybe_refill_radio(&mut self) {
        if !self.radio_on || self.radio_inflight || self.queue.upcoming_count() >= 3 {
            return;
        }
        let Some(seed) = self.queue.items().last().map(|t| t.video_id.clone()) else {
            return;
        };
        self.radio_inflight = true;
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = api.radio(&seed).await.map_err(|e| format!("{e}"));
            let _ = tx.send(AppEvent::Api(ApiMsg::RadioDone { result }));
        });
    }

    // ---------- async results ----------

    fn on_api_msg(&mut self, msg: ApiMsg) {
        match msg {
            ApiMsg::SearchDone { seq, kind, result } => {
                if seq != self.search_seq {
                    return; // stale query
                }
                self.search.loading = false;
                match result {
                    Ok(r) => {
                        match kind {
                            SearchKind::Songs => self.search.results.tracks = r.tracks,
                            SearchKind::Albums => self.search.results.albums = r.albums,
                            SearchKind::Artists => self.search.results.artists = r.artists,
                            SearchKind::Playlists => self.search.results.playlists = r.playlists,
                        }
                        if self.search.kind() == kind {
                            self.search.selected = 0;
                        }
                    }
                    Err(e) => self.toast(format!("搜索失败: {e}")),
                }
            }
            ApiMsg::AlbumDone { seq, result } => {
                if seq != self.browse_seq {
                    return;
                }
                match (self.browse_stack.last_mut(), result) {
                    (Some(BrowsePage::Album { data, .. }), Ok(d)) => *data = Some(d),
                    (_, Err(e)) => {
                        self.browse_stack.pop();
                        self.sync_view_after_pop();
                        self.toast(format!("加载专辑失败: {e}"));
                    }
                    _ => {}
                }
            }
            ApiMsg::ArtistDone { seq, result } => {
                if seq != self.browse_seq {
                    return;
                }
                match (self.browse_stack.last_mut(), result) {
                    (Some(BrowsePage::Artist { data, .. }), Ok(d)) => *data = Some(d),
                    (_, Err(e)) => {
                        self.browse_stack.pop();
                        self.sync_view_after_pop();
                        self.toast(format!("加载歌手失败: {e}"));
                    }
                    _ => {}
                }
            }
            ApiMsg::PlaylistDone { seq, result } => {
                if seq != self.browse_seq {
                    return;
                }
                match (self.browse_stack.last_mut(), result) {
                    (Some(BrowsePage::Playlist { data, .. }), Ok(d)) => *data = Some(d),
                    (_, Err(e)) => {
                        self.browse_stack.pop();
                        self.sync_view_after_pop();
                        self.toast(format!("加载歌单失败: {e}"));
                    }
                    _ => {}
                }
            }
            ApiMsg::RadioDone { result } => {
                self.radio_inflight = false;
                match result {
                    Ok(tracks) => {
                        let added = self.queue.append_unique(tracks);
                        if added > 0 {
                            self.toast(format!("电台已补充 {added} 首"));
                        }
                        // Queue had run dry while the request was in flight.
                        if self.pb.current_title.is_none() && added > 0 {
                            let adv = self.queue.next_manual();
                            if let Advance::Play(i) = adv {
                                self.start_track_at(i);
                            }
                        }
                    }
                    Err(e) => {
                        if self.radio_on {
                            self.toast(format!("电台获取失败: {e}"));
                        }
                    }
                }
            }
            ApiMsg::StreamResolved {
                video_id,
                attempt,
                result,
            } => {
                if self.current_video_id.as_deref() != Some(video_id.as_str())
                    || attempt != self.stream_attempt
                {
                    return;
                }
                let url = match result {
                    Ok(url) => url,
                    Err(_e) if self.stream_attempt < 2 => {
                        self.stream_attempt += 1;
                        self.toast(format!(
                            "解析播放地址失败，正在重试 ({}/2)",
                            self.stream_attempt
                        ));
                        let api = self.api.clone();
                        let tx = self.tx.clone();
                        let retry_id = video_id.clone();
                        let retry_attempt = self.stream_attempt;
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            let result = tokio::time::timeout(
                                Duration::from_secs(20),
                                api.stream_url(&retry_id),
                            )
                            .await
                            .map_err(|_| "解析播放地址超时".to_string())
                            .and_then(|result| result.map_err(|e| format!("{e:#}")));
                            let _ = tx.send(AppEvent::Api(ApiMsg::StreamResolved {
                                video_id: retry_id,
                                attempt: retry_attempt,
                                result,
                            }));
                        });
                        return;
                    }
                    Err(e) => {
                        if self.config.ytdlp_path.is_none() {
                            tracing::warn!("stream resolution failed, no ytdl fallback: {e}");
                            self.pb.loading = false;
                            self.toast(format!("解析播放地址失败，跳到下一首: {e}"));
                            let adv = self.queue.next_manual();
                            self.advance(adv);
                            return;
                        }
                        tracing::warn!("stream resolution failed, falling back to ytdl hook: {e}");
                        format!("https://music.youtube.com/watch?v={video_id}")
                    }
                };
                self.player.send(PlayerCmd::Load(url));
            }
            ApiMsg::LyricsDone { video_id, data } => {
                if video_id == self.lyrics.video_id {
                    self.lyrics.data = data;
                    self.lyrics.loading = false;
                    self.lyrics.scroll = 0;
                }
            }
            ApiMsg::SponsorDone { video_id, segments } => {
                if video_id == self.sponsor_video_id {
                    if !segments.is_empty() {
                        tracing::info!("sponsorblock: {} segments for {video_id}", segments.len());
                    }
                    self.sponsor_segments = segments;
                }
            }
            ApiMsg::CoverLoaded { video_id, image } => {
                // A late arrival for a track we already moved past is dropped.
                if video_id == self.cover.video_id {
                    self.cover.loading = false;
                    if let Some(picker) = &self.picker {
                        self.cover.protocol = Some(picker.new_resize_protocol(*image));
                    }
                }
            }
            ApiMsg::HomeDone { result } => {
                self.home.loading = false;
                match result {
                    Ok((tracks, albums)) => {
                        self.home.tracks = tracks;
                        self.home.albums = albums;
                        self.home.selected = 0;
                    }
                    Err(e) => self.toast(format!("加载推荐失败: {e}")),
                }
            }
            ApiMsg::LibraryTracks { title, result } => {
                self.library.loading = false;
                match result {
                    Ok(tracks) => self.push_browse(BrowsePage::Tracks {
                        title,
                        tracks,
                        selected: 0,
                    }),
                    Err(e) => self.toast(format!("加载失败: {e}")),
                }
            }
            ApiMsg::LibraryPlaylists { result } => {
                self.library.loading = false;
                match result {
                    Ok(items) => self.push_browse(BrowsePage::Playlists {
                        title: "我的歌单".into(),
                        items,
                        selected: 0,
                    }),
                    Err(e) => self.toast(format!("加载失败: {e}")),
                }
            }
            ApiMsg::LibraryAlbums { result } => {
                self.library.loading = false;
                match result {
                    Ok(items) => self.push_browse(BrowsePage::Albums {
                        title: "收藏的专辑".into(),
                        items,
                        selected: 0,
                    }),
                    Err(e) => self.toast(format!("加载失败: {e}")),
                }
            }
            ApiMsg::LibraryArtists { result } => {
                self.library.loading = false;
                match result {
                    Ok(items) => self.push_browse(BrowsePage::Artists {
                        title: "关注的歌手".into(),
                        items,
                        selected: 0,
                    }),
                    Err(e) => self.toast(format!("加载失败: {e}")),
                }
            }
            ApiMsg::LoginDone { result } => {
                self.library.loading = false;
                self.login.busy = false;
                match result {
                    Ok(()) => {
                        self.library.selected = 0;
                        self.toast("登录成功 - Enter 打开音乐库");
                    }
                    // Multi-line hints render as one status line; keep the
                    // first line, which carries the actionable part.
                    Err(e) => {
                        let first = e.lines().next().unwrap_or(&e).to_string();
                        self.toast(format!("登录失败: {first}"));
                    }
                }
            }
            ApiMsg::LogoutDone { result } => match result {
                Ok(()) => {
                    self.login.selected = 0;
                    self.toast("已退出登录");
                }
                Err(e) => self.toast(format!("退出失败: {e}")),
            },
        }
    }

    fn push_browse(&mut self, page: BrowsePage) {
        self.browse_stack.push(page);
        self.main_view = MainView::Browse;
        self.focus = Focus::Main;
    }

    // ---------- key handling ----------

    fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.help_visible {
            self.help_visible = false;
            return;
        }
        if self.input.is_some() {
            self.on_input_key(key);
            return;
        }

        if let Some(action) = self.keymap.get(&key.code).copied() {
            self.run_action(action);
            return;
        }
        match self.focus {
            Focus::Queue => self.on_queue_key(key),
            Focus::Main => match self.main_view {
                MainView::Search => self.on_search_key(key),
                MainView::Browse => self.on_browse_key(key),
                MainView::NowPlaying => self.on_lyrics_key(key),
                MainView::Home => self.on_home_key(key),
                MainView::Library => self.on_library_key(key),
                MainView::History => self.on_history_key(key),
            },
        }
    }

    fn run_action(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Search => {
                self.input = Some((InputMode::Search, self.search.query.clone()));
            }
            Action::Library => {
                self.focus = Focus::Main;
                self.main_view = MainView::Library;
            }
            Action::Help => self.help_visible = true,
            Action::FocusToggle => {
                self.focus = match self.focus {
                    Focus::Main => Focus::Queue,
                    Focus::Queue => Focus::Main,
                };
            }
            Action::PlayPause => self.player.send(PlayerCmd::TogglePause),
            Action::Mute => self.player.send(PlayerCmd::ToggleMute),
            Action::SeekBack => self.player.send(PlayerCmd::SeekRel(-5.0)),
            Action::SeekFwd => self.player.send(PlayerCmd::SeekRel(5.0)),
            Action::VolDown => self.player.send(PlayerCmd::AddVolume(-5)),
            Action::VolUp => self.player.send(PlayerCmd::AddVolume(5)),
            Action::NextTrack => {
                let adv = self.queue.next_manual();
                self.advance(adv);
            }
            Action::PrevTrack => {
                if self.pb.time_pos > 3.0 {
                    self.player.send(PlayerCmd::SeekAbs(0.0));
                } else {
                    let adv = self.queue.prev_manual();
                    self.advance(adv);
                }
            }
            Action::RepeatCycle => {
                self.queue.repeat = self.queue.repeat.cycle();
                self.toast(format!("循环模式: {}", self.queue.repeat.label()));
            }
            Action::RadioToggle => {
                self.radio_on = !self.radio_on;
                self.toast(if self.radio_on {
                    "电台续播: 开"
                } else {
                    "电台续播: 关"
                });
                self.maybe_refill_radio();
            }
            Action::Shuffle => {
                self.queue.shuffle_upcoming();
                self.toast("已打乱待播队列");
            }
            Action::LyricsToggle => {
                if self.main_view == MainView::NowPlaying {
                    self.main_view = self.prev_main_view;
                } else {
                    self.prev_main_view = self.main_view;
                    self.main_view = MainView::NowPlaying;
                }
            }
            Action::RestartPlayer => {
                if !self.pb.alive {
                    self.player_restart_requested = true;
                }
            }
            Action::History => {
                self.focus = Focus::Main;
                self.main_view = MainView::History;
            }
        }
    }

    fn on_input_key(&mut self, key: KeyEvent) {
        let Some((mode, buf)) = self.input.as_mut() else {
            return;
        };
        let mode = *mode;
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Enter => {
                let text = self
                    .input
                    .take()
                    .map(|(_, b)| b.trim().to_string())
                    .unwrap_or_default();
                if text.is_empty() {
                    return;
                }
                match mode {
                    InputMode::Search => self.submit_search(text),
                    InputMode::Login => self.submit_login(text),
                }
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => buf.push(c),
            _ => {}
        }
    }

    fn submit_search(&mut self, query: String) {
        self.search.query = query;
        self.search.results = SearchResults::default();
        self.search.selected = 0;
        self.main_view = MainView::Search;
        self.focus = Focus::Main;
        self.search_seq += 1;
        self.fire_kind_search();
    }

    fn submit_login(&mut self, input: String) {
        self.library.loading = true;
        self.toast("正在验证登录..");
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = api.login_cookie(&input).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::Api(ApiMsg::LoginDone { result }));
        });
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        let len = self.search.list_len();
        if nav_list(&mut self.search.selected, len, key.code) {
            return;
        }
        match key.code {
            KeyCode::Char(c @ '1'..='4') => {
                self.search.kind_idx = (c as u8 - b'1') as usize;
                self.search.selected = 0;
                if self.search.list_len() == 0 {
                    self.fire_kind_search();
                }
            }
            KeyCode::Char('[') | KeyCode::Char(']') => {
                let n = SearchKind::ALL.len();
                self.search.kind_idx = if key.code == KeyCode::Char(']') {
                    (self.search.kind_idx + 1) % n
                } else {
                    (self.search.kind_idx + n - 1) % n
                };
                self.search.selected = 0;
                if self.search.list_len() == 0 {
                    self.fire_kind_search();
                }
            }
            KeyCode::Enter => self.activate_search_selection(),
            KeyCode::Char('a') => {
                if let Some(t) = self.selected_search_track().cloned() {
                    self.queue.append(t);
                    self.toast("已加入队列");
                    self.maybe_refill_radio();
                }
            }
            KeyCode::Char('A') => {
                if let Some(t) = self.selected_search_track().cloned() {
                    self.queue.play_next(t);
                    self.toast("将作为下一首播放");
                }
            }
            KeyCode::Esc => self.main_view = MainView::Home,
            _ => {}
        }
    }

    fn selected_search_track(&self) -> Option<&Track> {
        if self.search.kind() == SearchKind::Songs {
            self.search.results.tracks.get(self.search.selected)
        } else {
            None
        }
    }

    fn activate_search_selection(&mut self) {
        let sel = self.search.selected;
        match self.search.kind() {
            SearchKind::Songs => {
                // Continue through the remaining results rather than
                // handing over to radio after a single song.
                let list = self.search.results.tracks.clone();
                self.play_context(list, sel);
            }
            SearchKind::Albums => {
                if let Some(a) = self.search.results.albums.get(sel) {
                    self.open_album(a.id.clone(), a.title.clone());
                }
            }
            SearchKind::Artists => {
                if let Some(a) = self.search.results.artists.get(sel) {
                    self.open_artist(a.id.clone(), a.name.clone());
                }
            }
            SearchKind::Playlists => {
                if let Some(p) = self.search.results.playlists.get(sel) {
                    self.open_playlist(p.id.clone(), p.title.clone());
                }
            }
        }
    }

    fn sync_view_after_pop(&mut self) {
        if self.browse_stack.is_empty() {
            self.main_view = if self.search.query.is_empty() {
                MainView::Home
            } else {
                MainView::Search
            };
        }
    }

    fn on_browse_key(&mut self, key: KeyEvent) {
        let Some(page) = self.browse_stack.last_mut() else {
            self.sync_view_after_pop();
            return;
        };
        let rows = page.row_count();
        if nav_list(page.selected_mut(), rows, key.code) {
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => {
                self.browse_stack.pop();
                self.sync_view_after_pop();
            }
            KeyCode::Enter => self.activate_browse_selection(false),
            KeyCode::Char('a') => self.activate_browse_selection(true),
            KeyCode::Char('A') => {
                if let Some(t) = self.selected_browse_track().cloned() {
                    self.queue.play_next(t);
                    self.toast("将作为下一首播放");
                }
            }
            KeyCode::Char('P') => self.play_all_browse(),
            _ => {}
        }
    }

    fn selected_browse_track(&self) -> Option<&Track> {
        let page = self.browse_stack.last()?;
        match page {
            BrowsePage::Album { data, selected, .. } => data.as_ref()?.tracks.get(*selected),
            BrowsePage::Playlist { data, selected, .. } => data.as_ref()?.tracks.get(*selected),
            BrowsePage::Artist { data, selected, .. } => {
                let d = data.as_ref()?;
                if *selected < d.top_tracks.len() {
                    d.top_tracks.get(*selected)
                } else {
                    None
                }
            }
            BrowsePage::Tracks {
                tracks, selected, ..
            } => tracks.get(*selected),
            _ => None,
        }
    }

    /// Enter (append=false): play track / open album row.
    /// 'a' (append=true): append track to queue.
    fn activate_browse_selection(&mut self, append: bool) {
        enum Act {
            /// Play the whole list from this index (list order continues).
            PlayFrom(Vec<Track>, usize),
            Append(Track),
            OpenAlbum(String, String),
            OpenArtist(String, String),
            OpenPlaylist(String, String),
        }
        fn track_act(list: &[Track], index: usize, append: bool) -> Option<Act> {
            let track = list.get(index)?;
            Some(if append {
                Act::Append(track.clone())
            } else {
                Act::PlayFrom(list.to_vec(), index)
            })
        }
        let act = {
            let Some(page) = self.browse_stack.last() else {
                return;
            };
            let act = match page {
                BrowsePage::Album { data, selected, .. } => data
                    .as_ref()
                    .and_then(|d| track_act(&d.tracks, *selected, append)),
                BrowsePage::Playlist { data, selected, .. } => data
                    .as_ref()
                    .and_then(|d| track_act(&d.tracks, *selected, append)),
                BrowsePage::Tracks {
                    tracks, selected, ..
                } => track_act(tracks, *selected, append),
                BrowsePage::Artist { data, selected, .. } => data.as_ref().and_then(|d| {
                    if *selected < d.top_tracks.len() {
                        track_act(&d.top_tracks, *selected, append)
                    } else {
                        d.albums
                            .get(*selected - d.top_tracks.len())
                            .map(|a| Act::OpenAlbum(a.id.clone(), a.title.clone()))
                    }
                }),
                BrowsePage::Albums {
                    items, selected, ..
                } => items
                    .get(*selected)
                    .map(|a| Act::OpenAlbum(a.id.clone(), a.title.clone())),
                BrowsePage::Artists {
                    items, selected, ..
                } => items
                    .get(*selected)
                    .map(|a| Act::OpenArtist(a.id.clone(), a.name.clone())),
                BrowsePage::Playlists {
                    items, selected, ..
                } => items
                    .get(*selected)
                    .map(|p| Act::OpenPlaylist(p.id.clone(), p.title.clone())),
            };
            match act {
                Some(a) => a,
                None => return,
            }
        };
        match act {
            Act::PlayFrom(list, idx) => self.play_context(list, idx),
            Act::Append(t) => {
                self.queue.append(t);
                self.toast("已加入队列");
                self.maybe_refill_radio();
            }
            Act::OpenAlbum(id, title) => self.open_album(id, title),
            Act::OpenArtist(id, name) => self.open_artist(id, name),
            Act::OpenPlaylist(id, title) => self.open_playlist(id, title),
        }
    }

    /// 'P' on a browse page: queue every track on the page and start playing
    /// from the first one.
    fn play_all_browse(&mut self) {
        let tracks: Vec<Track> = {
            let Some(page) = self.browse_stack.last() else {
                return;
            };
            match page {
                BrowsePage::Album { data, .. } => {
                    data.as_ref().map(|d| d.tracks.clone()).unwrap_or_default()
                }
                BrowsePage::Playlist { data, .. } => {
                    data.as_ref().map(|d| d.tracks.clone()).unwrap_or_default()
                }
                BrowsePage::Tracks { tracks, .. } => tracks.clone(),
                BrowsePage::Artist { data, .. } => data
                    .as_ref()
                    .map(|d| d.top_tracks.clone())
                    .unwrap_or_default(),
                _ => Vec::new(),
            }
        };
        self.play_context(tracks, 0);
    }

    fn on_lyrics_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.main_view = self.prev_main_view,
            KeyCode::Up | KeyCode::Char('k') => {
                self.lyrics.scroll = self.lyrics.scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.lyrics.scroll = self.lyrics.scroll.saturating_add(1);
            }
            _ => {}
        }
    }

    fn on_home_key(&mut self, key: KeyEvent) {
        let rows = self.home.row_count();
        if rows == 0 && key.code == KeyCode::Char('g') {
            self.load_home(); // retry after failure
            return;
        }
        if nav_list(&mut self.home.selected, rows, key.code) {
            return;
        }
        match key.code {
            KeyCode::Enter => self.activate_home_selection(),
            KeyCode::Char('a') | KeyCode::Char('A') => {
                let sel = self.home.selected;
                if let Some(t) = self.home.tracks.get(sel).cloned() {
                    if key.code == KeyCode::Char('a') {
                        self.queue.append(t);
                        self.toast("已加入队列");
                        self.maybe_refill_radio();
                    } else {
                        self.queue.play_next(t);
                        self.toast("将作为下一首播放");
                    }
                }
            }
            _ => {}
        }
    }

    fn activate_home_selection(&mut self) {
        let sel = self.home.selected;
        if sel < self.home.tracks.len() {
            let list = self.home.tracks.clone();
            self.play_context(list, sel);
        } else if let Some(a) = self.home.albums.get(sel - self.home.tracks.len()) {
            self.open_album(a.id.clone(), a.title.clone());
        }
    }

    // ---------- mouse ----------

    fn on_mouse(&mut self, me: MouseEvent) {
        if self.help_visible {
            if matches!(me.kind, MouseEventKind::Down(_)) {
                self.help_visible = false;
            }
            return;
        }
        match me.kind {
            MouseEventKind::Down(MouseButton::Left) => self.on_click(me.column, me.row),
            MouseEventKind::ScrollUp => self.on_scroll(me.column, me.row, -3),
            MouseEventKind::ScrollDown => self.on_scroll(me.column, me.row, 3),
            _ => {}
        }
    }

    /// True when this click is the second of a double-click on the same cell.
    fn register_click(&mut self, x: u16, y: u16) -> bool {
        let now = Instant::now();
        let double = matches!(
            self.last_click,
            Some((t, lx, ly))
                if lx == x && ly == y && now.duration_since(t).as_millis() <= 400
        );
        self.last_click = if double { None } else { Some((now, x, y)) };
        double
    }

    /// Jump to a top-level destination (nav bar click).
    pub fn goto_view(&mut self, view: MainView) {
        self.focus = Focus::Main;
        match view {
            // The search tab is a no-op without a query; open the editor.
            MainView::Search if self.search.query.is_empty() => {
                self.main_view = MainView::Search;
                self.input = Some((InputMode::Search, String::new()));
            }
            MainView::NowPlaying if self.main_view != MainView::NowPlaying => {
                self.prev_main_view = self.main_view;
                self.main_view = MainView::NowPlaying;
            }
            v => self.main_view = v,
        }
    }

    fn on_click(&mut self, x: u16, y: u16) {
        let layout = self.ui_layout;
        let pos = Position::new(x, y);
        let double = self.register_click(x, y);

        // Navigation bar takes precedence over everything below it.
        for tab in layout.nav_tabs.iter().flatten() {
            if tab.0.contains(pos) {
                self.goto_view(tab.1);
                return;
            }
        }

        // Progress bar → seek to the clicked ratio.
        if let Some(g) = layout.gauge {
            if g.contains(pos) && self.pb.duration > 0.0 {
                let ratio = f64::from(x.saturating_sub(g.x)) / f64::from(g.width.max(1));
                self.player
                    .send(PlayerCmd::SeekAbs(ratio.clamp(0.0, 1.0) * self.pb.duration));
                return;
            }
        }

        // Search category tabs → approximate hit by even quarters.
        if let Some(tabs) = layout.search_tabs {
            if tabs.contains(pos) && self.main_view == MainView::Search {
                let n = SearchKind::ALL.len() as u16;
                let idx = ((x.saturating_sub(tabs.x)) * n / tabs.width.max(1)).min(n - 1);
                if idx as usize != self.search.kind_idx {
                    self.search.kind_idx = idx as usize;
                    self.search.selected = 0;
                    if self.search.list_len() == 0 {
                        self.fire_kind_search();
                    }
                }
                return;
            }
        }

        // Queue rows.
        if let Some((qa, off)) = layout.queue_list {
            if qa.contains(pos) {
                self.focus = Focus::Queue;
                let idx = (y - qa.y) as usize + off;
                if idx < self.queue.len() {
                    self.queue_selected = idx;
                    if double {
                        if let Some(i) = self.queue.jump_to(idx) {
                            self.start_track_at(i);
                        }
                    }
                }
                return;
            }
        }
        if let Some(qp) = layout.queue_pane {
            if qp.contains(pos) {
                self.focus = Focus::Queue;
                return;
            }
        }

        // Main pane lists.
        if let Some((kind, la, off)) = layout.main_list {
            if la.contains(pos) {
                self.focus = Focus::Main;
                let row = (y - la.y) as usize + off;
                use crate::ui::MainListKind;
                match kind {
                    MainListKind::Search => {
                        if row < self.search.list_len() {
                            self.search.selected = row;
                            if double {
                                self.activate_search_selection();
                            }
                        }
                    }
                    MainListKind::Browse => {
                        let rows = self.browse_stack.last().map_or(0, |p| p.row_count());
                        if row < rows {
                            if let Some(page) = self.browse_stack.last_mut() {
                                *page.selected_mut() = row;
                            }
                            if double {
                                self.activate_browse_selection(false);
                            }
                        }
                    }
                    MainListKind::Library => {
                        if self.api.is_logged_in() {
                            if row < LIBRARY_MENU.len() {
                                self.library.selected = row;
                                if double {
                                    self.open_library_item(row);
                                }
                            }
                        } else if row < self.login.methods.len() {
                            self.login.selected = row;
                            if double {
                                self.activate_login_method(row);
                            }
                        }
                    }
                    MainListKind::Home => {
                        // Physical rows include the two section headers.
                        let t = self.home.tracks.len();
                        let logical = if row == 0 || row == t + 1 {
                            None
                        } else if row <= t {
                            Some(row - 1)
                        } else {
                            Some(row - 2)
                        };
                        if let Some(idx) = logical.filter(|i| *i < self.home.row_count()) {
                            self.home.selected = idx;
                            if double {
                                self.activate_home_selection();
                            }
                        }
                    }
                }
            }
        }
    }

    fn on_scroll(&mut self, x: u16, y: u16, delta: i32) {
        let layout = self.ui_layout;
        let pos = Position::new(x, y);
        let step = |sel: usize, len: usize| -> usize {
            if len == 0 {
                return 0;
            }
            if delta < 0 {
                sel.saturating_sub(delta.unsigned_abs() as usize)
            } else {
                (sel + delta as usize).min(len - 1)
            }
        };
        if let Some((qa, _)) = layout.queue_list {
            if qa.contains(pos) {
                self.queue_selected = step(self.queue_selected, self.queue.len());
                return;
            }
        }
        if let Some((kind, la, _)) = layout.main_list {
            if la.contains(pos) {
                use crate::ui::MainListKind;
                match kind {
                    MainListKind::Search => {
                        self.search.selected = step(self.search.selected, self.search.list_len());
                    }
                    MainListKind::Browse => {
                        if let Some(page) = self.browse_stack.last_mut() {
                            let rows = page.row_count();
                            let sel = page.selected_mut();
                            *sel = step(*sel, rows);
                        }
                    }
                    MainListKind::Library => {
                        if self.api.is_logged_in() {
                            self.library.selected = step(self.library.selected, LIBRARY_MENU.len());
                        } else {
                            self.login.selected =
                                step(self.login.selected, self.login.methods.len());
                        }
                    }
                    MainListKind::Home => {
                        self.home.selected = step(self.home.selected, self.home.row_count());
                    }
                }
            }
        } else if self.main_view == MainView::NowPlaying {
            self.lyrics.scroll = if delta < 0 {
                self.lyrics
                    .scroll
                    .saturating_sub(delta.unsigned_abs() as u16)
            } else {
                self.lyrics.scroll.saturating_add(delta as u16)
            };
        }
    }

    fn on_history_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.library.selected = self.library.selected.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.library.selected =
                    (self.library.selected + 1).min(self.history.entries().len().saturating_sub(1));
            }
            KeyCode::Enter => {
                if let Some(entry) = self.history.entries().get(self.library.selected) {
                    self.queue.set_context(vec![entry.track.clone()], 0);
                    self.start_track_at(0);
                }
            }
            KeyCode::Esc => self.main_view = MainView::Home,
            _ => {}
        }
    }

    fn on_library_key(&mut self, key: KeyEvent) {
        if !self.api.is_logged_in() {
            let len = self.login.methods.len();
            if nav_list(&mut self.login.selected, len, key.code) {
                return;
            }
            match key.code {
                KeyCode::Enter => self.activate_login_method(self.login.selected),
                KeyCode::Esc => self.main_view = MainView::Home,
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.library.selected = self.library.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.library.selected = (self.library.selected + 1).min(LIBRARY_MENU.len() - 1);
            }
            KeyCode::Enter => self.open_library_item(self.library.selected),
            KeyCode::Char('x') => {
                let api = self.api.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = api.logout().await.map_err(|e| format!("{e}"));
                    let _ = tx.send(AppEvent::Api(ApiMsg::LogoutDone { result }));
                });
            }
            KeyCode::Esc => self.main_view = MainView::Home,
            _ => {}
        }
    }

    fn activate_login_method(&mut self, index: usize) {
        if self.login.busy {
            return;
        }
        let Some(method) = self.login.methods.get(index).cloned() else {
            return;
        };
        match method {
            LoginMethod::Manual => {
                self.input = Some((InputMode::Login, String::new()));
            }
            LoginMethod::OpenBrowser => {
                match crate::browser_login::open_in_browser("https://music.youtube.com/") {
                    Ok(()) => self.toast("已打开浏览器 - 登录后回来选择「从 .. 导入登录」"),
                    Err(e) => self.toast(format!("打开浏览器失败: {e}")),
                }
            }
            LoginMethod::Browser { display, profile } => {
                self.login.busy = true;
                self.toast(format!("正在从 {display} 读取登录信息.."));
                let api = self.api.clone();
                let tx = self.tx.clone();
                let work_dir = self.data_dir.clone();
                tokio::spawn(async move {
                    // The cookie text stays inside this task: read, handed to
                    // the API, then dropped. Never logged.
                    // Reading is blocking file I/O, so keep it off the async
                    // worker threads.
                    let read = tokio::task::spawn_blocking(move || {
                        crate::browser_cookies::read_cookies(&profile, &work_dir)
                    })
                    .await;
                    let result = match read {
                        Ok(Ok(cookies)) => api
                            .login_cookie(&cookies)
                            .await
                            .map_err(|e| format!("{e:#}")),
                        Ok(Err(e)) => Err(format!("{e:#}")),
                        Err(e) => Err(format!("读取任务失败: {e}")),
                    };
                    let _ = tx.send(AppEvent::Api(ApiMsg::LoginDone { result }));
                });
            }
        }
    }

    fn open_library_item(&mut self, index: usize) {
        if self.library.loading {
            return;
        }
        self.library.loading = true;
        let api = self.api.clone();
        let tx = self.tx.clone();
        match index {
            0 => {
                tokio::spawn(async move {
                    let result = api.liked_tracks().await.map_err(|e| format!("{e}"));
                    let _ = tx.send(AppEvent::Api(ApiMsg::LibraryTracks {
                        title: "喜欢的音乐".into(),
                        result,
                    }));
                });
            }
            1 => {
                tokio::spawn(async move {
                    let result = api.saved_playlists().await.map_err(|e| format!("{e}"));
                    let _ = tx.send(AppEvent::Api(ApiMsg::LibraryPlaylists { result }));
                });
            }
            2 => {
                tokio::spawn(async move {
                    let result = api.saved_albums().await.map_err(|e| format!("{e}"));
                    let _ = tx.send(AppEvent::Api(ApiMsg::LibraryAlbums { result }));
                });
            }
            3 => {
                tokio::spawn(async move {
                    let result = api.saved_artists().await.map_err(|e| format!("{e}"));
                    let _ = tx.send(AppEvent::Api(ApiMsg::LibraryArtists { result }));
                });
            }
            _ => {
                tokio::spawn(async move {
                    let result = api.history().await.map_err(|e| format!("{e}"));
                    let _ = tx.send(AppEvent::Api(ApiMsg::LibraryTracks {
                        title: "播放历史".into(),
                        result,
                    }));
                });
            }
        }
    }

    fn on_queue_key(&mut self, key: KeyEvent) {
        let len = self.queue.len();
        if nav_list(&mut self.queue_selected, len, key.code) {
            return;
        }
        match key.code {
            KeyCode::Enter => {
                if let Some(i) = self.queue.jump_to(self.queue_selected) {
                    self.start_track_at(i);
                }
            }
            KeyCode::Char('x') => {
                if !self.queue.remove(self.queue_selected) {
                    if Some(self.queue_selected) == self.queue.current_index() {
                        self.toast("不能移除正在播放的曲目");
                    }
                } else if self.queue_selected >= self.queue.len() && self.queue_selected > 0 {
                    self.queue_selected -= 1;
                }
            }
            KeyCode::Char('K') => {
                if let Some(j) = self.queue.swap(self.queue_selected, true) {
                    self.queue_selected = j;
                }
            }
            KeyCode::Char('J') => {
                if let Some(j) = self.queue.swap(self.queue_selected, false) {
                    self.queue_selected = j;
                }
            }
            KeyCode::Esc => self.focus = Focus::Main,
            _ => {}
        }
    }
}
