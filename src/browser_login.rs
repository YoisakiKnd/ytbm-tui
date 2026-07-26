//! Browser-based login: import YouTube cookies straight from an installed
//! browser using yt-dlp's `--cookies-from-browser`, so the user never has to
//! copy-paste anything.
//!
//! YouTube Music's library endpoints are web-client endpoints that require a
//! browser cookie (rustypipe's OAuth path only covers the TV client), so the
//! cookie is the only way in.
//!
//! Security: extracted cookies are read once, handed to the API layer, and
//! the temporary file is deleted immediately. Cookie material is never
//! logged and never rendered.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tracing::info;

/// One importable browser profile.
#[derive(Debug, Clone)]
pub struct BrowserProfile {
    /// Label shown in the UI.
    pub display: String,
    /// yt-dlp `--cookies-from-browser` argument (`name` or `name:profile`).
    pub spec: String,
}

/// Browsers yt-dlp can read, with the Windows locations we probe.
/// (yt-dlp keyword, display name, [candidate profile dirs relative to base])
struct Candidate {
    keyword: &'static str,
    display: &'static str,
    /// (base env var or scoop marker, relative path)
    paths: &'static [(Base, &'static str)],
}

#[derive(Clone, Copy)]
enum Base {
    LocalAppData,
    RoamingAppData,
    /// `<scoop>/persist/<rel>`
    ScoopPersist,
}

const CANDIDATES: &[Candidate] = &[
    Candidate {
        keyword: "firefox",
        display: "Firefox",
        paths: &[
            (Base::ScoopPersist, "firefox/profile"),
            // Standard Firefox profiles are auto-discovered by yt-dlp, so no
            // explicit path is needed - see `detect`.
        ],
    },
    Candidate {
        keyword: "chrome",
        display: "Chrome",
        paths: &[(Base::LocalAppData, "Google/Chrome/User Data")],
    },
    Candidate {
        keyword: "edge",
        display: "Edge",
        paths: &[(Base::LocalAppData, "Microsoft/Edge/User Data")],
    },
    Candidate {
        keyword: "brave",
        display: "Brave",
        paths: &[
            (Base::LocalAppData, "BraveSoftware/Brave-Browser/User Data"),
            (Base::ScoopPersist, "brave/User Data"),
        ],
    },
    Candidate {
        keyword: "vivaldi",
        display: "Vivaldi",
        paths: &[
            (Base::LocalAppData, "Vivaldi/User Data"),
            (Base::ScoopPersist, "vivaldi/User Data"),
        ],
    },
    Candidate {
        keyword: "chromium",
        display: "Chromium",
        paths: &[
            (Base::LocalAppData, "Chromium/User Data"),
            (Base::ScoopPersist, "chromium/User Data"),
        ],
    },
    Candidate {
        keyword: "opera",
        display: "Opera",
        paths: &[(Base::RoamingAppData, "Opera Software/Opera Stable")],
    },
    // Chromium forks yt-dlp has no keyword for - read them as "chrome"
    // with an explicit profile directory.
    Candidate {
        keyword: "chrome",
        display: "Thorium",
        paths: &[
            (Base::ScoopPersist, "thorium/USER_DATA"),
            (Base::LocalAppData, "Thorium/User Data"),
        ],
    },
];

/// Probe for installed browsers. Cheap filesystem checks only.
pub fn detect() -> Vec<BrowserProfile> {
    let mut out = Vec::new();

    // Firefox in its standard location: yt-dlp finds the profile itself.
    if let Some(app) = std::env::var_os("APPDATA") {
        let p = PathBuf::from(app).join("Mozilla/Firefox/Profiles");
        if dir_has_entries(&p) {
            out.push(BrowserProfile {
                display: "Firefox".into(),
                spec: "firefox".into(),
            });
        }
    }

    for c in CANDIDATES {
        for (base, rel) in c.paths {
            let Some(dir) = resolve(*base, rel) else {
                continue;
            };
            if !dir.exists() {
                continue;
            }
            let spec = format!("{}:{}", c.keyword, dir.display());
            if out.iter().any(|b| b.spec == spec) {
                continue;
            }
            out.push(BrowserProfile {
                display: format!("{} ({})", c.display, shorten(&dir)),
                spec,
            });
        }
    }
    out
}

fn resolve(base: Base, rel: &str) -> Option<PathBuf> {
    let root = match base {
        Base::LocalAppData => PathBuf::from(std::env::var_os("LOCALAPPDATA")?),
        Base::RoamingAppData => PathBuf::from(std::env::var_os("APPDATA")?),
        Base::ScoopPersist => crate::config::scoop_root()?.join("persist"),
    };
    Some(root.join(rel))
}

fn dir_has_entries(p: &Path) -> bool {
    std::fs::read_dir(p).is_ok_and(|mut it| it.next().is_some())
}

/// Keep the tail of a long path so the UI row stays readable.
const PATH_MAX_COLS: usize = 38;

fn shorten(p: &Path) -> String {
    let s = p.display().to_string();
    #[cfg(windows)]
    let s = s.replace('/', "\\");
    if s.chars().count() <= PATH_MAX_COLS {
        return s;
    }
    // Budget includes the ".." marker so the row never overflows.
    let keep = PATH_MAX_COLS - 2;
    let tail: String = s
        .chars()
        .rev()
        .take(keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("..{tail}")
}

/// Run yt-dlp to export cookies for `spec` and return the cookies.txt text.
///
/// Called from a background task - it spawns a process and does file I/O.
pub async fn export_cookies(ytdlp_path: &str, spec: &str, work_dir: &Path) -> Result<String> {
    std::fs::create_dir_all(work_dir)?;
    // Unique name so concurrent attempts cannot clobber each other.
    let out = work_dir.join(format!("cookies-import-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&out);

    let mut cmd = tokio::process::Command::new(ytdlp_path);
    // No URL: yt-dlp still writes the cookie jar, and nothing hits the
    // network. It exits non-zero ("provide at least one URL"), so the
    // output file - not the exit status - is what tells us it worked.
    cmd.arg("--cookies-from-browser")
        .arg(spec)
        .arg("--cookies")
        .arg(&out)
        .arg("--ignore-config")
        .arg("--no-warnings")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    // tokio's Command has creation_flags natively (no CommandExt needed).
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let output = cmd
        .output()
        .await
        .with_context(|| format!("运行 yt-dlp 失败（{ytdlp_path}）"))?;

    let read = std::fs::read_to_string(&out);
    // Always remove the export before doing anything else with it.
    let _ = std::fs::remove_file(&out);

    let content = match read {
        Ok(c) => c,
        Err(_) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(explain_failure(spec, &stderr));
        }
    };

    let header =
        cookie_header_from_netscape(&content).map_err(|_| anyhow::anyhow!(explain_empty(spec)))?;
    if !has_auth_credential(&header) {
        bail!(explain_empty(spec));
    }

    // Log counts only - never cookie material.
    info!(
        "imported {} youtube cookies from browser profile",
        header.split(';').count()
    );
    Ok(header)
}

/// Build a `name=value; ..` Cookie header from Netscape cookies.txt text.
///
/// We parse this ourselves rather than handing the file to rustypipe:
/// its parser only accepts rows whose domain is exactly `.youtube.com`
/// (dropping host-only rows) and rejects the whole file on any short line,
/// which some browser exports contain.
pub fn cookie_header_from_netscape(text: &str) -> Result<String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for raw in text.lines() {
        let line = raw.strip_prefix("#HttpOnly_").unwrap_or(raw).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 7 {
            continue; // tolerate malformed rows instead of failing the import
        }
        let domain = cols[0].trim_start_matches('.');
        if domain != "youtube.com" && !domain.ends_with(".youtube.com") {
            continue;
        }
        let (name, value) = (cols[5].trim(), cols[6].trim());
        if name.is_empty() || pairs.iter().any(|(n, _)| n == name) {
            continue; // first occurrence wins
        }
        pairs.push((name.to_string(), value.to_string()));
    }
    if pairs.is_empty() {
        bail!("没有找到 YouTube 的登录信息");
    }

    // Request signing (SAPISIDHASH) reads the `SAPISID` cookie by name.
    // Some profiles - Firefox in particular - only carry the equivalent
    // `__Secure-*PAPISID` value, so mirror it under the expected name;
    // without this the requests silently go out unauthenticated.
    if !pairs.iter().any(|(n, _)| n == "SAPISID") {
        if let Some(value) = ["__Secure-3PAPISID", "__Secure-1PAPISID"]
            .iter()
            .find_map(|alias| {
                pairs
                    .iter()
                    .find(|(n, v)| n == alias && !v.is_empty())
                    .map(|(_, v)| v.clone())
            })
        {
            pairs.push(("SAPISID".to_string(), value));
        }
    }

    Ok(pairs
        .into_iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; "))
}

/// Does this cookie header carry the credential requests are signed with?
/// Checked by name because that is how the signing code looks it up.
pub fn has_auth_credential(header: &str) -> bool {
    header
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .any(|(name, value)| name == "SAPISID" && !value.is_empty())
}

fn explain_empty(spec: &str) -> String {
    format!(
        "{} 尚未登录 YouTube（没有找到登录凭据）。\n请先在该浏览器登录 music.youtube.com，再回来导入。",
        spec.split(':').next().unwrap_or(spec)
    )
}

fn explain_failure(spec: &str, stderr: &str) -> String {
    let name = spec.split(':').next().unwrap_or(spec);
    let lower = stderr.to_lowercase();
    if lower.contains("could not copy") || lower.contains("permission") || lower.contains("locked")
    {
        format!("无法读取 {name} 的 Cookie 数据库，请先完全关闭该浏览器再重试。")
    } else if lower.contains("decrypt") || lower.contains("dpapi") {
        format!(
            "{name} 的 Cookie 解密失败（较新的 Chrome 系浏览器有此限制）。\n可改用 Firefox，或选择「手动粘贴 Cookie」。"
        )
    } else if lower.contains("unsupported browser") {
        format!("yt-dlp 不支持浏览器 {name}。")
    } else {
        let tail: String = stderr
            .lines()
            .filter(|l| l.contains("ERROR") || l.contains("error"))
            .next_back()
            .unwrap_or("未知错误")
            .chars()
            .take(160)
            .collect();
        format!("从 {name} 导入 Cookie 失败: {tail}")
    }
}

/// Open a URL in the user's default browser (so they can log in first).
pub fn open_in_browser(url: &str) -> Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        // The empty "" is the window title arg `start` expects before the URL.
        c.args(["/C", "start", "", url]);
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("无法打开浏览器")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_keeps_tail_of_long_paths() {
        let long = Path::new("D:/Users/Somebody/scoop/persist/vivaldi/User Data/Default/Network");
        let s = shorten(long);
        assert!(s.starts_with(".."));
        assert!(s.ends_with("Network"));
        assert!(s.chars().count() <= 38);
    }

    #[test]
    #[cfg(windows)]
    fn shorten_normalizes_separators() {
        assert_eq!(shorten(Path::new("C:/a/b")), "C:\\a\\b");
    }

    /// Rows mirror real yt-dlp output: header comments, a `#HttpOnly_`
    /// prefix, unrelated domains, a subdomain row, and a short line.
    const SAMPLE: &str = "# Netscape HTTP Cookie File\n\
         # This file is generated by yt-dlp.\n\
         \n\
         .google.com\tTRUE\t/\tTRUE\t0\tSAPISID\tgoogle-value\n\
         #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tyt-value\n\
         .youtube.com\tTRUE\t/\tTRUE\t0\tLOGIN_INFO\tinfo\n\
         music.youtube.com\tFALSE\t/\tTRUE\t0\tPREF\tpref-value\n\
         truncated-line\n";

    #[test]
    fn netscape_parser_keeps_youtube_cookies_only() {
        let header = cookie_header_from_netscape(SAMPLE).expect("should parse");
        assert!(header.contains("SAPISID=yt-value"));
        assert!(header.contains("LOGIN_INFO=info"));
        // Host-only subdomain rows count too (rustypipe's parser drops them).
        assert!(header.contains("PREF=pref-value"));
        // Other domains never leak in, even with the same cookie name.
        assert!(!header.contains("google-value"));
    }

    #[test]
    fn netscape_parser_reports_when_no_youtube_cookies() {
        let only_other = ".google.com\tTRUE\t/\tTRUE\t0\tSAPISID\tv\n";
        assert!(cookie_header_from_netscape(only_other).is_err());
    }

    #[test]
    fn auth_credential_detection() {
        assert!(has_auth_credential("SAPISID=abc; PREF=x"));
        // Present but empty is not a credential.
        assert!(!has_auth_credential("SAPISID=; PREF=x"));
        // Logged-out profiles carry these but cannot authenticate.
        assert!(!has_auth_credential("VISITOR_INFO1_LIVE=x; YSC=y; PREF=z"));
        // The alias alone is not what the signing code looks up.
        assert!(!has_auth_credential("__Secure-3PAPISID=abc"));
    }

    #[test]
    fn secure_papisid_is_mirrored_as_sapisid() {
        // Firefox profiles often carry only the __Secure- variant; requests
        // would go out unauthenticated without this mapping.
        let firefox_style = ".youtube.com\tTRUE\t/\tTRUE\t0\t__Secure-3PAPISID\tsecret\n\
             .youtube.com\tTRUE\t/\tTRUE\t0\tSID\tsid-value\n";
        let header = cookie_header_from_netscape(firefox_style).unwrap();
        assert!(header.contains("SAPISID=secret"));
        assert!(has_auth_credential(&header));
    }

    #[test]
    fn existing_sapisid_is_not_overwritten() {
        let both = ".youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\treal\n\
                    .youtube.com\tTRUE\t/\tTRUE\t0\t__Secure-3PAPISID\talias\n";
        let header = cookie_header_from_netscape(both).unwrap();
        assert!(header.contains("SAPISID=real"));
        assert!(
            !header.contains("SAPISID=alias"),
            "real value was clobbered"
        );
        assert!(
            header.contains("__Secure-3PAPISID=alias"),
            "alias was dropped"
        );
    }

    #[test]
    fn failure_hints_are_actionable() {
        assert!(
            explain_failure("chrome:X", "ERROR: could not copy Cookies").contains("关闭该浏览器")
        );
        assert!(explain_failure("chrome:X", "Failed to decrypt with DPAPI").contains("Firefox"));
        assert!(explain_empty("firefox").contains("尚未登录"));
    }
}
