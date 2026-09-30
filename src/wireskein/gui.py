"""`wireskein gui`: the viewer (wireskein-web) on a local web server.

    wireskein gui capture.wireskein    # opens the browser on that capture (a .sr is converted on the fly)
    wireskein gui runs/                # a page listing the captures and runs below that directory

The server listens on 127.0.0.1 only (a remote bench: `ssh -L PORT:127.0.0.1:PORT bench` and open the printed
URL). Every start makes a token; the URL printed carries it once, the browser keeps it as a cookie, and requests
without it, or with a Host other than the local one, are refused - another web page cannot read local files
through it. It serves only the viewer and the WireSkein and .sr files below the directory given (told apart
by their content, not their names).

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

from . import __version__, fileformat

API = 1
COOKIE = "wireskein_token"
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
        self.cache: dict = {}                    # (file, mtime) -> annotations computed on request
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

    def _origin_ok(self) -> bool:
        """A write must come from this server's own page (a browser sends Origin on POST / PUT)."""
        origin = self.headers.get("Origin")
        if origin is None:
            return False
        o = urllib.parse.urlsplit(origin)
        return o.scheme == "http" and o.hostname in LOCAL and o.port == self.server_gui.port

    def _write(self, method: str):
        url = urllib.parse.urlsplit(self.path)
        query = urllib.parse.parse_qs(url.query)
        if not self._host_ok() or not self._token_ok({}) or not self._origin_ok():
            return self._send(403, b"writes need this server's page (token cookie, localhost, same origin)\n")
        if (self.headers.get("Content-Type") or "").split(";")[0].strip() != "application/json":
            return self._send(415, b"send application/json\n")
        n = int(self.headers.get("Content-Length") or 0)
        if n > 4 << 20:
            return self._send(413, b"too large\n")
        try:
            body = json.loads(self.rfile.read(n) or b"{}")
            p = self._capture_path(query.get("file", [""])[0])
            if fileformat.sniff(p) != "wireskein":
                raise PermissionError("only WireSkein files can be written to (convert a .sr / .vcd first)")
            path = urllib.parse.unquote(url.path)
            if method == "POST" and path == "/api/note":
                if not isinstance(body.get("text"), str) or not body["text"].strip():
                    raise ValueError("give text")
                return self._json({"note": fileformat.note(p, body["text"], by="viewer")})
            if method == "PUT" and path == "/api/markers":
                fileformat.set_markers(p, body.get("markers", []))
                return self._json({"markers": len(body.get("markers", []))})
            if method == "POST" and path == "/api/annotations":
                from . import annotate
                from .analyze import load
                doc = annotate.build(load(p))
                annotate.save(p, doc)
                return self._json(doc)
            return self._send(404, b"no such API\n")
        except FileNotFoundError as e:
            return self._send(404, f"{e}\n".encode())
        except PermissionError as e:
            return self._send(403, f"{e}\n".encode())
        except ValueError as e:
            return self._send(422, f"{e}\n".encode())

    def do_POST(self):
        self._write("POST")

    def do_PUT(self):
        self._write("PUT")

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
            if path == "/api/annotations":
                return self._annotations(query.get("file", [""])[0])
            if path == "/api/checks":
                return self._checks(query.get("file", [""])[0])
            return self._static(path)
        except FileNotFoundError as e:
            return self._send(404, f"{e}\n".encode())
        except PermissionError as e:
            return self._send(403, f"{e}\n".encode())
        except ValueError as e:
            return self._send(422, f"{e}\n".encode())

    def _json(self, obj) -> None:
        from ._engine.export import dumps
        self._send(200, dumps(obj).encode(), "application/json")

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

    def _capture_path(self, rel: str) -> Path:
        p = self._inside(self.server_gui.root, rel)
        if not p.is_file() or fileformat.sniff(p) is None:
            raise FileNotFoundError(rel)
        return p

    def _annotations(self, rel: str) -> None:
        """The file's decode/annotations.json, or the analysis run now (not stored)."""
        from . import annotate
        from .analyze import load
        p = self._capture_path(rel)
        stored = annotate.load(p) if fileformat.sniff(p) == "wireskein" else None
        if stored is not None:
            return self._json({**stored, "stored": True})
        key = (str(p), p.stat().st_mtime_ns)
        cache = self.server_gui.cache
        if key not in cache:
            cache[key] = annotate.build(load(p))
        return self._json({**cache[key], "stored": False})

    def _checks(self, rel: str) -> None:
        """The run's check results for this capture (when it belongs to a recorded run)."""
        from . import verify
        p = self._capture_path(rel)
        run = p.parent / "run.json"
        if not run.is_file():
            return self._json({"run": None, "results": []})
        rep = verify.verify(p.parent)
        mine = [r for r in rep["results"] if r.get("capture") == p.name]
        keep = ("path", "check", "status", "ok", "reason", "expected", "measured")
        return self._json({"run": p.parent.name, "summary": rep["summary"],
                           "results": json.loads(verify.dumps([{k: r.get(k) for k in keep} for r in mine]))})

    def _capture(self, rel: str) -> None:
        p = self._inside(self.server_gui.root, rel)
        kind = fileformat.sniff(p) if p.is_file() else None
        if kind is None:
            raise FileNotFoundError(rel)
        if kind == "wireskein":
            return self._send(200, p.read_bytes(), "application/octet-stream")
        # a .sr is shown as the WireSkein file it converts to (slow channels get their real rate back when it came from one)
        from .analyze import load, save
        cap = load(p)
        meta = {k: v for k, v in cap.meta.items()
                if k not in ("file", "sr_file", "vcd_file", "vcd_not_read", "tick_hz", "unitsize", "fixture", "extras",
                             "skipped_channels")}
        with tempfile.TemporaryDirectory() as d:
            out = save(Path(d) / (p.stem + fileformat.SUFFIX), cap, **meta)
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
            if p.is_file() and p.name != "run.json" and fileformat.sniff(p):
                link = "/?file=/files/" + urllib.parse.quote(r)
                rows.append(f'<li><input type="checkbox" value="/files/{html.escape(urllib.parse.quote(r))}"> '
                            f'<a href="{html.escape(link)}">{html.escape(r)}</a> '
                            f'<small>{p.stat().st_size:,} bytes</small></li>')
            elif p.is_file() and p.name == "run.json":
                rows.append(f"<li><b>{html.escape(p.parent.relative_to(root).as_posix() or '.')}</b> "
                            f"<small>recorded run (its captures are listed here too)</small></li>")
        title = html.escape(str(base))
        body = (f"<!doctype html><meta charset=utf-8><title>wireskein gui</title>"
                f"<style>body{{font:14px system-ui;margin:20px}}small{{color:#777}}</style>"
                f"<h1>{title}</h1><ul>{''.join(rows) or '<li>no WireSkein or .sr files here</li>'}</ul>"
                f"<p><button id=together disabled>Open the checked ones together</button> "
                f"<small>the first checked is the time reference; the others are drawn on its time "
                f"(aligned with <code>wireskein align --to</code>)</small></p>"
                f"<script>const boxes=[...document.querySelectorAll('input[type=checkbox]')];"
                f"const b=document.getElementById('together');"
                f"const picked=()=>boxes.filter(x=>x.checked).map(x=>x.value);"
                f"boxes.forEach(x=>x.addEventListener('change',()=>{{b.disabled=picked().length<2;}}));"
                f"b.addEventListener('click',()=>{{const [first,...rest]=picked();"
                f"location.href='/?file='+encodeURIComponent(first)+rest.map(r=>'&with='+encodeURIComponent(r)).join('');}});"
                f"</script>"
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
