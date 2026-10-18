use anyhow::{Result, bail};

/// Lowercase Crockford base32: digits and letters without i, l, o, u.
pub const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
pub const ID_LEN: usize = 8;
pub const MIN_PREFIX: usize = 4;

pub fn new_id() -> String {
    let bytes = crate::util::random_bytes::<ID_LEN>();
    bytes.iter().map(|b| ALPHABET[(*b % 32) as usize] as char).collect()
}

pub fn is_id_char(c: char) -> bool {
    c.is_ascii() && ALPHABET.contains(&(c as u8))
}

pub fn is_valid_id(s: &str) -> bool {
    s.len() == ID_LEN && s.chars().all(is_id_char)
}

/// Extracts the id from a note file name like `k3m9x2qa-some-title.md`.
pub fn id_from_file_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".md").unwrap_or(name);
    let head = stem.split('-').next()?;
    if is_valid_id(head) { Some(head.to_string()) } else { None }
}

/// Resolves a unique id prefix among `candidates` (pairs of id and a display label).
/// An id that several candidates share is ambiguous too, even when given in full.
pub fn resolve_prefix(prefix: &str, candidates: &[(String, String)]) -> Result<String> {
    let p = prefix.trim().to_ascii_lowercase();
    if p.len() < MIN_PREFIX {
        bail!("id prefix '{prefix}' is too short, use at least {MIN_PREFIX} characters");
    }
    let exact: Vec<&(String, String)> = candidates.iter().filter(|(id, _)| *id == p).collect();
    let hits: Vec<&(String, String)> = if exact.is_empty() { candidates.iter().filter(|(id, _)| id.starts_with(&p)).collect() } else { exact };
    let mut list: Vec<String> = hits.iter().map(|(id, label)| format!("  {id}  {label}")).collect();
    list.sort();
    list.dedup();
    match list.len() {
        0 => bail!("no note with id '{prefix}'"),
        1 => Ok(hits[0].0.clone()),
        _ if hits.iter().all(|(id, _)| *id == p) => bail!("id '{p}' is used by several files, rename or delete the copies:
{}", list.join("
")),
        _ => bail!("id prefix '{prefix}' is ambiguous, candidates:
{}", list.join("
")),
    }
}
