#!/usr/bin/env python3
"""Serve the WebRTC fixture and retain its one machine-readable result."""

from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import sys


class Handler(SimpleHTTPRequestHandler):
    def do_POST(self):  # noqa: N802 - callback name is fixed by http.server
        if self.path != "/result":
            self.send_error(404)
            return
        length = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(length)
        Path(os.environ["NOIRE_WEBRTC_RESULT_PATH"]).write_bytes(body)
        self.send_response(204)
        self.end_headers()

    def log_message(self, _format, *_arguments):
        return


if __name__ == "__main__":
    root = Path(__file__).resolve().parent
    os.chdir(root)
    server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler)
    server.serve_forever()
