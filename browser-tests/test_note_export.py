#!/usr/bin/env python3
"""Browser acceptance checks for the note PDF export lifecycle."""

import argparse
import asyncio
import json
from pathlib import Path
from urllib.parse import urlparse

from playwright.async_api import async_playwright, expect


BASE_URL = "http://127.0.0.1:6221"
PDF_BYTES = b"%PDF-1.7\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n"


def note(note_id: str, title: str, revision: int) -> dict:
    return {
        "id": note_id,
        "title": title,
        "content": f"# {title}\n\nBody for {note_id}.\n",
        "attachments": [],
        "labels": [],
        "created_at": 1,
        "updated_at": 2,
        "revision": revision,
    }


class MockApi:
    def __init__(self):
        self.capability = {"markdown": True, "pdf": True}
        self.capability_failure = False
        self.notes = {
            "note-a": note("note-a", "Alpha report", 7),
            "note-b": note("note-b", "Beta report", 9),
        }
        self.note_delays = {}
        self.note_requests = []
        self.pdf_requests = []
        self.pdf_responses = []
        self.delete_conflict = False

    async def handle(self, route):
        request = route.request
        path = urlparse(request.url).path
        if path == "/api/export/capabilities":
            if self.capability_failure:
                await route.abort()
            else:
                await route.fulfill(
                    status=200,
                    content_type="application/json",
                    body=json.dumps(self.capability),
                )
            return

        if path.startswith("/api/notes/") and path.endswith("/export/pdf"):
            note_id = path.split("/")[3]
            self.pdf_requests.append(request.url)
            response = self.pdf_responses.pop(0) if self.pdf_responses else {}
            delay = response.get("delay", 0)
            if delay:
                await asyncio.sleep(delay)
            status = response.get("status", 200)
            if status == 200:
                try:
                    await route.fulfill(
                        status=200,
                        headers={
                            "content-type": response.get("content_type", "application/pdf"),
                            "x-note-revision": str(
                                response.get("revision", self.notes[note_id]["revision"])
                            ),
                        },
                        body=PDF_BYTES,
                    )
                except Exception:
                    # Navigating away is expected to abort a delayed response.
                    pass
            else:
                code = response["code"]
                body = {
                    "code": code,
                    "message": response.get("message", "Safe export failure"),
                    "details": {},
                    "retryable": response.get("retryable", status >= 500 or status == 429),
                }
                try:
                    await route.fulfill(
                        status=status,
                        content_type="application/json",
                        body=json.dumps(body),
                    )
                except Exception:
                    pass
            return

        if request.method == "DELETE" and path.startswith("/api/notes/"):
            if self.delete_conflict:
                await route.fulfill(
                    status=409,
                    content_type="application/json",
                    body=json.dumps(
                        {
                            "code": "stale_revision",
                            "message": "The note changed before deletion.",
                            "details": {},
                            "retryable": False,
                        }
                    ),
                )
            else:
                await route.fulfill(status=204)
            return

        if path.startswith("/api/notes/"):
            note_id = path.split("/")[3]
            self.note_requests.append(note_id)
            delay = self.note_delays.get(note_id, 0)
            if delay:
                await asyncio.sleep(delay)
            try:
                await route.fulfill(
                    status=200,
                    content_type="application/json",
                    body=json.dumps(self.notes[note_id]),
                )
            except Exception:
                pass
            return

        if path == "/api/notes":
            await route.fulfill(status=200, content_type="application/json", body="[]")
            return
        if path == "/api/notes/count":
            await route.fulfill(
                status=200, content_type="application/json", body='{"total":0}'
            )
            return
        await route.abort()


async def open_note(browser, api: MockApi, note_id="note-a"):
    context = await browser.new_context(accept_downloads=True)
    await context.add_init_script(
        """
        window.__exportUrls = { created: 0, revoked: 0 };
        const create = URL.createObjectURL.bind(URL);
        const revoke = URL.revokeObjectURL.bind(URL);
        URL.createObjectURL = value => {
          window.__exportUrls.created += 1;
          return create(value);
        };
        URL.revokeObjectURL = value => {
          window.__exportUrls.revoked += 1;
          return revoke(value);
        };
        """
    )
    await context.route("**/api/**", api.handle)
    page = await context.new_page()
    await page.goto(f"{BASE_URL}/notes/{note_id}/show")
    await expect(
        page.get_by_role("heading", name=api.notes[note_id]["title"], level=2, exact=True)
    ).to_be_visible()
    return context, page


async def open_export_menu(page):
    trigger = page.get_by_role("button", name="Export note")
    await trigger.click()
    await expect(page.get_by_role("menu", name="Export note formats")).to_be_visible()
    return trigger


async def test_success_duplicate_and_cleanup(browser):
    api = MockApi()
    api.pdf_responses = [{"delay": 0.35}]
    context, page = await open_note(browser, api)
    await open_export_menu(page)
    pdf = page.get_by_role("menuitem", name="PDF (.pdf) Printable document")
    async with page.expect_download() as download_info:
        await pdf.evaluate("element => { element.click(); element.click(); }")
        await expect(page.get_by_text("Preparing PDF download…", exact=True)).to_be_visible()
        await open_export_menu(page)
        await expect(
            page.get_by_role("menuitem", name="Markdown (.md) Original source")
        ).to_have_attribute("aria-disabled", "false")
    download = await download_info.value
    assert download.suggested_filename == "Alpha report.pdf"
    assert Path(await download.path()).read_bytes().startswith(b"%PDF-")
    assert len(api.pdf_requests) == 1, api.pdf_requests
    await expect(page.get_by_text("PDF download initiated.", exact=True)).to_be_visible()
    await page.wait_for_timeout(1_150)
    urls = await page.evaluate("window.__exportUrls")
    assert urls == {"created": 1, "revoked": 1}, urls
    assert await page.locator("a[download][hidden]").count() == 0
    await context.close()


async def test_keyboard_focus(browser):
    api = MockApi()
    context, page = await open_note(browser, api)
    trigger = page.get_by_role("button", name="Export note")
    await trigger.focus()
    await trigger.press("Enter")
    markdown = page.get_by_role("menuitem", name="Markdown (.md) Original source")
    await expect(markdown).to_be_focused()
    await markdown.press("ArrowDown")
    pdf = page.get_by_role("menuitem", name="PDF (.pdf) Printable document")
    await expect(pdf).to_be_focused()
    async with page.expect_download():
        await pdf.press("Enter")
    await expect(trigger).to_be_focused()
    await trigger.press("Enter")
    await expect(markdown).to_be_focused()
    await markdown.press("Escape")
    await expect(trigger).to_be_focused()
    await context.close()


async def test_navigation_and_late_load_suppression(browser):
    api = MockApi()
    api.note_delays["note-a"] = 0.5
    context = await browser.new_context(accept_downloads=True)
    await context.route("**/api/**", api.handle)
    page = await context.new_page()
    downloads = []
    page.on("download", lambda download: downloads.append(download))
    await page.goto(f"{BASE_URL}/notes/note-a/show")
    await page.evaluate(
        """() => {
          history.pushState({}, '', '/notes/note-b/show');
          dispatchEvent(new PopStateEvent('popstate'));
        }"""
    )
    await expect(page.get_by_role("heading", name="Beta report", level=2)).to_be_visible()
    await page.wait_for_timeout(650)
    await expect(page.get_by_role("heading", name="Alpha report", level=2)).to_have_count(0)

    api.pdf_responses = [{"delay": 0.6}]
    await open_export_menu(page)
    await page.get_by_role("menuitem", name="PDF (.pdf) Printable document").click()
    await expect(page.get_by_text("Preparing PDF download…", exact=True)).to_be_visible()
    await page.evaluate(
        """() => {
          history.pushState({}, '', '/notes/note-a/show');
          dispatchEvent(new PopStateEvent('popstate'));
        }"""
    )
    await expect(page.get_by_role("heading", name="Alpha report", level=2)).to_be_visible()
    await page.wait_for_timeout(750)
    assert downloads == [], "late PDF completion initiated a download after navigation"
    await context.close()


async def test_capability_disabled_and_failure(browser):
    api = MockApi()
    api.capability = {"markdown": True, "pdf": False}
    context, page = await open_note(browser, api)
    await open_export_menu(page)
    pdf = page.get_by_role("menuitem", name="PDF (.pdf) PDF export is not configured.")
    await expect(pdf).to_have_attribute("aria-disabled", "true")
    await expect(page.get_by_role("menuitem", name="Markdown (.md) Original source")).to_have_attribute(
        "aria-disabled", "false"
    )
    await context.close()

    api = MockApi()
    api.capability_failure = True
    context, page = await open_note(browser, api)
    await expect(page.get_by_text("PDF availability could not be loaded. Markdown export remains available.")).to_be_visible()
    retry_capabilities = page.get_by_role("button", name="Retry availability")
    await expect(retry_capabilities).to_be_visible()
    api.capability_failure = False
    await retry_capabilities.click()
    await expect(retry_capabilities).to_have_count(0)
    await open_export_menu(page)
    await expect(page.get_by_role("menuitem", name="Markdown (.md) Original source")).to_be_visible()
    await expect(page.get_by_role("menuitem", name="PDF (.pdf) Printable document")).to_have_attribute(
        "aria-disabled", "false"
    )
    await context.close()


async def test_stale_reload_and_manual_recovery(browser):
    api = MockApi()
    api.pdf_responses = [
        {
            "status": 409,
            "code": "stale_revision",
            "message": "The note changed. Reload it before exporting.",
            "retryable": False,
        },
        {},
    ]
    context, page = await open_note(browser, api)
    await open_export_menu(page)
    await page.get_by_role("menuitem", name="PDF (.pdf) Printable document").click()
    reload_button = page.get_by_role("button", name="Reload note")
    await expect(reload_button).to_be_visible()
    assert len(api.pdf_requests) == 1
    note_reads_before = len(api.note_requests)
    await reload_button.click()
    await expect(reload_button).to_have_count(0)
    await page.wait_for_timeout(150)
    assert len(api.note_requests) == note_reads_before + 1
    assert len(api.pdf_requests) == 1, "reload automatically retried PDF export"
    await open_export_menu(page)
    async with page.expect_download():
        await page.get_by_role("menuitem", name="PDF (.pdf) Printable document").click()
    assert len(api.pdf_requests) == 2
    await context.close()


async def test_same_note_reload_aborts_active_export(browser):
    api = MockApi()
    api.delete_conflict = True
    api.pdf_responses = [{"delay": 0.7}]
    context, page = await open_note(browser, api)
    downloads = []
    page.on("download", lambda download: downloads.append(download))
    await open_export_menu(page)
    await page.get_by_role("menuitem", name="PDF (.pdf) Printable document").click()
    await expect(page.get_by_text("Preparing PDF download…", exact=True)).to_be_visible()
    await page.get_by_role("button", name="Delete note").click()
    await page.get_by_role("button", name="Move to Trash").click()
    reload_button = page.get_by_role("button", name="Reload note")
    await expect(reload_button).to_be_visible()
    note_reads_before = len(api.note_requests)
    await reload_button.click()
    await page.wait_for_timeout(850)
    assert len(api.note_requests) == note_reads_before + 1
    assert downloads == [], "same-note reload allowed an active PDF to download"
    await expect(page.get_by_text("Preparing PDF download…", exact=True)).to_have_count(0)
    await context.close()


async def test_failure_retry_and_mime_defense(browser):
    api = MockApi()
    api.pdf_responses = [
        {
            "status": 503,
            "code": "pdf_renderer_unavailable",
            "message": "The PDF renderer is temporarily unavailable.",
        },
        {"content_type": "text/html"},
        {},
    ]
    context, page = await open_note(browser, api)
    downloads = []
    page.on("download", lambda download: downloads.append(download))
    await open_export_menu(page)
    await page.get_by_role("menuitem", name="PDF (.pdf) Printable document").click()
    retry = page.get_by_role("button", name="Retry PDF export")
    await expect(retry).to_be_visible()
    await retry.click()
    await expect(retry).to_be_visible()
    assert downloads == [], "non-PDF response initiated a download"
    async with page.expect_download():
        await retry.click()
    assert len(api.pdf_requests) == 3
    await context.close()


async def run(browser_name: str):
    async with async_playwright() as playwright:
        browser_type = getattr(playwright, browser_name)
        browser = await browser_type.launch(headless=True)
        print(f"BROWSER [{browser_name}] {browser.version}")
        try:
            for test in (
                test_success_duplicate_and_cleanup,
                test_keyboard_focus,
                test_navigation_and_late_load_suppression,
                test_capability_disabled_and_failure,
                test_stale_reload_and_manual_recovery,
                test_same_note_reload_aborts_active_export,
                test_failure_retry_and_mime_defense,
            ):
                await test(browser)
                print(f"PASS [{browser_name}] {test.__name__}")
        finally:
            await browser.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--browser", choices=("chromium", "firefox"), required=True)
    arguments = parser.parse_args()
    asyncio.run(run(arguments.browser))
