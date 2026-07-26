//! Lyrics: LRCLIB (synced, primary) → YT Music plain text (fallback).

use std::sync::Arc;

use serde::Deserialize;
use tracing::debug;

use crate::api::models::Track;
use crate::api::MusicApi;

#[derive(Debug, Clone, Default)]
pub enum LyricsData {
    #[default]
    None,
    Plain(String),
    /// (timestamp seconds, line) sorted by timestamp.
    Synced(Vec<(f64, String)>),
}

#[derive(Debug, Deserialize)]
struct LrclibResponse {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
}

/// Best-effort fetch; absence of lyrics is a normal outcome, never an error.
pub async fn fetch(http: reqwest::Client, api: Arc<dyn MusicApi>, track: Track) -> LyricsData {
    if let Some(data) = fetch_lrclib(&http, &track).await {
        return data;
    }
    match api.plain_lyrics(&track.video_id).await {
        Ok(Some(text)) if !text.trim().is_empty() => LyricsData::Plain(text),
        _ => LyricsData::None,
    }
}

async fn fetch_lrclib(http: &reqwest::Client, track: &Track) -> Option<LyricsData> {
    let mut params: Vec<(&str, String)> = vec![
        ("artist_name", track.artists.clone()),
        ("track_name", track.title.clone()),
    ];
    if let Some(album) = &track.album {
        params.push(("album_name", album.clone()));
    }
    if let Some(d) = track.duration_secs {
        params.push(("duration", d.to_string()));
    }

    let resp = http
        .get("https://lrclib.net/api/get")
        .query(&params)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        debug!("lrclib miss ({})", resp.status());
        return None;
    }
    let body: LrclibResponse = resp.json().await.ok()?;

    if let Some(synced) = body.synced_lyrics.filter(|s| !s.trim().is_empty()) {
        let lines = parse_lrc(&synced);
        if !lines.is_empty() {
            return Some(LyricsData::Synced(lines));
        }
    }
    body.plain_lyrics
        .filter(|s| !s.trim().is_empty())
        .map(LyricsData::Plain)
}

/// Parse LRC text: `[mm:ss.xx]line`, multiple tags per line allowed.
/// Metadata tags like `[ar:...]` are skipped.
pub fn parse_lrc(text: &str) -> Vec<(f64, String)> {
    let mut out: Vec<(f64, String)> = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps: Vec<f64> = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(close) = stripped.find(']') else {
                break;
            };
            let tag = &stripped[..close];
            if let Some(t) = parse_timestamp(tag) {
                stamps.push(t);
                rest = stripped[close + 1..].trim_start();
            } else {
                // Metadata tag - not a lyric line.
                rest = "";
                break;
            }
        }
        if stamps.is_empty() {
            continue;
        }
        let line = rest.trim().to_string();
        for t in stamps {
            out.push((t, line.clone()));
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss.xxx` → seconds.
fn parse_timestamp(tag: &str) -> Option<f64> {
    let (m, s) = tag.split_once(':')?;
    let minutes: f64 = m.trim().parse().ok()?;
    let seconds: f64 = s.trim().parse().ok()?;
    if minutes < 0.0 || !(0.0..60.0).contains(&seconds) {
        return None;
    }
    Some(minutes * 60.0 + seconds)
}

/// Index of the line active at time `t` (last line with stamp <= t).
pub fn current_line(lines: &[(f64, String)], t: f64) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    match lines.binary_search_by(|(stamp, _)| stamp.total_cmp(&t)) {
        Ok(i) => Some(i),
        Err(0) => None, // before the first stamp
        Err(i) => Some(i - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_lrc() {
        let lrc = "[ar:Artist]\n[00:01.50]hello\n[00:10]world\n\n[01:00.250]end";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 3);
        assert!((lines[0].0 - 1.5).abs() < 1e-9);
        assert_eq!(lines[0].1, "hello");
        assert!((lines[1].0 - 10.0).abs() < 1e-9);
        assert!((lines[2].0 - 60.25).abs() < 1e-9);
    }

    #[test]
    fn parses_multi_stamp_lines() {
        let lrc = "[00:05.00][00:15.00]chorus";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].1, "chorus");
        assert_eq!(lines[1].1, "chorus");
        assert!(lines[0].0 < lines[1].0);
    }

    #[test]
    fn current_line_tracks_time() {
        let lines = vec![
            (1.0, "a".to_string()),
            (5.0, "b".to_string()),
            (9.0, "c".to_string()),
        ];
        assert_eq!(current_line(&lines, 0.0), None);
        assert_eq!(current_line(&lines, 1.0), Some(0));
        assert_eq!(current_line(&lines, 6.5), Some(1));
        assert_eq!(current_line(&lines, 100.0), Some(2));
    }
}
