//! Server-side inspection of MANAGED media (§80): what the bytes of a READY MANAGED
//! attachment actually are — container, duration, picture dimensions — measured, never
//! taken from the client's declaration.
//!
//! * Untrusted input: every parser is a pure function over a byte slice with checked
//!   arithmetic, bounded nesting ([`MAX_DEPTH`]) and a bounded number of elements
//!   ([`MAX_ELEMENTS`]); no `unsafe`, no allocation proportional to declared sizes, no
//!   metadata interpreted as anything but numbers. Malformed input is an error code,
//!   never a panic (tests feed truncations and random bytes).
//! * Formats: ISO-BMFF (MP4 / MOV / M4A: `mvhd`, `tkhd`, `hdlr`), Matroska / WebM (EBML
//!   `Info` / `Tracks`), Ogg (Opus / Vorbis, last-page granule position), RIFF WAVE,
//!   MPEG audio Layer III (frame walk), AAC ADTS (frame walk), AMR-NB (frame walk),
//!   PNG / JPEG (header dimensions). Anything else is `UNSUPPORTED_FORMAT`.
//! * PRIVATE_E2EE attachments are ciphertext and never reach this module
//!   ([`super::processing::eligible`]).

/// Deepest box / element nesting followed.
pub const MAX_DEPTH: usize = 8;
/// Most boxes / elements / frames examined in one file.
pub const MAX_ELEMENTS: usize = 200_000;

/// What inspection measured.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaFacts {
    /// `MP4`, `MATROSKA`, `WEBM`, `OGG_OPUS`, `OGG_VORBIS`, `WAV`, `MP3`, `AAC_ADTS`,
    /// `AMR`, `PNG`, `JPEG`.
    pub container: &'static str,
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub has_video: bool,
    pub has_audio: bool,
}

type R<T> = std::result::Result<T, &'static str>;

fn be32(b: &[u8], at: usize) -> R<u32> {
    let s = b
        .get(at..at.checked_add(4).ok_or("MALFORMED")?)
        .ok_or("TRUNCATED")?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}
fn be64(b: &[u8], at: usize) -> R<u64> {
    let s = b
        .get(at..at.checked_add(8).ok_or("MALFORMED")?)
        .ok_or("TRUNCATED")?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_be_bytes(a))
}
fn le16(b: &[u8], at: usize) -> R<u16> {
    let s = b
        .get(at..at.checked_add(2).ok_or("MALFORMED")?)
        .ok_or("TRUNCATED")?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn le32(b: &[u8], at: usize) -> R<u32> {
    let s = b
        .get(at..at.checked_add(4).ok_or("MALFORMED")?)
        .ok_or("TRUNCATED")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn le64(b: &[u8], at: usize) -> R<u64> {
    let s = b
        .get(at..at.checked_add(8).ok_or("MALFORMED")?)
        .ok_or("TRUNCATED")?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_le_bytes(a))
}

/// `units / per_second` in milliseconds, without overflow.
fn ms(units: u64, per_second: u64) -> Option<u64> {
    if per_second == 0 {
        return None;
    }
    let v = (units as u128) * 1000 / (per_second as u128);
    u64::try_from(v).ok()
}

/// Inspects `bytes` declared as `media_type`. The parser is chosen by the declared type
/// (already checked against the bytes' signature at finalization), never guessed.
pub fn inspect(bytes: &[u8], media_type: &str) -> R<MediaFacts> {
    match media_type {
        "video/mp4" | "video/quicktime" | "audio/mp4" => iso_bmff(bytes),
        "video/webm" | "audio/webm" => matroska(bytes),
        "audio/ogg" => ogg(bytes),
        "audio/wav" => wav(bytes),
        "audio/mpeg" => mp3(bytes),
        "audio/aac" => adts(bytes),
        "audio/amr" => amr(bytes),
        "image/png" | "image/jpeg" => image_header(bytes, media_type),
        _ => Err("UNSUPPORTED_FORMAT"),
    }
}

// ---------------------------------------------------------------- ISO-BMFF (MP4 / MOV)

struct Bmff {
    timescale: u64,
    duration: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    has_video: bool,
    has_audio: bool,
    saw_moov: bool,
    elements: usize,
}

/// One box: (type, payload range) starting at `at` within `end`.
fn bmff_box(b: &[u8], at: usize, end: usize) -> R<([u8; 4], usize, usize)> {
    let size32 = be32(b, at)? as u64;
    let kind: [u8; 4] = b
        .get(at + 4..at + 8)
        .ok_or("TRUNCATED")?
        .try_into()
        .map_err(|_| "MALFORMED")?;
    let (header, size) = match size32 {
        0 => (8u64, (end - at) as u64),
        1 => (16u64, be64(b, at + 8)?),
        n => (8u64, n),
    };
    if size < header {
        return Err("MALFORMED");
    }
    let box_end = (at as u64).checked_add(size).ok_or("MALFORMED")?;
    if box_end > end as u64 {
        return Err("TRUNCATED");
    }
    Ok((kind, at + header as usize, box_end as usize))
}

fn bmff_walk(
    b: &[u8],
    start: usize,
    end: usize,
    depth: usize,
    s: &mut Bmff,
    trak: &mut (Option<(u32, u32)>, Option<[u8; 4]>),
) -> R<()> {
    if depth > MAX_DEPTH {
        return Err("TOO_DEEP");
    }
    let mut at = start;
    while at + 8 <= end {
        s.elements += 1;
        if s.elements > MAX_ELEMENTS {
            return Err("TOO_MANY_ELEMENTS");
        }
        let (kind, body, next) = bmff_box(b, at, end)?;
        // Header fields are read from this box only.
        let need = |n: usize| {
            if body + n <= next {
                Ok(())
            } else {
                Err("TRUNCATED")
            }
        };
        match &kind {
            b"moov" => {
                s.saw_moov = true;
                bmff_walk(b, body, next, depth + 1, s, trak)?;
            }
            b"trak" => {
                let mut t = (None, None);
                bmff_walk(b, body, next, depth + 1, s, &mut t)?;
                match t.1.as_ref() {
                    Some(b"vide") => {
                        s.has_video = true;
                        if let (Some((w, h)), None) = (t.0, s.width) {
                            if w > 0 && h > 0 {
                                s.width = Some(w);
                                s.height = Some(h);
                            }
                        }
                    }
                    Some(b"soun") => s.has_audio = true,
                    _ => {}
                }
            }
            b"mdia" | b"minf" | b"edts" => bmff_walk(b, body, next, depth + 1, s, trak)?,
            b"mvhd" => {
                need(32)?;
                let version = b[body];
                let (timescale, duration) = if version == 1 {
                    (be32(b, body + 20)? as u64, be64(b, body + 24)?)
                } else {
                    (be32(b, body + 12)? as u64, be32(b, body + 16)? as u64)
                };
                s.timescale = timescale;
                // All ones: duration unknown.
                s.duration =
                    (duration != u64::MAX && duration != u32::MAX as u64).then_some(duration);
            }
            b"tkhd" => {
                need(84)?;
                let off = if b[body] == 1 { 88 } else { 76 };
                need(off + 8)?;
                let w = be32(b, body + off)? >> 16;
                let h = be32(b, body + off + 4)? >> 16;
                trak.0 = Some((w, h));
            }
            b"hdlr" => {
                need(12)?;
                trak.1 = Some(
                    b.get(body + 8..body + 12)
                        .ok_or("TRUNCATED")?
                        .try_into()
                        .map_err(|_| "MALFORMED")?,
                );
            }
            _ => {}
        }
        if next <= at {
            return Err("MALFORMED");
        }
        at = next;
    }
    Ok(())
}

fn iso_bmff(b: &[u8]) -> R<MediaFacts> {
    let first: &[u8] = b.get(4..8).ok_or("TRUNCATED")?;
    if !matches!(
        first,
        b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip"
    ) {
        return Err("UNDECODABLE");
    }
    let mut s = Bmff {
        timescale: 0,
        duration: None,
        width: None,
        height: None,
        has_video: false,
        has_audio: false,
        saw_moov: false,
        elements: 0,
    };
    bmff_walk(b, 0, b.len(), 0, &mut s, &mut (None, None))?;
    if !s.saw_moov {
        return Err("NO_MOVIE_HEADER");
    }
    Ok(MediaFacts {
        container: "MP4",
        duration_ms: s.duration.and_then(|d| ms(d, s.timescale)),
        width: s.width,
        height: s.height,
        has_video: s.has_video,
        has_audio: s.has_audio,
    })
}

// ------------------------------------------------------------------ Matroska / WebM

/// An EBML variable-length integer at `at`: (value, length). IDs keep their marker bit.
fn vint(b: &[u8], at: usize, keep_marker: bool) -> R<(u64, usize)> {
    let first = *b.get(at).ok_or("TRUNCATED")?;
    if first == 0 {
        return Err("MALFORMED");
    }
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return Err("MALFORMED");
    }
    let mut v: u64 = if keep_marker {
        first as u64
    } else {
        (first as u64) & (0xFF >> len)
    };
    for i in 1..len {
        v = (v << 8) | *b.get(at + i).ok_or("TRUNCATED")? as u64;
    }
    Ok((v, len))
}

const EBML_UNKNOWN: u64 = u64::MAX;

fn ebml_element(b: &[u8], at: usize, end: usize) -> R<(u64, usize, usize)> {
    let (id, il) = vint(b, at, true)?;
    let (size, sl) = vint(b, at + il, false)?;
    let body = at + il + sl;
    if body > end {
        return Err("TRUNCATED");
    }
    let all_ones = size == (1u64 << (7 * sl)) - 1;
    let size = if all_ones { EBML_UNKNOWN } else { size };
    let next = if size == EBML_UNKNOWN {
        end
    } else {
        body.checked_add(usize::try_from(size).map_err(|_| "MALFORMED")?)
            .ok_or("MALFORMED")?
    };
    // A Segment cut short (an interrupted recording) still has its headers.
    let next = if next > end && id == 0x18538067 {
        end
    } else {
        next
    };
    if next > end {
        return Err("TRUNCATED");
    }
    Ok((id, body, next))
}

fn ebml_uint(b: &[u8], body: usize, next: usize) -> R<u64> {
    let s = b.get(body..next).ok_or("TRUNCATED")?;
    if s.len() > 8 {
        return Err("MALFORMED");
    }
    Ok(s.iter().fold(0u64, |v, x| (v << 8) | *x as u64))
}

fn ebml_float(b: &[u8], body: usize, next: usize) -> R<f64> {
    match next - body {
        4 => Ok(f32::from_bits(be32(b, body)?) as f64),
        8 => Ok(f64::from_bits(be64(b, body)?)),
        0 => Ok(0.0),
        _ => Err("MALFORMED"),
    }
}

struct Mkv {
    doc_type: Option<&'static str>,
    timecode_scale: u64,
    duration: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
    has_video: bool,
    has_audio: bool,
    saw_segment: bool,
    elements: usize,
}

fn mkv_walk(b: &[u8], start: usize, end: usize, depth: usize, s: &mut Mkv) -> R<()> {
    if depth > MAX_DEPTH {
        return Err("TOO_DEEP");
    }
    let mut at = start;
    while at < end {
        s.elements += 1;
        if s.elements > MAX_ELEMENTS {
            return Err("TOO_MANY_ELEMENTS");
        }
        let (id, body, next) = match ebml_element(b, at, end) {
            Ok(e) => e,
            // A truncated tail after the headers were read (live recordings) is fine.
            Err("TRUNCATED") if s.saw_segment && depth > 0 => return Ok(()),
            Err(e) => return Err(e),
        };
        match id {
            0x1A45DFA3 => mkv_walk(b, body, next, depth + 1, s)?, // EBML header
            0x4282 => {
                s.doc_type = match b.get(body..next) {
                    Some(b"webm") => Some("WEBM"),
                    Some(b"matroska") => Some("MATROSKA"),
                    _ => return Err("UNSUPPORTED_FORMAT"),
                };
            }
            0x18538067 => {
                s.saw_segment = true;
                mkv_walk(b, body, next, depth + 1, s)?;
            }
            0x1549A966 | 0x1654AE6B => mkv_walk(b, body, next, depth + 1, s)?, // Info, Tracks
            0x2AD7B1 => s.timecode_scale = ebml_uint(b, body, next)?,
            0x4489 => s.duration = Some(ebml_float(b, body, next)?),
            0xAE => mkv_walk(b, body, next, depth + 1, s)?, // TrackEntry
            0x83 => match ebml_uint(b, body, next)? {
                1 => s.has_video = true,
                2 => s.has_audio = true,
                _ => {}
            },
            0xE0 => mkv_walk(b, body, next, depth + 1, s)?, // Video
            0xB0 if s.width.is_none() => s.width = u32::try_from(ebml_uint(b, body, next)?).ok(),
            0xBA if s.height.is_none() => s.height = u32::try_from(ebml_uint(b, body, next)?).ok(),
            // Media data: the headers come before it; nothing in clusters is needed.
            0x1F43B675 => return Ok(()),
            _ => {}
        }
        if next <= at {
            return Err("MALFORMED");
        }
        at = next;
    }
    Ok(())
}

fn matroska(b: &[u8]) -> R<MediaFacts> {
    if b.get(0..4) != Some(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Err("UNDECODABLE");
    }
    let mut s = Mkv {
        doc_type: None,
        timecode_scale: 1_000_000,
        duration: None,
        width: None,
        height: None,
        has_video: false,
        has_audio: false,
        saw_segment: false,
        elements: 0,
    };
    mkv_walk(b, 0, b.len(), 0, &mut s)?;
    let container = s.doc_type.ok_or("UNDECODABLE")?;
    if !s.saw_segment {
        return Err("UNDECODABLE");
    }
    let duration_ms = s.duration.and_then(|d| {
        let v = d * s.timecode_scale as f64 / 1_000_000.0;
        (v.is_finite() && (0.0..1e15).contains(&v)).then_some(v.round() as u64)
    });
    Ok(MediaFacts {
        container,
        duration_ms,
        width: s.width.filter(|w| *w > 0),
        height: s.height.filter(|h| *h > 0),
        has_video: s.has_video,
        has_audio: s.has_audio,
    })
}

// ------------------------------------------------------------------------------ Ogg

struct OggPage<'a> {
    granule: u64,
    serial: u32,
    payload: &'a [u8],
    len: usize,
}

fn ogg_page(b: &[u8], at: usize) -> R<OggPage<'_>> {
    if b.get(at..at + 4) != Some(b"OggS") || *b.get(at + 4).ok_or("TRUNCATED")? != 0 {
        return Err("MALFORMED");
    }
    let granule = le64(b, at + 6)?;
    let serial = le32(b, at + 14)?;
    let segments = *b.get(at + 26).ok_or("TRUNCATED")? as usize;
    let table = b.get(at + 27..at + 27 + segments).ok_or("TRUNCATED")?;
    let body_len: usize = table.iter().map(|x| *x as usize).sum();
    let start = at + 27 + segments;
    let payload = b.get(start..start + body_len).ok_or("TRUNCATED")?;
    Ok(OggPage {
        granule,
        serial,
        payload,
        len: 27 + segments + body_len,
    })
}

fn ogg(b: &[u8]) -> R<MediaFacts> {
    let first = ogg_page(b, 0).map_err(|_| "UNDECODABLE")?;
    let (container, rate, pre_skip) = if first.payload.starts_with(b"OpusHead") {
        ("OGG_OPUS", 48_000u64, le16(first.payload, 10)? as u64)
    } else if first.payload.starts_with(b"\x01vorbis") {
        ("OGG_VORBIS", le32(first.payload, 12)? as u64, 0)
    } else {
        return Err("UNSUPPORTED_FORMAT");
    };
    // Walk the pages of the first logical stream; the last granule position is the
    // stream's length in samples.
    let mut at = 0usize;
    let mut last: Option<u64> = None;
    let mut pages = 0usize;
    while at < b.len() {
        pages += 1;
        if pages > MAX_ELEMENTS {
            return Err("TOO_MANY_ELEMENTS");
        }
        let p = match ogg_page(b, at) {
            Ok(p) => p,
            Err(_) => break, // truncated tail: what was read stands
        };
        if p.serial == first.serial && p.granule != u64::MAX {
            last = Some(p.granule);
        }
        at += p.len;
    }
    let samples = last.ok_or("NO_DURATION")?.saturating_sub(pre_skip);
    Ok(MediaFacts {
        container,
        duration_ms: ms(samples, rate),
        has_audio: true,
        ..Default::default()
    })
}

// ------------------------------------------------------------------------------ WAV

fn wav(b: &[u8]) -> R<MediaFacts> {
    if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err("UNDECODABLE");
    }
    let mut at = 12usize;
    let (mut byte_rate, mut data) = (None, None);
    let mut n = 0usize;
    while at + 8 <= b.len() {
        n += 1;
        if n > MAX_ELEMENTS {
            return Err("TOO_MANY_ELEMENTS");
        }
        let id = &b[at..at + 4];
        let size = le32(b, at + 4)? as usize;
        match id {
            b"fmt " => byte_rate = Some(le32(b, at + 16)? as u64),
            // The data chunk may be truncated by a recorder; the size field is what the
            // file claims, capped by what is really there.
            b"data" => data = Some(size.min(b.len().saturating_sub(at + 8)) as u64),
            _ => {}
        }
        at = at.checked_add(8 + size + (size & 1)).ok_or("MALFORMED")?;
    }
    let (rate, data) = (byte_rate.ok_or("NO_FORMAT")?, data.ok_or("NO_DATA")?);
    Ok(MediaFacts {
        container: "WAV",
        duration_ms: ms(data, rate),
        has_audio: true,
        ..Default::default()
    })
}

// ------------------------------------------------------------------- MPEG Layer III

const MP3_BITRATES_V1: [u32; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const MP3_BITRATES_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// (frame length, samples, sample rate) of a Layer III frame header at `at`.
fn mp3_frame(b: &[u8], at: usize) -> Option<(usize, u64, u64)> {
    let h = be32(b, at).ok()?;
    if h >> 21 != 0x7FF {
        return None;
    }
    let version = (h >> 19) & 3; // 0: 2.5, 2: 2, 3: 1
    let layer = (h >> 17) & 3; // 1: Layer III
    let bitrate_index = ((h >> 12) & 0xF) as usize;
    let rate_index = ((h >> 10) & 3) as usize;
    let padding = ((h >> 9) & 1) as usize;
    if version == 1 || layer != 1 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    let rates = match version {
        3 => [44_100, 48_000, 32_000],
        2 => [22_050, 24_000, 16_000],
        _ => [11_025, 12_000, 8_000],
    };
    let rate = rates[rate_index] as usize;
    let (kbps, coeff, samples) = if version == 3 {
        (MP3_BITRATES_V1[bitrate_index], 144usize, 1152u64)
    } else {
        (MP3_BITRATES_V2[bitrate_index], 72usize, 576u64)
    };
    let len = coeff * kbps as usize * 1000 / rate + padding;
    (len >= 4).then_some((len, samples, rate as u64))
}

fn mp3(b: &[u8]) -> R<MediaFacts> {
    let mut at = 0usize;
    if b.get(0..3) == Some(b"ID3") {
        let s = b.get(6..10).ok_or("TRUNCATED")?;
        if s.iter().any(|x| x & 0x80 != 0) {
            return Err("MALFORMED");
        }
        let size = s.iter().fold(0usize, |v, x| (v << 7) | *x as usize);
        at = 10 + size;
    }
    let (mut frames, mut samples, mut rate) = (0usize, 0u64, 0u64);
    while let Some((len, n, r)) = mp3_frame(b, at) {
        if at + len > b.len() {
            break;
        }
        if frames > 0 && r != rate {
            break;
        }
        frames += 1;
        if frames > MAX_ELEMENTS * 10 {
            return Err("TOO_MANY_ELEMENTS");
        }
        samples += n;
        rate = r;
        at += len;
    }
    // A few consecutive valid frames before calling it MPEG audio.
    if frames < 3 {
        return Err("UNDECODABLE");
    }
    Ok(MediaFacts {
        container: "MP3",
        duration_ms: ms(samples, rate),
        has_audio: true,
        ..Default::default()
    })
}

// ------------------------------------------------------------------------- AAC ADTS

const ADTS_RATES: [u64; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

fn adts(b: &[u8]) -> R<MediaFacts> {
    let (mut at, mut frames, mut samples, mut rate) = (0usize, 0usize, 0u64, 0u64);
    while at + 7 <= b.len() {
        let h = &b[at..at + 7];
        if h[0] != 0xFF || h[1] & 0xF6 != 0xF0 {
            break;
        }
        let rate_index = ((h[2] >> 2) & 0xF) as usize;
        let r = *ADTS_RATES.get(rate_index).ok_or("MALFORMED")?;
        let len = (((h[3] & 0x3) as usize) << 11) | ((h[4] as usize) << 3) | ((h[5] >> 5) as usize);
        let blocks = (h[6] & 0x3) as u64 + 1;
        if len < 7 || at + len > b.len() || (frames > 0 && r != rate) {
            break;
        }
        frames += 1;
        if frames > MAX_ELEMENTS * 10 {
            return Err("TOO_MANY_ELEMENTS");
        }
        samples += blocks * 1024;
        rate = r;
        at += len;
    }
    if frames < 3 {
        return Err("UNDECODABLE");
    }
    Ok(MediaFacts {
        container: "AAC_ADTS",
        duration_ms: ms(samples, rate),
        has_audio: true,
        ..Default::default()
    })
}

// ------------------------------------------------------------------------------ AMR

/// AMR-NB speech frame sizes (bytes after the 1-byte header) by frame type.
const AMR_NB_SIZES: [usize; 16] = [12, 13, 15, 17, 19, 20, 26, 31, 5, 6, 5, 5, 0, 0, 0, 0];

fn amr(b: &[u8]) -> R<MediaFacts> {
    if !b.starts_with(b"#!AMR\n") {
        return Err("UNSUPPORTED_FORMAT"); // AMR-WB ("#!AMR-WB\n") and multichannel: not inspected
    }
    let (mut at, mut frames) = (6usize, 0u64);
    while at < b.len() {
        let ft = ((b[at] >> 3) & 0x0F) as usize;
        let next = at + 1 + AMR_NB_SIZES[ft];
        if next > b.len() {
            break;
        }
        frames += 1;
        if frames > (MAX_ELEMENTS * 10) as u64 {
            return Err("TOO_MANY_ELEMENTS");
        }
        at = next;
    }
    if frames == 0 {
        return Err("UNDECODABLE");
    }
    Ok(MediaFacts {
        container: "AMR",
        duration_ms: Some(frames * 20),
        has_audio: true,
        ..Default::default()
    })
}

// --------------------------------------------------------------------------- Images

fn image_header(b: &[u8], media_type: &str) -> R<MediaFacts> {
    let (container, format) = match media_type {
        "image/png" => ("PNG", image::ImageFormat::Png),
        _ => ("JPEG", image::ImageFormat::Jpeg),
    };
    let mut r = image::ImageReader::with_format(std::io::Cursor::new(b), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(65_535);
    limits.max_image_height = Some(65_535);
    limits.max_alloc = Some(64 * 1024 * 1024);
    r.limits(limits);
    let (w, h) = r.into_dimensions().map_err(|_| "UNDECODABLE")?;
    Ok(MediaFacts {
        container,
        width: Some(w),
        height: Some(h),
        ..Default::default()
    })
}

/// Whether a client-declared duration agrees with the measured one (±max(1 s, 5 %)).
pub fn duration_matches(declared_ms: i64, measured_ms: u64) -> bool {
    let declared = declared_ms.max(0) as u64;
    let tolerance = (measured_ms / 20).max(1_000);
    declared.abs_diff(measured_ms) <= tolerance
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn tkhd(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0u8; 84];
        b[76..80].copy_from_slice(&(w << 16).to_be_bytes());
        b[80..84].copy_from_slice(&(h << 16).to_be_bytes());
        bx(b"tkhd", &b)
    }

    fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
        let mut b = vec![0u8; 24];
        b[8..12].copy_from_slice(kind);
        bx(b"hdlr", &b)
    }

    pub fn mp4(timescale: u32, duration: u32) -> Vec<u8> {
        let mut mvhd = vec![0u8; 100];
        mvhd[12..16].copy_from_slice(&timescale.to_be_bytes());
        mvhd[16..20].copy_from_slice(&duration.to_be_bytes());
        let video = bx(
            b"trak",
            &[tkhd(640, 360), bx(b"mdia", &hdlr(b"vide"))].concat(),
        );
        let audio = bx(b"trak", &[tkhd(0, 0), bx(b"mdia", &hdlr(b"soun"))].concat());
        let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), video, audio].concat());
        [
            bx(b"ftyp", b"isom\0\0\x02\0isomiso2mp41"),
            moov,
            bx(b"mdat", &[7u8; 64]),
        ]
        .concat()
    }

    fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        // 8-byte size vint.
        let mut size = (body.len() as u64).to_be_bytes();
        size[0] = 0x01;
        v.extend_from_slice(&size);
        v.extend_from_slice(body);
        v
    }

    pub fn webm(duration_ms: f64) -> Vec<u8> {
        let header = el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"webm"));
        let info = el(
            &[0x15, 0x49, 0xA9, 0x66],
            &[
                el(&[0x2A, 0xD7, 0xB1], &1_000_000u32.to_be_bytes()),
                el(&[0x44, 0x89], &duration_ms.to_be_bytes()),
            ]
            .concat(),
        );
        let video = el(
            &[0xE0],
            &[el(&[0xB0], &[0x01, 0x40]), el(&[0xBA], &[0xF0])].concat(),
        );
        let tracks = el(
            &[0x16, 0x54, 0xAE, 0x6B],
            &el(&[0xAE], &[el(&[0x83], &[1]), video].concat()),
        );
        let cluster = el(&[0x1F, 0x43, 0xB6, 0x75], &[0u8; 32]);
        [
            header,
            el(&[0x18, 0x53, 0x80, 0x67], &[info, tracks, cluster].concat()),
        ]
        .concat()
    }

    fn page(granule: u64, serial: u32, seq: u32, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 255);
        let mut v = b"OggS\0\0".to_vec();
        v.extend_from_slice(&granule.to_le_bytes());
        v.extend_from_slice(&serial.to_le_bytes());
        v.extend_from_slice(&seq.to_le_bytes());
        v.extend_from_slice(&[0, 0, 0, 0, 1, payload.len() as u8]);
        v.extend_from_slice(payload);
        v
    }

    pub fn opus(seconds: u64) -> Vec<u8> {
        let mut head = b"OpusHead\x01\x01".to_vec();
        head.extend_from_slice(&312u16.to_le_bytes());
        head.extend_from_slice(&48_000u32.to_le_bytes());
        head.extend_from_slice(&[0, 0, 0]);
        [
            page(0, 7, 0, &head),
            page(0, 7, 1, b"OpusTags\0\0\0\0\0\0\0\0"),
            page(312 + 48_000 * seconds / 2, 7, 2, &[1u8; 100]),
            page(312 + 48_000 * seconds, 7, 3, &[2u8; 100]),
        ]
        .concat()
    }

    pub fn wav(ms_len: u32) -> Vec<u8> {
        let data = vec![0u8; (32 * ms_len) as usize]; // 16 kHz, mono, 16-bit = 32000 B/s
        let mut fmt = vec![1, 0, 1, 0];
        fmt.extend_from_slice(&16_000u32.to_le_bytes());
        fmt.extend_from_slice(&32_000u32.to_le_bytes());
        fmt.extend_from_slice(&[2, 0, 16, 0]);
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&((4 + 8 + fmt.len() + 8 + data.len()) as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        v.extend_from_slice(&fmt);
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&data);
        v
    }

    fn mp3(frames: usize) -> Vec<u8> {
        // MPEG-1 Layer III, 128 kbit/s, 44.1 kHz, no padding: 417-byte frames.
        let mut frame = vec![0u8; 417];
        frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        let mut v = b"ID3\x04\0\0\0\0\0\x0a".to_vec();
        v.extend_from_slice(&[0u8; 10]);
        for _ in 0..frames {
            v.extend_from_slice(&frame);
        }
        v
    }

    fn adts(frames: usize) -> Vec<u8> {
        let mut frame = [0u8; 100];
        frame[..7].copy_from_slice(&[0xFF, 0xF1, 0x50, 0x80, 0x0C, 0x9F, 0xFC]);
        frame.repeat(frames)
    }

    fn amr(frames: usize) -> Vec<u8> {
        let mut v = b"#!AMR\n".to_vec();
        for _ in 0..frames {
            v.push(0x3C);
            v.extend_from_slice(&[0u8; 31]);
        }
        v
    }

    fn samples() -> Vec<(&'static str, Vec<u8>, MediaFacts)> {
        vec![
            (
                "video/mp4",
                mp4(1000, 12_345),
                MediaFacts {
                    container: "MP4",
                    duration_ms: Some(12_345),
                    width: Some(640),
                    height: Some(360),
                    has_video: true,
                    has_audio: true,
                },
            ),
            (
                "video/webm",
                webm(7_500.0),
                MediaFacts {
                    container: "WEBM",
                    duration_ms: Some(7_500),
                    width: Some(320),
                    height: Some(240),
                    has_video: true,
                    has_audio: false,
                },
            ),
            (
                "audio/ogg",
                opus(3),
                MediaFacts {
                    container: "OGG_OPUS",
                    duration_ms: Some(3_000),
                    has_audio: true,
                    ..Default::default()
                },
            ),
            (
                "audio/wav",
                wav(2_000),
                MediaFacts {
                    container: "WAV",
                    duration_ms: Some(2_000),
                    has_audio: true,
                    ..Default::default()
                },
            ),
            (
                "audio/mpeg",
                mp3(100),
                MediaFacts {
                    container: "MP3",
                    duration_ms: Some(2_612),
                    has_audio: true,
                    ..Default::default()
                },
            ),
            (
                "audio/aac",
                adts(100),
                MediaFacts {
                    container: "AAC_ADTS",
                    duration_ms: Some(2_321),
                    has_audio: true,
                    ..Default::default()
                },
            ),
            (
                "audio/amr",
                amr(150),
                MediaFacts {
                    container: "AMR",
                    duration_ms: Some(3_000),
                    has_audio: true,
                    ..Default::default()
                },
            ),
        ]
    }

    #[test]
    fn measures_each_supported_format() {
        for (media_type, bytes, expected) in samples() {
            assert_eq!(inspect(&bytes, media_type), Ok(expected), "{media_type}");
        }
    }

    #[test]
    fn truncated_input_never_panics() {
        for (media_type, bytes, _) in samples() {
            for n in 0..bytes.len() {
                let _ = inspect(&bytes[..n], media_type);
            }
        }
    }

    #[test]
    fn random_and_mutated_input_never_panics() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x5eed);
        let types = [
            "video/mp4",
            "video/webm",
            "audio/ogg",
            "audio/wav",
            "audio/mpeg",
            "audio/aac",
            "audio/amr",
            "image/png",
            "image/jpeg",
        ];
        for _ in 0..3_000 {
            let len = rng.gen_range(0..4_096);
            let bytes: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
            for t in types {
                let _ = inspect(&bytes, t);
            }
        }
        // Valid files with random byte flips (sizes, counts, nesting).
        for (media_type, bytes, _) in samples() {
            for _ in 0..2_000 {
                let mut b = bytes.clone();
                for _ in 0..rng.gen_range(1..8) {
                    let i = rng.gen_range(0..b.len());
                    b[i] = rng.gen();
                }
                let _ = inspect(&b, media_type);
            }
        }
    }

    #[test]
    fn nesting_and_element_bombs_are_bounded() {
        // 10 000 nested empty moov boxes: refused, not a stack overflow.
        let mut b = Vec::new();
        for _ in 0..10_000 {
            b = bx(b"moov", &b);
        }
        let mut file = bx(b"ftyp", b"isom");
        file.extend_from_slice(&b);
        assert_eq!(inspect(&file, "video/mp4"), Err("TOO_DEEP"));
        // A box claiming more bytes than exist.
        let mut lie = mp4(1000, 1000);
        let at = lie.len() - 72;
        lie[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(inspect(&lie, "video/mp4").is_err());
    }

    #[test]
    fn wrong_or_unsupported_formats_are_refused() {
        assert_eq!(inspect(&wav(100), "video/mp4"), Err("UNDECODABLE"));
        assert_eq!(inspect(&mp4(1, 1), "audio/ogg"), Err("UNDECODABLE"));
        assert_eq!(
            inspect(b"#!AMR-WB\n\0\0", "audio/amr"),
            Err("UNSUPPORTED_FORMAT")
        );
        assert_eq!(inspect(b"GIF89a", "image/gif"), Err("UNSUPPORTED_FORMAT"));
        assert_eq!(inspect(&[0u8; 64], "audio/mpeg"), Err("UNDECODABLE"));
    }

    #[test]
    fn declared_durations_are_compared_with_tolerance() {
        assert!(duration_matches(12_000, 12_345));
        assert!(duration_matches(100_000, 104_000)); // within 5 %
        assert!(!duration_matches(60_000, 12_345));
        assert!(!duration_matches(-5, 3_000));
    }
}
