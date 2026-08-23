#!/usr/bin/env bash
set -euo pipefail

# Black-box acceptance for the read-only Org content and operations routes. Local fixture creation is
# deliberately outside this script: ORG_CONSOLE_WORKSPACE_ID and
# ORG_CONSOLE_ITEM_ID must identify records prepared before the browser run.

readonly BASE_URL="${ORG_CONSOLE_BASE_URL:?set ORG_CONSOLE_BASE_URL}"
readonly WORKSPACE_ID="${ORG_CONSOLE_WORKSPACE_ID:-}"
readonly ITEM_ID="${ORG_CONSOLE_ITEM_ID:-}"
readonly DEVTOOLS="${CHROME_DEVTOOLS_BIN:-chrome-devtools}"
readonly WAIT_MS="${ORG_CONSOLE_WAIT_MS:-20000}"

fail() {
    printf 'Org console browser gate: FAIL: %s\n' "$*" >&2
    exit 1
}

report() {
    printf 'Org console browser gate: %s\n' "$*"
}

command -v "$DEVTOOLS" >/dev/null 2>&1 || fail "chrome-devtools CLI is unavailable"
command -v jq >/dev/null 2>&1 || fail "jq is unavailable"
command -v curl >/dev/null 2>&1 || fail "curl is unavailable"

if [[ -n "$WORKSPACE_ID" && -z "$ITEM_ID" ]] || [[ -z "$WORKSPACE_ID" && -n "$ITEM_ID" ]]; then
    fail "set both ORG_CONSOLE_WORKSPACE_ID and ORG_CONSOLE_ITEM_ID, or neither for directory-only mode"
fi

base="${BASE_URL%/}"
[[ "$base" =~ ^https?:// ]] || fail "ORG_CONSOLE_BASE_URL must be an http(s) URL"

if [[ -n "$WORKSPACE_ID" ]]; then
    mode="full"
else
    mode="directory-only"
fi
report "mode=$mode (directory-only is partial remote evidence)"

assert_spa_route() {
    local url="$1"
    local body_file status
    body_file="$(mktemp)"
    status="$(curl --show-error --silent --output "$body_file" --write-out '%{http_code}' "$url")" || {
        rm -f "$body_file"
        fail "direct route request failed: $url"
    }
    [[ "$status" == "200" ]] || {
        rm -f "$body_file"
        fail "direct route returned HTTP $status instead of 200: $url"
    }
    grep -Eiq '<!doctype[[:space:]]+html|<html([[:space:]>])' "$body_file" || {
        rm -f "$body_file"
        fail "direct route did not return the frontend index: $url"
    }
    rm -f "$body_file"
}

assert_rest_owns_api() {
    local body_file status
    body_file="$(mktemp)"
    status="$(curl --show-error --silent --output "$body_file" --write-out '%{http_code}' \
        "$base/api/org/workspaces?limit=1")" || {
        rm -f "$body_file"
        fail "/api/org workspace list request failed"
    }
    [[ "$status" == "200" ]] || {
        rm -f "$body_file"
        fail "/api/org workspace list returned HTTP $status instead of 200"
    }
    local body
    body="$(<"$body_file")"
    rm -f "$body_file"
    jq -e 'type == "object" and (.items | type == "array")' <<<"$body" >/dev/null ||
        fail "/api/org was not claimed by the REST router"
}

assert_spa_route "$base/org?include_archived=false&limit=50"
assert_rest_owns_api
if [[ "$mode" == "full" ]]; then
    assert_spa_route "$base/org/$WORKSPACE_ID?view=ready&limit=50"
    assert_spa_route "$base/org/$WORKSPACE_ID/items/$ITEM_ID?return_view=ready&return_limit=50"
fi
report "direct-route SPA fallback and /api/org REST ownership verified"

urlencode() {
    jq -rn --arg value "$1" '$value | @uri'
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

wait_for_any() {
    local selectors="$1"
    local description="$2"
    assert_eval "() => new Promise((resolve, reject) => {
        const selectors = $selectors;
        const deadline = Date.now() + $WAIT_MS;
        const check = () => {
            const match = selectors.map((selector) => document.querySelector(selector)).find(Boolean);
            if (match) return resolve('ORG_OK:' + match.dataset.testid);
            if (Date.now() >= deadline) return reject(new Error('timed out waiting for ' + selectors.join(', ')));
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

network_json() {
    devtools_json list_network_requests
}

org_request_count() {
    jq --arg base "$base" '[
        (.networkRequests // [])[]
        | select(.url == ($base + "/api/org")
            or (.url | startswith($base + "/api/org/"))
            or (.url | startswith($base + "/api/org?")))
    ] | length'
}

assert_network_boundary() {
    local network="$1"
    jq -e '[(.networkRequests // [])[] | select(.url | contains("/mcp"))] | length == 0' \
        <<<"$network" >/dev/null || fail "browser contacted /mcp"
    jq -e --arg base "$base" '
        [(.networkRequests // [])[]
            | select(.url | contains("/api/org"))
            | select(((
                (.url == ($base + "/api/org")
                    or (.url | startswith($base + "/api/org/"))
                    or (.url | startswith($base + "/api/org?")))
                and .method == "GET"
            )) | not)
        ] | length == 0
    ' <<<"$network" >/dev/null ||
        fail "browser issued a non-GET or cross-origin /api/org request"
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

assert_no_sensitive_text() {
    assert_eval "() => {
        const text = document.documentElement.innerText.toLowerCase();
        const forbidden = [
            /fencing[\\s_-]*token/,
            /lease[\\s_-]*token/,
            /token[\\s_-]*(hash|digest)/,
            /access[\\s_-]*token/,
            /api[\\s_-]*key/,
            /bearer\\s+[a-z0-9._~+\\/=-]+/,
        ];
        return forbidden.some((pattern) => pattern.test(text)) ? 'ORG_FAIL:sensitive text' : 'ORG_OK';
    }" "page renders token-shaped text"
}

assert_no_polling() {
    local before after network
    network="$(network_json)"
    assert_network_boundary "$network"
    before="$(org_request_count <<<"$network")"
    sleep 2
    network="$(network_json)"
    assert_network_boundary "$network"
    after="$(org_request_count <<<"$network")"
    [[ "$before" == "$after" ]] || fail "Org requests repeated without manual refresh ($before -> $after)"
}

click_testid() {
    local testid="$1"
    local quoted
    quoted="$(js_string "$testid")"
    assert_eval "() => {
        const element = document.querySelector('[data-testid=' + $quoted + ']');
        if (!element) return 'ORG_FAIL:missing control';
        element.click();
        return 'ORG_OK';
    }" "could not click data-testid=$testid"
}

assert_directory_refresh() {
    local testid="$1"
    local before network new_requests list_prefix
    network="$(network_json)"
    before="$(org_request_count <<<"$network")"
    click_testid "$testid"
    wait_for_any '["[data-testid=org-directory-ready]", "[data-testid=org-directory-empty]", "[data-testid=org-directory-error]"]' \
        "directory refresh did not reach a settled state"
    sleep 1
    network="$(network_json)"
    assert_network_boundary "$network"
    new_requests="$(jq --arg base "$base" --argjson before "$before" '
        [(.networkRequests // [])[]
            | select(.url == ($base + "/api/org")
                or (.url | startswith($base + "/api/org/"))
                or (.url | startswith($base + "/api/org?")))][$before:]' \
        <<<"$network")"
    list_prefix="$base/api/org/workspaces?"
    jq -e --arg list "$list_prefix" '
        length == 1
        and .[0].method == "GET"
        and (.[0].url | startswith($list))
    ' <<<"$new_requests" >/dev/null || {
        printf '%s\n' "$new_requests" >&2
        fail "directory refresh did not issue exactly its workspace-list GET"
    }
}

assert_workspace_refresh() {
    local testid="$1"
    local workspace="$2"
    local before after network new_requests detail_prefix list_prefix
    network="$(network_json)"
    before="$(org_request_count <<<"$network")"
    click_testid "$testid"
    wait_for_any '["[data-testid=org-workspace-ready]", "[data-testid=org-workspace-empty]", "[data-testid=org-workspace-error]"]' \
        "workspace refresh did not reach a settled state"
    sleep 1
    network="$(network_json)"
    assert_network_boundary "$network"
    after="$(org_request_count <<<"$network")"
    new_requests="$(jq --arg base "$base" --argjson before "$before" '
        [(.networkRequests // [])[]
            | select(.url == ($base + "/api/org")
                or (.url | startswith($base + "/api/org/"))
                or (.url | startswith($base + "/api/org?")))][$before:]' \
        <<<"$network")"
    detail_prefix="$base/api/org/workspaces/$workspace"
    list_prefix="$base/api/org/workspaces?"
    jq -e --arg detail "$detail_prefix" --arg list "$list_prefix" '
        length >= 3
        and ([.[] | select(.url == $detail)] | length == 1)
        and ([.[] | select(.url | startswith($list))] | length >= 1)
        and ([.[] | select(.url | test("/api/org/(queue|agenda)\\?"))] | length == 1)
        and (all(.[]; .url == $detail
            or (.url | startswith($list))
            or (.url | test("/api/org/(queue|agenda)\\?"))))
    ' <<<"$new_requests" >/dev/null || {
        printf '%s\n' "$new_requests" >&2
        fail "workspace refresh issued an unexpected Org GET set ($((after - before)) requests)"
    }
}

assert_item_refresh() {
    local testid="$1"
    local workspace="$2"
    local item="$3"
    local before network new_requests context_url events_url
    network="$(network_json)"
    before="$(org_request_count <<<"$network")"
    click_testid "$testid"
    wait_for_any '["[data-testid=org-item-ready]", "[data-testid=org-item-missing]", "[data-testid=org-item-error]"]' \
        "item refresh did not reach a settled context state"
    wait_for_any '["[data-testid=org-events-ready]", "[data-testid=org-events-empty]", "[data-testid=org-item-error]"]' \
        "item refresh did not reach a settled event state"
    sleep 1
    network="$(network_json)"
    assert_network_boundary "$network"
    new_requests="$(jq --arg base "$base" --argjson before "$before" '
        [(.networkRequests // [])[]
            | select(.url == ($base + "/api/org")
                or (.url | startswith($base + "/api/org/"))
                or (.url | startswith($base + "/api/org?")))][$before:]' \
        <<<"$network")"
    context_url="$base/api/org/items/$item/context?workspace_id=$workspace"
    events_url="$base/api/org/workspaces/$workspace/events?subject_kind=work_item&subject_id=$item&limit=50"
    jq -e --arg context "$context_url" --arg events "$events_url" '
        length == 2
        and ([.[] | select(.method == "GET" and .url == $context)] | length == 1)
        and ([.[] | select(.method == "GET" and .url == $events)] | length == 1)
    ' <<<"$new_requests" >/dev/null || {
        printf '%s\n' "$new_requests" >&2
        fail "item refresh did not issue exactly context + subject-filtered events GETs"
    }
}

assert_common_page_contract() {
    assert_eval "() => {
        const heading = document.querySelector('main h2');
        const orgPage = document.querySelector('[data-testid^=org-][data-testid$=-page]');
        const expectedTitle = location.pathname.includes('/items/')
            ? 'Org item '
            : location.pathname === '/org'
                ? 'Org | agent-note'
                : 'Org workspace ';
        return heading && orgPage && document.title.startsWith(expectedTitle)
            ? 'ORG_OK:' + heading.textContent.trim()
            : 'ORG_FAIL:missing heading/page landmark or route-specific title';
    }" "Org page heading, readiness landmark, or route-specific title is missing"
    assert_no_sensitive_text
    assert_console_clean
}

assert_mobile_navigation_visible() {
    assert_eval "() => {
        const expected = ['Home', 'Notes', 'Org', 'New note', 'Labels', 'Trash', 'System'];
        const links = [...document.querySelectorAll('nav[aria-label=\"Primary navigation\"] a')];
        const visible = expected.every((label) => {
            const link = links.find((candidate) => candidate.textContent.trim() === label);
            if (!link || !link.getAttribute('href')) return false;
            const rect = link.getBoundingClientRect();
            return rect.width > 0 && rect.height > 0 && rect.left >= 0 && rect.right <= innerWidth;
        });
        const noOverflow = document.documentElement.scrollWidth <= document.documentElement.clientWidth;
        return visible && noOverflow ? 'ORG_OK:mobile navigation' : 'ORG_FAIL:mobile navigation';
    }" "mobile primary navigation is clipped, unreachable, or causes viewport overflow"
}

directory_url="$base/org?include_archived=false&limit=50"
navigate "$directory_url"
wait_for_any '["[data-testid=org-directory-ready]", "[data-testid=org-directory-empty]", "[data-testid=org-directory-error]"]' \
    "directory did not settle"
assert_common_page_contract

if [[ "$mode" == "directory-only" ]]; then
    assert_eval "() => document.querySelector('[data-testid=org-directory-empty], [data-testid=org-directory-error]')
        ? 'ORG_OK:partial directory state'
        : 'ORG_FAIL:directory-only mode requires an empty or structured-error state'" \
        "directory-only deployment unexpectedly contains data; provide workspace and item IDs for the full gate"
    assert_no_polling
    assert_directory_refresh "org-directory-refresh"
    "$DEVTOOLS" resize_page 1440 900 >/dev/null
    "$DEVTOOLS" take_screenshot --fullPage=true --filePath=/tmp/org-console-directory-desktop.png >/dev/null
    "$DEVTOOLS" resize_page 390 844 >/dev/null
    assert_mobile_navigation_visible
    "$DEVTOOLS" take_screenshot --fullPage=true --filePath=/tmp/org-console-directory-mobile.png >/dev/null
    "$DEVTOOLS" take_snapshot --verbose=true --filePath=/tmp/org-console-directory.snapshot.txt >/dev/null
    report "PASS: directory-only partial evidence; screenshots and accessibility snapshot are in /tmp"
    exit 0
fi

wait_for_any '["[data-testid=org-directory-ready]"]' "fixture workspace directory table is missing"
assert_eval "() => document.querySelector('[data-testid=org-directory-table] caption')
    && document.querySelectorAll('[data-testid=org-directory-table] thead th').length === 14
    ? 'ORG_OK:directory ledger' : 'ORG_FAIL:directory ledger columns'" \
    "directory ledger does not expose workspace fields and all ten counts"
assert_no_polling
assert_directory_refresh "org-directory-refresh"

# A malformed opaque cursor exercises the structured directory error without
# any fixture mutation, then the gate returns to the fixture routes.
navigate "$base/org?include_archived=false&cursor=browser-gate-invalid&limit=50"
wait_for_any '["[data-testid=org-directory-error]"]' "invalid cursor did not render a structured directory error"
assert_eval "() => document.querySelector('main h2')?.textContent.includes('Org workspaces')
    ? 'ORG_OK:structured error heading' : 'ORG_FAIL:error heading'" \
    "structured directory error lost its page heading"
assert_no_sensitive_text
assert_network_boundary "$(network_json)"

workspace_encoded="$(urlencode "$WORKSPACE_ID")"
item_encoded="$(urlencode "$ITEM_ID")"
workspace_url="$base/org/$workspace_encoded?view=ready&limit=50"
navigate "$workspace_url"
wait_for_any '["[data-testid=org-workspace-ready]"]' "fixture item is not present in the ready view"
assert_common_page_contract
assert_eval "() => document.querySelectorAll('[data-testid^=org-view-]').length === 10
    && document.querySelector('[data-testid=org-workspace-ready] table caption')
    && document.querySelector('[data-testid=org-filter-priority]').value === ''
    ? 'ORG_OK:ten views and ledger' : 'ORG_FAIL:workspace view contract'" \
    "workspace does not expose all ten views and its semantic ledger"
assert_no_polling
assert_workspace_refresh "org-workspace-refresh" "$workspace_encoded"

click_testid "org-view-failed"
wait_for_any '["[data-testid=org-workspace-ready]", "[data-testid=org-workspace-empty]"]' \
    "failed view did not settle"
assert_eval "() => location.search.includes('view=failed') ? 'ORG_OK:view changed' : 'ORG_FAIL:view URL'" \
    "view change is not URL-backed"

navigate "$workspace_url"
wait_for_any '["[data-testid=org-workspace-ready]"]' "ready view did not return"
assert_eval "() => {
    const input = document.querySelector('[data-testid=org-filter-state]');
    if (!input) return 'ORG_FAIL:state filter';
    input.value = 'NO_MATCH_FOR_BROWSER_TEST';
    input.dispatchEvent(new Event('input', {bubbles: true}));
    return 'ORG_OK';
}" "state filter is unavailable"
click_testid "org-filters-apply"
wait_for_any '["[data-testid=org-workspace-empty]"]' "no-match filter did not render the empty state"
assert_eval "() => location.search.includes('state=NO_MATCH_FOR_BROWSER_TEST')
    ? 'ORG_OK:filter changed' : 'ORG_FAIL:filter URL'" "filter change is not URL-backed"

navigate "$workspace_url"
wait_for_any '["[data-testid=org-workspace-ready]"]' "ready view did not restore after filter check"
assert_eval "() => {
    const input = document.querySelector('[data-testid=org-filter-state]');
    if (!input) return 'ORG_FAIL:matching state filter';
    input.value = 'READY';
    input.dispatchEvent(new Event('input', {bubbles: true}));
    return 'ORG_OK';
}" "matching state filter is unavailable"
click_testid "org-filters-apply"
wait_for_any '["[data-testid=org-workspace-ready]"]' "matching state filter did not retain the fixture item"
workspace_filtered_url="$base/org/$workspace_encoded?view=ready&state=READY&limit=50"
workspace_filtered_url_js="$(js_string "$workspace_filtered_url")"
assert_eval "() => {
    const expected = $workspace_filtered_url_js;
    return location.href === expected ? 'ORG_OK:origin' : 'ORG_FAIL:' + location.href;
}" "filtered workspace origin URL is not canonical"

item_selector=".org-item-link[href*='/items/$item_encoded']"
item_selector_js="$(js_string "$item_selector")"
assert_eval "() => {
    const link = document.querySelector($item_selector_js);
    if (!link) return 'ORG_FAIL:fixture item link';
    link.click();
    return 'ORG_OK';
}" "fixture item link is missing from the ready ledger"
wait_for_any '["[data-testid=org-item-ready]"]' "item context did not settle"
wait_for_any '["[data-testid=org-events-ready]", "[data-testid=org-events-empty]", "[data-testid=org-item-error]"]' \
    "item event history did not settle"
assert_common_page_contract
assert_eval "() => location.search.includes('return_view=ready')
    && location.search.includes('return_state=READY')
    && location.search.includes('return_limit=50')
    && document.querySelector('[data-testid=org-item-ready]')
    ? 'ORG_OK:typed return context' : 'ORG_FAIL:item return context'" \
    "item route did not preserve typed return state"
assert_eval "() => document.querySelector('[data-testid=org-item-ready] [aria-labelledby=org-overview-title]')
    && document.querySelector('[aria-labelledby=org-events-title]')
    ? 'ORG_OK:item sections' : 'ORG_FAIL:item sections'" \
    "item context or event history section is missing"
assert_no_polling
assert_item_refresh "org-item-refresh" "$workspace_encoded" "$item_encoded"

click_testid "org-item-back"
wait_for_any '["[data-testid=org-workspace-ready]"]' "typed back link did not restore the ready ledger"
assert_eval "() => location.href === $workspace_filtered_url_js ? 'ORG_OK:exact return' : 'ORG_FAIL:' + location.href" \
    "typed back link did not restore the exact originating URL"
assert_common_page_contract
assert_no_polling

"$DEVTOOLS" resize_page 1440 900 >/dev/null
"$DEVTOOLS" take_screenshot --fullPage=true --filePath=/tmp/org-console-desktop.png >/dev/null
"$DEVTOOLS" resize_page 390 844 >/dev/null
assert_mobile_navigation_visible
"$DEVTOOLS" take_screenshot --fullPage=true --filePath=/tmp/org-console-mobile.png >/dev/null
"$DEVTOOLS" take_snapshot --verbose=true --filePath=/tmp/org-console-mobile.snapshot.txt >/dev/null
assert_console_clean
assert_network_boundary "$(network_json)"
report "PASS: all three read-only routes, exact typed return, GET-only REST, manual refresh, and no polling"
report "desktop/mobile screenshots and accessibility snapshot are in /tmp"
