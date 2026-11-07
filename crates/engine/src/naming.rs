use percent_encoding::percent_decode_str;
use std::path::Path;
use url::Url;

/// Extension used for files that are still downloading.
pub const PART_EXT: &str = ".vdpart";

/// Parse `Content-Disposition`, preferring RFC 5987 `filename*=UTF-8''...`.
pub fn from_content_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for part in value.split(';') {
        let Some((key, val)) = part.trim().split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let val = val.trim();
        if key == "filename*" {
            if let Some(idx) = val.find("''") {
                let encoded = val[idx + 2..].trim_matches('"');
                extended = Some(percent_decode_str(encoded).decode_utf8_lossy().into_owned());
            }
        } else if key == "filename" {
            plain = Some(val.trim_matches('"').to_string());
        }
    }
    extended.or(plain).filter(|s| !s.trim().is_empty())
}

pub fn from_url(url: &Url) -> Option<String> {
    let last = url.path_segments()?.rev().find(|s| !s.is_empty())?;
    let decoded = percent_decode_str(last).decode_utf8_lossy().into_owned();
    (!decoded.trim().is_empty()).then_some(decoded)
}

/// Make a name safe on every OS (Windows is the strictest).
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut s = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if s.is_empty() {
        return "download".into();
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    if s.chars().count() > 200 {
        let (stem, ext) = split_ext(&s);
        let keep = 200usize.saturating_sub(ext.chars().count());
        s = stem.chars().take(keep).collect::<String>() + ext;
    }
    s
}

fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Pick `name`, or `name (1).ext`, `name (2).ext`, ... so we never overwrite
/// an existing file or collide with another download.
pub fn unique(dir: &Path, name: &str, taken: impl Fn(&str) -> bool) -> String {
    let free = |c: &str| {
        !dir.join(c).exists() && !dir.join(format!("{c}{PART_EXT}")).exists() && !taken(c)
    };
    if free(name) {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    (1..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|c| free(c))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition() {
        assert_eq!(
            from_content_disposition(r#"attachment; filename="a b.zip""#).as_deref(),
            Some("a b.zip")
        );
        assert_eq!(
            from_content_disposition("attachment; filename=x.bin; filename*=UTF-8''%E2%9C%93.bin")
                .as_deref(),
            Some("✓.bin")
        );
        assert_eq!(from_content_disposition("inline"), None);
    }

    #[test]
    fn url_names() {
        let u = Url::parse("https://x.com/files/my%20file.iso?token=1").unwrap();
        assert_eq!(from_url(&u).as_deref(), Some("my file.iso"));
        assert_eq!(from_url(&Url::parse("https://x.com/").unwrap()), None);
    }

    #[test]
    fn no_path_traversal() {
        // A hostile server must not be able to write outside the chosen folder.
        for evil in [
            "../../evil.exe",
            r"..\..\evil.exe",
            "/etc/passwd",
            r"C:\Windows\x.dll",
            "..",
        ] {
            let name = sanitize(evil);
            assert!(
                !name.contains('/') && !name.contains('\\') && !name.contains(':'),
                "{name}"
            );
            assert_ne!(name, "..");
        }
        assert_eq!(
            from_content_disposition(r#"attachment; filename="../../evil.exe""#)
                .map(|n| sanitize(&n))
                .as_deref(),
            Some(".._.._evil.exe")
        );
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize("a/b:c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize("CON.txt"), "_CON.txt");
        assert_eq!(sanitize("  ..  "), "download");
    }
}
