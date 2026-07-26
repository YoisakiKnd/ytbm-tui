//! SponsorBlock integration: fetch community-labelled segments for a video
//! and decide when to auto-skip. For music the key category is
//! `music_offtopic` (non-music sections in music videos).

use serde::Deserialize;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub category: String,
    pub skipped: bool,
}

#[derive(Debug, Deserialize)]
struct ApiSegment {
    segment: [f64; 2],
    category: String,
}

/// 404 (no data - the common case for pure audio tracks) → empty list.
pub async fn fetch(
    http: reqwest::Client,
    video_id: String,
    categories: Vec<String>,
) -> Vec<Segment> {
    let cats_json = match serde_json::to_string(&categories) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let resp = match http
        .get("https://sponsor.ajay.app/api/skipSegments")
        .query(&[
            ("videoID", video_id.as_str()),
            ("categories", cats_json.as_str()),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            debug!("sponsorblock fetch failed: {e}");
            return Vec::new();
        }
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let raw: Vec<ApiSegment> = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            debug!("sponsorblock parse failed: {e}");
            return Vec::new();
        }
    };
    let mut segs: Vec<Segment> = raw
        .into_iter()
        .filter(|s| s.segment[1] > s.segment[0])
        .map(|s| Segment {
            start: s.segment[0],
            end: s.segment[1],
            category: s.category,
            skipped: false,
        })
        .collect();
    segs.sort_by(|a, b| a.start.total_cmp(&b.start));
    segs
}

/// If `t` falls inside an unskipped segment (with >1s remaining, so we never
/// seek-loop at a boundary), mark it skipped and return the skip target.
pub fn check_skip(segments: &mut [Segment], t: f64) -> Option<(f64, String, f64)> {
    for seg in segments.iter_mut() {
        if !seg.skipped && t >= seg.start && t < seg.end && (seg.end - t) > 1.0 {
            seg.skipped = true;
            return Some((seg.end, seg.category.clone(), seg.end - seg.start));
        }
    }
    None
}

pub fn category_label(cat: &str) -> &str {
    match cat {
        "sponsor" => "广告推广",
        "selfpromo" => "自我推广",
        "interaction" => "互动提醒",
        "intro" => "片头",
        "outro" => "片尾",
        "music_offtopic" => "非音乐片段",
        "preview" => "预告回顾",
        "filler" => "闲聊填充",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64) -> Segment {
        Segment {
            start,
            end,
            category: "music_offtopic".into(),
            skipped: false,
        }
    }

    #[test]
    fn skips_inside_segment_once() {
        let mut segs = vec![seg(10.0, 25.0)];
        assert!(check_skip(&mut segs, 5.0).is_none());
        let hit = check_skip(&mut segs, 11.0).expect("should skip");
        assert!((hit.0 - 25.0).abs() < 1e-9);
        assert!((hit.2 - 15.0).abs() < 1e-9);
        // Already skipped - never fires again this play-through.
        assert!(check_skip(&mut segs, 12.0).is_none());
    }

    #[test]
    fn ignores_segment_tail_to_avoid_seek_loop() {
        let mut segs = vec![seg(10.0, 20.0)];
        assert!(check_skip(&mut segs, 19.5).is_none());
    }

    #[test]
    fn picks_matching_segment_among_many() {
        let mut segs = vec![seg(0.0, 5.0), seg(30.0, 40.0)];
        let hit = check_skip(&mut segs, 31.0).unwrap();
        assert!((hit.0 - 40.0).abs() < 1e-9);
        assert!(!segs[0].skipped);
        assert!(segs[1].skipped);
    }
}
