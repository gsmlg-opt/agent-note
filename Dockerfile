# syntax=docker/dockerfile:1

# Full-stack single image: builds the wasm frontend and the note-server binary, then serves the UI
# (from NOTE_STATIC_DIR) alongside the REST API and the MCP Streamable HTTP endpoint on one port.

# ---- Stage 1: build the Yew/Wasm frontend into a static bundle ----
FROM rust:1-bookworm AS frontend
ENV CARGO_HTTP_TIMEOUT=600 \
    CARGO_NET_RETRY=5
RUN mkdir -p /usr/local/cargo \
    && printf '[source.crates-io]\nreplace-with = "rsproxy"\n\n[source.rsproxy]\nregistry = "sparse+https://rsproxy.cn/index/"\n\n[net]\nretry = 5\n' > /usr/local/cargo/config.toml
RUN rustup target add wasm32-unknown-unknown \
    && cargo install --locked trunk
WORKDIR /build
COPY . .
# note-frontend is excluded from the workspace (own Cargo.lock); build it from its own dir.
RUN cd crates/note-frontend && trunk build --release
# -> /build/crates/note-frontend/dist

# ---- Stage 2: build the note-server binary (release) ----
FROM rust:1-bookworm AS backend
ENV CARGO_HTTP_TIMEOUT=600 \
    CARGO_NET_RETRY=5
RUN mkdir -p /usr/local/cargo \
    && printf '[source.crates-io]\nreplace-with = "rsproxy"\n\n[source.rsproxy]\nregistry = "sparse+https://rsproxy.cn/index/"\n\n[net]\nretry = 5\n' > /usr/local/cargo/config.toml
WORKDIR /build
COPY . .
# ort uses load-dynamic, so no ONNX Runtime lib is needed at build time.
RUN cargo build --release -p note-server --bins
# -> /build/target/release/note-server

# ---- Stage 2b: fetch the ONNX Runtime shared library (CPU) ----
# ort 2.0.0-rc.12 targets ONNX Runtime 1.24.x and loads it at runtime via ORT_DYLIB_PATH
# (load-dynamic). Baked in so the real OrtEmbedder works as soon as NOTE_MODEL_PATH is set.
FROM debian:bookworm-slim AS onnxruntime
ARG ORT_VERSION=1.24.2
RUN sed -i \
        -e 's|http://deb.debian.org/debian-security|http://mirrors.tuna.tsinghua.edu.cn/debian-security|g' \
        -e 's|http://deb.debian.org/debian|http://mirrors.tuna.tsinghua.edu.cn/debian|g' \
        /etc/apt/sources.list.d/debian.sources \
    && apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && curl --retry 5 --retry-delay 3 --connect-timeout 30 -fsSL -o /tmp/ort.tgz \
        "https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-x64-${ORT_VERSION}.tgz" \
    && mkdir -p /opt/ort \
    && tar -xzf /tmp/ort.tgz -C /opt/ort --strip-components=1 \
    && rm /tmp/ort.tgz
# -> /opt/ort/lib/libonnxruntime.so.1.24.2

# ---- Stage 3: slim runtime ----
FROM debian:bookworm-slim AS runtime
# fonts-dejavu-core gives mermaid-rs-renderer a font for diagram text metrics (the slim base has
# none, which would break server-side mermaid rendering).
RUN sed -i \
        -e 's|http://deb.debian.org/debian-security|http://mirrors.tuna.tsinghua.edu.cn/debian-security|g' \
        -e 's|http://deb.debian.org/debian|http://mirrors.tuna.tsinghua.edu.cn/debian|g' \
        /etc/apt/sources.list.d/debian.sources \
    && apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update \
    && apt-get install -y --no-install-recommends ca-certificates fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=backend /build/target/release/note-server /usr/local/bin/note-server
COPY --from=frontend /build/crates/note-frontend/dist /app/static
COPY --from=onnxruntime /opt/ort/lib/libonnxruntime.so.1.24.2 /usr/local/lib/libonnxruntime.so
# 0.0.0.0 so the container is reachable via `docker run -p`; the DB lives on a volume so notes
# survive container restarts. ORT_DYLIB_PATH points ort at the bundled ONNX Runtime; set
# NOTE_MODEL_PATH (e.g. a mounted model_quantized.onnx) to switch from the stub to real BGE-M3.
ENV NOTE_BIND_ADDR=0.0.0.0:6222 \
    NOTE_STATIC_DIR=/app/static \
    NOTE_DB_PATH=/app/data/notes.db \
    ORT_DYLIB_PATH=/usr/local/lib/libonnxruntime.so
RUN mkdir -p /app/data
EXPOSE 6222
VOLUME ["/app/data"]
CMD ["note-server"]
