#!/usr/bin/env python3
"""Check native form-button submission against local Wasm and mocked APIs only."""

import argparse
import asyncio
import json
import re
import traceback
from pathlib import Path
from urllib.parse import parse_qs, urlparse

from playwright.async_api import async_playwright, expect


NOTE_ID = "aaaaaaaa-aaaa-4aaa-baaa-aaaaaaaaaaaa"


class MockApi:
    def __init__(self):
        self.note = {
            "id": NOTE_ID, "title": "Existing note", "content": "Original body",
            "attachments": [], "labels": [], "revision": 7,
            "created_at": 1, "updated_at": 2,
        }
        self.labels = [{"key": "project", "description": "Project label", "value_type": "text"}]
        self.writes = []
        self.searches = []
        self.unexpected = []

    async def handle(self, route):
        request = route.request
        path = urlparse(request.url).path
        method = request.method
        if method == "GET" and path == "/api/labels":
            body = self.labels
        elif method == "GET" and path == "/api/notes/count":
            body = {"total": 1}
        elif method == "GET" and path == "/api/notes":
            body = [self.note]
        elif method == "GET" and path == f"/api/notes/{NOTE_ID}":
            body = self.note
        elif method == "GET" and path == "/api/export/capabilities":
            body = {"markdown": True, "pdf": False}
        elif method == "POST" and path == "/api/notes/search":
            self.searches.append(request.post_data_json)
            body = []
        elif method == "POST" and path == "/api/notes":
            payload = request.post_data_json
            self.writes.append((method, path, payload))
            self.note.update(payload, revision=1)
            body = {"id": NOTE_ID}
        elif method == "PUT" and path == f"/api/notes/{NOTE_ID}":
            payload = request.post_data_json
            self.writes.append((method, path, payload))
            self.note.update({key: value for key, value in payload.items() if key != "expected_revision"})
            self.note["revision"] += 1
            body = self.note
        elif method == "POST" and path == "/api/labels":
            payload = request.post_data_json
            self.writes.append((method, path, payload))
            self.labels.append(payload)
            body = {}
        elif method == "PUT" and path == "/api/labels/project":
            payload = request.post_data_json
            self.writes.append((method, path, payload))
            self.labels[0].update(payload)
            body = {}
        else:
            self.unexpected.append((method, request.url))
            await route.abort()
            return
        await route.fulfill(status=200, content_type="application/json", body=json.dumps(body))


async def click_request(page, button, method, path):
    async with page.expect_request(
        lambda request: request.method == method and urlparse(request.url).path == path,
        timeout=5000,
    ) as pending:
        await button.click()
    request = await pending.value
    response = await request.response()
    assert response is not None and response.ok, "Mocked form request did not succeed"
    return request


async def new_note(page, api, base_url):
    await page.goto(f"{base_url}/notes/new")
    await page.get_by_label("Title", exact=True).fill("Created through Save")
    await page.get_by_role("textbox", name="Markdown source", exact=True).fill("New markdown body")
    await page.locator(".label-key-input").fill("project")
    await page.locator(".label-value-input").fill("browser")
    await page.get_by_role("button", name="Add label", exact=True).click()
    await expect(page.locator(".applied-labels")).to_contain_text("project: browser")
    await page.get_by_role("tab", name="Preview", exact=True).click()
    await expect(page.get_by_role("tab", name="Preview", exact=True)).to_have_attribute("aria-selected", "true")
    await page.get_by_role("tab", name="Write", exact=True).click()
    await expect(page.get_by_role("textbox", name="Markdown source", exact=True)).to_be_visible()
    assert api.writes == [], "Add label/Write/Preview submitted the note"
    expected = {"title": "Created through Save", "content": "New markdown body",
                "labels": [["project", "browser"]], "attachments": []}
    request = await click_request(page, page.get_by_role("button", name="Save note", exact=True), "POST", "/api/notes")
    assert request.post_data_json == expected
    await expect(page).to_have_url(re.compile(r"/notes(?:\?.*)?$"))
    await expect(page.get_by_role("link", name=expected["title"], exact=True)).to_be_visible()
    assert api.writes == [("POST", "/api/notes", expected)]


async def edit_note(page, api, base_url):
    await page.goto(f"{base_url}/notes/{NOTE_ID}/edit")
    await expect(page.get_by_label("Title", exact=True)).to_have_value("Existing note")
    await page.get_by_label("Title", exact=True).fill("Edited through Save")
    await page.get_by_role("textbox", name="Markdown source", exact=True).fill("Edited markdown body")
    expected = {"title": "Edited through Save", "content": "Edited markdown body",
                "labels": [], "attachments": [], "expected_revision": 7}
    request = await click_request(page, page.get_by_role("button", name="Save changes", exact=True), "PUT", f"/api/notes/{NOTE_ID}")
    assert request.post_data_json == expected
    await expect(page).to_have_url(f"{base_url}/notes/{NOTE_ID}/show")
    await expect(page.get_by_role("heading", name=expected["title"], exact=True)).to_be_visible()
    assert api.writes == [("PUT", f"/api/notes/{NOTE_ID}", expected)]


async def create_label(page, api, base_url):
    await page.goto(f"{base_url}/labels")
    await page.get_by_role("button", name="New label", exact=True).click()
    dialog = page.get_by_role("dialog", name="New label", exact=True)
    await dialog.get_by_label("Key", exact=True).fill("team")
    await dialog.get_by_label("Description", exact=True).fill("Browser team")
    expected = {"key": "team", "description": "Browser team", "value_type": "text"}
    request = await click_request(page, dialog.get_by_role("button", name="Create label", exact=True), "POST", "/api/labels")
    assert request.post_data_json == expected
    await expect(dialog).to_have_count(0)
    await expect(page.locator(".label-row").filter(has_text="Browser team")).to_be_visible()
    assert api.writes == [("POST", "/api/labels", expected)]


async def edit_label(page, api, base_url):
    await page.goto(f"{base_url}/labels")
    await page.locator(".label-row").filter(has_text="Project label").get_by_role("button", name="Edit", exact=True).click()
    dialog = page.get_by_role("dialog", name="Edit label", exact=True)
    await dialog.get_by_label("Description", exact=True).fill("Updated browser label")
    await dialog.get_by_role("combobox").select_option("number")
    expected = {"description": "Updated browser label", "value_type": "number"}
    request = await click_request(page, dialog.get_by_role("button", name="Save changes", exact=True), "PUT", "/api/labels/project")
    assert request.post_data_json == expected
    await expect(dialog).to_have_count(0)
    await expect(page.locator(".label-row").filter(has_text="Updated browser label")).to_be_visible()
    assert api.writes == [("PUT", "/api/labels/project", expected)]


async def search(page, api, base_url):
    await page.goto(f"{base_url}/notes")
    await expect(page.get_by_role("link", name="Existing note", exact=True)).to_be_visible()
    await page.get_by_placeholder("Retrieve by title or content", exact=True).fill("browser search phrase")
    request = await click_request(page, page.get_by_role("button", name="Retrieve", exact=True), "POST", "/api/notes/search")
    body = request.post_data_json
    assert body["query"] == "browser search phrase"
    assert isinstance(body["limit"], int) and body["limit"] > 0
    assert set(body) == {"query", "limit"}
    await expect(page).to_have_url(re.compile(r"[?&]search=browser"))
    assert parse_qs(urlparse(page.url).query)["search"] == [body["query"]]
    assert api.searches == [body]
    assert api.writes == []


CASES = {test.__name__: test for test in (new_note, edit_note, create_label, edit_label, search)}


async def main(args):
    assert urlparse(args.base_url).hostname in {"127.0.0.1", "localhost", "::1"}
    args.output.mkdir(parents=True, exist_ok=True)
    results = {}
    async with async_playwright() as playwright:
        launch = {"executable_path": args.executable} if args.executable else {}
        browser = await playwright.chromium.launch(headless=True, **launch)
        try:
            for name in CASES if args.case == "all" else [args.case]:
                api = MockApi()
                context = await browser.new_context()
                await context.route("**/api/**", api.handle)
                page = await context.new_page()
                errors = []
                page.on("pageerror", lambda error: errors.append(str(error)))
                try:
                    await CASES[name](page, api, args.base_url.rstrip("/"))
                    assert not api.unexpected, api.unexpected
                    assert not errors, errors
                    results[name] = {"status": "PASS"}
                except Exception:
                    results[name] = {"status": "FAIL", "error": traceback.format_exc(),
                                     "unexpected": api.unexpected, "page_errors": errors}
                    await page.screenshot(path=str(args.output / f"{name}.png"), full_page=True)
                finally:
                    await context.close()
                print(json.dumps({name: results[name]}), flush=True)
        finally:
            await browser.close()
    (args.output / "browser-result.json").write_text(json.dumps(results, indent=2) + "\n")
    return 0 if all(result["status"] == "PASS" for result in results.values()) else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:6221")
    parser.add_argument("--executable", help="Optional system Chromium executable")
    parser.add_argument("--output", type=Path, default=Path("/tmp/agent-note-form-submit-browser"))
    parser.add_argument("--case", choices=["all", *CASES], default="all")
    raise SystemExit(asyncio.run(main(parser.parse_args())))
