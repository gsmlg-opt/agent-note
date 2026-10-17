#!/usr/bin/env python3
"""Exercise sync diagnostics using local UI assets and fully mocked API responses."""

import argparse
import asyncio
import copy
import json
from pathlib import Path
from urllib.parse import urlparse

from playwright.async_api import async_playwright, expect


FAILURE = {
    "timestamp": 1792242000000,
    "level": "error",
    "stage": "websocket",
    "code": "http_403",
    "message": "GaoNote rejected the WebSocket handshake with HTTP 403. Check the receiver endpoint and transport authorization.",
}
CONNECTION = {
    "id": "local-test",
    "url": "ws://127.0.0.1:9/note-sync/websocket?vsn=2.0.0",
    "mode": "direct",
    "ssh": None,
    "max_blob_bytes": 536870912,
    "has_client_cert": False,
    "has_client_key": False,
    "has_ca_cert": False,
}


class MockApi:
    def __init__(self):
        self.status = {
            "profiles": [{"id": CONNECTION["id"], "url": CONNECTION["url"]}],
            "state": "error",
            "last_error": FAILURE["message"],
            "last_success_at": None,
            "pending_notes": 7,
            "initializing": False,
            "node_id": "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa",
            "peer_node_id": None,
            "blocked_notes": [],
            "last_attempt_at": FAILURE["timestamp"],
            "next_retry_at": FAILURE["timestamp"] + 32000,
            "diagnostics": [copy.deepcopy(FAILURE)],
        }
        self.unavailable = False
        self.retries = 0
        self.unexpected = []
        self.writes = []

    async def handle(self, route):
        request = route.request
        path = urlparse(request.url).path
        if request.method != "GET":
            self.writes.append((request.method, path))
        if path == "/api/system/sync/retry" and request.method == "POST":
            self.retries += 1
            self.status["state"] = "connecting"
            self.status["next_retry_at"] = None
            await route.fulfill(status=204)
            return
        if request.method != "GET":
            self.unexpected.append((request.method, path))
            await route.abort()
            return
        if path == "/api/system/config":
            body = {
                "sync": {
                    "enabled": True,
                    "connection_profile": CONNECTION["id"],
                    "rules": [{"id": "rule-1", "enabled": True, "selector": "project=bear_note"}],
                }
            }
        elif path == "/api/system/sync/connections":
            body = [CONNECTION]
        elif path == "/api/labels":
            body = []
        elif path == "/api/system/sync":
            if self.unavailable:
                await route.fulfill(status=503, body="Sync status temporarily unavailable")
                return
            body = self.status
        else:
            self.unexpected.append((request.method, path))
            await route.abort()
            return
        await route.fulfill(status=200, content_type="application/json", body=json.dumps(body))


async def qualify(browser, base_url, output):
    api = MockApi()
    context = await browser.new_context(viewport={"width": 1440, "height": 1000})
    await context.route("**/api/**", api.handle)
    page = await context.new_page()
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))
    await page.goto(f"{base_url}/system/sync")
    region = page.get_by_role("region", name="Connection diagnostics", exact=True)
    await expect(region).to_be_visible()
    await expect(region.get_by_text(FAILURE["message"], exact=True)).to_be_visible()
    await expect(region.get_by_text("http_403", exact=False)).to_be_visible()
    await expect(page.get_by_text("Last connection attempt", exact=True)).to_be_visible()
    await expect(page.get_by_text("Next automatic retry", exact=True)).to_be_visible()
    await expect(page.get_by_role("button", name="Save settings", exact=True)).to_be_enabled()
    assert await region.locator("time").count() == 1
    await page.screenshot(path=str(output / "failure-desktop.png"))

    await page.get_by_role("button", name="Retry sync", exact=True).click()
    await expect(page.get_by_text("connecting", exact=True)).to_be_visible()
    await expect(region.get_by_text(FAILURE["message"], exact=True)).to_be_visible()
    await expect(page.get_by_text("Next automatic retry", exact=True)).to_have_count(0)
    assert api.retries == 1

    latest = {
        "timestamp": FAILURE["timestamp"] + 60000,
        "level": "error",
        "stage": "tcp",
        "code": "connection_refused",
        "message": "TCP connection refused. Verify that GaoNote is listening on the configured port. " + "LongDiagnostic" * 30,
    }
    history = [
        {**FAILURE, "message": f"Historical connection failure {index}."}
        for index in range(18)
    ] + [FAILURE, latest]
    api.status.update(state="error", last_error=latest["message"], diagnostics=history)
    await expect(region.get_by_text(latest["message"], exact=True)).to_be_visible(timeout=10000)
    await expect(region.locator("li").first).to_contain_text("connection_refused")
    await expect(region.locator("li")).to_have_count(20)
    history_list = region.get_by_role("list", name="Recent connection events", exact=True)
    await history_list.focus()
    await expect(history_list).to_be_focused()
    assert await history_list.evaluate("el => el.scrollHeight > el.clientHeight"), "history should scroll without expanding the settings form"
    await page.set_viewport_size({"width": 320, "height": 850})
    await region.scroll_into_view_if_needed()
    await page.screenshot(path=str(output / "failure-mobile.png"))
    layout = await page.evaluate("""() => ({
        viewport: innerWidth, width: document.documentElement.scrollWidth,
        overflow: Array.from(document.querySelectorAll('main *')).filter(el =>
            el.getBoundingClientRect().right > innerWidth + 1).slice(0, 12).map(el =>
            ({tag: el.tagName, class: el.className, width: el.getBoundingClientRect().width}))
    })""")
    (output / "mobile-layout.json").write_text(json.dumps(layout, indent=2) + "\n")
    assert layout["width"] <= layout["viewport"], "diagnostics overflow mobile viewport"

    recovery = {
        "timestamp": FAILURE["timestamp"] + 120000,
        "level": "info",
        "stage": "phoenix",
        "code": "connected",
        "message": "Connected to GaoNote.",
    }
    api.status.update(state="connected", last_error=None, diagnostics=[FAILURE, latest, recovery])
    await expect(region.get_by_text(recovery["message"], exact=True)).to_be_visible(timeout=10000)
    await expect(region.locator("li").first).to_contain_text("Connected to GaoNote.")
    await expect(region.get_by_text(FAILURE["message"], exact=True)).to_be_visible()
    await expect(page.get_by_text("Next automatic retry", exact=True)).to_have_count(0)

    api.unavailable = True
    await expect(page.get_by_text("server error 503: Sync status temporarily unavailable", exact=True)).to_be_visible(timeout=10000)
    await expect(region.get_by_text(FAILURE["message"], exact=True)).to_be_visible()
    api.unavailable = False
    api.status.update(state="disabled", last_error=None)
    for field in ("diagnostics", "last_attempt_at", "next_retry_at"):
        api.status.pop(field, None)
    await expect(region.locator("li")).to_have_count(0, timeout=10000)
    await expect(page.get_by_role("button", name="Save settings", exact=True)).to_be_enabled()
    await expect(page.get_by_text("Last connection attempt", exact=True)).to_have_count(0)
    assert api.writes == [("POST", "/api/system/sync/retry")], api.writes
    assert not api.unexpected, api.unexpected
    assert not errors, errors
    await context.close()
    return {"failure": "PASS", "retry_history": "PASS", "recovery_history": "PASS", "status_outage": "PASS", "legacy_status": "PASS", "mobile_wrap": "PASS", "bounded_keyboard_history": "PASS", "unexpected_requests": api.unexpected, "page_errors": errors}


async def main(args):
    assert urlparse(args.base_url).hostname in {"127.0.0.1", "localhost", "::1"}
    args.output.mkdir(parents=True, exist_ok=True)
    async with async_playwright() as playwright:
        launch = {"executable_path": args.executable} if args.executable else {}
        browser = await playwright.chromium.launch(**launch)
        try:
            result = await qualify(browser, args.base_url.rstrip("/"), args.output)
        finally:
            await browser.close()
    (args.output / "browser-result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:6221")
    parser.add_argument("--executable", help="Optional system Chromium executable")
    parser.add_argument("--output", type=Path, default=Path("/tmp/agent-note-sync-diagnostics-browser"))
    asyncio.run(main(parser.parse_args()))
