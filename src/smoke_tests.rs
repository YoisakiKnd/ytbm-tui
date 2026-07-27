//! Network smoke tests for the whole API layer - excluded from normal
//! `cargo test`, run explicitly with `cargo test -- --ignored`.

#![cfg(test)]

use std::sync::Arc;

use crate::api::models::{SearchKind, Track};
use crate::api::rustypipe::RustyPipeApi;
use crate::api::MusicApi;

fn api() -> Arc<dyn MusicApi> {
    let dir = std::env::temp_dir()
        .join("ytbm-tui-smoke")
        .join("rustypipe");
    Arc::new(RustyPipeApi::new(dir).expect("init rustypipe"))
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("ytbm-tui-smoke/0.1")
        .build()
        .unwrap()
}

#[tokio::test]
#[ignore]
async fn search_songs_zh() {
    let results = api()
        .search("周杰伦", SearchKind::Songs)
        .await
        .expect("search failed");
    assert!(!results.tracks.is_empty(), "no tracks for 周杰伦");
    let t = &results.tracks[0];
    assert!(!t.video_id.is_empty());
    assert!(!t.title.is_empty());
    println!("first: {} - {} [{}]", t.title, t.artists, t.video_id);
}

#[tokio::test]
#[ignore]
async fn search_albums_and_open_album() {
    let a = api();
    let results = a
        .search("Taylor Swift", SearchKind::Albums)
        .await
        .expect("album search failed");
    assert!(!results.albums.is_empty());
    let album = a.album(&results.albums[0].id).await.expect("album failed");
    assert!(!album.tracks.is_empty(), "album has no tracks");
    println!("album: {} ({} tracks)", album.title, album.tracks.len());
}

#[tokio::test]
#[ignore]
async fn radio_from_track() {
    let a = api();
    let results = a
        .search("Never Gonna Give You Up", SearchKind::Songs)
        .await
        .expect("search failed");
    let seed = &results.tracks[0];
    let radio = a.radio(&seed.video_id).await.expect("radio failed");
    assert!(
        radio.len() >= 5,
        "radio returned too few tracks: {}",
        radio.len()
    );
    println!("radio: {} tracks, first: {}", radio.len(), radio[0].title);
}

/// The in-process replacement for mpv's yt-dlp hook: resolve a stream URL and
/// prove it is actually fetchable by pulling the first bytes with a Range
/// request. A URL that resolves but 403s would otherwise look like success.
///
/// The URL carries credentials in its query string, so only the host and the
/// response status are printed.
#[tokio::test]
#[ignore]
async fn resolve_and_fetch_stream() {
    let a = api();
    let results = a
        .search("Never Gonna Give You Up", SearchKind::Songs)
        .await
        .expect("search failed");
    let track = &results.tracks[0];
    let url = a
        .stream_url(&track.video_id)
        .await
        .expect("stream resolution failed");
    assert!(url.starts_with("https://"), "not an https URL");

    let resp = http()
        .get(&url)
        .header("Range", "bytes=0-65535")
        .send()
        .await
        .expect("stream request failed");
    let status = resp.status();
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let bytes = resp.bytes().await.expect("reading stream body failed");
    println!(
        "stream for {}: host={host} status={status} got {} bytes",
        track.video_id,
        bytes.len()
    );
    assert!(
        status.is_success(),
        "stream URL not playable: HTTP {status}"
    );
    assert!(!bytes.is_empty(), "stream returned no data");
}

#[tokio::test]
#[ignore]
async fn yt_plain_lyrics() {
    let a = api();
    let results = a
        .search("Shape of You Ed Sheeran", SearchKind::Songs)
        .await
        .expect("search failed");
    let lyrics = a
        .plain_lyrics(&results.tracks[0].video_id)
        .await
        .expect("lyrics call failed");
    println!(
        "yt lyrics: {}",
        lyrics
            .as_deref()
            .map(|s| &s[..s.len().min(60)])
            .unwrap_or("(none)")
    );
}

#[tokio::test]
#[ignore]
async fn lrclib_synced_lyrics() {
    let track = Track {
        video_id: "test".into(),
        title: "Never Gonna Give You Up".into(),
        artists: "Rick Astley".into(),
        album: None,
        duration_secs: Some(213),
        cover_url: None,
    };
    let data = crate::lyrics::fetch(http(), api(), track).await;
    match data {
        crate::lyrics::LyricsData::Synced(lines) => {
            assert!(!lines.is_empty());
            println!(
                "synced lyrics: {} lines, first: {:?}",
                lines.len(),
                lines[0]
            );
        }
        other => panic!("expected synced lyrics, got {other:?}"),
    }
}

/// Structural check of the native browser-import plumbing: detection finds
/// real profiles and reading them yields YouTube cookies. Never prints cookie
/// material - counts and booleans only.
#[tokio::test]
#[ignore]
async fn browser_cookie_import_plumbing() {
    let browsers = crate::browser_cookies::detect();
    println!("detected {} browser profile(s):", browsers.len());
    for b in &browsers {
        println!("  {} [{:?}]", b.display, b.store);
    }
    assert!(!browsers.is_empty(), "no browsers detected");

    let work = std::env::temp_dir().join("ytbm-tui-smoke");

    // read_cookies returns a ready-to-send Cookie header, and only succeeds
    // when the profile actually carries a YouTube credential.
    let mut any_usable = false;
    for b in &browsers {
        match crate::browser_cookies::read_cookies(b, &work) {
            Ok(header) => {
                assert!(
                    crate::browser_login::has_auth_credential(&header),
                    "read succeeded without an auth credential"
                );
                println!(
                    "  {} → {} cookies, credential ok",
                    b.display,
                    header.split(';').count()
                );
                any_usable = true;
            }
            Err(e) => println!(
                "  {} → {}",
                b.display,
                e.to_string().lines().next().unwrap_or("")
            ),
        }
    }
    assert!(
        any_usable,
        "no browser profile yielded a usable YouTube credential"
    );
    // The reader must not leave its temporary DB copies behind.
    let leftovers: Vec<_> = std::fs::read_dir(&work)
        .map(|it| {
            it.filter_map(Result::ok)
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with("ff-cookies") || n.starts_with("cr-cookies")
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "temporary cookie DB copy was not cleaned up"
    );
}

/// End-to-end: browser cookie → authenticated YouTube Music library call.
/// Uses a throwaway storage dir (the real profile is untouched) and reports
/// counts only - no titles, no cookie material.
#[tokio::test]
#[ignore]
async fn browser_login_authenticates() {
    let work = std::env::temp_dir().join("ytbm-tui-smoke");
    let scratch = work.join("auth-check");
    let _ = std::fs::remove_dir_all(&scratch);

    let api = RustyPipeApi::new(scratch.clone()).expect("init api");
    assert!(!api.is_logged_in(), "fresh profile must start logged out");

    let mut authenticated = false;
    for b in crate::browser_cookies::detect() {
        let Ok(cookies) = crate::browser_cookies::read_cookies(&b, &work) else {
            continue;
        };
        match api.login_cookie(&cookies).await {
            Ok(()) => {
                assert!(api.is_logged_in());
                let playlists = api.saved_playlists().await.expect("library call failed");
                let liked = api.liked_tracks().await.map(|t| t.len());
                println!(
                    "authenticated via {} → saved playlists: {}, liked tracks: {:?}",
                    b.display,
                    playlists.len(),
                    liked
                );
                authenticated = true;
                break;
            }
            Err(e) => println!("  {} → {e:#}", b.display),
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        authenticated,
        "no browser produced a working YouTube Music login - this test \
         requires at least one browser profile on this machine to be signed \
         in to YouTube (leftover Google cookies are not enough)"
    );
}

/// Playlists come back paginated (~100 per page). Find a large public
/// playlist and prove we follow the continuations instead of stopping at
/// the first page.
#[tokio::test]
#[ignore]
async fn large_playlist_loads_past_first_page() {
    let a = api();
    let found = a
        .search("top hits 2024", SearchKind::Playlists)
        .await
        .expect("playlist search failed");
    assert!(!found.playlists.is_empty(), "no playlists found");

    // A single page is 100 items; loading more than that proves we follow
    // continuations. Try a few candidates in case the first ones are small.
    let mut best = 0usize;
    for p in found.playlists.iter().take(5) {
        let Ok(detail) = a.playlist(&p.id).await else {
            continue;
        };
        println!("  {} → {} tracks", p.title, detail.tracks.len());
        assert!(
            detail.tracks.iter().all(|t| !t.video_id.is_empty()),
            "a track came back without a playable id"
        );
        best = best.max(detail.tracks.len());
        if best > 100 {
            break;
        }
    }
    assert!(
        best > 100,
        "largest playlist loaded only {best} tracks - pagination is not \
         following continuations (one page is 100)"
    );
}

/// The now-playing page needs album art: check the API surfaces a cover URL
/// and that it downloads and decodes into a real image.
#[tokio::test]
#[ignore]
async fn track_cover_downloads_and_decodes() {
    let results = api()
        .search("Rick Astley Never Gonna Give You Up", SearchKind::Songs)
        .await
        .expect("search failed");
    let track = results.tracks.first().expect("no tracks");
    let url = track
        .cover_url
        .as_deref()
        .expect("track carries no cover url");
    println!("cover host: {}", url.split('/').nth(2).unwrap_or("?"));

    let bytes = http()
        .get(url)
        .send()
        .await
        .expect("fetch")
        .bytes()
        .await
        .expect("body");
    let img = image::load_from_memory(&bytes).expect("decode failed");
    println!("cover decoded: {}x{}", img.width(), img.height());
    assert!(img.width() >= 200 && img.height() >= 200, "cover too small");
}

#[tokio::test]
#[ignore]
async fn home_recommendations() {
    let (tracks, albums) = api().home().await.expect("home failed");
    assert!(
        !tracks.is_empty() || !albums.is_empty(),
        "home returned nothing"
    );
    println!(
        "home: {} chart tracks, {} new albums",
        tracks.len(),
        albums.len()
    );
}

#[tokio::test]
#[ignore]
async fn sponsorblock_fetch_known_video() {
    // Despacito MV - segment presence can change over time, so only assert
    // that the request itself succeeds (404 → empty list is fine too).
    let segs = crate::sponsorblock::fetch(
        http(),
        "kJQP7kiw5Fk".into(),
        vec![
            "sponsor".into(),
            "selfpromo".into(),
            "intro".into(),
            "outro".into(),
            "music_offtopic".into(),
        ],
    )
    .await;
    println!("sponsorblock segments: {}", segs.len());
    for s in &segs {
        println!("  {} {:.1}-{:.1}", s.category, s.start, s.end);
    }
}
