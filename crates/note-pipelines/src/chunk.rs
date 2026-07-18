use text_splitter::MarkdownSplitter;

pub const CHUNK_CAPACITY_CHARS: usize = 1000;

pub fn chunk_content(content: &str) -> Vec<String> {
    if content.trim().is_empty() {
        return Vec::new();
    }

    let chunks: Vec<String> = MarkdownSplitter::new(CHUNK_CAPACITY_CHARS)
        .chunks(content)
        .map(str::to_string)
        .collect();

    chunks
}

#[cfg(test)]
mod tests {
    use super::chunk_content;

    #[test]
    fn long_markdown_yields_multiple_chunks() {
        let paragraph =
            "This is a long paragraph with enough content to exercise markdown-aware chunking. ";
        let content = format!(
            "# Heading\n\n{}\n\n## Details\n\n{}",
            paragraph.repeat(20),
            paragraph.repeat(20)
        );

        assert!(chunk_content(&content).len() > 1);
    }

    #[test]
    fn short_markdown_yields_one_chunk() {
        assert_eq!(chunk_content("# Heading\n\nShort body.").len(), 1);
    }

    #[test]
    fn blank_markdown_yields_no_chunks() {
        assert!(chunk_content("").is_empty());
        assert!(chunk_content(" \n\t\r\n").is_empty());
    }
}
