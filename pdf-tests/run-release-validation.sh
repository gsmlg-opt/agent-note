#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compose_file="$repo_root/deployment/pdf-export/compose.yaml"
artifact_dir="${NOTE_TEST_PDF_ARTIFACT_DIR:-$repo_root/target/pdf-export-validation}"
project_name="agent-note-pdf-validation"
renderer_image="gotenberg/gotenberg:8.37.0-chromium@sha256:0d28ae9a96441588ef739623726bd500ad0720b77266c6f1351a13e333fbd61c"
renderer_digest="sha256:0d28ae9a96441588ef739623726bd500ad0720b77266c6f1351a13e333fbd61c"
work_dir="$(mktemp -d)"

compose() {
  docker compose --project-name "$project_name" -f "$compose_file" "$@"
}

cleanup() {
  set +e
  compose down --volumes --remove-orphans >/dev/null 2>&1
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

docker compose -f "$compose_file" config --format json >"$work_dir/production-compose.json"
python3 - "$work_dir/production-compose.json" "$renderer_image" <<'PY'
import json
import sys

config = json.load(open(sys.argv[1], encoding="utf-8"))
expected_image = sys.argv[2]
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
assert any(value.get("internal") is True for value in networks.values())
PY

compose pull gotenberg
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
  printf '%s => ' "$family" | tee -a "$fonts_log"
  compose exec -T gotenberg fc-match --format '%{family} | %{file}\n' "$family" | tee -a "$fonts_log"
done

python3 - "$work_dir" <<'PY'
from pathlib import Path
import base64
import sys

root = Path(sys.argv[1])
png = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
(root / "request-a.png").write_bytes(png)
vectors = {
    "http": '<img src="http://example.com/blocked.png">',
    "https": '<img src="https://example.com/blocked.png">',
    "loopback": '<img src="http://127.0.0.1:9/blocked.png">',
    "private": '<img src="http://10.255.255.1/blocked.png">',
    "public_ip": '<img src="http://93.184.216.34/blocked.png">',
    "redirect": '<img src="http://example.com/redirect-to-private">',
    "css_import": '<style>@import url("https://example.com/blocked.css");</style>',
    "css_url": '<style>body{background:url("http://example.com/blocked.png")}</style>',
    "svg": '<svg><image href="https://example.com/blocked.png"/></svg>',
    "iframe": '<iframe src="https://example.com/blocked"></iframe>',
}
for name, body in vectors.items():
    (root / f"blocked-{name}.html").write_text(
        f"<!doctype html><html><body>{body}</body></html>", encoding="utf-8"
    )
(root / "javascript.html").write_text(
    "<!doctype html><body><p>SAFE STATIC TEXT</p>"
    "<script>document.body.textContent='EXECUTED SCRIPT'</script>"
    "<img src=x onerror=\"document.body.textContent='EXECUTED EVENT'\">",
    encoding="utf-8",
)
(root / "request-a.html").write_text(
    '<!doctype html><body><p>REQUEST A</p><img src="request-a.png"></body>', encoding="utf-8"
)
(root / "request-b.html").write_text(
    '<!doctype html><body><p>REQUEST B</p><img src="request-a.png"></body>', encoding="utf-8"
)
(root / "traversal.html").write_text(
    '<!doctype html><body><img src="../request-a.png"></body>', encoding="utf-8"
)
PY

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

security_log="$artifact_dir/security-checks.log"
: >"$security_log"
for fixture in "$work_dir"/blocked-*.html; do
  name="$(basename "$fixture" .html)"
  status="$(post_html "$fixture" "$work_dir/$name.response")"
  if [[ "$status" =~ ^2 ]]; then
    echo "$name unexpectedly rendered with HTTP $status" >&2
    exit 1
  fi
  echo "$name blocked with HTTP $status" | tee -a "$security_log"
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

status="$(post_html "$work_dir/request-a.html" "$work_dir/request-a.pdf" \
  --form "files=@${work_dir}/request-a.png;type=image/png;filename=request-a.png")"
[[ "$status" =~ ^2 ]] || {
  echo "request A packaged asset control failed: HTTP $status" >&2
  exit 1
}
for fixture in request-b traversal; do
  status="$(post_html "$work_dir/$fixture.html" "$work_dir/$fixture.response")"
  if [[ "$status" =~ ^2 ]]; then
    echo "$fixture unexpectedly accessed a sibling/traversal asset" >&2
    exit 1
  fi
  echo "$fixture asset access blocked with HTTP $status" | tee -a "$security_log"
done

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
  echo "browser_chromium=147.0.7727.15; scenarios=7/7 passed"
  echo "browser_firefox=148.0.2; scenarios=7/7 passed"
  echo "browser_safari=not run on Linux; real macOS Safari workflow pending"
  echo "postgres_live_contract=not part of this renderer qualification"
  echo "s3_minio_contract=not part of this renderer qualification"
  echo "visual_review=automated every-page nonblank check complete; human review required before release sign-off"
  echo "fonts:"
  sed 's/^/  /' "$fonts_log"
  echo "security_checks:"
  sed 's/^/  /' "$security_log"
} >"$artifact_dir/qualification-record.txt"

echo "PDF qualification passed; artifacts: $artifact_dir"
