from http.server import BaseHTTPRequestHandler, HTTPServer
import os
import sys

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
INSTALL_PATH = os.path.join(SCRIPT_DIR, "install.sh")

# Fail fast: load install.sh once at startup
try:
    with open(INSTALL_PATH, "rb") as f:
        INSTALL_DATA = f.read()
except OSError as e:
    print(f"Fatal: cannot read install.sh: {e}", file=sys.stderr)
    sys.exit(1)


class Handler(BaseHTTPRequestHandler):
    def _wants_html(self) -> bool:
        accept = self.headers.get("Accept", "")
        return ("text/html" in accept)

    def _send_redirect(self):
        self.send_response(302)
        self.send_header("Location", "https://wasix.org")
        self.end_headers()

    def _send_install_headers(self, include_length: bool = True):
        self.send_response(200)
        self.send_header("Content-Type", "application/x-sh")
        self.send_header("Content-Disposition", 'attachment; filename="install.sh"')
        if include_length:
            self.send_header("Content-Length", str(len(INSTALL_DATA)))
        self.end_headers()

    def do_HEAD(self):
        if self._wants_html():
            self._send_redirect()
            return
        self._send_install_headers(include_length=True)

    def do_GET(self):
        if self._wants_html():
            self._send_redirect()
            return
        self._send_install_headers(include_length=True)
        self.wfile.write(INSTALL_DATA)


if __name__ == "__main__":
    HTTPServer(("0.0.0.0", 8000), Handler).serve_forever()