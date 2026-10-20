#!/usr/bin/env python3
"""Qualify sync progress against local Wasm assets and mocked APIs only."""
import argparse
import asyncio
import copy
import json
from pathlib import Path
from urllib.parse import urlparse

from playwright.async_api import async_playwright, expect
from test_sync_diagnostics import MockApi, FAILURE


NOTE = {"id": "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb", "title": "<img src=x onerror=alert(1)> " + "LongTitle" * 35,
        "phase": "metadata", "attachments_total": 2, "attachments_completed": 0,
        "attachment_bytes_total": 100, "attachment_bytes_done": 0}


def direction(**changes):
    value = {"run_id": "round-upload-1", "phase": "checking", "basis": "remote_verified",
             "notes_total": 4, "notes_checked": 1, "notes_equal": 1,
             "notes_planned": 0, "notes_completed": 0, "notes_superseded": 0,
             "notes_remaining": None, "blocked_notes": 0, "current_note": None}
    value.update(changes)
    return value


class ProgressApi(MockApi):
    def __init__(self):
        super().__init__()
        self.status.update(state="connected", last_error=None, diagnostics=[])
        self.config = {"sync": {"enabled": True, "connection_profile": "local-test",
                              "rules": [{"id": "rule-1", "enabled": True, "selector": "project=bear_note"}]}}
        self.polls = 0
        self.delay = 0
        self.in_flight = 0
        self.max_in_flight = 0

    async def handle(self, route):
        path = urlparse(route.request.url).path
        if path == "/api/system/config":
            if route.request.method == "PUT":
                self.writes.append(("PUT", path))
                self.config = route.request.post_data_json
                await route.fulfill(status=204)
            else:
                await route.fulfill(status=200, content_type="application/json", body=json.dumps(self.config))
            return
        if path == "/api/system/sync":
            self.polls += 1
            self.in_flight += 1
            self.max_in_flight = max(self.max_in_flight, self.in_flight)
            try:
                await asyncio.sleep(self.delay)
                await super().handle(route)
            finally:
                self.in_flight -= 1
            return
        await super().handle(route)


async def qualify(browser, base_url, output):
    api = ProgressApi()
    context = await browser.new_context(viewport={"width": 1440, "height": 1100})
    await context.route("**/api/**", api.handle)
    page = await context.new_page()
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))
    await page.goto(f"{base_url}/system/sync")
    await expect(page.get_by_role("region", name="Connection diagnostics", exact=True)).to_be_visible()
    await expect(page.get_by_role("region", name="Sending to GaoNote", exact=True)).to_have_count(0)
    await expect(page.get_by_role("button", name="Save settings", exact=True)).to_be_enabled()

    api.status["progress"] = {"updated_at": FAILURE["timestamp"], "tracked_notes": 1761, "upload": direction(),
                              "download": direction(run_id="round-download-1", basis="local_queue", notes_total=None, notes_checked=0, notes_equal=0)}
    upload = page.get_by_role("region", name="Sending to GaoNote", exact=True)
    download = page.get_by_role("region", name="Receiving from GaoNote", exact=True)
    await expect(upload).to_be_visible(timeout=6000)
    await expect(page.get_by_text("Locally tracked at last full check: 1761 notes", exact=True)).to_be_visible()
    await expect(upload.get_by_text("Checking notes", exact=True)).to_be_visible()
    await expect(upload).to_contain_text("1 of 4 checked")
    await expect(upload).to_contain_text("Remaining being checked")
    await expect(download).to_contain_text("Total unknown")
    assert await download.get_by_role("progressbar", name="Receiving from GaoNote round progress", exact=True).evaluate("el => !el.hasAttribute('value')")
    await page.screenshot(path=str(output / "checking-desktop.png"), full_page=True)

    up = api.status["progress"]["upload"]
    up.update(phase="transferring", notes_checked=4, notes_equal=2, notes_planned=2, notes_remaining=2,
              current_note={**NOTE, "phase": "preparing", "attachments_total": 0, "attachment_bytes_total": 0})
    await expect(upload.get_by_text("Preparing note", exact=True)).to_be_visible(timeout=6000)
    await expect(upload).to_contain_text("0 / 0 attachments")
    up["current_note"] = copy.deepcopy(NOTE)
    await expect(upload.get_by_text("Transferring note content", exact=True)).to_be_visible(timeout=6000)
    await expect(upload.get_by_text(NOTE["title"][:240], exact=True)).to_be_visible()
    assert await upload.locator("img").count() == 0
    up["current_note"].update(phase="attachments", attachments_completed=1, attachment_bytes_done=50)
    api.status["progress"]["download"].update(phase="transferring", current_note={**NOTE, "title": "Inbound note", "phase": "applying"})
    await expect(upload.get_by_text("Transferring attachments", exact=True)).to_be_visible(timeout=6000)
    await expect(upload).to_contain_text("50 / 100 bytes")
    await expect(download.get_by_text("Applying note", exact=True)).to_be_visible()
    await expect(upload.get_by_role("progressbar", name="Sending to GaoNote attachment bytes", exact=True)).to_have_attribute("value", "50")
    await page.screenshot(path=str(output / "attachments-desktop.png"), full_page=True)
    await page.set_viewport_size({"width": 320, "height": 850})
    await upload.scroll_into_view_if_needed()
    await page.screenshot(path=str(output / "attachments-mobile.png"), full_page=True)
    assert await page.evaluate("document.documentElement.scrollWidth <= innerWidth"), "progress overflows mobile viewport"

    up["current_note"].update(phase="awaiting_commit", attachments_completed=2, attachment_bytes_done=100)
    await expect(upload.get_by_text("Awaiting confirmation", exact=True)).to_be_visible(timeout=6000)
    await expect(upload).to_contain_text("0 delivered")
    up.update(phase="blocked", blocked_notes=1, notes_completed=1, notes_remaining=1, current_note=None)
    await expect(upload.get_by_text("Needs attention", exact=True)).to_be_visible(timeout=6000)
    await expect(upload).to_contain_text("1 blocked")
    api.status.update(blocked_notes=[{"note_id": NOTE["id"], "event_id": "event-1",
                                     "reason": "catalog_conflict"}], blocked_notes_total=65)
    await expect(page.get_by_text("Showing 1 of 65 notes needing attention.", exact=True)).to_be_visible(timeout=6000)
    api.status.update(blocked_notes=[], blocked_notes_total=0)
    await expect(upload.get_by_text(NOTE["title"][:240], exact=True)).to_have_count(0)

    up.update(phase="idle", blocked_notes=0, notes_completed=1, notes_superseded=1, notes_remaining=0)
    await expect(upload.get_by_text("Round complete", exact=True)).to_be_visible(timeout=6000)
    await expect(upload).to_contain_text("2 already equal")
    await expect(upload).to_contain_text("1 superseded")
    await expect(page.get_by_text("Up to date", exact=True)).to_have_count(0)
    api.status["state"] = "disconnected"
    up.update(phase="reconnecting", current_note=copy.deepcopy(NOTE))
    await expect(upload.get_by_text("Reconnecting", exact=True)).to_be_visible(timeout=6000)
    await expect(upload.get_by_text(NOTE["title"][:240], exact=True)).to_have_count(0)
    await page.get_by_role("button", name="Retry sync", exact=True).click()
    assert api.retries == 1

    api.status["state"] = "connected"
    up.update(phase="paused", current_note=None)
    await expect(upload.get_by_text("Paused", exact=True)).to_be_visible(timeout=6000)
    api.status["progress"]["download"].update(basis="legacy_replay", phase="idle", current_note=None)
    await expect(download).to_contain_text("Legacy replay; remote totals unavailable", timeout=6000)
    await expect(download).not_to_contain_text("Round complete")

    up.update(phase="idle", notes_total=0, notes_checked=0, notes_equal=0, notes_planned=0,
              notes_completed=0, notes_superseded=0, notes_remaining=0)
    await expect(upload.get_by_text("Round complete", exact=True)).to_be_visible(timeout=6000)
    await expect(upload.get_by_role("progressbar", name="Sending to GaoNote round progress", exact=True)).to_have_attribute("value", "1")
    await expect(upload.get_by_role("progressbar", name="Sending to GaoNote round progress", exact=True)).to_have_attribute("max", "1")

    up["active_task"] = {"id": "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa:42",
                         "operation": "inventory_start", "state": "running", "phase": "checking",
                         "updated_at": FAILURE["timestamp"], "error": None}
    await expect(upload.get_by_text("Processing task", exact=True)).to_be_visible(timeout=6000)
    await expect(upload.get_by_text("Checking note inventory · Running", exact=True)).to_be_visible()
    await expect(upload.get_by_text("Round complete", exact=True)).to_have_count(0)
    task = up["active_task"]
    task.update(operation="transfer_commit", phase="outcome_unknown")
    await expect(upload.get_by_text("Saving received note · Checking commit outcome", exact=True)).to_be_visible(timeout=6000)
    task.update(state="failed", phase="verifying", operation="blob_end",
                error="Checksum mismatch <img src=x onerror=alert(1)>")
    await expect(upload.get_by_text("Task failed", exact=True)).to_be_visible(timeout=6000)
    await expect(upload.get_by_role("alert")).to_contain_text(task["error"])
    assert api.status["state"] == "connected"
    assert await upload.locator("img").count() == 0
    await page.screenshot(path=str(output / "task-failure-mobile.png"), full_page=True)
    assert await page.evaluate("document.documentElement.scrollWidth <= innerWidth")
    up["active_task"] = None
    await expect(upload.get_by_text("Round complete", exact=True)).to_be_visible(timeout=6000)

    up["manager"] = {"mode": "initial_scan", "page_size": 32, "pages_scanned": 2,
                     "pages_checked": 1, "notes_scanned": 64, "scan_complete": False,
                     "source_cursor": "<img src=x onerror=alert(1)>",
                     "last_progress_at": FAILURE["timestamp"]}
    await expect(upload.get_by_text("Discovering notes", exact=True)).to_be_visible(timeout=6000)
    await expect(upload).to_contain_text("2 pages discovered · 1 page compared")
    await expect(upload).to_contain_text("64 notes discovered · up to 32 notes per page")
    await expect(upload).to_contain_text("Total being counted")
    await expect(upload.get_by_text("Round complete", exact=True)).to_have_count(0)
    assert await upload.get_by_role("progressbar", name="Sending to GaoNote round progress", exact=True).evaluate("el => !el.hasAttribute('value')")
    progress_time = upload.get_by_label("Sending to GaoNote scan").locator("time")
    previous_time = await progress_time.get_attribute("datetime")
    api.status["progress"]["updated_at"] += 60_000
    up["manager"]["scan_complete"] = True
    up["notes_total"] = 64
    await expect(upload.get_by_text("Comparing note pages", exact=True)).to_be_visible(timeout=6000)
    await expect(upload).to_contain_text("0 known remaining · comparison in progress")
    up.update(phase="transferring", notes_remaining=1)
    await expect(upload).to_contain_text("1 known remaining · comparison in progress", timeout=6000)
    up.update(phase="idle", notes_remaining=0)
    await expect(upload.get_by_text("Comparing note pages", exact=True)).to_be_visible(timeout=6000)
    assert await progress_time.get_attribute("datetime") == previous_time
    await expect(upload.get_by_text("Round complete", exact=True)).to_have_count(0)
    assert await upload.locator("img").count() == 0
    await page.screenshot(path=str(output / "paged-scan-mobile.png"), full_page=True)
    assert await page.evaluate("document.documentElement.scrollWidth <= innerWidth")
    up["manager"].update(state="failed", error="Page comparison failed <img src=x onerror=alert(1)>")
    await expect(upload.get_by_text("Sync manager failed", exact=True)).to_be_visible(timeout=6000)
    await expect(upload.get_by_role("alert")).to_contain_text(up["manager"]["error"])
    assert await upload.locator("img").count() == 0
    up["manager"].update(mode="changes", pages_checked=2, state="idle", error=None)
    await expect(upload.get_by_text("Watching for changes", exact=True)).to_be_visible(timeout=6000)
    await expect(upload).to_contain_text("Durable changes")
    up["manager"] = None

    api.unavailable = True
    await expect(page.get_by_text("server error 503: Sync status temporarily unavailable", exact=True)).to_be_visible(timeout=6000)
    await expect(upload).to_be_visible()
    await expect(page.get_by_text("Last progress sample", exact=False)).to_be_visible()
    api.unavailable = False
    api.delay = 2.6
    old_polls = api.polls
    await expect(page.get_by_text("server error 503: Sync status temporarily unavailable", exact=True)).to_have_count(0, timeout=8000)
    assert api.polls > old_polls
    assert api.max_in_flight == 1, api.max_in_flight
    api.delay = 0
    await page.get_by_role("button", name="Save settings", exact=True).click()
    await expect(page.get_by_text("GaoNote sync settings saved.", exact=True)).to_be_visible()
    assert ("PUT", "/api/system/config") in api.writes
    assert not api.unexpected, api.unexpected
    assert not errors, errors
    await context.close()
    return {"legacy": "PASS", "stages": "PASS", "bidirectional_unknown_totals": "PASS", "blocked_reconnect": "PASS",
            "escaped_responsive": "PASS", "durable_completion": "PASS", "poll_failure_recovery_overlap": "PASS", "save_retry": "PASS",
            "task_state_independent_of_connection": "PASS", "unknown_commit_outcome": "PASS",
            "paged_discovery_progress": "PASS", "progress_time_independent_of_heartbeat": "PASS",
            "remaining_is_partial_until_all_pages_are_compared": "PASS"}


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
    parser.add_argument("--executable")
    parser.add_argument("--output", type=Path, default=Path("/tmp/agent-note-sync-progress-ui-20261020"))
    asyncio.run(main(parser.parse_args()))
