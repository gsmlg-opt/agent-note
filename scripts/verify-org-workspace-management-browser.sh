#!/usr/bin/env bash
set -euo pipefail

# Black-box create/edit/archive acceptance for the Org workspace Web UI.
# The target must be disposable because this script intentionally leaves one
# uniquely named archived workspace as audit evidence.

readonly BASE_URL="${ORG_CONSOLE_BASE_URL:?set ORG_CONSOLE_BASE_URL}"
readonly DEVTOOLS="${CHROME_DEVTOOLS_BIN:-chrome-devtools}"
readonly WAIT_MS="${ORG_CONSOLE_WAIT_MS:-20000}"
readonly SLUG="org-ui-$(date +%s)-$$"
readonly DISPLAY_NAME="Org UI browser fixture"

fail() {
    printf 'Org workspace management browser gate: FAIL: %s\n' "$*" >&2
    exit 1
}

report() {
    printf 'Org workspace management browser gate: %s\n' "$*"
}

command -v "$DEVTOOLS" >/dev/null 2>&1 || fail "chrome-devtools CLI is unavailable"
command -v curl >/dev/null 2>&1 || fail "curl is unavailable"
command -v jq >/dev/null 2>&1 || fail "jq is unavailable"
[[ -r /proc/sys/kernel/random/uuid ]] || fail "kernel UUID source is unavailable"

base="${BASE_URL%/}"
[[ "$base" =~ ^https?:// ]] || fail "ORG_CONSOLE_BASE_URL must be an http(s) URL"

devtools_json() {
    "$DEVTOOLS" "$@" --output-format=json
}

eval_json() {
    devtools_json evaluate_script "$1"
}

assert_eval() {
    local script="$1"
    local description="$2"
    local output
    output="$(eval_json "$script")"
    jq -e 'if type == "object" then (.message // "") | contains("ORG_OK") else false end' \
        <<<"$output" >/dev/null || {
        printf '%s\n' "$output" >&2
        fail "$description"
    }
}

wait_for() {
    local condition="$1"
    local description="$2"
    assert_eval "() => new Promise((resolve, reject) => {
        const deadline = Date.now() + $WAIT_MS;
        const check = () => {
            if ($condition) return resolve('ORG_OK');
            if (Date.now() >= deadline) return reject(new Error('timed out'));
            setTimeout(check, 100);
        };
        check();
    })" "$description"
}

navigate() {
    local url="$1"
    local output
    output="$(devtools_json navigate_page --type=url --url="$url" --timeout="$WAIT_MS")"
    jq -e '.message | contains("Successfully navigated")' <<<"$output" >/dev/null || {
        printf '%s\n' "$output" >&2
        fail "could not navigate to $url"
    }
}

fill_input() {
    local selector="$1"
    local value="$2"
    local selector_json value_json
    selector_json="$(jq -n --arg value "$selector" '$value')"
    value_json="$(jq -n --arg value "$value" '$value')"
    assert_eval "() => {
        const input = document.querySelector($selector_json);
        if (!input) return 'ORG_FAIL:missing input';
        input.value = $value_json;
        input.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'insertText'}));
        return 'ORG_OK';
    }" "could not fill $selector"
}

click_selector() {
    local selector="$1"
    local selector_json
    selector_json="$(jq -n --arg value "$selector" '$value')"
    assert_eval "() => {
        const element = document.querySelector($selector_json);
        if (!element) return 'ORG_FAIL:missing control';
        (element.closest('a,button') || element).click();
        return 'ORG_OK';
    }" "could not click $selector"
}

assert_console_clean() {
    local console
    console="$(devtools_json list_console_messages)"
    jq -e '[(.consoleMessages // [])[] | select(.type == "error")] | length == 0' \
        <<<"$console" >/dev/null || {
        printf '%s\n' "$console" >&2
        fail "browser console contains errors"
    }
}

assert_network_boundary() {
    local network="$1"
    # Preserve workspace create/update/archive while admitting only the approved document
    # create/rename/archive/restore lifecycle exception; raw source and workflow stay forbidden.
    jq -e --arg base "$base" '
        [(.networkRequests // [])[]
            | select(.url | contains("/api/org"))
            | select(((.method == "GET")
                or (.method == "POST" and .url == ($base + "/api/org/workspaces"))
                or (.method == "PATCH" and (.url | test("/api/org/workspaces/[^/?]+$")))
                or (.method == "POST" and (.url | test("/api/org/workspaces/[^/?]+/archive$")))
                or (.method == "POST" and (.url | test("/api/org/workspaces/[^/?]+/documents$")))
                or (.method == "PATCH" and (.url | test("/api/org/documents/[^/?]+/path$")))
                or (.method == "POST" and (.url | test("/api/org/documents/[^/?]+/(archive|restore)$"))))
                | not)
        ] | length == 0
    ' <<<"$network" >/dev/null || {
        printf '%s\n' "$network" >&2
        fail "browser issued an Org request outside approved workspace/document lifecycle traffic"
    }
    jq -e '[(.networkRequests // [])[] | select(.url | contains("/mcp"))] | length == 0' \
        <<<"$network" >/dev/null || fail "browser contacted /mcp"
}

current_path() {
    eval_json "() => location.pathname" |
        jq -r '.message | capture("```json\\n(?<value>.*)\\n```").value | fromjson'
}

external_revision_bump() {
    local workspace_id="$1"
    local workspace_file body_file status operation_id
    workspace_file="$(mktemp)"
    body_file="$(mktemp)"
    operation_id="$(tr -d '\n' </proc/sys/kernel/random/uuid)"
    curl --fail --silent --show-error "$base/api/org/workspaces/$workspace_id" >"$workspace_file"
    jq --arg operation_id "$operation_id" '
        {
            schema_version: 1,
            actor_id: "browser-fixture",
            operation_id: $operation_id,
            expected_revision: .revision,
            slug: .slug,
            display_name: .display_name,
            description: "Concurrent browser fixture update",
            timezone: .timezone,
            policy_schema_version: .policy_schema_version,
            policy: .policy
        }
    ' "$workspace_file" >"$body_file"
    status="$(curl --silent --show-error --output /tmp/org-workspace-browser-conflict.json \
        --write-out '%{http_code}' -X PATCH -H 'content-type: application/json' \
        --data-binary "@$body_file" "$base/api/org/workspaces/$workspace_id")"
    rm -f "$workspace_file" "$body_file"
    [[ "$status" == "200" ]] || fail "could not create the stale-revision fixture (HTTP $status)"
}

curl --fail --silent --show-error "$base/api/org/workspaces?limit=1" >/dev/null ||
    fail "Org REST workspace endpoint is unavailable"
navigate "$base/org/new"
wait_for "document.querySelector('[data-testid=org-workspace-new-page]')" "create page did not load"
assert_eval "() => document.querySelectorAll('[data-testid^=org-workspace-form-]').length >= 10
    && document.querySelector('[data-testid=org-workspace-form]')
    ? 'ORG_OK' : 'ORG_FAIL:structured sections'" "complete structured form is missing"

fill_input "#workspace-slug" "$SLUG"
fill_input "#workspace-display-name" "$DISPLAY_NAME"
fill_input "#workspace-description" "Created by browser acceptance"
fill_input "#workspace-timezone" "Asia/Shanghai"
click_selector "[data-testid=org-workspace-submit]"
wait_for "location.pathname.match(/^\\/org\\/[0-9a-f-]+$/)" "create did not navigate to the workspace"
workspace_path="$(current_path)"
workspace_id="${workspace_path##*/}"
[[ "$workspace_id" =~ ^[0-9a-f-]{36}$ ]] || fail "created workspace ID is invalid: $workspace_id"
wait_for "document.querySelector('[data-testid=org-workspace-page]')" "created workspace page did not settle"
report "created workspace $workspace_id"

click_selector "[data-testid=org-workspace-edit]"
wait_for "document.querySelector('[data-testid=org-workspace-settings-page]')" "settings page did not load"
external_revision_bump "$workspace_id"
fill_input "#workspace-description" "This stale update must not win"
click_selector "[data-testid=org-workspace-submit]"
wait_for "document.querySelector('[data-testid=org-workspace-server-error] code')?.textContent === 'stale_revision'" \
    "stale revision did not render a structured conflict"
click_selector "[data-testid=org-workspace-server-error] .org-workspace-form-inline-actions button:last-child"
wait_for "document.querySelector('[data-testid=org-workspace-settings-page]')
    && document.querySelector('#workspace-description')?.value === 'Concurrent browser fixture update'" \
    "Reload latest did not refresh the workspace revision"

fill_input "#workspace-description" "Edited by browser acceptance"
fill_input "#workspace-concurrency-limit" "5"
click_selector "[data-testid=org-workspace-submit]"
wait_for "location.pathname === '/org/$workspace_id'
    && document.querySelector('[data-testid=org-workspace-page]')" "workspace edit did not save"

click_selector "[data-testid=org-workspace-archive-open]"
wait_for "document.querySelector('[data-testid=org-workspace-archive-dialog]')" "archive dialog did not open"
assert_eval "() => document.querySelector('[data-testid=org-workspace-archive-submit]').disabled
    ? 'ORG_OK' : 'ORG_FAIL:archive gate'" "archive was not gated by exact slug confirmation"
fill_input "[data-testid=org-workspace-archive-confirmation]" "$SLUG"
click_selector "[data-testid=org-workspace-archive-submit]"
wait_for "location.pathname === '/org' && location.search.includes('include_archived=true')" \
    "archive did not return to the archived directory"

navigate "$base/org/$workspace_id?view=ready&limit=50"
wait_for "document.querySelector('[data-testid=org-workspace-page]')
    && document.body.innerText.includes('Archived · read-only')" "archived workspace is not readable"
assert_eval "() => !document.querySelector('[data-testid=org-workspace-edit]')
    && !document.querySelector('[data-testid=org-workspace-archive-open]')
    ? 'ORG_OK' : 'ORG_FAIL:archived controls'" "archived workspace still exposes management actions"

"$DEVTOOLS" resize_page 1440 900 >/dev/null
"$DEVTOOLS" take_screenshot --fullPage=true --filePath=/tmp/org-workspace-management-desktop.png >/dev/null
"$DEVTOOLS" resize_page 390 844 >/dev/null
assert_eval "() => document.documentElement.scrollWidth <= document.documentElement.clientWidth
    ? 'ORG_OK' : 'ORG_FAIL:mobile overflow'" "workspace management page overflows at 390px"
"$DEVTOOLS" take_screenshot --fullPage=true --filePath=/tmp/org-workspace-management-mobile.png >/dev/null
"$DEVTOOLS" take_snapshot --verbose=true --filePath=/tmp/org-workspace-management-mobile.snapshot.txt >/dev/null

network="$(devtools_json list_network_requests --includePreservedRequests=true)"
assert_network_boundary "$network"
assert_console_clean
report "PASS: create, stale conflict, reload, structured edit, archive, and archived read-only state"
report "desktop/mobile screenshots and accessibility snapshot are in /tmp"
