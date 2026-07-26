//! Shared music domain models - deliberately independent of any backend
//! (rustypipe today, an authenticated client later).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    /// Display string, artists already joined ("A / B").
    pub artists: String,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
    /// Album art, shown on the now-playing page when the terminal can.
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlbumSummary {
    pub id: String,
    pub title: String,
    pub artists: String,
    pub year: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct ArtistSummary {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct PlaylistSummary {
    pub id: String,
    pub title: String,
    pub track_count: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Songs,
    Albums,
    Artists,
    Playlists,
}

impl SearchKind {
    pub const ALL: [SearchKind; 4] = [
        SearchKind::Songs,
        SearchKind::Albums,
        SearchKind::Artists,
        SearchKind::Playlists,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            SearchKind::Songs => "歌曲",
            SearchKind::Albums => "专辑",
            SearchKind::Artists => "歌手",
            SearchKind::Playlists => "歌单",
        }
    }
}

/// One search response - only the vector matching the queried kind is filled.
#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub albums: Vec<AlbumSummary>,
    pub artists: Vec<ArtistSummary>,
    pub playlists: Vec<PlaylistSummary>,
}

#[derive(Debug, Clone)]
pub struct AlbumDetail {
    pub title: String,
    pub artists: String,
    pub year: Option<u16>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone)]
pub struct ArtistDetail {
    pub name: String,
    pub top_tracks: Vec<Track>,
    pub albums: Vec<AlbumSummary>,
}

#[derive(Debug, Clone)]
pub struct PlaylistDetail {
    pub title: String,
    pub tracks: Vec<Track>,
}

pub fn format_duration(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}
