#!/usr/bin/env python3
"""Small real-Safari smoke test for the macOS workflow."""

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait


class ApiHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/api/export/capabilities":
            self.respond({"markdown": True, "pdf": False})
        elif self.path == "/api/notes/safari-smoke":
            self.respond(
                {
                    "id": "safari-smoke",
                    "title": "Safari export smoke",
                    "content": "# Safari\n\nSmoke test.",
                    "attachments": [],
                    "labels": [],
                    "revision": 1,
                }
            )
        else:
            self.send_error(404)

    def respond(self, payload):
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", 6222), ApiHandler)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
driver = webdriver.Safari()
try:
    driver.get("http://127.0.0.1:6221/notes/safari-smoke/show")
    wait = WebDriverWait(driver, 20)
    wait.until(lambda value: "Safari export smoke" in value.page_source)
    trigger = driver.find_element(By.CSS_SELECTOR, "button[aria-label='Export note']")
    trigger.click()
    wait.until(lambda value: value.find_element(By.CSS_SELECTOR, "[role='menu']").is_displayed())
    items = driver.find_elements(By.CSS_SELECTOR, "[role='menuitem']")
    assert len(items) == 2
    assert items[0].get_attribute("aria-disabled") == "false"
    assert items[1].get_attribute("aria-disabled") == "true"
    assert "PDF export is not configured." in items[1].text
finally:
    driver.quit()
    server.shutdown()
