# Default Public Bind Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist a configurable HTTP bind address and default the server to `0.0.0.0:6222`.

**Architecture:** Add a `[server]` file-config section and resolve its `bind_addr` using file, environment, then default precedence. The config loader owns both missing-default-file creation and atomic insertion of a missing bind setting, while `main` consumes only the resolved `RuntimeConfig` value.

**Tech Stack:** Rust 2021, Serde, TOML, tempfile, Axum, Cargo tests

---

### Task 1: Resolve and persist the server bind address

**Files:**
- Modify: `crates/note-server/src/config.rs`

- [x] **Step 1: Write failing configuration tests**

Add tests covering:

```rust
assert_eq!(config.bind_addr, "0.0.0.0:6222");
assert!(generated.contains("[server]\nbind_addr = \"0.0.0.0:6222\""));
```

Also cover file-over-environment precedence, environment-over-default precedence,
atomic insertion into an existing implicit config, preservation of existing
contents, no overwrite of an existing file value, creation of a missing implicit
config regardless of debug/release policy, and rejection of a missing explicit
config.

- [x] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test -p note-server config::tests
```

Expected: compilation or assertion failures because `RuntimeConfig::bind_addr`,
`FileServerConfig`, persistence, and the new default do not exist.

- [x] **Step 3: Implement minimal config support**

Add:

```rust
const DEFAULT_BIND_ADDR: &str = "0.0.0.0:6222";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServerConfig {
    bind_addr: Option<String>,
}
```

Add `server: Option<FileServerConfig>` to `FileConfig`, `bind_addr: String` to
`RuntimeConfig`, and `bind_addr: Option<String>` to `EnvValues`. Resolve with
file value first, then `NOTE_BIND_ADDR`, then `DEFAULT_BIND_ADDR`.

Include `[server]` in `DEFAULT_DEV_CONFIG`. When loading any existing
file without `server.bind_addr`, atomically rewrite it with the resolved value,
preserving its parsed settings and existing text. Create a missing implicit
config in all build profiles; keep a missing explicit config as an error.

- [x] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p note-server config::tests
```

Expected: all focused configuration tests pass.

### Task 2: Consume the resolved bind address

**Files:**
- Modify: `crates/note-server/src/main.rs`

- [x] **Step 1: Update construction sites and write the failing assertion**

Update test `RuntimeConfig` literals with `bind_addr`, and add a focused assertion
that the HTTP startup path uses `config.bind_addr`.

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p note-server --bin note-server
```

Expected: failure while `main` still reads `NOTE_BIND_ADDR` directly.

- [x] **Step 3: Use resolved configuration**

Replace the direct environment read with:

```rust
let bind_addr = &config.bind_addr;
```

Keep the existing listener error propagation and startup logging.

- [x] **Step 4: Run focused server tests and verify GREEN**

Run:

```bash
cargo test -p note-server --bin note-server
```

Expected: all binary tests pass.

### Task 3: Document and verify externally

**Files:**
- Modify: `README.md`

- [x] **Step 1: Update runtime configuration documentation**

Document `[server] bind_addr`, precedence
`config.toml -> NOTE_BIND_ADDR -> 0.0.0.0:6222`, automatic creation/migration
of the default config, explicit missing-file errors, and trusted-network
exposure.

- [x] **Step 2: Run formatting and scoped tests**

Run:

```bash
cargo fmt --all -- --check
cargo test -p note-server
```

Expected: both commands exit successfully.

- [x] **Step 3: Verify the live MCP endpoint**

Restart without `NOTE_BIND_ADDR`, then POST an MCP `initialize` request to:

```text
http://10.100.10.10:6222/mcp
```

Expected: HTTP 200 with protocol version `2025-06-18`.
