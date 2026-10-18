//! Message encoding and segmentation (3GPP TS 23.038 / 23.040).
//!
//! * GSM-7: every character in the default alphabet or its extension table (an extension
//!   character costs two septets: ESC + char). 160 septets in one segment; a concatenated
//!   message carries 153 septets per segment (UDH), and an extension pair never splits
//!   across segments.
//! * UCS-2: anything else. Counted in UTF-16 code units (a character outside the BMP is a
//!   surrogate pair, never split): 70 per single segment, 67 per concatenated segment.
//!
//! At most [`MAX_SEGMENTS`] segments; longer content is refused (CONTENT_TOO_LONG), never
//! truncated.

/// The upper bound of one message.
pub const MAX_SEGMENTS: usize = 10;

const GSM7_BASIC: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞ\u{1b}ÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";
const GSM7_EXTENSION: &str = "\u{c}^{}\\[~]|€";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Gsm7,
    Ucs2,
}

impl Encoding {
    pub fn as_str(&self) -> &'static str {
        match self {
            Encoding::Gsm7 => "GSM7",
            Encoding::Ucs2 => "UCS2",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "GSM7" => Some(Encoding::Gsm7),
            "UCS2" => Some(Encoding::Ucs2),
            _ => None,
        }
    }
}

/// The analysis of one message body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segmentation {
    pub encoding: Encoding,
    /// Characters (Unicode scalar values) of the body.
    pub characters: usize,
    /// Encoding units: GSM-7 septets or UTF-16 code units.
    pub units: usize,
    pub segments: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SegmentError {
    #[error("the message is empty")]
    Empty,
    #[error("the message needs more than {MAX_SEGMENTS} segments")]
    TooLong,
    #[error("the message contains a control character")]
    ControlCharacter,
}

fn gsm7_cost(c: char) -> Option<usize> {
    if GSM7_BASIC.contains(c) && c != '\u{1b}' {
        Some(1)
    } else if GSM7_EXTENSION.contains(c) {
        Some(2)
    } else {
        None
    }
}

/// Segments `units` of `per` units each, never splitting an indivisible pair.
fn concatenated(costs: &[usize], per: usize) -> usize {
    let mut segments = 1;
    let mut used = 0;
    for &c in costs {
        if used + c > per {
            segments += 1;
            used = 0;
        }
        used += c;
    }
    segments
}

/// Encoding and segment count of `text`.
pub fn analyze(text: &str) -> Result<Segmentation, SegmentError> {
    if text.is_empty() {
        return Err(SegmentError::Empty);
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r'))
    {
        return Err(SegmentError::ControlCharacter);
    }
    let characters = text.chars().count();
    let gsm: Option<Vec<usize>> = text.chars().map(gsm7_cost).collect();
    let (encoding, costs, single, per) = match gsm {
        Some(costs) => (Encoding::Gsm7, costs, 160, 153),
        None => (
            Encoding::Ucs2,
            text.chars().map(char::len_utf16).collect(),
            70,
            67,
        ),
    };
    let units: usize = costs.iter().sum();
    let segments = if units <= single {
        1
    } else {
        concatenated(&costs, per)
    };
    if segments > MAX_SEGMENTS {
        return Err(SegmentError::TooLong);
    }
    Ok(Segmentation {
        encoding,
        characters,
        units,
        segments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gsm7_boundaries() {
        let s = analyze(&"a".repeat(160)).unwrap();
        assert_eq!((s.encoding, s.units, s.segments), (Encoding::Gsm7, 160, 1));
        assert_eq!(analyze(&"a".repeat(161)).unwrap().segments, 2);
        assert_eq!(analyze(&"a".repeat(306)).unwrap().segments, 2);
        assert_eq!(analyze(&"a".repeat(307)).unwrap().segments, 3);
        // Extension characters cost two septets…
        let s = analyze(&"€".repeat(80)).unwrap();
        assert_eq!((s.encoding, s.units, s.segments), (Encoding::Gsm7, 160, 1));
        assert_eq!(analyze(&"€".repeat(81)).unwrap().segments, 2);
        // …and never split across segments: 152 + a pair overflows the first segment.
        let text = format!("{}€{}", "a".repeat(152), "b".repeat(10));
        assert_eq!(analyze(&text).unwrap().segments, 2);
        assert_eq!(
            analyze("Hello, Åsa! @home £5").unwrap().encoding,
            Encoding::Gsm7
        );
    }

    #[test]
    fn ucs2_boundaries_and_surrogates() {
        let s = analyze(&"ж".repeat(70)).unwrap();
        assert_eq!((s.encoding, s.segments), (Encoding::Ucs2, 1));
        assert_eq!(analyze(&"ж".repeat(71)).unwrap().segments, 2);
        assert_eq!(analyze(&"ж".repeat(134)).unwrap().segments, 2);
        assert_eq!(analyze(&"ж".repeat(135)).unwrap().segments, 3);
        // One emoji is two UTF-16 units and forces UCS-2 for the whole message.
        let s = analyze(&format!("{}😀", "a".repeat(68))).unwrap();
        assert_eq!(
            (s.encoding, s.units, s.characters, s.segments),
            (Encoding::Ucs2, 70, 69, 1)
        );
        let s = analyze(&"😀".repeat(36)).unwrap();
        assert_eq!(
            (s.units, s.segments),
            (72, 2),
            "a pair never splits: 33 + 3"
        );
    }

    #[test]
    fn bounds_are_enforced_not_truncated() {
        assert_eq!(analyze(""), Err(SegmentError::Empty));
        assert_eq!(analyze(&"a".repeat(1530)).unwrap().segments, 10);
        assert_eq!(analyze(&"a".repeat(1531)), Err(SegmentError::TooLong));
        assert_eq!(analyze(&"ж".repeat(671)), Err(SegmentError::TooLong));
        assert_eq!(analyze("bell\u{7}"), Err(SegmentError::ControlCharacter));
        assert_eq!(analyze("line\nbreak").unwrap().encoding, Encoding::Gsm7);
    }
}
