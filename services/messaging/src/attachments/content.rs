//! Content-type security for MANAGED attachments (§73, §79).
//!
//! Only where the security profile lets the server see the bytes (MANAGED). For
//! PRIVATE_E2EE the uploaded object is ciphertext: nothing here applies, and nothing is
//! inferred from it (no plaintext format, magic bytes or file name exists server-side);
//! plaintext-format validation belongs to the authorized clients' security contract.
//!
//! * The declared media type must belong to the attachment kind's allow-list.
//! * The bytes' signature ("magic") must agree with the declared type (MIME confusion).
//! * Executables, scripts and active web content (HTML / SVG / XML with scripts) are
//!   refused whatever is declared.
//! * File names are sanitized for presentation only (never a storage key): last path
//!   segment, no control / quote / separator characters, bounded; prohibited extensions
//!   and executable "double extensions" (`invoice.exe.pdf`, `photo.jpg.exe`) refused.

/// Media types allowed per attachment kind.
pub fn allowed_media_types(kind: &str) -> &'static [&'static str] {
    match kind {
        "IMAGE" => &[
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "image/heic",
        ],
        "DOCUMENT" => &[
            "application/pdf",
            "text/plain",
            "text/csv",
            "application/rtf",
            "application/msword",
            "application/vnd.ms-excel",
            "application/vnd.ms-powerpoint",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "application/vnd.oasis.opendocument.text",
            "application/vnd.oasis.opendocument.spreadsheet",
        ],
        "VOICE_NOTE" => &[
            "audio/ogg",
            "audio/mpeg",
            "audio/mp4",
            "audio/aac",
            "audio/webm",
            "audio/wav",
            "audio/amr",
        ],
        "VIDEO" => &["video/mp4", "video/webm", "video/quicktime"],
        "FILE" => &[
            "application/octet-stream",
            "application/zip",
            "application/gzip",
            "application/x-7z-compressed",
            "application/json",
            "text/plain",
            "text/csv",
            "application/pdf",
            "image/png",
            "image/jpeg",
            "audio/mpeg",
            "video/mp4",
        ],
        _ => &[],
    }
}

/// Extensions never accepted (executables, scripts, installers, shortcuts).
pub const PROHIBITED_EXTENSIONS: &[&str] = &[
    "exe", "dll", "com", "bat", "cmd", "msi", "msp", "scr", "pif", "cpl", "ps1", "psm1", "vbs",
    "vbe", "js", "jse", "wsf", "wsh", "hta", "jar", "sh", "bash", "zsh", "csh", "app", "apk",
    "ipa", "dmg", "pkg", "deb", "rpm", "lnk", "reg", "inf", "scf", "url", "html", "htm", "xhtml",
    "svg", "xml", "php", "py", "pl", "rb",
];

/// What the bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniffed {
    Png,
    Jpeg,
    Gif,
    Webp,
    Heic,
    Pdf,
    Zip,
    Ole,
    Rtf,
    Gzip,
    SevenZip,
    Ogg,
    Mp3,
    Iso, // ISO base media (mp4 / mov / m4a)
    Matroska,
    Wav,
    Amr,
    Aac,
    Executable,
    ActiveWeb,
    Text,
    Unknown,
}

pub fn sniff(b: &[u8]) -> Sniffed {
    let starts = |p: &[u8]| b.starts_with(p);
    if starts(b"MZ")
        || starts(b"\x7fELF")
        || starts(&[0xfe, 0xed, 0xfa, 0xce])
        || starts(&[0xfe, 0xed, 0xfa, 0xcf])
        || starts(&[0xcf, 0xfa, 0xed, 0xfe])
        || starts(&[0xce, 0xfa, 0xed, 0xfe])
        || starts(&[0xca, 0xfe, 0xba, 0xbe])
        || starts(b"#!")
        || starts(b"\0asm")
    {
        return Sniffed::Executable;
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Sniffed::Png;
    }
    if starts(&[0xff, 0xd8, 0xff]) {
        return Sniffed::Jpeg;
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Sniffed::Gif;
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Sniffed::Webp;
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        return Sniffed::Wav;
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        let brand = &b[8..12];
        return if brand == b"heic" || brand == b"heix" || brand == b"mif1" {
            Sniffed::Heic
        } else {
            Sniffed::Iso
        };
    }
    if starts(b"%PDF-") {
        return Sniffed::Pdf;
    }
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        return Sniffed::Zip;
    }
    if starts(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
        return Sniffed::Ole;
    }
    if starts(b"{\\rtf") {
        return Sniffed::Rtf;
    }
    if starts(&[0x1f, 0x8b]) {
        return Sniffed::Gzip;
    }
    if starts(b"7z\xbc\xaf\x27\x1c") {
        return Sniffed::SevenZip;
    }
    if starts(b"OggS") {
        return Sniffed::Ogg;
    }
    if starts(b"ID3")
        || (b.len() >= 2 && b[0] == 0xff && (b[1] & 0xe0) == 0xe0 && (b[1] & 0x06) != 0)
    {
        return Sniffed::Mp3;
    }
    if b.len() >= 2 && b[0] == 0xff && (b[1] & 0xf6) == 0xf0 {
        return Sniffed::Aac;
    }
    if starts(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Sniffed::Matroska;
    }
    if starts(b"#!AMR") {
        return Sniffed::Amr;
    }
    // Text: valid UTF-8 without NUL. Active web content is refused whatever is declared.
    let head = &b[..b.len().min(4096)];
    if let Ok(t) =
        std::str::from_utf8(head).or_else(|e| std::str::from_utf8(&head[..e.valid_up_to()]))
    {
        if !t.contains('\0') && !t.is_empty() {
            let lower = t
                .trim_start_matches('\u{feff}')
                .trim_start()
                .to_ascii_lowercase();
            if [
                "<!doctype html",
                "<html",
                "<svg",
                "<?xml",
                "<script",
                "<iframe",
                "<object",
                "<embed",
            ]
            .iter()
            .any(|p| lower.starts_with(p))
                || lower.contains("<script")
            {
                return Sniffed::ActiveWeb;
            }
            return Sniffed::Text;
        }
    }
    Sniffed::Unknown
}

fn agrees(media_type: &str, s: Sniffed) -> bool {
    use Sniffed::*;
    match media_type {
        "image/png" => s == Png,
        "image/jpeg" => s == Jpeg,
        "image/gif" => s == Gif,
        "image/webp" => s == Webp,
        "image/heic" => s == Heic,
        "application/pdf" => s == Pdf,
        "text/plain" | "text/csv" | "application/json" => s == Text,
        "application/rtf" => s == Rtf,
        "application/msword" | "application/vnd.ms-excel" | "application/vnd.ms-powerpoint" => {
            s == Ole
        }
        t if t.starts_with("application/vnd.openxmlformats")
            || t.starts_with("application/vnd.oasis") =>
        {
            s == Zip
        }
        "application/zip" => s == Zip,
        "application/gzip" => s == Gzip,
        "application/x-7z-compressed" => s == SevenZip,
        "audio/ogg" => s == Ogg,
        "audio/mpeg" => s == Mp3,
        "audio/mp4" | "video/mp4" | "video/quicktime" => s == Iso,
        "audio/aac" => s == Aac,
        "audio/webm" | "video/webm" => s == Matroska,
        "audio/wav" => s == Wav,
        "audio/amr" => s == Amr,
        "application/octet-stream" => !matches!(s, Executable | ActiveWeb),
        _ => false,
    }
}

/// Sanitizes a presentation file name (never used as a storage key).
pub fn sanitize_file_name(name: &str) -> Option<String> {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if matches!(c, '"' | '\'' | ';' | '<' | '>' | ':' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim().to_string();
    if cleaned.is_empty() {
        return None;
    }
    let mut out = String::new();
    for c in cleaned.chars() {
        if out.len() + c.len_utf8() > 255 {
            break;
        }
        out.push(c);
    }
    let reserved = ["con", "prn", "aux", "nul", "com1", "lpt1"];
    let stem = out.split('.').next().unwrap_or("").to_ascii_lowercase();
    if reserved.contains(&stem.as_str()) {
        return None;
    }
    Some(out)
}

/// Every extension of a name, lower-cased, outermost last.
fn extensions(name: &str) -> Vec<String> {
    name.split('.')
        .skip(1)
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}

/// Validates a MANAGED attachment. `Err(reason)` with a stable code.
pub fn check_managed(
    kind: &str,
    media_type: &str,
    file_name: Option<&str>,
    bytes: &[u8],
) -> Result<(), &'static str> {
    if !allowed_media_types(kind).contains(&media_type) {
        return Err("MEDIA_TYPE_NOT_ALLOWED");
    }
    if let Some(name) = file_name {
        let exts = extensions(name);
        if exts
            .iter()
            .any(|e| PROHIBITED_EXTENSIONS.contains(&e.as_str()))
        {
            // Outermost or inner (`invoice.exe.pdf`): both are tricks.
            return Err("PROHIBITED_FILE_NAME");
        }
    }
    let sniffed = sniff(bytes);
    match sniffed {
        Sniffed::Executable => return Err("EXECUTABLE_CONTENT"),
        Sniffed::ActiveWeb => return Err("ACTIVE_CONTENT"),
        _ => {}
    }
    if !agrees(media_type, sniffed) {
        return Err("CONTENT_TYPE_MISMATCH");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn declared_types_must_match_the_bytes_and_active_content_is_refused() {
        assert_eq!(
            check_managed("IMAGE", "image/png", Some("cat.png"), PNG),
            Ok(())
        );
        assert_eq!(
            check_managed("IMAGE", "image/jpeg", Some("cat.jpg"), PNG),
            Err("CONTENT_TYPE_MISMATCH")
        );
        assert_eq!(
            check_managed("IMAGE", "text/html", Some("x.html"), b"<html>"),
            Err("MEDIA_TYPE_NOT_ALLOWED")
        );
        assert_eq!(
            check_managed(
                "FILE",
                "application/octet-stream",
                Some("tool.bin"),
                b"MZ\x90\0"
            ),
            Err("EXECUTABLE_CONTENT")
        );
        assert_eq!(
            check_managed(
                "FILE",
                "application/octet-stream",
                Some("x.bin"),
                b"\x7fELF\x02"
            ),
            Err("EXECUTABLE_CONTENT")
        );
        assert_eq!(
            check_managed(
                "DOCUMENT",
                "text/plain",
                Some("notes.txt"),
                b"#!/bin/sh\nrm -rf /"
            ),
            Err("EXECUTABLE_CONTENT")
        );
        assert_eq!(
            check_managed(
                "DOCUMENT",
                "text/plain",
                Some("a.txt"),
                b"<!DOCTYPE html><script>x</script>"
            ),
            Err("ACTIVE_CONTENT")
        );
        assert_eq!(
            check_managed(
                "DOCUMENT",
                "text/plain",
                Some("a.txt"),
                b"hello\n<script>alert(1)</script>"
            ),
            Err("ACTIVE_CONTENT")
        );
        assert_eq!(
            check_managed("IMAGE", "image/png", Some("x.svg"), PNG),
            Err("PROHIBITED_FILE_NAME")
        );
        assert_eq!(
            check_managed(
                "DOCUMENT",
                "application/pdf",
                Some("invoice.exe.pdf"),
                b"%PDF-1.7"
            ),
            Err("PROHIBITED_FILE_NAME")
        );
        assert_eq!(
            check_managed(
                "DOCUMENT",
                "application/pdf",
                Some("invoice.pdf"),
                b"%PDF-1.7"
            ),
            Ok(())
        );
        assert_eq!(
            check_managed("VOICE_NOTE", "audio/ogg", None, b"OggS\0\x02"),
            Ok(())
        );
        assert_eq!(
            check_managed("VIDEO", "video/mp4", None, b"\0\0\0\x18ftypmp42\0\0"),
            Ok(())
        );
        assert_eq!(
            check_managed("VOICE_NOTE", "video/mp4", None, b"\0\0\0\x18ftypmp42"),
            Err("MEDIA_TYPE_NOT_ALLOWED")
        );
    }

    #[test]
    fn file_names_are_sanitized_for_presentation_only() {
        assert_eq!(
            sanitize_file_name("../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(
            sanitize_file_name("C:\\\\Users\\\\x\\\\report.pdf").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(
            sanitize_file_name("a\"; filename=evil.exe").as_deref(),
            Some("a__ filename=evil.exe")
        );
        assert_eq!(sanitize_file_name("..."), None);
        assert_eq!(sanitize_file_name("con.txt"), None);
        assert_eq!(
            sanitize_file_name("bell\u{7}.txt").as_deref(),
            Some("bell.txt")
        );
        assert!(sanitize_file_name(&"é".repeat(300)).unwrap().len() <= 255);
    }
}
