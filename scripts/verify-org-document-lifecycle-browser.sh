#!/usr/bin/env bash
set -euo pipefail

# Destructive black-box acceptance for the Org document lifecycle UI. The target must be a
# disposable service: this script creates documents and intentionally leaves its fixture workspace
# archived as audit evidence.

readonly BASE_URL="${ORG_CONSOLE_BASE_URL:?set ORG_CONSOLE_BASE_URL}"
readonly DISPOSABLE="${ORG_CONSOLE_DISPOSABLE:-}"
readonly DEVTOOLS="${CHROME_DEVTOOLS_BIN:-chrome-devtools}"
readonly WAIT_MS="${ORG_CONSOLE_WAIT_MS:-30000}"
readonly SLUG="org-files-$(date +%s)-$$"
readonly DISPLAY_NAME="Org document lifecycle browser fixture"
readonly PRIMARY_INITIAL_PATH="projects/roadmap.org"
readonly PRIMARY_FIRST_RENAME="projects/roadmap-renamed.org"
readonly PRIMARY_CONCURRENT_PATH="projects/roadmap-concurrent.org"
readonly PRIMARY_STALE_DRAFT="projects/roadmap-stale-draft.org"
readonly PRIMARY_FINAL_PATH="projects/roadmap-final.org"
readonly PRIMARY_ARCHIVED_PATH="archive/roadmap.org"
readonly ARTIFACT_PREFIX="/tmp/org-document-lifecycle-$SLUG"
readonly LIGHTHOUSE_DIR="$ARTIFACT_PREFIX-lighthouse"

tmp_dir=""
workspace_id=""
primary_document_id=""
allowed_stale_console_conflicts=0

fail() {
    printf 'Org document lifecycle browser gate: FAIL: %s\n' "$*" >&2
    exit 1
}

report() {
    printf 'Org document lifecycle browser gate: %s\n' "$*"
}

cleanup() {
    if [[ -n "$tmp_dir" && -d "$tmp_dir" ]]; then
        rm -rf -- "$tmp_dir"
    fi
}
trap cleanup EXIT

for command in "$DEVTOOLS" curl jq uuidgen; do
    command -v "$command" >/dev/null 2>&1 || fail "$command is unavailable"
done

lowercase_ascii() {
    LC_ALL=C tr '[:upper:]' '[:lower:]'
}

base="${BASE_URL%/}"
[[ "$base" =~ ^https?:// ]] || fail "ORG_CONSOLE_BASE_URL must be an http(s) URL"
[[ "$DISPOSABLE" == "1" ]] || fail "set ORG_CONSOLE_DISPOSABLE=1 for an explicitly disposable target"
normalized_base="$(printf '%s' "$base" | lowercase_ascii)"
case "$normalized_base" in
    *agent-note.gsmlg.net*|*gsmlg.net*)
        fail "refusing the public production host"
        ;;
esac
if [[ ! "$normalized_base" =~ ^https?://(127\.0\.0\.1|localhost|0\.0\.0\.0|\[::1\])(:[0-9]+)?$ ]] \
    && [[ "${ORG_CONSOLE_ALLOW_NON_LOOPBACK_DISPOSABLE:-}" != "1" ]]; then
    fail "non-loopback targets require ORG_CONSOLE_ALLOW_NON_LOOPBACK_DISPOSABLE=1"
fi

tmp_dir="$(mktemp -d /tmp/org-document-lifecycle-browser.XXXXXX)"

new_uuid() {
    uuidgen | tr '[:upper:]' '[:lower:]'
}

js_string() {
    jq -n --arg value "$1" '$value'
}

devtools_json() {
    "$DEVTOOLS" "$@" --output-format=json
}

eval_json() {
    devtools_json evaluate_script "$1"
}

eval_value() {
    eval_json "$1" |
        jq -r '.message | capture("```json\\n(?<value>.*)\\n```").value | fromjson'
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

open_isolated_page() {
    local url="$1"
    devtools_json new_page "$url" --isolatedContext="$SLUG" --timeout="$WAIT_MS" >/dev/null ||
        fail "could not open an isolated browser context for $url"
}

navigate_history() {
    local direction="$1"
    local output
    output="$(devtools_json navigate_page --type="$direction" --timeout="$WAIT_MS")"
    jq -e '.message | contains("Successfully navigated")' <<<"$output" >/dev/null || {
        printf '%s\n' "$output" >&2
        fail "browser $direction navigation failed"
    }
}

click_selector() {
    local selector_json
    selector_json="$(js_string "$1")"
    assert_eval "() => {
        const element = document.querySelector($selector_json);
        if (!element) return 'ORG_FAIL:missing control';
        (element.closest('a,button') || element).click();
        return 'ORG_OK';
    }" "could not click $1"
}

focus_and_click_selector() {
    local selector_json
    selector_json="$(js_string "$1")"
    assert_eval "() => {
        const element = document.querySelector($selector_json);
        if (!element) return 'ORG_FAIL:missing opener';
        element.focus();
        if (document.activeElement !== element) return 'ORG_FAIL:opener focus';
        element.click();
        return 'ORG_OK';
    }" "could not focus and click $1"
}

assert_focus_selector() {
    local selector_json
    selector_json="$(js_string "$1")"
    assert_eval "() => document.activeElement === document.querySelector($selector_json)
        ? 'ORG_OK' : 'ORG_FAIL:' + (document.activeElement?.outerHTML || 'no active element')" \
        "$2"
}

click_button_text() {
    local text_json scope_json
    text_json="$(js_string "$1")"
    scope_json="$(js_string "${2:-document}")"
    assert_eval "() => {
        const scopeName = $scope_json;
        const scope = scopeName === 'document' ? document : document.querySelector(scopeName);
        const button = scope && [...scope.querySelectorAll('button,a')]
            .find((candidate) => candidate.textContent.trim() === $text_json);
        if (!button) return 'ORG_FAIL:missing button';
        button.click();
        return 'ORG_OK';
    }" "could not click button named $1"
}

fill_input() {
    local selector_json value_json
    selector_json="$(js_string "$1")"
    value_json="$(js_string "$2")"
    assert_eval "() => {
        const input = document.querySelector($selector_json);
        if (!input) return 'ORG_FAIL:missing input';
        input.focus();
        input.value = $value_json;
        input.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'insertText'}));
        return input.value === $value_json ? 'ORG_OK' : 'ORG_FAIL:value';
    }" "could not fill $1"
}

change_select_value() {
    local selector_json value_json
    selector_json="$(js_string "$1")"
    value_json="$(js_string "$2")"
    assert_eval "() => {
        const select = document.querySelector($selector_json);
        if (!select) return 'ORG_FAIL:missing select';
        select.value = $value_json;
        select.dispatchEvent(new Event('change', {bubbles: true}));
        return 'ORG_OK';
    }" "could not change $1 to $2"
}

click_file_action() {
    local action_json path_json
    action_json="$(js_string "$1")"
    path_json="$(js_string "$2")"
    assert_eval "() => {
        const name = $action_json + ' ' + $path_json;
        const button = [...document.querySelectorAll('button[aria-label]')]
            .find((candidate) => candidate.getAttribute('aria-label') === name);
        if (!button) return 'ORG_FAIL:missing ' + name;
        button.click();
        return 'ORG_OK';
    }" "missing accessible $1 action for $2"
}

wait_files_settled() {
    wait_for "document.querySelector('[data-testid=org-workspace-files-page]')
        && document.querySelector('nav[aria-label=\"File status\"]')
        && document.querySelector('[role=status]')
        && !document.querySelector('.loading')
        && document.querySelector('[role=status]').textContent.trim() !== 'Loading files'" \
        "files route did not settle"
}

assert_path_visible() {
    local path_json
    path_json="$(js_string "$1")"
    assert_eval "() => [...document.querySelectorAll('.org-file-identity strong')]
        .some((element) => element.textContent.trim() === $path_json)
        ? 'ORG_OK' : 'ORG_FAIL:path'" "file path is not visible: $1"
}

assert_path_absent() {
    local path_json
    path_json="$(js_string "$1")"
    assert_eval "() => ![...document.querySelectorAll('.org-file-identity strong')]
        .some((element) => element.textContent.trim() === $path_json)
        ? 'ORG_OK' : 'ORG_FAIL:path'" "file path should be absent: $1"
}

assert_new_live_text() {
    local text_json
    text_json="$(js_string "$1")"
    wait_for "(window.__orgLifecycleAction?.values || []).some((value) => value === $text_json)" \
        "live region did not newly announce $1"
}

install_live_observer() {
    assert_eval "() => {
        const region = document.querySelector('[role=status]');
        if (!region) return 'ORG_FAIL:live region';
        window.__orgLifecycleAction = {sequence: 0, values: []};
        window.__orgLifecycleObserver?.disconnect();
        window.__orgLifecycleObserver = new MutationObserver(() => {
            const value = region.textContent.trim();
            if (value) window.__orgLifecycleAction?.values.push(value);
        });
        window.__orgLifecycleObserver.observe(region, {subtree: true, childList: true, characterData: true});
        return 'ORG_OK';
    }" "could not observe live-region announcements"
}

reset_live_observer() {
    assert_eval "() => {
        if (!window.__orgLifecycleObserver || !window.__orgLifecycleAction) {
            return 'ORG_FAIL:observer unavailable';
        }
        window.__orgLifecycleAction = {
            sequence: window.__orgLifecycleAction.sequence + 1,
            values: [],
        };
        return 'ORG_OK:' + window.__orgLifecycleAction.sequence;
    }" "could not reset the per-action live announcement log"
}

curl_json() {
    local method="$1"
    local url="$2"
    local body="${3:-}"
    if [[ -n "$body" ]]; then
        curl --fail --silent --show-error -X "$method" -H 'content-type: application/json' \
            --data-binary "$body" "$url"
    else
        curl --fail --silent --show-error -X "$method" "$url"
    fi
}

request_status() {
    local method="$1"
    local url="$2"
    local body="$3"
    local output="$4"
    curl --silent --show-error --output "$output" --write-out '%{http_code}' -X "$method" \
        -H 'content-type: application/json' --data-binary "$body" "$url"
}

mutation_envelope() {
    jq -n --arg operation_id "$(new_uuid)" '{
        schema_version: 1,
        actor_id: "browser-fixture",
        operation_id: $operation_id
    }'
}

create_workspace_fixture() {
    workspace_id="$(new_uuid)"
    local body
    body="$(jq -n \
        --arg workspace_id "$workspace_id" \
        --arg slug "$SLUG" \
        --arg display_name "$DISPLAY_NAME" \
        --arg operation_id "$(new_uuid)" '
        {
            schema_version: 1,
            actor_id: "browser-fixture",
            operation_id: $operation_id,
            workspace_id: $workspace_id,
            slug: $slug,
            display_name: $display_name,
            description: "Disposable browser acceptance fixture",
            timezone: "Asia/Shanghai",
            policy_schema_version: 1,
            policy: {
                allow_cross_workspace_agenda: false,
                allowed_types: ["project", "epic", "issue", "task", "subtask", "review", "approval", "incident", "milestone"],
                states: ["BACKLOG", "READY", "RUNNING", "BLOCKED", "REVIEW", "DONE", "FAILED", "CANCELLED"],
                transitions: [
                    ["BACKLOG", "READY"], ["BACKLOG", "CANCELLED"],
                    ["READY", "RUNNING"], ["READY", "CANCELLED"],
                    ["RUNNING", "BLOCKED"], ["RUNNING", "READY"], ["RUNNING", "REVIEW"],
                    ["RUNNING", "DONE"], ["RUNNING", "FAILED"], ["RUNNING", "CANCELLED"],
                    ["BLOCKED", "READY"], ["BLOCKED", "CANCELLED"],
                    ["REVIEW", "DONE"], ["REVIEW", "READY"], ["REVIEW", "CANCELLED"],
                    ["FAILED", "READY"], ["FAILED", "RUNNING"], ["FAILED", "CANCELLED"]
                ],
                initial_state: "BACKLOG",
                running_state: "RUNNING",
                executable_states: ["READY"],
                review_state: "REVIEW",
                failed_state: "FAILED",
                cancelled_state: "CANCELLED",
                successful_terminal_states: ["DONE"],
                terminal_states: ["DONE", "CANCELLED"],
                release_state: "READY",
                review_rejection_state: "READY",
                lease_expiry_recovery_state: "READY",
                review_required_types: [],
                claim_policy: "assignment_restricted",
                lease_duration_secs: 900,
                retry_limit: 2,
                concurrency_limit: 4,
                tag_rules: {}
            }
        }')"
    curl_json POST "$base/api/org/workspaces" "$body" >"$tmp_dir/workspace-create.json"
    jq -e --arg id "$workspace_id" '.workspace_id == $id and .workspace_revision == 1' \
        "$tmp_dir/workspace-create.json" >/dev/null || fail "workspace fixture response is invalid"
}

get_document() {
    curl_json GET "$base/api/org/documents/$1?workspace_id=$workspace_id"
}

list_documents() {
    curl_json GET "$base/api/org/workspaces/$workspace_id/documents?$1"
}

rename_document_rest() {
    local document_id="$1"
    local new_path="$2"
    local expected_revision="$3"
    local body
    body="$(jq -n \
        --arg operation_id "$(new_uuid)" \
        --arg workspace_id "$workspace_id" \
        --arg new_path "$new_path" \
        --argjson expected_revision "$expected_revision" '{
            schema_version: 1,
            actor_id: "browser-fixture",
            operation_id: $operation_id,
            workspace_id: $workspace_id,
            new_path: $new_path,
            expected_revision: $expected_revision
        }')"
    curl_json PATCH "$base/api/org/documents/$document_id/path" "$body"
}

create_document_rest() {
    local document_id="$1"
    local path="$2"
    local body
    body="$(jq -n \
        --arg operation_id "$(new_uuid)" \
        --arg document_id "$document_id" \
        --arg path "$path" '{
            schema_version: 1,
            actor_id: "browser-fixture",
            operation_id: $operation_id,
            document_id: $document_id,
            path: $path
        }')"
    curl_json POST "$base/api/org/workspaces/$workspace_id/documents" "$body"
}

archive_document_rest() {
    local document_id="$1"
    local expected_revision="$2"
    local body
    body="$(jq -n \
        --arg operation_id "$(new_uuid)" \
        --arg workspace_id "$workspace_id" \
        --argjson expected_revision "$expected_revision" '{
            schema_version: 1,
            actor_id: "browser-fixture",
            operation_id: $operation_id,
            workspace_id: $workspace_id,
            expected_revision: $expected_revision
        }')"
    curl_json POST "$base/api/org/documents/$document_id/archive" "$body"
}

archive_workspace_fixture() {
    local workspace revision body
    workspace="$(curl_json GET "$base/api/org/workspaces/$workspace_id")"
    revision="$(jq -r '.revision' <<<"$workspace")"
    body="$(jq -n \
        --arg operation_id "$(new_uuid)" \
        --argjson expected_revision "$revision" '{
            schema_version: 1,
            actor_id: "browser-fixture",
            operation_id: $operation_id,
            expected_revision: $expected_revision
        }')"
    curl_json POST "$base/api/org/workspaces/$workspace_id/archive" "$body" \
        >"$tmp_dir/workspace-archive.json"
}

assert_dialog_accessible() {
    assert_eval "() => {
        const dialog = document.querySelector('[role=dialog][aria-modal=true]');
        if (!dialog || !dialog.contains(document.activeElement)) return 'ORG_FAIL:focus';
        const title = document.getElementById(dialog.getAttribute('aria-labelledby'));
        const orphaned = [...dialog.querySelectorAll('input,select,textarea')].filter((input) =>
            !(input.labels?.length || input.getAttribute('aria-label') || input.getAttribute('aria-labelledby'))
        );
        return title && orphaned.length === 0 ? 'ORG_OK' : 'ORG_FAIL:labels';
    }" "dialog focus or input labelling is invalid"
}

assert_dialog_focus_position() {
    local expected="$1"
    local expected_json
    expected_json="$(js_string "$expected")"
    assert_eval "() => {
        const dialog = document.querySelector('[role=dialog][aria-modal=true]');
        if (!dialog) return 'ORG_FAIL:no dialog';
        const focusable = [...dialog.querySelectorAll(
            'a[href]:not([aria-disabled=true]):not([hidden]),'
            + 'button:not([disabled]):not([hidden]),'
            + 'input:not([disabled]):not([type=hidden]):not([hidden]),'
            + 'select:not([disabled]):not([hidden]),'
            + 'textarea:not([disabled]):not([hidden]),'
            + '[tabindex]:not([tabindex=\"-1\"]):not([hidden])'
        )].filter((element) => {
            const style = getComputedStyle(element);
            const rect = element.getBoundingClientRect();
            return style.visibility !== 'hidden' && style.display !== 'none'
                && (rect.width > 0 || rect.height > 0);
        });
        const expected = $expected_json;
        const matches = expected === 'inside'
            ? dialog.contains(document.activeElement)
            : expected === 'first'
                ? document.activeElement === focusable[0]
                : document.activeElement === focusable[focusable.length - 1];
        return matches ? 'ORG_OK' : 'ORG_FAIL:' + JSON.stringify({
            expected,
            active: document.activeElement?.outerHTML,
            first: focusable[0]?.outerHTML,
            last: focusable[focusable.length - 1]?.outerHTML,
        });
    }" "dialog focus is not at the expected $expected position"
}

exercise_dialog_focus_trap() {
    assert_dialog_accessible
    devtools_json press_key Shift+Tab >/dev/null
    assert_dialog_focus_position last
    devtools_json press_key Tab >/dev/null
    assert_dialog_focus_position first
    for key in Tab Tab Shift+Tab Shift+Tab Tab; do
        devtools_json press_key "$key" >/dev/null
        assert_dialog_focus_position inside
    done
}

assert_interactive_targets() {
    assert_eval "() => {
        const controls = [...document.querySelectorAll(
            '.org-files-page button, .org-files-page input, .org-files-page select, .org-files-status-filters a'
        )].filter((element) => {
            const style = getComputedStyle(element);
            const rect = element.getBoundingClientRect();
            return style.display !== 'none' && style.visibility !== 'hidden' && rect.width > 0 && rect.height > 0;
        });
        const undersized = controls.map((element) => {
            const rect = element.getBoundingClientRect();
            return {name: element.getAttribute('aria-label') || element.textContent.trim(), width: rect.width, height: rect.height};
        }).filter((target) => target.width < 44 || target.height < 44);
        return undersized.length === 0 ? 'ORG_OK' : 'ORG_FAIL:' + JSON.stringify(undersized);
    }" "one or more lifecycle interactive targets are smaller than 44px"
}

assert_global_accessibility() {
    assert_eval "() => {
        const viewport = document.querySelector('meta[name=viewport]')?.content || '';
        const h1 = document.querySelectorAll('main h1').length;
        const h2 = document.querySelectorAll('main h2').length;
        const table = document.querySelector('.org-files-ledger');
        return document.documentElement.lang === 'en'
            && document.title === 'Org files $workspace_id | agent-note'
            && viewport.includes('width=device-width')
            && h1 === 1 && h2 === 0
            && table?.querySelector('caption')
            && table?.querySelectorAll('thead th').length === 4
            ? 'ORG_OK' : 'ORG_FAIL:global semantics';
    }" "global metadata, heading hierarchy, or semantic table is invalid"
}

assert_console_and_issues_clean() {
    local phase="${1:?phase is required}"
    local console issues
    console="$(devtools_json list_console_messages --includePreservedMessages=true)"
    issues="$(devtools_json list_console_messages --types=issue --includePreservedMessages=true)"
    jq -cn --arg phase "$phase" --argjson console "$console" --argjson issues "$issues" '{
        phase: $phase,
        console_messages: ($console.consoleMessages // []),
        browser_issues: ($issues.consoleMessages // [])
    }' >>"$ARTIFACT_PREFIX-console.jsonl"
    jq -e --argjson allowed_stale "$allowed_stale_console_conflicts" '
        [(.consoleMessages // [])[] | select(.type == "error")] as $errors
        | ($errors | length) <= $allowed_stale
        and all($errors[];
            .text == "Failed to load resource: the server responded with a status of 409 (Conflict)")
    ' <<<"$console" >/dev/null || {
        printf '%s\n' "$console" >&2
        fail "browser console contains an unexpected error after $phase"
    }
    jq -e '[(.consoleMessages // [])[]] | length == 0' <<<"$issues" >/dev/null || {
        printf '%s\n' "$issues" >&2
        fail "Chrome reported browser issues after $phase"
    }
}

assert_network_boundary() {
    local network="$1"
    jq -e --arg base "$base" '
        [(.networkRequests // [])[]
            | select((.url | contains("/api/org")) or (.url | startswith($base + "/api/")))
            | select(((
                (.url == ($base + "/api/org")
                    or (.url | startswith($base + "/api/org/"))
                    or (.url | startswith($base + "/api/org?")))
                and (
                    .method == "GET"
                    or (.method == "POST" and .url == ($base + "/api/org/workspaces"))
                    or (.method == "PATCH" and (.url | test("/api/org/workspaces/[^/?]+$")))
                    or (.method == "POST" and (.url | test("/api/org/workspaces/[^/?]+/archive$")))
                    or (.method == "POST" and (.url | test("/api/org/workspaces/[^/?]+/documents$")))
                    or (.method == "PATCH" and (.url | test("/api/org/documents/[^/?]+/path$")))
                    or (.method == "POST" and (.url | test("/api/org/documents/[^/?]+/(archive|restore)$")))
                )
            )) | not)
        ] | length == 0
    ' <<<"$network" >/dev/null || {
        printf '%s\n' "$network" >&2
        fail "browser issued an Org request outside approved workspace/document lifecycle traffic"
    }
    jq -e '
        [(.networkRequests // [])[]
            | select(.method == "PUT" or .method == "DELETE"
                or (.url | contains("/mcp"))
                or (.url | test("/api/org/(items|queue|agenda|notes)(/|\\?|$)"))
                or (.url | test("/(claim|review|progress|result|transition|dependencies|note-links|import|export)(/|\\?|$)"))
                or (.url | test("/(auth|login|session)(/|\\?|$)")))
        ] | length == 0
    ' <<<"$network" >/dev/null || {
        printf '%s\n' "$network" >&2
        fail "browser contacted source/workflow/auth/MCP traffic outside the lifecycle boundary"
    }
}

capture_network_phase() {
    local phase="$1"
    local network
    network="$(devtools_json list_network_requests --includePreservedRequests=true)"
    assert_network_boundary "$network"
    jq -c --arg phase "$phase" '{phase: $phase, requests: [(.networkRequests // [])[] | {method, url, status}]}' \
        <<<"$network" >>"$ARTIFACT_PREFIX-network.jsonl"
}

capture_browser_phase() {
    local phase="$1"
    capture_network_phase "$phase"
    assert_console_and_issues_clean "$phase"
}

assert_pagination_page() {
    local expected_limit="$1"
    local expected_rows="$2"
    local expect_cursor="$3"
    local expect_previous_disabled="$4"
    local expect_next_disabled="$5"
    assert_eval "() => {
        const query = new URLSearchParams(location.search);
        const select = document.querySelector('#org-files-page-size');
        const selected = select?.selectedOptions?.[0];
        const rows = document.querySelectorAll('.org-files-ledger tbody .org-file-row').length;
        const buttons = [...document.querySelectorAll('nav[aria-label=\"File pages\"] button')];
        const previous = buttons.find((button) => button.textContent.trim() === 'Previous page');
        const next = buttons.find((button) => button.textContent.trim() === 'Next page');
        const cursorMatches = $expect_cursor
            ? Boolean(query.get('cursor'))
            : !query.has('cursor');
        return query.get('limit') === '$expected_limit'
            && select?.value === '$expected_limit'
            && selected?.value === '$expected_limit'
            && selected?.selected === true
            && rows === $expected_rows
            && cursorMatches
            && previous?.disabled === $expect_previous_disabled
            && next?.disabled === $expect_next_disabled
            ? 'ORG_OK'
            : 'ORG_FAIL:' + JSON.stringify({
                urlLimit: query.get('limit'),
                selectValue: select?.value,
                selectedValue: selected?.value,
                rows,
                hasCursor: query.has('cursor'),
                previousDisabled: previous?.disabled,
                nextDisabled: next?.disabled,
            });
    }" "pagination URL, select, selected option, or row count disagrees"
}

assert_lighthouse_boundary() {
    local report_path="${1:-$ARTIFACT_PREFIX-lighthouse-failures.json}"
    jq -e '
        (.accessibility_score == 1 and (.failed_accessibility_audits | length) == 0)
        or
        (.accessibility_score >= 0.96
            and (.failed_accessibility_audits | length) == 1
            and .failed_accessibility_audits[0].id == "color-contrast"
            and (.failed_accessibility_audits[0].items | length) == 1
            and all(.failed_accessibility_audits[0].items[];
                ((.node.selector // "") | endswith("a.app-brand"))
                and ((.node.snippet // "") | contains("class=\"app-brand\""))))
    ' "$report_path" >/dev/null || {
        jq . "$report_path" >&2
        fail "Lighthouse found an accessibility failure outside the accepted global app-brand contrast"
    }
}

curl --fail --silent --show-error "$base/api/org/workspaces?limit=1" >/dev/null ||
    fail "Org REST workspace endpoint is unavailable"
create_workspace_fixture
report "created disposable workspace $workspace_id"

open_isolated_page "$base/org/$workspace_id/files?status=active&limit=10"
wait_files_settled
install_live_observer
assert_eval "() => location.pathname === '/org/$workspace_id/files'
    && document.querySelector('nav[aria-label=\"File status\"] a[aria-current=page]')?.textContent.trim() === 'Active'
    && document.querySelector('[data-testid=org-files-add]')
    ? 'ORG_OK' : 'ORG_FAIL:route'" "files route, page, or active filter is invalid"

# Modal focus, Escape, and Cancel are independently exercised before the real create.
focus_and_click_selector "[data-testid=org-files-add]"
wait_for "document.querySelector('[role=dialog]')" "add dialog did not open"
assert_dialog_accessible
devtools_json take_snapshot --verbose=true --filePath="$ARTIFACT_PREFIX-add-dialog.snapshot.txt" >/dev/null
exercise_dialog_focus_trap
devtools_json press_key Escape >/dev/null
wait_for "!document.querySelector('[role=dialog]')" "Escape did not dismiss the add dialog"
assert_focus_selector "[data-testid=org-files-add]" "Escape closed the dialog but focus restored to Add opener failed"
focus_and_click_selector "[data-testid=org-files-add]"
wait_for "document.querySelector('[role=dialog]')" "add dialog did not reopen"
exercise_dialog_focus_trap
click_button_text "Cancel" ".app-modal-panel"
wait_for "!document.querySelector('[role=dialog]')" "Cancel did not dismiss the add dialog"
assert_focus_selector "[data-testid=org-files-add]" "Cancel closed the dialog but focus restored to Add opener failed"
assert_console_and_issues_clean "initial-dialogs"

focus_and_click_selector "[data-testid=org-files-add]"
wait_for "document.querySelector('#org-files-path')" "add file input is missing"
fill_input "#org-files-path" "$PRIMARY_INITIAL_PATH"
reset_live_observer
click_button_text "Add file" ".app-modal-panel"
assert_new_live_text "Created"
wait_files_settled
assert_path_visible "$PRIMARY_INITIAL_PATH"
primary_document_id="$(list_documents 'status=active&limit=50' |
    jq -r --arg path "$PRIMARY_INITIAL_PATH" '.items[] | select(.path == $path) | .id')"
[[ "$primary_document_id" =~ ^[0-9a-f-]{36}$ ]] || fail "created file UUID is invalid"
primary_source="$(get_document "$primary_document_id")"
primary_hash="$(jq -r '.content_hash' <<<"$primary_source")"
jq -e --arg id "$primary_document_id" --arg path "$PRIMARY_INITIAL_PATH" '
    .id == $id and .path == $path and .source == "" and .revision == 1 and .archived_at == null
' <<<"$primary_source" >/dev/null || fail "empty create did not preserve UUID/source/revision contract"
capture_browser_phase "empty-create"

click_file_action Rename "$PRIMARY_INITIAL_PATH"
wait_for "document.querySelector('#org-files-path')" "rename dialog did not open"
exercise_dialog_focus_trap
fill_input "#org-files-path" "$PRIMARY_FIRST_RENAME"
reset_live_observer
click_button_text "Rename file" ".app-modal-panel"
assert_new_live_text "Renamed"
wait_files_settled
assert_path_visible "$PRIMARY_FIRST_RENAME"
capture_browser_phase "initial-rename"

# Produce a real stale browser draft: the dialog keeps revision 2 while REST commits revision 3.
click_file_action Rename "$PRIMARY_FIRST_RENAME"
wait_for "document.querySelector('#org-files-path')" "stale rename dialog did not open"
rename_document_rest "$primary_document_id" "$PRIMARY_CONCURRENT_PATH" 2 \
    >"$tmp_dir/concurrent-rename.json"
jq -e --arg id "$primary_document_id" '.document_revisions[$id] == 3' \
    "$tmp_dir/concurrent-rename.json" >/dev/null || fail "external concurrent rename did not reach revision 3"
fill_input "#org-files-path" "$PRIMARY_STALE_DRAFT"
reset_live_observer
click_button_text "Rename file" ".app-modal-panel"
wait_for "document.querySelector('.org-files-error code')?.textContent.trim() === 'stale_revision'" \
    "stale revision did not render"
assert_eval "() => document.querySelector('#org-files-path')?.value === $(js_string "$PRIMARY_STALE_DRAFT")
    && ![...document.querySelectorAll('.app-modal-panel button')]
        .some((button) => button.textContent.trim() === 'Retry')
    && [...document.querySelectorAll('.app-modal-panel button')]
        .some((button) => button.textContent.trim() === 'Refresh files')
    && [...document.querySelectorAll('.app-modal-panel button')]
        .find((button) => button.textContent.trim() === 'Rename file')?.disabled
    ? 'ORG_OK' : 'ORG_FAIL:stale draft/retry gate'" \
    "stale conflict did not preserve the draft and require Refresh files"
assert_eval "() => !(window.__orgLifecycleAction?.values || []).some((value) => value === 'Renamed')
    ? 'ORG_OK' : 'ORG_FAIL:stale success announcement'" \
    "stale rename announced a false success"
allowed_stale_console_conflicts=1
capture_browser_phase "stale-rename"
click_button_text "Refresh files" ".app-modal-panel"
wait_for "document.querySelector('#org-files-path')
    && !document.querySelector('.org-files-error')
    && ![...document.querySelectorAll('.app-modal-panel button')]
        .find((button) => button.textContent.trim() === 'Rename file')?.disabled" \
    "Refresh files did not load the latest revision and re-enable deliberate submit"
capture_browser_phase "stale-refresh"
fill_input "#org-files-path" "$PRIMARY_FINAL_PATH"
reset_live_observer
click_button_text "Rename file" ".app-modal-panel"
assert_new_live_text "Renamed"
wait_files_settled
assert_path_visible "$PRIMARY_FINAL_PATH"
primary_source="$(get_document "$primary_document_id")"
jq -e --arg id "$primary_document_id" --arg path "$PRIMARY_FINAL_PATH" --arg hash "$primary_hash" '
    .id == $id and .path == $path and .source == "" and .content_hash == $hash
    and .revision == 4 and .archived_at == null
' <<<"$primary_source" >/dev/null || fail "deliberate rename did not preserve source/projection identity"
capture_browser_phase "refreshed-rename"

click_file_action Archive "$PRIMARY_FINAL_PATH"
wait_for "document.querySelector('#org-files-archive-confirmation')" "archive dialog did not open"
exercise_dialog_focus_trap
assert_eval "() => [...document.querySelectorAll('.app-modal-panel button')]
    .find((button) => button.textContent.trim() === 'Archive file')?.disabled
    ? 'ORG_OK' : 'ORG_FAIL:confirmation gate'" "archive did not require exact-path confirmation"
fill_input "#org-files-archive-confirmation" "$PRIMARY_FINAL_PATH"
reset_live_observer
click_button_text "Archive file" ".app-modal-panel"
assert_new_live_text "Archived"
wait_files_settled
assert_path_absent "$PRIMARY_FINAL_PATH"
capture_browser_phase "archive"
click_button_text "Archived" "nav[aria-label=\"File status\"]"
wait_files_settled
assert_path_visible "$PRIMARY_FINAL_PATH"
primary_source="$(get_document "$primary_document_id")"
jq -e --arg id "$primary_document_id" --arg hash "$primary_hash" '
    .id == $id and .source == "" and .content_hash == $hash and .revision == 5 and .archived_at != null
' <<<"$primary_source" >/dev/null || fail "archived read did not preserve UUID/source/projection identity"
capture_browser_phase "archived-filter"

# Archived files may be renamed. The new path remains reserved; the old path becomes reusable.
click_file_action Rename "$PRIMARY_FINAL_PATH"
wait_for "document.querySelector('#org-files-path')" "archived rename dialog did not open"
fill_input "#org-files-path" "$PRIMARY_ARCHIVED_PATH"
reset_live_observer
click_button_text "Rename file" ".app-modal-panel"
assert_new_live_text "Renamed"
wait_files_settled
assert_path_visible "$PRIMARY_ARCHIVED_PATH"
reserved_id="$(new_uuid)"
reserved_body="$(jq -n \
    --arg operation_id "$(new_uuid)" \
    --arg document_id "$reserved_id" \
    --arg path "$PRIMARY_ARCHIVED_PATH" '{
        schema_version: 1, actor_id: "browser-fixture", operation_id: $operation_id,
        document_id: $document_id, path: $path
    }')"
reserved_status="$(request_status POST "$base/api/org/workspaces/$workspace_id/documents" \
    "$reserved_body" "$tmp_dir/reserved-conflict.json")"
[[ "$reserved_status" == "409" ]] || fail "archived path was not reserved (HTTP $reserved_status)"
jq -e '.code == "document_path_conflict"' "$tmp_dir/reserved-conflict.json" >/dev/null ||
    fail "archived path reservation returned the wrong error"
reuse_id="$(new_uuid)"
create_document_rest "$reuse_id" "$PRIMARY_FINAL_PATH" >"$tmp_dir/reused-old-path.json"
jq -e --arg id "$reuse_id" '.document_revisions[$id] == 1' "$tmp_dir/reused-old-path.json" >/dev/null ||
    fail "renaming an archived file did not release its old path for reuse"
archive_document_rest "$reuse_id" 1 >"$tmp_dir/reused-old-path-archive.json"
capture_browser_phase "rename-while-archived"

click_file_action Restore "$PRIMARY_ARCHIVED_PATH"
wait_for "document.querySelector('[role=dialog]')" "restore dialog did not open"
assert_dialog_accessible
reset_live_observer
click_button_text "Restore file" ".app-modal-panel"
assert_new_live_text "Restored"
wait_files_settled
capture_browser_phase "restore"
click_button_text "Active" "nav[aria-label=\"File status\"]"
wait_files_settled
assert_path_visible "$PRIMARY_ARCHIVED_PATH"
primary_source="$(get_document "$primary_document_id")"
jq -e --arg id "$primary_document_id" --arg path "$PRIMARY_ARCHIVED_PATH" --arg hash "$primary_hash" '
    .id == $id and .path == $path and .source == "" and .content_hash == $hash
    and .revision == 7 and .archived_at == null
' <<<"$primary_source" >/dev/null || fail "restore did not preserve UUID/source/projection identity"
capture_browser_phase "active-filter-after-restore"

# Seed three active cursor pages in the disposable workspace.
for index in $(seq -w 1 25); do
    create_document_rest "$(new_uuid)" "seed/$index.org" >/dev/null
done
navigate "$base/org/$workspace_id/files?status=active&limit=10"
wait_files_settled
page_one="$(eval_value '() => location.href')"
assert_pagination_page 10 10 false true false
assert_eval "() => [...document.querySelectorAll('nav[aria-label=\"File pages\"] button')]
    .find((button) => button.textContent.trim() === 'Previous page')?.disabled
    ? 'ORG_OK' : 'ORG_FAIL:page one previous'" "Previous page must be disabled on page one"
capture_browser_phase "history-page-one"
click_button_text "Next page" "nav[aria-label=\"File pages\"]"
wait_files_settled
page_two="$(eval_value '() => location.href')"
[[ "$page_two" != "$page_one" ]] || fail "Next page did not change the cursor URL"
assert_pagination_page 10 10 true false false
capture_browser_phase "history-page-two"
click_button_text "Next page" "nav[aria-label=\"File pages\"]"
wait_files_settled
page_three="$(eval_value '() => location.href')"
[[ "$page_three" != "$page_two" ]] || fail "third cursor page did not change the URL"
assert_pagination_page 10 6 true false true
capture_browser_phase "history-page-three"
navigate_history back
wait_files_settled
[[ "$(eval_value '() => location.href')" == "$page_two" ]] || fail "browser Back did not restore page two"
assert_pagination_page 10 10 true false false
capture_browser_phase "history-back-page-two"
navigate_history back
wait_files_settled
[[ "$(eval_value '() => location.href')" == "$page_one" ]] || fail "browser Back did not restore page one"
assert_pagination_page 10 10 false true false
assert_eval "() => [...document.querySelectorAll('nav[aria-label=\"File pages\"] button')]
    .find((button) => button.textContent.trim() === 'Previous page')?.disabled
    ? 'ORG_OK' : 'ORG_FAIL:round-trip page one previous'" \
    "Previous page lost its disabled state after browser Back"
capture_browser_phase "history-back-page-one"
navigate_history forward
wait_files_settled
[[ "$(eval_value '() => location.href')" == "$page_two" ]] || fail "browser Forward did not restore page two"
assert_pagination_page 10 10 true false false
capture_browser_phase "history-forward-page-two"
navigate_history forward
wait_files_settled
[[ "$(eval_value '() => location.href')" == "$page_three" ]] || fail "browser Forward did not restore page three"
assert_pagination_page 10 6 true false true
assert_eval "() => ![...document.querySelectorAll('nav[aria-label=\"File pages\"] button')]
    .find((button) => button.textContent.trim() === 'Previous page')?.disabled
    ? 'ORG_OK' : 'ORG_FAIL:round-trip page three previous'" \
    "Previous page was not enabled after the three-page Back/Forward round trip"
capture_browser_phase "history-forward-page-three"
click_button_text "Previous page" "nav[aria-label=\"File pages\"]"
wait_files_settled
[[ "$(eval_value '() => location.href')" == "$page_two" ]] || fail "Previous page target was not restored after Back/Forward"
assert_pagination_page 10 10 true false false
capture_browser_phase "history-previous-page-two"

# Exercise the page-size control itself after the three-page limit-10 round trip.
navigate "$page_one"
wait_files_settled
assert_pagination_page 10 10 false true false
change_select_value "#org-files-page-size" 25
wait_files_settled
limit_25_page_one="$(eval_value '() => location.href')"
assert_pagination_page 25 25 false true false
capture_browser_phase "limit-25-page-one"
click_button_text "Next page" "nav[aria-label=\"File pages\"]"
wait_files_settled
limit_25_page_two="$(eval_value '() => location.href')"
[[ "$limit_25_page_two" != "$limit_25_page_one" ]] || fail "limit-25 Next did not change the cursor URL"
assert_pagination_page 25 1 true false true
capture_browser_phase "limit-25-page-two"
navigate_history back
wait_files_settled
[[ "$(eval_value '() => location.href')" == "$limit_25_page_one" ]] || fail "limit-25 Back did not restore page one"
assert_pagination_page 25 25 false true false
capture_browser_phase "limit-25-back-page-one"
navigate_history forward
wait_files_settled
[[ "$(eval_value '() => location.href')" == "$limit_25_page_two" ]] || fail "limit-25 Forward did not restore page two"
assert_pagination_page 25 1 true false true
capture_browser_phase "limit-25-forward-page-two"

navigate "$base/org/$workspace_id/files?status=active&limit=10"
wait_files_settled
assert_pagination_page 10 10 false true false
capture_browser_phase "accessibility-page"
assert_global_accessibility
assert_interactive_targets
devtools_json resize_page 1440 900 >/dev/null
assert_eval "() => document.documentElement.scrollWidth <= document.documentElement.clientWidth
    ? 'ORG_OK' : 'ORG_FAIL:desktop overflow'" "files page overflows at 1440px"
devtools_json take_screenshot --fullPage=true --filePath="$ARTIFACT_PREFIX-desktop.png" >/dev/null
devtools_json take_snapshot --verbose=true --filePath="$ARTIFACT_PREFIX-desktop.snapshot.txt" >/dev/null
assert_console_and_issues_clean "desktop-responsive"
devtools_json resize_page 390 844 >/dev/null
assert_eval "() => document.documentElement.scrollWidth <= document.documentElement.clientWidth
    ? 'ORG_OK' : 'ORG_FAIL:mobile overflow'" "files page overflows at 390px"
assert_interactive_targets
devtools_json take_screenshot --fullPage=true --filePath="$ARTIFACT_PREFIX-mobile.png" >/dev/null
devtools_json take_snapshot --verbose=true --filePath="$ARTIFACT_PREFIX-mobile.snapshot.txt" >/dev/null
assert_console_and_issues_clean "mobile-responsive"

mkdir -p "$LIGHTHOUSE_DIR"
devtools_json lighthouse_audit --mode=navigation --device=desktop --outputDirPath="$LIGHTHOUSE_DIR" \
    >"$ARTIFACT_PREFIX-lighthouse.command.json" || fail "Lighthouse accessibility navigation failed"
lighthouse_json="$(find "$LIGHTHOUSE_DIR" -maxdepth 1 -type f -name '*.json' -print -quit)"
[[ -n "$lighthouse_json" ]] || fail "Lighthouse did not write a JSON report"
jq '{
    accessibility_score: (.categories.accessibility.score // null),
    failed_accessibility_audits: [
        .categories.accessibility.auditRefs[]?.id as $id
        | .audits[$id]
        | select(.score != null and .score < 1)
        | {id, title, score, items: (.details.items // [])}
    ]
}' "$lighthouse_json" >"$ARTIFACT_PREFIX-lighthouse-failures.json"
[[ "$(jq -r '.accessibility_score' "$ARTIFACT_PREFIX-lighthouse-failures.json")" != "null" ]] ||
    fail "Lighthouse accessibility score is unavailable"
assert_lighthouse_boundary
capture_browser_phase "lighthouse-navigation"

archive_workspace_fixture
navigate "$base/org/$workspace_id/files?status=active&limit=10"
wait_files_settled
assert_eval "() => document.body.innerText.includes('Archived workspace · files read-only')
    && !document.querySelector('[data-testid=org-files-add]')
    && ![...document.querySelectorAll('.org-files-row-actions button')]
        .some((button) => /^(Rename|Archive|Restore)/.test(button.getAttribute('aria-label') || button.textContent.trim()))
    ? 'ORG_OK' : 'ORG_FAIL:archived workspace controls'" \
    "archived workspace did not expose a read-only files list"

capture_browser_phase "archived-workspace-read-only"

report "PASS: empty create, stable identity, stale-refresh rename, archive/rename/restore, path reservation, archived-workspace read-only, and three-page history"
report "Lighthouse: $(jq -c '{accessibility_score, failed_count: (.failed_accessibility_audits | length)}' "$ARTIFACT_PREFIX-lighthouse-failures.json")"
report "artifacts: $ARTIFACT_PREFIX-desktop.png $ARTIFACT_PREFIX-mobile.png $ARTIFACT_PREFIX-desktop.snapshot.txt $ARTIFACT_PREFIX-add-dialog.snapshot.txt $ARTIFACT_PREFIX-lighthouse-failures.json $ARTIFACT_PREFIX-network.jsonl $ARTIFACT_PREFIX-console.jsonl"
