#!/usr/bin/env python3
"""Check that a load-test redirect cannot disclose a bearer credential."""
import http.server
import threading
import unittest

import load


class CredentialRedirectTest(unittest.TestCase):
    def test_redirect_does_not_forward_credentials(self):
        received = []

        class Destination(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                received.append(self.headers.get('Authorization') is not None)
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'{}')

            def log_message(self, *args):
                pass

        destination = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Destination)

        class Origin(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(302)
                self.send_header('Location', f'http://127.0.0.1:{destination.server_port}/sink')
                self.end_headers()

            def log_message(self, *args):
                pass

        origin = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Origin)
        servers = [origin, destination]
        threads = [threading.Thread(target=server.serve_forever, daemon=True) for server in servers]
        for thread in threads:
            thread.start()
        try:
            driver = load.Driver(f'http://127.0.0.1:{origin.server_port}', {'author': 'test-only-credential'}, 2)
            with self.assertRaisesRegex(load.Failure, 'HTTP 302'):
                driver.request('author', '/api/repositories')
            self.assertEqual(received, [], 'redirect destination received a request')
        finally:
            for server in servers:
                server.shutdown()
                server.server_close()
            for thread in threads:
                thread.join(timeout=2)


if __name__ == '__main__':
    unittest.main()
