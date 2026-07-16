# Repository Guidelines

## Project Structure & Module Organization

Native Rust 2021 crates live under `crates/`. `note-core` contains pure logic; `note-storage` and `note-embedding` own database and inference boundaries; `note-pipelines` composes workflows; and `note-mcp` plus `note-server` expose transports. Keep `note-core` free of sibling dependencies, compose core and effects in `note-pipelines`, and call pipelines from transports. Never put I/O or transport logic in the core.

`crates/note-frontend/` is a separate Yew/Wasm project, excluded from the root workspace. Pages and components live in `src/pages/` and `src/components/`; CSS and `index.html` sit at the crate root. Integration tests, where present, live in each crate's `tests/` directory. Architecture documents are under `docs/`, and `models/` is reserved for local ONNX assets.

## Build, Test, and Development Commands

- `cargo build --workspace` builds all native crates.
- `cargo test --workspace` runs the backend test suite.
- `cd crates/note-frontend && cargo test` runs frontend logic tests.
- `cargo run` starts the backend on `127.0.0.1:6222` and the Trunk UI on port `6221`.
- `cargo check --workspace --all-targets` and `cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown` mirror CI checks.

Prepare frontend tooling with `cargo install --locked trunk` and `rustup target add wasm32-unknown-unknown`.

## Coding Style & Naming Conventions

Use default `rustfmt` with four-space indentation; check native formatting with `cargo fmt --all -- --check`. Name modules, functions, and tests in `snake_case`; types and traits in `PascalCase`; constants in `SCREAMING_SNAKE_CASE`. Pass database and embedding dependencies through `note_pipelines::Context`. Treat `crates/note-frontend/duskmoon-core.css` as vendored output; refresh it only as documented in `README.md`.

## Testing Guidelines

Place unit tests beside code in `#[cfg(test)]` modules and integration tests in `tests/*_test.rs`. Use descriptive behavior names, `#[test]` for pure logic, and `#[tokio::test]` for async backend paths. Iterate with a focused target such as `cargo test -p note-storage --test notes_test`; before review, run the workspace suite, frontend suite, or both according to the affected crates. No coverage threshold exists.

## Commit & Pull Request Guidelines

History follows Conventional Commits with imperative summaries, such as `feat(server): configure request limits` or `fix(notes): page list responses`. Keep commits focused. Pull requests should summarize behavior, name affected crates, link applicable issues, and list checks performed. Include before/after screenshots for visible frontend changes.

## Security & Local Configuration

Do not commit databases, `dev-data/`, untracked build output, or ONNX models; tracked vendored assets are exempt. Optional `NOTE_DB_PATH`, `NOTE_ATTACHMENTS_DIR`, and `NOTE_MODEL_PATH` values configure local runtime assets. The Trunk server binds to `0.0.0.0`; use it only on a trusted network.
