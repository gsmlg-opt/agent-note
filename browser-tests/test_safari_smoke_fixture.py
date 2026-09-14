#!/usr/bin/env python3
"""Native fixture checks for the Safari WebDriver smoke API."""

import http.client
import threading
import unittest

from safari_smoke import ApiHandler, PdfRequestLog


class SafariSmokeFixtureTest(unittest.TestCase):
    def setUp(self):
        self.log = PdfRequestLog()
        handler = ApiHandler.with_request_log(self.log, pdf_delay_seconds=0)
        from http.server import ThreadingHTTPServer

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def get(self, path):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port)
        connection.request("GET", path)
        response = connection.getresponse()
        body = response.read()
        headers = dict(response.getheaders())
        connection.close()
        return response.status, headers, body

    def test_capability_enables_pdf_and_exact_revision_request_returns_pdf(self):
        status, _, body = self.get("/api/export/capabilities")
        self.assertEqual(status, 200)
        self.assertIn(b'"pdf": true', body)

        status, headers, body = self.get(
            "/api/notes/safari-smoke/export/pdf?expected_revision=1"
        )
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/pdf")
        self.assertEqual(headers["X-Note-Revision"], "1")
        self.assertTrue(body.startswith(b"%PDF-"))
        self.assertEqual(
            self.log.requests,
            ["/api/notes/safari-smoke/export/pdf?expected_revision=1"],
        )


if __name__ == "__main__":
    unittest.main()
