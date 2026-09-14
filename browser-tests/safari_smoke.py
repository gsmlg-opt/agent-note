#!/usr/bin/env python3
"""Small real-Safari smoke test for the macOS workflow."""

import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


PDF_BYTES = b"%PDF-1.7\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n"


class PdfRequestLog:
    def __init__(self):
        self._requests = []
        self._lock = threading.Lock()

    def record(self, path):
        with self._lock:
            self._requests.append(path)

    @property
    def requests(self):
        with self._lock:
            return list(self._requests)


class ApiHandler(BaseHTTPRequestHandler):
    pdf_request_log = None
    pdf_delay_seconds = 0.5

    @classmethod
    def with_request_log(cls, request_log, pdf_delay_seconds=0.5):
        class ConfiguredApiHandler(cls):
            pdf_request_log = request_log

        ConfiguredApiHandler.pdf_delay_seconds = pdf_delay_seconds
        return ConfiguredApiHandler

    def do_GET(self):
        if self.path == "/api/export/capabilities":
            self.respond_json({"markdown": True, "pdf": True})
        elif self.path == "/api/notes/safari-smoke":
            self.respond_json(
                {
                    "id": "safari-smoke",
                    "title": "Safari export smoke",
                    "content": "# Safari\n\nSmoke test.",
                    "attachments": [],
                    "labels": [],
                    "revision": 1,
                }
            )
        elif self.path == "/api/notes/safari-smoke/export/pdf?expected_revision=1":
            if self.pdf_request_log is not None:
                self.pdf_request_log.record(self.path)
            time.sleep(self.pdf_delay_seconds)
            self.send_response(200)
            self.send_header("Content-Type", "application/pdf")
            self.send_header("X-Note-Revision", "1")
            self.send_header("Content-Length", str(len(PDF_BYTES)))
            self.end_headers()
            self.wfile.write(PDF_BYTES)
        else:
            self.send_error(404)

    def respond_json(self, payload):
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def run_smoke():
    from selenium import webdriver
    from selenium.webdriver.common.by import By
    from selenium.webdriver.support.ui import WebDriverWait

    pdf_requests = PdfRequestLog()
    handler = ApiHandler.with_request_log(pdf_requests, pdf_delay_seconds=1.5)
    server = ThreadingHTTPServer(("127.0.0.1", 6222), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    driver = webdriver.Safari()
    try:
        driver.get("http://127.0.0.1:6221/notes/safari-smoke/show")
        wait = WebDriverWait(driver, 20)
        wait.until(lambda value: "Safari export smoke" in value.page_source)
        trigger = driver.find_element(By.CSS_SELECTOR, "button[aria-label='Export note']")
        trigger.click()
        wait.until(
            lambda value: value.find_element(By.CSS_SELECTOR, "[role='menu']").is_displayed()
        )
        items = driver.find_elements(By.CSS_SELECTOR, "[role='menuitem']")
        assert len(items) == 2
        assert items[0].get_attribute("aria-disabled") == "false"
        wait.until(
            lambda value: value.find_elements(By.CSS_SELECTOR, "[role='menuitem']")[
                1
            ].get_attribute("aria-disabled")
            == "false"
        )
        pdf_item = driver.find_elements(By.CSS_SELECTOR, "[role='menuitem']")[1]
        assert "Printable document" in pdf_item.text
        pdf_item.click()
        wait.until(
            lambda value: value.find_element(
                By.CSS_SELECTOR, "button[aria-label='Export note']"
            ).get_attribute("aria-busy")
            == "true"
        )
        wait.until(lambda value: "Preparing PDF download…" in value.page_source)
        wait.until(lambda value: "PDF download initiated." in value.page_source)
        assert pdf_requests.requests == [
            "/api/notes/safari-smoke/export/pdf?expected_revision=1"
        ]
    finally:
        driver.quit()
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


if __name__ == "__main__":
    run_smoke()
