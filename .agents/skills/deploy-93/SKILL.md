---
name: deploy-93
description: Deploy the agent-note Rust web app to 10.1.132.93. Use when asked to copy code, build the Docker image on that server, use its build cache, update /home/gao/docker-compose/docker-compose.yaml, restart the service, or verify https://notes.web-dev.zdns.cn/.
---

# Deploy 93

## Scope

Deploy this repo (`agent-note`) to `gao@10.1.132.93` using the server-side Docker build directory and compose file.

- Remote source/build dir: `/home/gao/agent-note-build`
- Compose file: `/home/gao/docker-compose/docker-compose.yaml`
- Service: `agent-note`
- Image tag: `agent-note:async-embedding`
- Public URL: `https://notes.web-dev.zdns.cn/`
- Runtime data/model mounts:
  - `./volume/agent-note/models:/models:ro`
  - `./volume/agent-note/data:/app/data`

The deployed image is a single Rust binary image: `note-server` serves the frontend/API and starts the embedding worker subprocess internally. Do not deploy or keep a separate `agent-note-embedding` service.

## Before Deploying

Check the local tree first:

```sh
git status --short --branch
```

Deploy the current working tree, including untracked files, when the user asks to copy current code. Do not require a commit unless the user asks for one.

Check the remote compose service:

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'grep -n -A40 -B5 "agent-note:" /home/gao/docker-compose/docker-compose.yaml'
```

Expected `agent-note` service shape:

```yaml
agent-note:
  image: agent-note:async-embedding
  container_name: agent-note
  pull_policy: missing
  restart: always
  ports:
    - "6222:6222"
  environment:
    TZ: "Asia/Shanghai"
    NOTE_BIND_ADDR: "0.0.0.0:6222"
    NOTE_MODEL_PATH: "/models/model_quantized.onnx"
    NOTE_DB_PATH: "/app/data/notes.db"
    NOTE_STATIC_DIR: "/app/static"
  volumes:
    - ./volume/agent-note/models:/models:ro
    - ./volume/agent-note/data:/app/data
```

## Sync Source

Do not use plain `rsync` on this host if it emits locale warnings before rsync protocol startup. Use tar over ssh instead.

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'set -e; rm -rf /home/gao/agent-note-build.tmp; mkdir -p /home/gao/agent-note-build.tmp'

tar \
  --exclude './target' \
  --exclude './crates/note-frontend/target' \
  --exclude './crates/note-frontend/dist' \
  --exclude './*.db' \
  --exclude './*.db-journal' \
  --exclude './models/*.onnx' \
  --exclude './.git' \
  --exclude './.trees' \
  -czf - . \
| ssh -F /dev/null gao@10.1.132.93 \
  'set -e;
   tar -xzf - -C /home/gao/agent-note-build.tmp;
   rm -rf /home/gao/agent-note-build.prev;
   if [ -d /home/gao/agent-note-build ]; then mv /home/gao/agent-note-build /home/gao/agent-note-build.prev; fi;
   mv /home/gao/agent-note-build.tmp /home/gao/agent-note-build'
```

The `LIBARCHIVE.xattr.com.apple.provenance` tar warnings from macOS extended attributes are harmless.

## Compose Cleanup

If the compose file still contains `agent-note-embedding`, remove it before deploying. The current app no longer ships an `embedding-service` binary.

Back up and remove only that service block:

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'set -e;
   cd /home/gao/docker-compose;
   backup="docker-compose.yaml.bak.$(date +%Y%m%d%H%M%S)";
   cp docker-compose.yaml "$backup";
   awk '\''/^  agent-note-embedding:/ {skip=1; next} skip && /^  [[:alnum:]_-]+:/ {skip=0} !skip {print}'\'' docker-compose.yaml > docker-compose.yaml.tmp;
   mv docker-compose.yaml.tmp docker-compose.yaml;
   echo "backup=$backup";
   docker compose -f docker-compose.yaml config --services | grep -E "^agent-note$"'
```

Do not run compose with `--remove-orphans`; this project has unrelated orphan containers that should not be removed during an agent-note deploy.

## Build With Cache

Build the image on the server from `/home/gao/agent-note-build`.

Use the legacy builder on this host:

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'cd /home/gao/agent-note-build && DOCKER_BUILDKIT=0 docker build -t agent-note:async-embedding .'
```

Reason: BuildKit/buildx has previously stalled on this server while loading early build stages. `DOCKER_BUILDKIT=0` gives plain progress and completes reliably.

Cache rules:

- Reuse the same tag, same build directory, and same Dockerfile to keep Docker layer cache hot.
- Do not pass `--no-cache` unless explicitly debugging a cache corruption.
- Do not prune Docker cache before a deploy.
- If SSH disconnects near the final Dockerfile steps, rerun the same build command. Docker should resume from cached layers and finish the final tag step quickly.
- Keep the build context small. It should be around 1-2 MB. If it is much larger, check the excludes and `.dockerignore`.

Useful build-state checks:

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'ps -ef | grep -E "docker build.*agent-note:async-embedding|cargo|rustc|trunk" | grep -v grep || true'

ssh -F /dev/null gao@10.1.132.93 \
  'docker images agent-note:async-embedding --format "image={{.ID}} created={{.CreatedAt}} size={{.Size}}"'
```

## Restart Service

Stop and remove the old standalone embedding container if it exists:

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'docker stop agent-note-embedding 2>/dev/null || true; docker rm agent-note-embedding 2>/dev/null || true'
```

Recreate only the main service:

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'cd /home/gao/docker-compose && docker compose -f docker-compose.yaml up -d --force-recreate agent-note'
```

## Verify

Check container state:

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'docker ps -a --filter name=agent-note --format "{{.Names}} {{.Image}} {{.Status}} {{.Ports}}"'
```

Check logs:

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'docker logs --tail 120 agent-note 2>&1'
```

Healthy logs should include:

- `embedder: ONNX (/models/model_quantized.onnx)`
- `embedding worker ready: ...`
- `note-server listening on http://0.0.0.0:6222`
- `serving frontend from /app/static`

Check local server responses:

```sh
ssh -F /dev/null gao@10.1.132.93 \
  'curl -fsS -I http://127.0.0.1:6222/;
   curl -fsS -o /dev/null -w "notes_api_status=%{http_code} size=%{size_download} time=%{time_total}\n" "http://127.0.0.1:6222/api/notes?limit=10&offset=0";
   curl -fsS -o /dev/null -w "notes_count_status=%{http_code} size=%{size_download} time=%{time_total}\n" "http://127.0.0.1:6222/api/notes/count"'
```

Check public ingress:

```sh
curl -fsS -I https://notes.web-dev.zdns.cn/
curl -fsS -o /dev/null -w 'public_notes_api_status=%{http_code} size=%{size_download} time=%{time_total}\n' \
  'https://notes.web-dev.zdns.cn/api/notes?limit=10&offset=0'
```

For the notes list endpoint, the response should be small because list rows return only `id`, `title`, `labels`, `created_at`, and `updated_at`. Full note `content` should only be fetched from `/api/notes/{id}`.

## Failure Handling

- If `/api/notes` returns hundreds of MB, the frontend or API is still loading all notes or including `content` in list rows. Fix pagination/list DTOs before redeploying.
- If `agent-note` starts but logs show no `embedding worker ready`, inspect the worker startup path and model mount.
- If compose warns about unrelated orphan containers, ignore the warning unless the user explicitly asks to clean them.
- If `rsync` fails with `unexpected end of file`, use the tar-over-ssh sync command above.
