use note_core::{apply_content_patch, ContentPatch, ContentUpdate};

fn patch(source: &str) -> ContentPatch {
    ContentPatch {
        apply_patch: source.to_owned(),
    }
}

#[test]
fn content_update_deserializes_a_string_as_replace() {
    let update: ContentUpdate = serde_json::from_str(r#""replacement""#).unwrap();
    assert_eq!(update, ContentUpdate::Replace("replacement".to_owned()));
}

#[test]
fn content_update_deserializes_an_apply_patch_object() {
    let update: ContentUpdate =
        serde_json::from_str(r#"{"apply_patch":"@@\n-old\n+new"}"#).unwrap();
    assert_eq!(update, ContentUpdate::ApplyPatch(patch("@@\n-old\n+new")));
}

#[test]
fn applies_one_hunk() {
    assert_eq!(
        apply_content_patch("before\nold\nafter\n", "@@\n before\n-old\n+new\n after").unwrap(),
        "before\nnew\nafter\n"
    );
}

#[test]
fn applies_multiple_ordered_non_overlapping_hunks() {
    assert_eq!(
        apply_content_patch(
            "zero\none\ntwo\nthree\nfour\n",
            "@@\n one\n-two\n+TWO\n@@\n three\n-four\n+FOUR",
        )
        .unwrap(),
        "zero\none\nTWO\nthree\nFOUR\n"
    );
}

#[test]
fn preserves_unicode_crlf_mixed_line_endings_final_newline_and_unterminated_line() {
    assert_eq!(
        apply_content_patch("α\r\nold\nβ\r\nlast", "@@\n α\n-old\n+新\n β",).unwrap(),
        "α\r\n新\r\nβ\r\nlast"
    );
}

#[test]
fn deleting_unterminated_final_line_does_not_add_lf_to_predecessor() {
    assert_eq!(
        apply_content_patch("kept\nremoved", "@@\n kept\n-removed").unwrap(),
        "kept"
    );
}

#[test]
fn deleting_unterminated_final_line_does_not_add_crlf_to_predecessor() {
    assert_eq!(
        apply_content_patch("kept\r\nremoved", "@@\n kept\n-removed").unwrap(),
        "kept"
    );
}

#[test]
fn rejects_invalid_or_unsafe_patches() {
    let cases = [
        ("empty hunk", "@@", "old\n"),
        ("unanchored insertion", "@@\n+new", "old\n"),
        ("missing context", "@@\n-missing\n+new", "old\n"),
        (
            "ambiguous context",
            "@@\n same\n-old\n+new",
            "same\nold\nsame\nold\n",
        ),
        ("fuzzy whitespace", "@@\n-old \n+new", "old\n"),
        ("overlap", "@@\n a\n-b\n+B\n@@\n b\n-c\n+C", "a\nb\nc\n"),
        (
            "reverse order",
            "@@\n three\n-four\n+FOUR\n@@\n one\n-two\n+TWO",
            "one\ntwo\nthree\nfour\n",
        ),
        (
            "file directive",
            "*** Update File: note.md\n@@\n-old\n+new",
            "old\n",
        ),
        (
            "patch envelope",
            "*** Begin Patch\n@@\n-old\n+new\n*** End Patch",
            "old\n",
        ),
        ("line header", "@@ -1 +1 @@\n-old\n+new", "old\n"),
        (
            "eof marker",
            "@@\n-old\n+new\n\\ No newline at end of file",
            "old\n",
        ),
        (
            "later failed hunk",
            "@@\n-old\n+new\n@@\n-missing\n+other",
            "old\n",
        ),
    ];

    for (name, source, content) in cases {
        assert!(
            apply_content_patch(content, source).is_err(),
            "{name} must be rejected"
        );
    }
}
