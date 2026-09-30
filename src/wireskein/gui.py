"""`wireskein gui`: the viewer (wireskein-web) on a local web server.

    wireskein gui capture.wsc          # opens the browser on that capture (.sr is converted on the fly)
    wireskein gui runs/                # a page listing the captures and runs below that directory

The server listens on 127.0.0.1 only (a remote bench: `ssh -L PORT:127.0.0.1:PORT bench` and open the printed
URL). Every start makes a token; the URL printed carries it once, the browser keeps it as a cookie, and requests
without it, or with a Host other than the local one, are refused - another web page cannot read local files
through it. It serves only the viewer and the .wsc / .sr files below the directory given.

The viewer comes with the wheel (src/wireskein/web, the pinned wireskein-web release, tools/fetch_web.py);
WIRESKEIN_WEB_DIR points it at another build (development of wireskein-web).
"""

from __future__ import annotations

import html
import json
import mimetypes
import os
import secrets
import tempfile
import threading
import urllib.parse
import webbrowser
from http.cookies import SimpleCookie
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from . import __version__

API = 1
COOKIE = "wireskein_token"
CAPTURES = (".wsc", ".sr")
LOCAL = {"127.0.0.1", "localhost", "::1"}


def web_dir() -> Path:
    env = os.environ.get("WIRESKEIN_WEB_DIR")
    d = Path(env) if env else Path(__file__).resolve().parent / "web"
    if not (d / "index.html").exists():
        raise FileNotFoundError(f"the viewer is not at {d} (run tools/fetch_web.py, or set WIRESKEIN_WEB_DIR "
                                f"to a wireskein-web site/ directory)")
    return d


def web_version(d: Path) -> str | None:
    v = d / "VERSION"
    return v.read_text().strip() if v.exists() else None


class Gui:
    """The server. root: the directory whose captures it may serve."""

    def __init__(self, root: Path, host: str = "127.0.0.1", port: int = 0, web: Path | None = None):
        self.root = root.resolve()
        self.web = (web or web_dir()).resolve()
        self.token = secrets.token_urlsafe(24)
        gui = self

        class Handler(_Handler):
            server_gui = gui
        self.httpd = ThreadingHTTPServer((host, port), Handler)
        self.host, self.port = self.httpd.server_address[:2]

    def url(self, path: str = "/") -> str:
        """The first URL to open: it carries the token."""
        sep = "&" if "?" in path else "?"
        return f"http://127.0.0.1:{self.port}{path}{sep}token={self.token}"

    def page_for(self, target: Path) -> str:
        """The page showing a capture or a directory."""
        target = target.resolve()
        if target.is_dir():
            return "/browse" + ("?dir=" + urllib.parse.quote(str(target.relative_to(self.root)))
                                if target != self.root else "")
        rel = urllib.parse.quote(target.relative_to(self.root).as_posix())
        return f"/?file=/files/{rel}"

    def serve(self) -> None:
        self.httpd.serve_forever()

    def start(self) -> threading.Thread:
        t = threading.Thread(target=self.serve, daemon=True)
        t.start()
        return t

    def stop(self) -> None:
        self.httpd.shutdown()
        self.httpd.server_close()


class _Handler(BaseHTTPRequestHandler):
    server_gui: Gui
    server_version = f"wireskein/{__version__}"

    def log_message(self, format, *args):     # quiet: the terminal is the user's
        pass

    # ---- checks ----

    def _host_ok(self) -> bool:
        host = (self.headers.get("Host") or "").strip()
        name = host.rsplit(":", 1)[0].strip("[]") if host.count(":") <= 1 or host.startswith("[") else host
        return name in LOCAL

    def _token_ok(self, query: dict) -> bool:
        cookie = SimpleCookie(self.headers.get("Cookie") or "")
        got = cookie[COOKIE].value if COOKIE in cookie else query.get("token", [None])[0]
        return got is not None and secrets.compare_digest(got, self.server_gui.token)

    def _send(self, code: int, body: bytes, ctype: str = "text/plain; charset=utf-8", extra: dict | None = None):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    # ---- routes ----

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        query = urllib.parse.parse_qs(url.query)
        if not self._host_ok():
            return self._send(403, b"wireskein gui answers only to localhost\n")
        if not self._token_ok(query):
            return self._send(403, b"open the URL wireskein gui printed (it carries the token)\n")
        if "token" in query:        # first visit: keep the token as a cookie, drop it from the address bar
            rest = urllib.parse.urlencode({k: v for k, v in query.items() if k != "token"}, doseq=True)
            return self._send(303, b"", extra={
                "Location": url.path + ("?" + rest if rest else ""),
                "Set-Cookie": f"{COOKIE}={self.server_gui.token}; Path=/; HttpOnly; SameSite=Strict"})
        path = urllib.parse.unquote(url.path)
        try:
            if path == "/api/version":
                return self._json({"wireskein": __version__, "web": web_version(self.server_gui.web), "api": API})
            if path.startswith("/files/"):
                return self._capture(path[len("/files/"):])
            if path == "/browse":
                return self._browse(query.get("dir", [""])[0])
            return self._static(path)
        except FileNotFoundError as e:
            return self._send(404, f"{e}\n".encode())
        except PermissionError as e:
            return self._send(403, f"{e}\n".encode())
        except ValueError as e:
            return self._send(422, f"{e}\n".encode())

    def _json(self, obj) -> None:
        self._send(200, json.dumps(obj).encode(), "application/json")

    def _inside(self, base: Path, rel: str) -> Path:
        p = (base / rel.lstrip("/")).resolve()
        if p != base and not p.is_relative_to(base):
            raise PermissionError("outside the served directory")
        return p

    def _static(self, path: str) -> None:
        web = self.server_gui.web
        p = self._inside(web, path)
        if p.is_dir():
            p = p / "index.html"
        if not p.is_file():
            raise FileNotFoundError(path)
        ctype = mimetypes.guess_type(p.name)[0] or "application/octet-stream"
        if p.suffix == ".js":
            ctype = "text/javascript"
        self._send(200, p.read_bytes(), ctype + ("; charset=utf-8" if ctype.startswith("text/") else ""))

    def _capture(self, rel: str) -> None:
        p = self._inside(self.server_gui.root, rel)
        if p.suffix not in CAPTURES or not p.is_file():
            raise FileNotFoundError(rel)
        if p.suffix == ".wsc":
            return self._send(200, p.read_bytes(), "application/octet-stream")
        # a .sr is shown as the .wsc it converts to (slow channels get their real rate back when it came from one)
        from .analyze import load, save
        cap = load(p)
        meta = {k: v for k, v in cap.meta.items()
                if k not in ("file", "sr_file", "tick_hz", "unitsize", "fixture", "extras", "skipped_channels")}
        with tempfile.TemporaryDirectory() as d:
            out = save(Path(d) / (p.stem + ".wsc"), cap, **meta)
            self._send(200, out.read_bytes(), "application/octet-stream")

    def _browse(self, rel: str) -> None:
        root = self.server_gui.root
        base = self._inside(root, rel)
        if not base.is_dir():
            raise FileNotFoundError(rel)
        rows = []
        for p in sorted(base.rglob("*")):
            if len(p.relative_to(base).parts) > 4:
                continue
            r = p.relative_to(root).as_posix()
            if p.is_file() and p.suffix in CAPTURES:
                link = "/?file=/files/" + urllib.parse.quote(r)
                rows.append(f'<li><a href="{html.escape(link)}">{html.escape(r)}</a> '
                            f'<small>{p.stat().st_size:,} bytes</small></li>')
            elif p.is_file() and p.name == "run.json":
                rows.append(f"<li><b>{html.escape(p.parent.relative_to(root).as_posix() or '.')}</b> "
                            f"<small>recorded run (its captures are listed here too)</small></li>")
        title = html.escape(str(base))
        body = (f"<!doctype html><meta charset=utf-8><title>wireskein gui</title>"
                f"<style>body{{font:14px system-ui;margin:20px}}small{{color:#777}}</style>"
                f"<h1>{title}</h1><ul>{''.join(rows) or '<li>no .wsc or .sr files here</li>'}</ul>"
                f"<p><small>wireskein {__version__} · wireskein-web {web_version(self.server_gui.web) or '(dev)'}"
                f"</small></p>")
        self._send(200, body.encode(), "text/html; charset=utf-8")


def main(path: Path, port: int = 0, open_browser: bool = True) -> None:
    path = path.resolve()
    if not path.exists():
        raise FileNotFoundError(path)
    root = path if path.is_dir() else path.parent
    gui = Gui(root, port=port)
    url = gui.url(gui.page_for(path))
    print(f"wireskein gui: {url}", flush=True)
    print(f"  serving {root} on 127.0.0.1:{gui.port} (a remote bench: ssh -L {gui.port}:127.0.0.1:{gui.port} HOST)", flush=True)
    print("  Ctrl-C stops it", flush=True)
    if open_browser:
        webbrowser.open(url)
    try:
        gui.serve()
    except KeyboardInterrupt:
        pass
    finally:
        gui.httpd.server_close()
