use percent_encoding::percent_decode_str;
use std::path::Path;
use url::Url;

/// Extension used for files that are still downloading.
pub const PART_EXT: &str = ".gtfpart";

/// Parse `Content-Disposition`, preferring RFC 5987 `filename*=UTF-8''...`.
pub fn from_content_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for part in split_params(value) {
        let Some((key, val)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let val = val.trim();
        if key == "filename*" {
            // A broken `filename*` must not hide a usable `filename`.
            extended = decode_extended(val).or(extended);
        } else if key == "filename" {
            plain = Some(decode_plain(val));
        }
    }
    extended.or(plain).filter(|s| !s.trim().is_empty())
}

/// Split a header value on `;`, except inside double quotes (file names may
/// contain semicolons).
fn split_params(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (i, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

/// `"a \"b\".zip"` -> `a "b".zip`. Other backslashes are kept (servers send
/// unescaped Windows paths); `sanitize` deals with them.
fn unquote(val: &str) -> String {
    let Some(inner) = val.strip_prefix('"') else {
        return val.to_string();
    };
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    inner.replace("\\\"", "\"").replace("\\\\", "\\")
}

/// A plain `filename=`. Many servers percent-encode it although the standard
/// doesn't say so; like browsers, decode it when that gives valid UTF-8.
fn decode_plain(val: &str) -> String {
    let name = unquote(val);
    if name.contains('%') {
        if let Ok(decoded) = percent_decode_str(&name).decode_utf8() {
            return decoded.into_owned();
        }
    }
    name
}

/// RFC 5987: `charset'language'percent-encoded`, e.g. `UTF-8'en'%E2%9C%93.bin`.
fn decode_extended(val: &str) -> Option<String> {
    let mut it = val.trim_matches('"').splitn(3, '\'');
    let (charset, _language, encoded) = (it.next()?, it.next()?, it.next()?);
    let bytes = percent_decode_str(encoded);
    Some(if charset.eq_ignore_ascii_case("iso-8859-1") {
        // Latin-1 bytes are the first 256 code points.
        bytes.map(char::from).collect()
    } else {
        bytes.decode_utf8_lossy().into_owned()
    })
}

pub fn from_url(url: &Url) -> Option<String> {
    let last = url.path_segments()?.rev().find(|s| !s.is_empty())?;
    let decoded = percent_decode_str(last).decode_utf8_lossy().into_owned();
    (!decoded.trim().is_empty()).then_some(decoded)
}

/// Longest file name we create, in UTF-8 bytes. File systems allow 255
/// (bytes on Linux and macOS, UTF-16 units on Windows); the rest is room for
/// `.gtfpart` and a ` (2)` suffix.
const MAX_NAME_BYTES: usize = 200;
/// A longer "extension" is just a dot somewhere in a long name.
const MAX_EXT_BYTES: usize = 32;

/// Invisible characters that change the direction text is displayed in. They
/// can make `photo<RLO>gpj.exe` show up as `photoexe.jpg`, so they are
/// removed from names (as browsers do).
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Longest prefix of `s` that fits in `max` bytes without cutting a character.
fn truncate_bytes(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Make a name safe on every OS (Windows is the strictest).
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !is_bidi_control(*c))
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut s = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if s.len() > MAX_NAME_BYTES {
        let (stem, ext) = split_ext(&s);
        s = if ext.len() <= MAX_EXT_BYTES {
            truncate_bytes(stem, MAX_NAME_BYTES - ext.len()).to_string() + ext
        } else {
            truncate_bytes(&s, MAX_NAME_BYTES).to_string()
        };
        s.truncate(s.trim_end_matches(['.', ' ']).len());
    }
    if s.is_empty() {
        return "download".into();
    }
    // Windows also treats `CON .txt` as the device.
    let stem = s
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
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
    fn content_disposition_edge_cases() {
        let cd = from_content_disposition;
        // A language tag between the quotes (RFC 5987).
        assert_eq!(
            cd("attachment; filename*=UTF-8'en'%E2%9C%93.bin").as_deref(),
            Some("✓.bin")
        );
        assert_eq!(
            cd("attachment; filename*=iso-8859-1''caf%E9.txt").as_deref(),
            Some("café.txt")
        );
        // Semicolons and escaped quotes inside a quoted name.
        assert_eq!(
            cd(r#"attachment; filename="a;b \"c\".zip"; size=3"#).as_deref(),
            Some(r#"a;b "c".zip"#)
        );
        // Percent-encoded plain name, as many servers send it.
        assert_eq!(
            cd("attachment; filename=%E4%B8%AD%E6%96%87.zip").as_deref(),
            Some("中文.zip")
        );
        // Not valid UTF-8 once decoded: left alone.
        assert_eq!(
            cd(r#"attachment; filename="100%.txt""#).as_deref(),
            Some("100%.txt")
        );
        // A broken filename* doesn't hide a usable filename.
        assert_eq!(
            cd("attachment; filename=x.bin; filename*=garbage").as_deref(),
            Some("x.bin")
        );
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
        assert_eq!(sanitize("con .txt"), "_con .txt");
        assert_eq!(sanitize("  ..  "), "download");
    }

    #[test]
    fn long_names_fit_the_file_system() {
        // 300 three-byte characters: must stay under 255 bytes with `.gtfpart`.
        let name = sanitize(&format!("{}.tar.gz", "中".repeat(300)));
        assert!(name.len() <= MAX_NAME_BYTES, "{}", name.len());
        assert!(name.starts_with('中') && name.ends_with(".gz"));
        // A dot early in a long name is not an extension worth keeping.
        let name = sanitize(&format!("v1.{}", "x".repeat(400)));
        assert_eq!(name.len(), MAX_NAME_BYTES);
        // Cutting must not leave a trailing dot or space (invalid on Windows).
        let name = sanitize(&format!("{} bbbbbbbb", "a".repeat(MAX_NAME_BYTES - 1)));
        assert_eq!(name, "a".repeat(MAX_NAME_BYTES - 1));
    }

    #[test]
    fn strips_direction_overrides() {
        // Would be displayed as "photoexe.jpg".
        assert_eq!(sanitize("photo\u{202E}gpj.exe"), "photogpj.exe");
    }
}
