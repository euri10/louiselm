//! Byte-level scanners shared by Inspection.
//!
//! Everything here is deterministic and model-free: given the same bytes and
//! the same policy it returns the same occurrences, in the same order, forever.
//! Nothing in this module decides whether a skill is safe; it decides what a
//! reviewer is shown.

use crate::policy::Policy;

/// The longest evidence snippet shown for one occurrence, in characters.
pub const MAX_EVIDENCE_CHARS: usize = 160;

/// A file format recognized from its leading bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Magic {
    /// A Linux executable or shared object.
    Elf,
    /// A PNG image.
    Png,
    /// A JPEG image.
    Jpeg,
    /// A GIF image.
    Gif,
    /// A WebP image.
    Webp,
    /// A Windows or OS/2 bitmap.
    Bmp,
    /// A ZIP archive, which also covers jar, wheel, and docx-style containers.
    Zip,
    /// A gzip stream.
    Gzip,
    /// A WebAssembly module.
    Wasm,
    /// A PDF document.
    Pdf,
}

impl Magic {
    /// Returns the short name used in findings.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Elf => "elf",
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Zip => "zip",
            Self::Gzip => "gzip",
            Self::Wasm => "wasm",
            Self::Pdf => "pdf",
        }
    }

    /// Reports whether the format is an image format.
    #[must_use]
    pub fn is_image(self) -> bool {
        matches!(
            self,
            Self::Png | Self::Jpeg | Self::Gif | Self::Webp | Self::Bmp
        )
    }
}

/// Identifies `bytes` by its leading bytes, when the format is known.
#[must_use]
pub fn magic_of(bytes: &[u8]) -> Option<Magic> {
    const SIGNATURES: [(&[u8], Magic); 9] = [
        (b"\x7fELF", Magic::Elf),
        (b"\x89PNG\r\n\x1a\n", Magic::Png),
        (b"\xff\xd8\xff", Magic::Jpeg),
        (b"GIF8", Magic::Gif),
        (b"BM", Magic::Bmp),
        (b"PK\x03\x04", Magic::Zip),
        (b"\x1f\x8b", Magic::Gzip),
        (b"\x00asm", Magic::Wasm),
        (b"%PDF-", Magic::Pdf),
    ];
    for (signature, magic) in SIGNATURES {
        if bytes.starts_with(signature) {
            return Some(magic);
        }
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(Magic::Webp);
    }
    None
}

/// Reports whether `bytes` look like SVG markup.
#[must_use]
pub fn looks_like_svg(bytes: &[u8]) -> bool {
    let prefix = &bytes[..bytes.len().min(1024)];
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let lowered = text.trim_start().to_ascii_lowercase();
    lowered.starts_with("<svg") || (lowered.starts_with("<?xml") && lowered.contains("<svg"))
}

/// Reports whether `bytes` contain a NUL in the region a reader would sample.
#[must_use]
pub fn contains_nul(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0)
}

/// One located match inside a text file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Byte offset of the match from the start of the file.
    pub byte_offset: u64,
    /// One-based line number the match sits on.
    pub line: u64,
    /// Escaped snippet showing the match in context.
    pub evidence: String,
    /// Escaped short fact about the match: a class name, a format, a target.
    pub detail: String,
}

/// A text file prepared for scanning: line index, lowercase copy, and limits.
pub struct TextScan<'a> {
    text: &'a str,
    lowered: String,
    line_starts: Vec<u64>,
    /// Whether the scanned region stops short of the whole file.
    pub truncated: bool,
    /// Number of bytes actually scanned.
    pub scanned_bytes: u64,
}

impl<'a> TextScan<'a> {
    /// Prepares `text`, scanning at most `limit` bytes of it.
    #[must_use]
    pub fn new(text: &'a str, limit: u64) -> Self {
        // A limit larger than the address space cannot truncate an existing str.
        let mut end = text.len().min(usize::try_from(limit).unwrap_or(usize::MAX));
        while end < text.len() && !text.is_char_boundary(end) {
            end -= 1;
        }
        let region = &text[..end];
        let mut line_starts = vec![0_u64];
        for (offset, byte) in region.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset as u64 + 1);
            }
        }
        Self {
            text: region,
            lowered: region.to_ascii_lowercase(),
            line_starts,
            truncated: end < text.len(),
            scanned_bytes: end as u64,
        }
    }

    /// Borrows the region being scanned.
    #[must_use]
    pub fn region(&self) -> &str {
        self.text
    }

    /// Returns the one-based line number containing `offset`.
    #[must_use]
    pub fn line_of(&self, offset: u64) -> u64 {
        match self.line_starts.binary_search(&offset) {
            Ok(index) => index as u64 + 1,
            Err(index) => index as u64,
        }
    }

    /// Builds a located match at `offset`, describing it with `detail`.
    ///
    /// # Panics
    /// Panics unless `offset` is a UTF-8 character boundary within the scanned region.
    #[must_use]
    pub fn located(&self, offset: u64, detail: &str) -> Match {
        Match {
            byte_offset: offset,
            line: self.line_of(offset),
            evidence: self.snippet(offset),
            detail: escape(detail),
        }
    }

    /// Finds every occurrence of any `needle`, matched case-insensitively.
    ///
    /// Needles are compared against an ASCII-lowercased copy, so a policy
    /// entry must itself be lowercase; the policy tests assert that.
    #[must_use]
    pub fn find_any(&self, needles: &[String]) -> Vec<Match> {
        let mut matches = Vec::new();
        for needle in needles {
            let mut from = 0;
            while let Some(found) = self.lowered[from..].find(needle.as_str()) {
                let offset = (from + found) as u64;
                matches.push(self.located(offset, needle));
                from += found + needle.len().max(1);
            }
        }
        matches.sort_by_key(|found| found.byte_offset);
        matches
    }

    /// Finds every URL beginning with one of `schemes`.
    #[must_use]
    pub fn find_urls(&self, schemes: &[String]) -> Vec<Match> {
        let mut matches = self.find_any(schemes);
        for found in &mut matches {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "find_any widens this scan's usize offsets to u64; narrowing them is lossless."
            )]
            let offset = found.byte_offset as usize;
            found.evidence = escape(&url_at(self.text, offset));
        }
        matches
    }

    /// Finds runs of `class` characters at least `minimum` long.
    pub fn find_runs(&self, minimum: usize, class: fn(u8) -> bool, detail: &str) -> Vec<Match> {
        let mut matches = Vec::new();
        let bytes = self.text.as_bytes();
        let mut start = None;
        for index in 0..=bytes.len() {
            let inside = index < bytes.len() && class(bytes[index]);
            match (inside, start) {
                (true, None) => start = Some(index),
                (false, Some(from)) => {
                    let length = index - from;
                    if length >= minimum {
                        matches
                            .push(self.located(from as u64, &format!("{detail}, {length} bytes")));
                    }
                    start = None;
                }
                _ => {}
            }
        }
        matches
    }

    fn snippet(&self, offset: u64) -> String {
        #[expect(
            clippy::expect_used,
            reason = "located requires an offset into this str; an unrepresentable index is API misuse."
        )]
        let offset = usize::try_from(offset).expect("offset must index the scanned region");
        let line_start = self.text[..offset].rfind('\n').map_or(0, |index| index + 1);
        let line_end = self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |index| offset + index);
        let line = &self.text[line_start..line_end];
        escape(&line.chars().take(MAX_EVIDENCE_CHARS).collect::<String>())
    }
}

/// Reports whether `byte` may appear in a base64 payload.
#[must_use]
pub fn is_base64_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' || byte == b'='
}

/// Reports whether `byte` may appear in a hexadecimal payload.
#[must_use]
pub fn is_hex_byte(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || (b'A'..=b'F').contains(&byte)
}

/// Renders `text` so no byte in it can act on a terminal or a renderer.
///
/// Every character outside printable ASCII becomes `\u{...}`, including the
/// homoglyphs and hidden characters Inspection exists to expose: showing a
/// zero-width space as itself would hide the finding inside the report of it.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character == '\\' {
            escaped.push_str("\\\\");
        } else if (' '..='~').contains(&character) {
            escaped.push(character);
        } else {
            escaped.extend(character.escape_unicode());
        }
    }
    escaped
}

/// Returns every code point in `text` the policy names, with its class.
#[must_use]
pub fn hidden_characters(text: &str, policy: &Policy) -> Vec<(u64, char, String)> {
    text.char_indices()
        .filter_map(|(offset, character)| {
            policy
                .unicode_class(character as u32)
                .map(|class| (offset as u64, character, class.to_owned()))
        })
        .collect()
}

/// Returns every code point in `text` that imitates an ASCII character.
#[must_use]
pub fn confusable_characters(text: &str, policy: &Policy) -> Vec<(u64, char, char)> {
    text.char_indices()
        .filter_map(|(offset, character)| {
            policy
                .confusable_target(character)
                .map(|target| (offset as u64, character, target))
        })
        .collect()
}

/// Returns every control character in `text` other than tab, newline, return.
#[must_use]
pub fn control_characters(text: &str) -> Vec<(u64, char)> {
    text.char_indices()
        .filter(|(_, character)| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        .map(|(offset, character)| (offset as u64, character))
        .collect()
}

fn url_at(text: &str, offset: usize) -> String {
    let tail = &text[offset..];
    let end = tail
        .find(|character: char| {
            character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>' | ')' | '`')
        })
        .unwrap_or(tail.len());
    tail[..end].chars().take(MAX_EVIDENCE_CHARS).collect()
}
