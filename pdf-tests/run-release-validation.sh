#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compose_file="$repo_root/deployment/pdf-export/compose.yaml"
artifact_dir="${NOTE_TEST_PDF_ARTIFACT_DIR:-$repo_root/target/pdf-export-validation}"
renderer_image="gotenberg/gotenberg:8.37.0-chromium@sha256:0d28ae9a96441588ef739623726bd500ad0720b77266c6f1351a13e333fbd61c"
renderer_digest="sha256:0d28ae9a96441588ef739623726bd500ad0720b77266c6f1351a13e333fbd61c"
recorder_image="python:3.13-alpine@sha256:7415fbc3c9e4979cc717d92377ab2bc7b2b4a2af1ac03cc52b5f3f88efedaf3a"
work_dir="$(mktemp -d)"
run_id="$(basename "$work_dir" | tr -cd '[:alnum:]' | tr '[:upper:]' '[:lower:]')"
project_name="agent-note-pdf-validation-${run_id}"
qualification_network="${project_name}-internal"
public_probe_network="${project_name}-public-probe"

compose() {
  AGENT_NOTE_PDF_NETWORK="$qualification_network" \
    docker compose --project-name "$project_name" -f "$compose_file" "$@"
}

cleanup() {
  set +e
  docker rm --force "${project_name}-recorder" >/dev/null 2>&1
  compose down --volumes --remove-orphans >/dev/null 2>&1
  docker network rm "$public_probe_network" >/dev/null 2>&1
  rm -rf "$work_dir"
}
trap cleanup EXIT

for command in docker curl python3 cargo qpdf pdfinfo pdftotext pdftoppm identify; do
  command -v "$command" >/dev/null || {
    echo "required PDF qualification command is missing: $command" >&2
    exit 1
  }
done
docker compose version
[[ -f "$compose_file" ]] || {
  echo "PDF deployment compose file is missing" >&2
  exit 1
}

mkdir -p "$artifact_dir"
rm -f "$artifact_dir"/agent-note-export.pdf \
  "$artifact_dir"/agent-note-export.txt \
  "$artifact_dir"/agent-note-export-page-*.png \
  "$artifact_dir"/qualification-record.txt \
  "$artifact_dir"/security-checks.log

AGENT_NOTE_PDF_NETWORK="$qualification_network" \
  docker compose -f "$compose_file" config --format json >"$work_dir/production-compose.json"
python3 - "$work_dir/production-compose.json" "$renderer_image" "$qualification_network" <<'PY'
import json
import sys

config = json.load(open(sys.argv[1], encoding="utf-8"))
expected_image = sys.argv[2]
expected_network = sys.argv[3]
service = config["services"]["gotenberg"]
assert service["image"] == expected_image, service["image"]
assert not service.get("ports"), "production renderer must not publish ports"
assert not service.get("volumes"), "renderer must not mount host or named volumes"
assert service.get("read_only") is True
assert "ALL" in service.get("cap_drop", [])
assert any("no-new-privileges" in item for item in service.get("security_opt", []))
assert service.get("pids_limit", 0) > 0
assert service.get("mem_limit")
assert service.get("cpus")
assert service.get("tmpfs"), "renderer needs only bounded ephemeral tmpfs"
environment = service.get("environment", {}) or {}
for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"):
    assert not environment.get(key), f"proxy inheritance must be empty: {key}"
command = " ".join(service.get("command", []))
required = (
    "--api-disable-download-from=true",
    "--webhook-disable=true",
    "--chromium-disable-javascript=true",
    "--chromium-deny-private-ips=true",
    "--chromium-deny-public-ips=true",
    "--chromium-max-concurrency=2",
    "--chromium-clear-cache=true",
    "--chromium-clear-cookies=true",
    "--chromium-clear-storage=true",
)
for flag in required:
    assert flag in command, f"missing renderer hardening flag: {flag}"
for forbidden in ("proxy-server", "host-resolver-rules", "user-agent", "--chromium-cookies"):
    assert forbidden not in command, f"forbidden renderer option: {forbidden}"
networks = config.get("networks", {})
assert any(
    value.get("internal") is True and value.get("name") == expected_network
    for value in networks.values()
), f"qualification renderer must use its unique internal network: {expected_network}"
PY

compose pull gotenberg
docker pull "$recorder_image"
repo_digests="$(docker image inspect "$renderer_image" --format '{{json .RepoDigests}}')"
grep -F "$renderer_digest" <<<"$repo_digests" >/dev/null || {
  echo "pulled image does not expose the required manifest digest: $repo_digests" >&2
  exit 1
}
compose up -d --wait gotenberg
container_id="$(compose ps -q gotenberg)"
[[ -n "$container_id" ]] || {
  echo "Gotenberg container did not start" >&2
  exit 1
}

renderer_ip="$(docker inspect "$container_id" --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}')"
[[ -n "$renderer_ip" ]] || {
  echo "Gotenberg has no internal bridge address" >&2
  exit 1
}
renderer_url="http://${renderer_ip}:3000"
for _ in $(seq 1 60); do
  if curl --fail --silent "$renderer_url/health" >/dev/null; then
    break
  fi
  sleep 1
done
curl --fail --silent "$renderer_url/health" >/dev/null

docker inspect "$container_id" >"$work_dir/container-inspect.json"
python3 - "$work_dir/container-inspect.json" <<'PY'
import json
import sys

container = json.load(open(sys.argv[1], encoding="utf-8"))[0]
host = container["HostConfig"]
assert host["ReadonlyRootfs"] is True
assert "ALL" in host.get("CapDrop", [])
assert any("no-new-privileges" in value for value in host.get("SecurityOpt", []))
assert not container.get("Mounts"), "renderer must not have persistent or host mounts"
bindings = host.get("PortBindings", {}) or {}
assert not bindings, f"renderer must not publish any host port: {bindings}"
environment = container["Config"].get("Env", [])
for item in environment:
    key, _, value = item.partition("=")
    if key in {"HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"}:
        assert not value, f"proxy environment must be empty: {key}"
    assert "PASSWORD" not in key and "TOKEN" not in key and "SECRET" not in key
PY

fonts_log="$work_dir/fonts.log"
for family in "Noto Sans CJK SC" "Noto Sans CJK JP" "Noto Sans Mono CJK SC" "Noto Sans" "DejaVu Sans"; do
  match="$(compose exec -T gotenberg fc-match --format '%{family} | %{file}\n' "$family")"
  [[ "$match" == "$family | "* ]] || {
    echo "required renderer font family resolved to a fallback: $family => $match" >&2
    exit 1
  }
  printf '%s => %s\n' "$family" "$match" | tee -a "$fonts_log"
done

python3 - "$work_dir" <<'PY'
from pathlib import Path
import base64
import sys

root = Path(sys.argv[1])
png = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
(root / "request-a.png").write_bytes(png)
vectors = {
    "http": '<img src="http://recorder:8080/blocked.png">',
    "https": '<img src="https://public-recorder:8443/blocked.png">',
    "loopback": '<img src="http://127.0.0.1:9/blocked.png">',
    "private": '<img src="http://recorder:8080/private.png">',
    "public_dns": '<img src="http://public-recorder:8080/blocked.png">',
    "public_ip": '<img src="http://203.0.113.10:8080/blocked.png">',
    "redirect": '<img src="http://recorder:8080/redirect">',
    "css_import": '<style>@import url("http://recorder:8080/blocked.css");</style>',
    "css_url": '<style>body{background:url("http://recorder:8080/blocked.png")}</style>',
    "svg": '<svg><image href="http://recorder:8080/blocked.png"/></svg>',
    "iframe": '<iframe src="http://recorder:8080/blocked"></iframe>',
}
for name, body in vectors.items():
    (root / f"blocked-{name}.html").write_text(
        f"<!doctype html><html><body><p>BLOCKED VECTOR CONTROL {name}</p>{body}</body></html>",
        encoding="utf-8",
    )
(root / "javascript.html").write_text(
    "<!doctype html><body onload=\"document.body.textContent='EXECUTED EVENT'\">"
    "<p>SAFE STATIC TEXT</p>"
    "<script>document.body.textContent='EXECUTED SCRIPT'</script></body>",
    encoding="utf-8",
)
(root / "request-a.html").write_text(
    '<!doctype html><body><p>IN-FLIGHT REQUEST A SECRET 9f4ca771</p></body>',
    encoding="utf-8",
)
(root / "requests.log").write_text("", encoding="utf-8")
(root / "connections.log").write_text("", encoding="utf-8")
(root / "recorder.py").write_text(
    '''from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import socketserver
import threading

PNG = bytes.fromhex("89504e470d0a1a0a0000000d4948445200000001000000010804000000b51c0c020000000b4944415478da6364f80f00010501012718e3660000000049454e44ae426082")

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        with Path("/work/requests.log").open("a", encoding="utf-8") as log:
            log.write(self.path + "\\n")
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "http://recorder:8080/redirect-target")
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Content-Length", str(len(PNG)))
        self.end_headers()
        self.wfile.write(PNG)

    def log_message(self, *_args):
        pass

class ConnectionHandler(socketserver.BaseRequestHandler):
    def handle(self):
        with Path("/work/connections.log").open("a", encoding="utf-8") as log:
            log.write(f"{self.client_address[0]}\\n")

tcp_server = socketserver.ThreadingTCPServer(("0.0.0.0", 8443), ConnectionHandler)
threading.Thread(target=tcp_server.serve_forever, daemon=True).start()
ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
''',
    encoding="utf-8",
)
PY

set +e
docker rm --force "${project_name}-recorder" >/dev/null 2>&1
docker network rm "$public_probe_network" >/dev/null 2>&1
set -e
docker network create --internal --subnet 203.0.113.0/24 "$public_probe_network" >/dev/null
docker run --detach --rm \
  --name "${project_name}-recorder" \
  --network "$qualification_network" \
  --network-alias recorder \
  --mount "type=bind,src=${work_dir},dst=/work" \
  "$recorder_image" python /work/recorder.py >/dev/null
docker network connect \
  --alias public-recorder \
  --ip 203.0.113.10 \
  "$public_probe_network" \
  "${project_name}-recorder"
docker network connect --ip 203.0.113.11 "$public_probe_network" "$container_id"
[[ "$(docker network inspect "$public_probe_network" --format '{{.Internal}}')" == "true" ]]
for _ in $(seq 1 30); do
  if docker exec "${project_name}-recorder" python -c \
    "import socket; socket.create_connection(('127.0.0.1', 8080), 1).close()" \
    >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker exec "${project_name}-recorder" python -c \
  "import socket; socket.create_connection(('127.0.0.1', 8080), 1).close()"

# Prove both recorder paths are routable from the renderer container. Conversion requests below
# must still produce no recorder traffic because Chromium's private/public destination filters
# reject them before a connection is attempted.
docker exec "$container_id" curl --fail --silent http://recorder:8080/private-control >/dev/null
docker exec "$container_id" curl --fail --silent http://203.0.113.10:8080/public-control >/dev/null
set +e
docker exec "$container_id" curl --silent --max-time 2 https://public-recorder:8443/https-control \
  >/dev/null 2>&1
https_control_status=$?
set -e
[[ "$https_control_status" -ne 0 ]]
grep -F "/private-control" "$work_dir/requests.log" >/dev/null
grep -F "/public-control" "$work_dir/requests.log" >/dev/null
[[ -s "$work_dir/connections.log" ]]

post_html() {
  local html="$1"
  local output="$2"
  shift 2
  curl --silent --show-error --output "$output" --write-out '%{http_code}' \
    --form "files=@${html};type=text/html;filename=index.html" \
    --form "preferCssPageSize=true" \
    --form "printBackground=true" \
    --form "skipNetworkIdleEvent=false" \
    --form "failOnResourceLoadingFailed=true" \
    "$@" "$renderer_url/forms/chromium/convert/html"
}

post_isolation_probe() {
  local html="$1"
  local output="$2"
  curl --silent --show-error --output "$output" --write-out '%{http_code}' \
    --form "files=@${html};type=text/html;filename=index.html" \
    --form "preferCssPageSize=true" \
    --form "printBackground=true" \
    --form "skipNetworkIdleEvent=true" \
    --form "failOnResourceLoadingFailed=false" \
    "$renderer_url/forms/chromium/convert/html"
}

security_log="$artifact_dir/security-checks.log"
: >"$security_log"
if docker exec "$container_id" curl --fail --silent --show-error --max-time 5 \
  http://example.com/agent-note-egress-probe >/dev/null 2>&1; then
  echo "renderer unexpectedly reached public egress" >&2
  exit 1
fi
echo "renderer public egress probe blocked" | tee -a "$security_log"
for fixture in "$work_dir"/blocked-*.html; do
  name="$(basename "$fixture" .html)"
  : >"$work_dir/requests.log"
  : >"$work_dir/connections.log"
  status="$(post_html "$fixture" "$work_dir/$name.response")"
  [[ "$status" =~ ^2 ]] || {
    echo "$name failed to render while testing blocked fetches: HTTP $status" >&2
    exit 1
  }
  qpdf --check "$work_dir/$name.response" >/dev/null || {
    echo "$name returned a non-PDF response while testing blocked fetches" >&2
    exit 1
  }
  pdftotext "$work_dir/$name.response" "$work_dir/$name.txt"
  grep -F "BLOCKED VECTOR CONTROL ${name#blocked-}" "$work_dir/$name.txt" >/dev/null || {
    echo "$name PDF omitted its static rendering control" >&2
    exit 1
  }
  sleep 0.25
  if [[ -s "$work_dir/requests.log" ]]; then
    echo "$name reached the forbidden recorder: $(tr '\n' ' ' <"$work_dir/requests.log")" >&2
    exit 1
  fi
  if [[ -s "$work_dir/connections.log" ]]; then
    echo "$name opened a forbidden recorder connection: $(tr '\n' ' ' <"$work_dir/connections.log")" >&2
    exit 1
  fi
  echo "$name fetch blocked; conversion HTTP $status; recorder requests 0; raw connections 0" | tee -a "$security_log"
done

status="$(post_html "$work_dir/javascript.html" "$work_dir/javascript.pdf")"
[[ "$status" =~ ^2 ]] || {
  echo "JavaScript-disabled control failed to render: HTTP $status" >&2
  exit 1
}
pdftotext "$work_dir/javascript.pdf" "$work_dir/javascript.txt"
grep -F "SAFE STATIC TEXT" "$work_dir/javascript.txt" >/dev/null
if grep -E "EXECUTED (SCRIPT|EVENT)" "$work_dir/javascript.txt" >/dev/null; then
  echo "JavaScript or an event handler executed inside the renderer" >&2
  exit 1
fi
echo "script and event handlers did not execute" | tee -a "$security_log"

post_html "$work_dir/request-a.html" "$work_dir/request-a.pdf" \
  --form "waitDelay=20s" >"$work_dir/request-a.status" &
request_a_pid=$!
active_request_file=""
for _ in $(seq 1 60); do
  if ! kill -0 "$request_a_pid" 2>/dev/null; then
    break
  fi
  set +e
  active_request_file="$(docker exec "$container_id" grep -rl \
    'IN-FLIGHT REQUEST A SECRET 9f4ca771' /tmp 2>/dev/null | sed -n '1p')"
  set -e
  if [[ -n "$active_request_file" ]]; then
    break
  fi
  sleep 0.1
done
[[ "$active_request_file" == /tmp/* ]] || {
  echo "could not identify request A's live packaged file" >&2
  exit 1
}
kill -0 "$request_a_pid" 2>/dev/null || {
  echo "request A completed before the concurrent isolation probe" >&2
  exit 1
}
python3 - "$work_dir" "$active_request_file" <<'PY'
from pathlib import Path
import html
import sys

root = Path(sys.argv[1])
request_file = sys.argv[2]
target = html.escape(f"file://{request_file}", quote=True)
(root / "request-b.html").write_text(
    f'<!doctype html><body><p>REQUEST B CONTROL</p><iframe src="{target}"></iframe></body>',
    encoding="utf-8",
)
PY
status="$(post_isolation_probe "$work_dir/request-b.html" "$work_dir/request-b.response")"
kill -0 "$request_a_pid" 2>/dev/null || {
  echo "request A was not live throughout the concurrent isolation probe" >&2
  exit 1
}
[[ "$status" =~ ^2 ]] || {
  echo "request B isolation probe failed to render: HTTP $status" >&2
  exit 1
}
pdftotext "$work_dir/request-b.response" "$work_dir/request-b.txt"
grep -F "REQUEST B CONTROL" "$work_dir/request-b.txt" >/dev/null || {
  echo "request B control text is missing from the isolation probe" >&2
  exit 1
}
if grep -F "IN-FLIGHT REQUEST A SECRET 9f4ca771" "$work_dir/request-b.txt" >/dev/null; then
  echo "request B accessed request A's in-flight packaged file" >&2
  exit 1
fi
set +e
wait "$request_a_pid"
request_a_exit=$?
set -e
[[ "$request_a_exit" -eq 0 && "$(<"$work_dir/request-a.status")" =~ ^2 ]] || {
  echo "request A packaged asset control failed" >&2
  exit 1
}
pdftotext "$work_dir/request-a.pdf" "$work_dir/request-a.txt"
grep -F "IN-FLIGHT REQUEST A SECRET 9f4ca771" "$work_dir/request-a.txt" >/dev/null
echo "concurrent request B could not read request A's known live renderer path; conversion HTTP $status" \
  | tee -a "$security_log"

container_hostname="$(docker exec "$container_id" cat /etc/hostname | tr -d '\r\n')"
[[ -n "$container_hostname" ]] || {
  echo "could not read the renderer traversal probe target" >&2
  exit 1
}
cat >"$work_dir/traversal.html" <<'HTML'
<!doctype html><body><p>TRAVERSAL CONTROL</p><iframe src="file:///tmp/../../etc/hostname"></iframe></body>
HTML
status="$(post_isolation_probe "$work_dir/traversal.html" "$work_dir/traversal.response")"
[[ "$status" =~ ^2 ]] || {
  echo "file traversal probe failed to render its control document: HTTP $status" >&2
  exit 1
}
pdftotext "$work_dir/traversal.response" "$work_dir/traversal.txt"
grep -F "TRAVERSAL CONTROL" "$work_dir/traversal.txt" >/dev/null || {
  echo "file traversal control text is missing" >&2
  exit 1
}
if grep -F "$container_hostname" "$work_dir/traversal.txt" >/dev/null; then
  echo "renderer file traversal exposed /etc/hostname" >&2
  exit 1
fi
echo "file traversal to a known renderer-local target was blocked; conversion HTTP $status" \
  | tee -a "$security_log"

NOTE_TEST_REAL_PDF=1 \
NOTE_TEST_GOTENBERG_URL="$renderer_url" \
NOTE_TEST_PDF_ARTIFACT_DIR="$artifact_dir" \
cargo test --manifest-path "$repo_root/Cargo.toml" -p note-server \
  --test export_real_renderer_test -- --nocapture

functional_commit="$(git -C "$repo_root" rev-parse HEAD)"
image_id="$(docker image inspect "$renderer_image" --format '{{.Id}}')"
platform="$(docker image inspect "$renderer_image" --format '{{.Os}}/{{.Architecture}}')"
{
  echo "Agent Note PDF release qualification"
  echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "functional_commit=$functional_commit"
  echo "renderer_image=$renderer_image"
  echo "renderer_manifest_digest=$renderer_digest"
  echo "renderer_image_id=$image_id"
  echo "renderer_platform=$platform"
  echo "docker=$(docker --version)"
  echo "compose=$(docker compose version --short)"
  echo "cargo=$(cargo --version)"
  echo "qpdf=$(qpdf --version | head -1)"
  echo "pdfinfo=$(pdfinfo -v 2>&1 | head -1)"
  echo "pdftoppm=$(pdftoppm -v 2>&1 | head -1)"
  echo "imagemagick=$(identify --version | head -1)"
  echo "browser_coverage=not executed by this PDF-only qualification; use the separate browser gate"
  echo "postgres_live_contract=not part of this renderer qualification"
  echo "s3_minio_contract=not part of this renderer qualification"
  echo "visual_review=automated every-page nonblank check complete; human review required before release sign-off"
  echo "fonts:"
  sed 's/^/  /' "$fonts_log"
  echo "security_checks:"
  sed 's/^/  /' "$security_log"
} >"$artifact_dir/qualification-record.txt"

echo "PDF qualification passed; artifacts: $artifact_dir"
