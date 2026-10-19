use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, SecondsFormat, TimeZone, Utc};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn now() -> DateTime<Utc> {
    let n = Utc::now();
    Utc.timestamp_opt(n.timestamp(), 0).single().unwrap_or(n)
}

pub fn fmt_time(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn fmt_date(t: &DateTime<Utc>) -> String {
    t.format("%Y-%m-%d").to_string()
}

pub fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    for f in ["%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f %:z"] {
        if let Ok(t) = DateTime::parse_from_str(s, f) {
            return Some(t.with_timezone(&Utc));
        }
    }
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, f) {
            return Some(Utc.from_utc_datetime(&t));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?));
    }
    None
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    if getrandom::fill(&mut buf).is_err() {
        // Extremely unlikely, fall back to time and address entropy.
        let seed = Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64
            ^ (&buf as *const _ as u64)
            ^ std::process::id() as u64;
        let mut x = seed | 1;
        for b in buf.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
    }
    buf
}

/// Writes via a temp file in the same directory and a rename, so readers never see partial files.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let r = random_bytes::<6>();
    let tmp_name = format!(
        ".breadcrumb-{}.tmp",
        r.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let tmp = dir.join(tmp_name);
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all().ok();
    }
    // Windows can briefly refuse the rename while another process (indexer, git, editor) has the target open.
    let mut last_err = None;
    for attempt in 0..40 {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(25 + attempt * 5));
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(last_err.unwrap()).with_context(|| format!("writing {}", path.display()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lowercase, [a-z0-9-], collapsed dashes, trimmed, at most `max` chars.
pub fn slugify(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let mut out = out.trim_matches('-').to_string();
    if out.len() > max {
        out.truncate(max);
        out = out.trim_end_matches('-').to_string();
    }
    out
}

pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

pub fn normalize_newlines(s: &str) -> String {
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Forward slashes, with the Windows verbatim prefix removed (`\\?\UNC\host\share` becomes `//host/share`).
pub fn path_str(p: &Path) -> String {
    strip_verbatim(&p.to_string_lossy().replace('\\', "/"))
}

pub fn strip_verbatim(s: &str) -> String {
    if let Some(x) = s.strip_prefix("//?/UNC/") {
        format!("//{x}")
    } else {
        s.strip_prefix("//?/").unwrap_or(s).to_string()
    }
}

pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(3)).collect();
        t.push_str("...");
        t
    }
}

pub fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|c| c.join(p)).unwrap_or_else(|_| p.to_path_buf())
    }
}

pub fn read_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn read_body_arg(body: Option<String>, body_file: Option<PathBuf>) -> Result<Option<String>> {
    if let Some(f) = body_file {
        return Ok(Some(normalize_newlines(&read_text(&f)?)));
    }
    match body {
        Some(b) if b == "-" => {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
            Ok(Some(normalize_newlines(&s)))
        }
        other => Ok(other.map(|s| normalize_newlines(&s))),
    }
}

/// Windows gives every console child of a windowless process (MCP server, background sync) its own visible console, so all spawns go through here.
pub fn hidden(c: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}
