use note_org::validate_document_path;

#[test]
fn accepts_portable_relative_lowercase_org_paths() {
    for path in ["roadmap.org", "projects/roadmap.org", "release notes.org"] {
        assert_eq!(
            validate_document_path(path),
            Ok(()),
            "expected valid path {path}"
        );
    }
}

#[test]
fn rejects_non_portable_or_non_org_paths() {
    for path in [
        "",
        " roadmap.org",
        "roadmap.org ",
        "/roadmap.org",
        "../roadmap.org",
        "./roadmap.org",
        "projects//roadmap.org",
        "projects\\roadmap.org",
        "C:/roadmap.org",
        "roadmap.ORG",
        "roadmap.txt",
    ] {
        assert!(
            validate_document_path(path).is_err(),
            "expected invalid path {path}"
        );
    }
}
