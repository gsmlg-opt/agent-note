# syntax=docker/dockerfile:1

# Full-stack single image: builds the wasm frontend and the note-server binary, then serves the UI
# (from NOTE_STATIC_DIR) alongside the REST API and the MCP Streamable HTTP endpoint on one port.

# ---- Stage 1: build the Yew/Wasm frontend into a static bundle ----
FROM rust:1-bookworm AS frontend
RUN rustup target add wasm32-unknown-unknown \
    && cargo install --locked trunk
WORKDIR /build
COPY . .
# note-frontend is excluded from the workspace (own Cargo.lock); build it from its own dir.
RUN cd crates/note-frontend && trunk build --release
# -> /build/crates/note-frontend/dist

# ---- Stage 2: build the note-server binary (release) ----
FROM rust:1-bookworm AS backend
WORKDIR /build
COPY . .
# ort uses load-dynamic, so no ONNX Runtime lib is needed at build time; the default StubEmbedder
# means none is needed at runtime either (real OrtEmbedder wiring is deferred — see the README).
RUN cargo build --release -p note-server
# -> /build/target/release/note-server

# ---- Stage 3: slim runtime ----
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=backend /build/target/release/note-server /usr/local/bin/note-server
COPY --from=frontend /build/crates/note-frontend/dist /app/static
# 0.0.0.0 so the container is reachable via `docker run -p`; the DB lives on a volume so notes
# survive container restarts.
ENV NOTE_BIND_ADDR=0.0.0.0:6222 \
    NOTE_STATIC_DIR=/app/static \
    NOTE_DB_PATH=/app/data/notes.db
RUN mkdir -p /app/data
EXPOSE 6222
VOLUME ["/app/data"]
CMD ["note-server"]
