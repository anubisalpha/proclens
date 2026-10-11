//! "Check for updates" (only when the user clicks it) and the two links the footer opens.
//!
//! The check asks GitHub for the project's release list and compares versions. It never downloads
//! or runs anything: if a newer release exists the UI offers a link to its page, and the user
//! downloads and checks the file themselves. Everything that decides what is newer, or what URL is
//! opened, is plain code with tests; only `fetch_releases` and `open_url` touch Windows.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

/// The project page the footer links to.
pub const PROJECT_URL: &str = "https://github.com/anubisalpha/proclens";
const RELEASES_HOST: &str = "api.github.com";
const RELEASES_PATH: &str = "/repos/anubisalpha/proclens/releases?per_page=30";
/// Only the user's own version is ever sent (in the User-Agent); nothing else identifies them.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A parsed `major.minor.patch[-prerelease]` version. Build metadata (`+...`) is not used here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    pre: Vec<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Text(String),
}

impl Version {
    /// Accepts `0.1.0-beta.2` or `v0.1.0-beta.2`. Anything else is `None`.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let (core_part, pre_part) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut nums = core_part.split('.');
        let mut core = [0u64; 3];
        for slot in core.iter_mut() {
            *slot = number(nums.next()?)?;
        }
        if nums.next().is_some() {
            return None;
        }
        let pre = match pre_part {
            None => Vec::new(),
            Some(p) => {
                let mut out = Vec::new();
                for id in p.split('.') {
                    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                        return None;
                    }
                    out.push(match number(id) {
                        Some(n) => Ident::Num(n),
                        None => Ident::Text(id.to_string()),
                    });
                }
                out
            }
        };
        Some(Version { core, pre })
    }

    /// True for `1.0.0-beta.1`, false for `1.0.0`.
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

/// Digits only, no sign, no leading zero (other than "0" itself), fits in u64.
fn number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

impl Ord for Version {
    /// Semantic Versioning precedence: a release outranks any of its pre-releases; pre-release
    /// identifiers compare one by one, numbers numerically and below text.
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&other.pre) {
                    let o = match (a, b) {
                        (Ident::Num(x), Ident::Num(y)) => x.cmp(y),
                        (Ident::Num(_), Ident::Text(_)) => Ordering::Less,
                        (Ident::Text(_), Ident::Num(_)) => Ordering::Greater,
                        (Ident::Text(x), Ident::Text(y)) => x.cmp(y),
                    };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The fields we read from one entry of GitHub's release list.
#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    /// GitHub's own "pre-release" flag on the release.
    #[serde(default)]
    prerelease: bool,
}

/// What the footer shows after a check.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateResult {
    UpToDate { current: String },
    UpdateAvailable { current: String, latest: String, tag: String },
}

/// Picks the newest published release and compares it with `current`. Drafts and releases whose
/// tag is not a version are ignored. Pre-releases only count when `current` is itself a
/// pre-release (every ProcLens release so far is a beta): someone on a stable version is not told
/// a beta is an update.
pub fn evaluate(current: &str, releases_json: &str) -> Result<UpdateResult, String> {
    let current_v = Version::parse(current).ok_or_else(|| format!("this app's version \"{current}\" is not a version number"))?;
    let releases: Vec<Release> = serde_json::from_str(releases_json).map_err(|_| "GitHub's reply was not the release list we expected".to_string())?;
    let newest = releases
        .iter()
        .filter(|r| !r.draft)
        .filter(|r| current_v.is_prerelease() || !r.prerelease)
        .filter_map(|r| Version::parse(&r.tag_name).map(|v| (v, r)))
        .filter(|(v, _)| current_v.is_prerelease() || !v.is_prerelease())
        .max_by(|a, b| a.0.cmp(&b.0));
    match newest {
        Some((v, r)) if v > current_v => Ok(UpdateResult::UpdateAvailable {
            current: current.to_string(),
            latest: r.tag_name.trim_start_matches('v').to_string(),
            tag: r.tag_name.clone(),
        }),
        // No published release at all, or none newer than this one.
        _ => Ok(UpdateResult::UpToDate { current: current.to_string() }),
    }
}

/// The page to open for a release. The tag must itself parse as a version, so the URL can only be
/// this project's own release page.
pub fn release_url(tag: &str) -> Result<String, String> {
    match Version::parse(tag) {
        Some(_) if tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') => {
            Ok(format!("{PROJECT_URL}/releases/tag/{tag}"))
        }
        _ => Err("not a release tag".to_string()),
    }
}

#[cfg(windows)]
mod win {
    use super::{RELEASES_HOST, RELEASES_PATH};
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Networking::WinHttp::*;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    /// Closes a WinHTTP handle when dropped.
    struct Handle(*mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                let _ = unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    const MAX_BODY: usize = 1 << 20;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// GETs the release list over HTTPS using Windows' own TLS and proxy settings.
    pub fn fetch_releases(user_agent: &str) -> Result<String, String> {
        unsafe {
            let session = Handle(WinHttpOpen(
                PCWSTR(wide(user_agent).as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ));
            if session.0.is_null() {
                return Err("could not start a network session".into());
            }
            WinHttpSetTimeouts(session.0, 8000, 8000, 8000, 15000).map_err(|e| format!("timeouts: {e}"))?;
            let connection = Handle(WinHttpConnect(session.0, PCWSTR(wide(RELEASES_HOST).as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0));
            if connection.0.is_null() {
                return Err("could not connect to GitHub".into());
            }
            let request = Handle(WinHttpOpenRequest(
                connection.0,
                PCWSTR(wide("GET").as_ptr()),
                PCWSTR(wide(RELEASES_PATH).as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ));
            if request.0.is_null() {
                return Err("could not build the request".into());
            }
            let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n".encode_utf16().collect();
            WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0)
                .map_err(|_| "could not reach GitHub (offline, or blocked by a firewall or proxy?)".to_string())?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|_| "GitHub did not answer".to_string())?;

            let mut status: u32 = 0;
            let mut len = std::mem::size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut core::ffi::c_void),
                &mut len,
                std::ptr::null_mut(),
            )
            .map_err(|_| "could not read GitHub's status".to_string())?;
            if status == 403 || status == 429 {
                return Err("GitHub is rate-limiting this network right now; try again later".into());
            }
            if status != 200 {
                return Err(format!("GitHub answered with status {status}"));
            }

            let mut body = Vec::new();
            let mut buf = [0u8; 16 * 1024];
            loop {
                let mut got: u32 = 0;
                WinHttpReadData(request.0, buf.as_mut_ptr() as *mut core::ffi::c_void, buf.len() as u32, &mut got)
                    .map_err(|_| "the connection to GitHub dropped".to_string())?;
                if got == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..got as usize]);
                if body.len() > MAX_BODY {
                    return Err("GitHub's reply was unexpectedly large".into());
                }
            }
            String::from_utf8(body).map_err(|_| "GitHub's reply was not text".to_string())
        }
    }

    /// Opens `url` in the user's default browser.
    pub fn open_url(url: &str) -> Result<(), String> {
        let result = unsafe { ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL) };
        // ShellExecute reports success with a value above 32.
        if (result.0 as isize) > 32 {
            Ok(())
        } else {
            Err("could not open the browser".into())
        }
    }
}

#[tauri::command]
pub async fn check_for_update() -> Result<UpdateResult, String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        {
            let body = win::fetch_releases(&format!("ProcLens/{CURRENT_VERSION}"))?;
            evaluate(CURRENT_VERSION, &body)
        }
        #[cfg(not(windows))]
        {
            Err("update checks are only available on Windows".to_string())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn open_project_page() -> Result<(), String> {
    open(PROJECT_URL)
}

#[tauri::command]
pub fn open_release_page(tag: String) -> Result<(), String> {
    open(&release_url(&tag)?)
}

fn open(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        win::open_url(url)
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err("opening links is only available on Windows".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parses_tags_with_and_without_v() {
        assert_eq!(v("v0.1.0-beta.2"), v("0.1.0-beta.2"));
        assert!(Version::parse("0.1").is_none());
        assert!(Version::parse("0.1.0.0").is_none());
        assert!(Version::parse("01.1.0").is_none());
        assert!(Version::parse("0.1.0-").is_none());
        assert!(Version::parse("latest").is_none());
        assert!(Version::parse("").is_none());
    }

    #[test]
    fn orders_like_semver() {
        // The precedence example list from semver.org section 11.
        let ordered = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0"];
        for pair in ordered.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} should be below {}", pair[0], pair[1]);
        }
        assert!(v("0.1.0-beta.10") > v("0.1.0-beta.9"), "numeric, not text, comparison");
        assert!(v("0.2.0-beta.1") > v("0.1.9"));
        assert!(v("0.1.0") > v("0.1.0-beta.2"));
    }

    const LIST: &str = r#"[
      {"tag_name":"v0.1.0-beta.2","draft":false,"prerelease":true},
      {"tag_name":"v0.1.0-beta.1","draft":false,"prerelease":true},
      {"tag_name":"nightly","draft":false,"prerelease":true}
    ]"#;

    #[test]
    fn newer_release_is_offered() {
        match evaluate("0.1.0-beta.1", LIST).unwrap() {
            UpdateResult::UpdateAvailable { latest, tag, .. } => {
                assert_eq!(latest, "0.1.0-beta.2");
                assert_eq!(tag, "v0.1.0-beta.2");
            }
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[test]
    fn same_or_older_is_up_to_date() {
        assert!(matches!(evaluate("0.1.0-beta.2", LIST).unwrap(), UpdateResult::UpToDate { .. }));
        assert!(matches!(evaluate("0.2.0", LIST).unwrap(), UpdateResult::UpToDate { .. }));
    }

    #[test]
    fn order_in_the_list_does_not_matter_and_drafts_and_odd_tags_are_ignored() {
        let json = r#"[
          {"tag_name":"v0.1.0-beta.1","draft":false},
          {"tag_name":"v9.9.9","draft":true},
          {"tag_name":"v0.1.0-beta.3","draft":false},
          {"tag_name":"not-a-version","draft":false}
        ]"#;
        match evaluate("0.1.0-beta.2", json).unwrap() {
            UpdateResult::UpdateAvailable { tag, .. } => assert_eq!(tag, "v0.1.0-beta.3"),
            other => panic!("expected beta.3, got {other:?}"),
        }
    }

    #[test]
    fn stable_users_are_not_offered_prereleases() {
        let json = r#"[
          {"tag_name":"v0.2.0-beta.1","draft":false,"prerelease":true},
          {"tag_name":"v0.1.1","draft":false,"prerelease":false},
          {"tag_name":"v0.3.0-rc.1","draft":false}
        ]"#;
        // On stable 0.1.0 only the stable 0.1.1 counts; the newer-looking betas are ignored.
        match evaluate("0.1.0", json).unwrap() {
            UpdateResult::UpdateAvailable { tag, .. } => assert_eq!(tag, "v0.1.1"),
            other => panic!("expected 0.1.1, got {other:?}"),
        }
        // Already on the newest stable: up to date, even though a beta of a later version exists.
        assert!(matches!(evaluate("0.1.1", json).unwrap(), UpdateResult::UpToDate { .. }));
        // A beta user still sees later betas.
        match evaluate("0.1.0-beta.1", json).unwrap() {
            UpdateResult::UpdateAvailable { tag, .. } => assert_eq!(tag, "v0.3.0-rc.1"),
            other => panic!("expected the rc, got {other:?}"),
        }
    }

    #[test]
    fn empty_list_and_bad_replies() {
        assert!(matches!(evaluate("0.1.0-beta.2", "[]").unwrap(), UpdateResult::UpToDate { .. }));
        assert!(evaluate("0.1.0-beta.2", "{\"message\":\"Not Found\"}").is_err());
        assert!(evaluate("0.1.0-beta.2", "<html>").is_err());
        assert!(evaluate("garbage", "[]").is_err());
    }

    #[test]
    fn release_urls_are_only_ever_this_projects_release_pages() {
        assert_eq!(release_url("v0.1.0-beta.2").unwrap(), "https://github.com/anubisalpha/proclens/releases/tag/v0.1.0-beta.2");
        for bad in ["", "latest", "v1.0.0/../../evil", "v1.0.0?x=1", "https://evil.example", "v1.0.0 calc", "v1.0.0#x", "..\\x"] {
            assert!(release_url(bad).is_err(), "{bad:?} must be refused");
        }
    }
}
