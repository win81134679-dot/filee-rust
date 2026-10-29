//! Small helpers shared by the core modules: SHA-256 (single-instance and watch-state names), upstream-style JSON
//! writing, atomic file writes, local time in the formats upstream writes, hidden child processes.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};

/// `CREATE_NO_WINDOW`.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `BELOW_NORMAL_PRIORITY_CLASS`.
pub const BELOW_NORMAL: u32 = 0x0000_4000;

// ---------------------------------------------------------------------------------------------------------------
// SHA-256 (FIPS 180-4). Small, so no crate.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// Lower-case hex of the first `n` bytes of SHA-256(UTF-8 of `text`).
pub fn sha256_hex_prefix(text: &str, n_hex: usize) -> String {
    let digest = sha256(text.as_bytes());
    let mut s = String::new();
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s.truncate(n_hex);
    s
}

// ---------------------------------------------------------------------------------------------------------------
// JSON the way upstream writes it: UTF-8 without BOM, two-space indent, non-ASCII and HTML-sensitive characters as
// \uXXXX (System.Text.Json's default encoder), CRLF or LF as the file already had.

pub fn to_upstream_json(value: &Value, crlf: bool) -> String {
    escape_like_upstream(&serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string()), crlf)
}

/// Compact (`JsonSerializer.Serialize` defaults, used by the watch state file).
pub fn to_compact_upstream_json(value: &Value) -> String {
    escape_like_upstream(&serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()), false)
}

fn escape_like_upstream(pretty: &str, crlf: bool) -> String {
    let mut out = String::with_capacity(pretty.len() + pretty.len() / 8);
    for ch in pretty.chars() {
        match ch {
            '\n' if crlf => out.push_str("\r\n"),
            '\'' | '`' | '<' | '>' | '&' => out.push_str(&format!("\\u{:04X}", ch as u32)),
            c if (c as u32) < 0x80 => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04X}"));
                }
            }
        }
    }
    out
}

/// Reads a JSON file (BOM and comments tolerated only as far as serde does: BOM yes, comments no).
pub fn read_json_file(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// True when the file's text uses CRLF.
pub fn file_uses_crlf(path: &Path, default: bool) -> bool {
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => bytes.windows(2).any(|w| w == b"\r\n"),
        _ => default,
    }
}

/// Size and last-write time without opening the file. `std::fs::metadata` opens a handle, and while any handle is
/// open upstream's save (write .tmp, then replace) fails with "access denied"; the directory listing doesn't.
pub fn file_stamp(path: &Path) -> Option<(u64, SystemTime)> {
    use windows::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, WIN32_FIND_DATAW};
    let name = wide(&path.to_string_lossy());
    let mut data = WIN32_FIND_DATAW::default();
    let handle = unsafe { FindFirstFileW(windows::core::PCWSTR(name.as_ptr()), &mut data) }.ok()?;
    let _ = unsafe { FindClose(handle) };
    let len = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
    let ticks = ((data.ftLastWriteTime.dwHighDateTime as u64) << 32) | data.ftLastWriteTime.dwLowDateTime as u64;
    let since_1970 = ticks.checked_sub(FILETIME_UNIX_EPOCH)?;
    Some((len, UNIX_EPOCH + std::time::Duration::from_nanos(since_1970 * 100)))
}

/// `<path>.tmp-<pid>` then rename over `path` (replaces; same folder). Upstream uses `<path>.tmp` itself, so our own
/// name keeps the two from truncating each other's half-written file. Replacing fails with "access denied" while
/// another process has `path` open (upstream reading it, an antivirus scan), so that is waited out for up to 2 s.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    let mut attempt = 1;
    loop {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 40 && e.kind() == std::io::ErrorKind::PermissionDenied => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Time

const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000; // 100 ns ticks between 1601 and 1970

/// Days since 1970-01-01 of a civil date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// FILETIME ticks (100 ns since 1601) of a `SystemTime`.
pub fn filetime_of(time: SystemTime) -> u64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => FILETIME_UNIX_EPOCH + (d.as_secs() * 10_000_000) + u64::from(d.subsec_nanos() / 100),
        Err(_) => 0,
    }
}

/// ISO 8601 UTC with seven fraction digits: `2026-10-28T17:48:28.9016082Z`.
pub fn utc_iso(filetime: u64) -> String {
    let (y, mo, d, h, mi, s, frac) = split_filetime(filetime);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{frac:07}Z")
}

fn split_filetime(filetime: u64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let unix_ticks = filetime.saturating_sub(FILETIME_UNIX_EPOCH);
    let secs = (unix_ticks / 10_000_000) as i64;
    let frac = (unix_ticks % 10_000_000) as i64;
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    (y, mo, d, rem / 3600, rem % 3600 / 60, rem % 60, frac)
}

/// Local time with the UTC offset, as .NET writes a local `DateTime`: `2026-10-29T01:48:28.9071793+08:00`.
pub fn local_iso_now() -> String {
    let now = filetime_of(SystemTime::now());
    let offset_min = utc_offset_minutes();
    let local = (now as i64 + offset_min * 60 * 10_000_000) as u64;
    let (y, mo, d, h, mi, s, frac) = split_filetime(local);
    let sign = if offset_min < 0 { '-' } else { '+' };
    let off = offset_min.abs();
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{frac:07}{sign}{:02}:{:02}", off / 60, off % 60)
}

/// Local minus UTC, in minutes (from the two system clocks, so daylight saving is included).
pub fn utc_offset_minutes() -> i64 {
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let secs = |t: &windows::Win32::Foundation::SYSTEMTIME| {
        days_from_civil(i64::from(t.wYear), i64::from(t.wMonth), i64::from(t.wDay)) * 86_400
            + i64::from(t.wHour) * 3600
            + i64::from(t.wMinute) * 60
            + i64::from(t.wSecond)
    };
    let diff = secs(&local) - secs(&utc);
    // Round to 15 minutes: the two reads are a few microseconds apart.
    ((diff as f64 / 900.0).round() as i64) * 15
}

/// A 10-hex-digit id (upstream's job ids), unique enough per call.
pub fn new_job_id(seed: u64) -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    sha256_hex_prefix(&format!("{nanos}-{}-{seed}", std::process::id()), 10)
}

// ---------------------------------------------------------------------------------------------------------------
// Processes and paths

/// A command that opens no console window.
pub fn hidden_command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Windows path comparison: ASCII-insensitive is not enough for upstream (OrdinalIgnoreCase), so lower-case both.
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// True when `path` is inside `folder` (never equal to it).
pub fn is_inside(path: &Path, folder: &Path) -> bool {
    let f = path_key(folder);
    if f.is_empty() {
        return false;
    }
    path_key(path).starts_with(&(f + "\\"))
}

pub fn same_folder(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_waits_while_another_handle_has_the_file_open() {
        let dir = std::env::temp_dir().join(format!("filee-rust-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, b"old").unwrap();
        // As upstream's File.OpenRead: FILE_SHARE_READ only. (Rust's own handles allow delete, and its rename uses
        // POSIX semantics, so those never block.)
        use std::os::windows::fs::OpenOptionsExt;
        let holder = std::fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(holder);
        });
        atomic_write(&path, b"new").unwrap();
        release.join().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_stamp_matches_metadata_and_does_not_block_a_replace() {
        let dir = std::env::temp_dir().join(format!("filee-rust-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, b"12345").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(file_stamp(&path), Some((5, meta.modified().unwrap())));
        assert_eq!(file_stamp(&dir.join("missing.json")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(sha256_hex_prefix("", 64), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex_prefix("abc", 64), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // The values spec 02 computed with Python: single-instance and the watch state file of the CLI.
        assert_eq!(sha256_hex_prefix("User", 12).to_uppercase(), "B512D97E7CBF");
        assert_eq!(sha256_hex_prefix("cli", 16), "99bb88401742848e");
        assert_eq!(
            sha256_hex_prefix("alice|C:\\USERS\\ALICE\\APPDATA\\ROAMING\\FILEE", 12).to_uppercase(),
            "A887F1A84BE2"
        );
    }

    #[test]
    fn sha256_multi_block() {
        let long = "a".repeat(1000);
        assert_eq!(sha256_hex_prefix(&long, 64), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    }

    #[test]
    fn json_is_written_like_upstream() {
        let v: Value = serde_json::from_str(r#"{"a":"一'x","b":[],"c":{"d":1}}"#).unwrap();
        let text = to_upstream_json(&v, true);
        assert_eq!(text, "{\r\n  \"a\": \"\\u4E00\\u0027x\",\r\n  \"b\": [],\r\n  \"c\": {\r\n    \"d\": 1\r\n  }\r\n}");
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, v);
        assert!(to_upstream_json(&v, false).contains("\n  \"b\""));
        assert!(!to_upstream_json(&v, false).contains('\r'));
    }

    #[test]
    fn astral_characters_become_surrogate_pairs() {
        let v = Value::String("😀".into());
        assert_eq!(to_upstream_json(&v, false), "\"\\uD83D\\uDE00\"");
    }

    #[test]
    fn time_formats() {
        // 2026-10-28T17:48:28.9016082Z as FILETIME: unix 1_793_209_708 s.
        let ft = FILETIME_UNIX_EPOCH + 1_793_209_708u64 * 10_000_000 + 9_016_082;
        assert_eq!(utc_iso(ft), "2026-10-28T17:48:28.9016082Z");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2026, 10, 29)), (2026, 10, 29));
        let local = local_iso_now();
        assert_eq!(local.len(), "2026-10-29T01:48:28.9071793+08:00".len());
    }

    #[test]
    fn inside_and_same_folder() {
        assert!(is_inside(Path::new(r"C:\In\Sub\a.png"), Path::new(r"c:\in\")));
        assert!(!is_inside(Path::new(r"C:\In"), Path::new(r"C:\In")));
        assert!(!is_inside(Path::new(r"C:\Input\a.png"), Path::new(r"C:\In")));
        assert!(same_folder(Path::new(r"C:\In\"), Path::new(r"c:\in")));
    }
}
