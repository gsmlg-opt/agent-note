const MAX_FILENAME_BYTES: usize = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Pdf,
}

impl ExportFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Markdown => ".md",
            Self::Pdf => ".pdf",
        }
    }
}

pub fn export_filename(title: &str, note_id: &str, format: ExportFormat) -> String {
    let extension = format.extension();
    let stem_budget = MAX_FILENAME_BYTES - extension.len();
    let mut stem = sanitize_stem(title, stem_budget);

    if stem.is_empty() || is_windows_reserved_name(&stem) {
        stem = sanitize_stem(&format!("note-{note_id}"), stem_budget);
    }
    if stem.is_empty() || is_windows_reserved_name(&stem) {
        stem = "note".to_string();
    }

    format!("{stem}{extension}")
}

fn sanitize_stem(value: &str, max_bytes: usize) -> String {
    let mut sanitized = String::with_capacity(value.len().min(max_bytes));
    for ch in value.chars() {
        if is_filename_unsafe(ch) || sanitized.len() + ch.len_utf8() > max_bytes {
            continue;
        }
        sanitized.push(ch);
    }
    sanitized.truncate(sanitized.trim_end_matches([' ', '.']).len());
    sanitized
}

fn is_filename_unsafe(ch: char) -> bool {
    ch.is_control()
        || matches!(ch, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*')
        || matches!(
            ch,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn is_windows_reserved_name(stem: &str) -> bool {
    let basename = stem.split('.').next().unwrap_or(stem).trim_end_matches(' ');
    let upper = basename.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(is_windows_device_number)
}

fn is_windows_device_number(number: &str) -> bool {
    (number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9'))
        || matches!(number, "¹" | "²" | "³")
}

#[cfg(test)]
mod tests {
    use super::{export_filename, ExportFormat};

    #[test]
    fn preserves_ordinary_unicode_and_appends_the_requested_extension() {
        assert_eq!(
            export_filename("会議メモ", "123", ExportFormat::Markdown),
            "会議メモ.md"
        );
        assert_eq!(
            export_filename("会議メモ", "123", ExportFormat::Pdf),
            "会議メモ.pdf"
        );
    }

    #[test]
    fn removes_separators_controls_bidi_and_platform_unsafe_characters() {
        assert_eq!(
            export_filename(
                "road/map\\draft:<bad>|?*\"\r\n\u{0000}\u{202e}. ",
                "note-7",
                ExportFormat::Markdown,
            ),
            "roadmapdraftbad.md"
        );
    }

    #[test]
    fn rejects_windows_reserved_device_stems() {
        for title in [
            "CON",
            "con.txt",
            "AUX",
            "NUL",
            "COM1",
            "lpt9.report",
            "COM¹",
            "com².txt",
            "Com³.report",
            "LPT¹",
            "lpt².txt",
            "Lpt³.report",
        ] {
            assert_eq!(
                export_filename(title, "abc-123", ExportFormat::Markdown),
                "note-abc-123.md"
            );
        }
    }

    #[test]
    fn falls_back_when_the_title_has_no_usable_content() {
        assert_eq!(
            export_filename(" /\\:*?\"<>|. \r\n", "abc-123", ExportFormat::Markdown),
            "note-abc-123.md"
        );
    }

    #[test]
    fn bounds_long_unicode_filenames_without_splitting_characters() {
        let filename = export_filename(&"界".repeat(100), "abc", ExportFormat::Pdf);

        assert!(filename.len() <= 180);
        assert!(filename.ends_with(".pdf"));
        assert!(filename
            .strip_suffix(".pdf")
            .unwrap()
            .chars()
            .all(|ch| ch == '界'));
    }
}
