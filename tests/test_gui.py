"""`wireskein gui`: the local server around the viewer (wireskein-web)."""

import http.client
import json
import urllib.parse

import pytest

from wireskein import wsc
from wireskein.gui import Gui


@pytest.fixture
def served(tmp_path):
    web = tmp_path / "web"                     # a stand-in for the wireskein-web site
    web.mkdir()
    (web / "index.html").write_text("<!doctype html><title>viewer</title>")
    (web / "app.js").write_text("export {};")
    (web / "VERSION").write_text("9.9.9\n")
    data = tmp_path / "data"
    (data / "run1").mkdir(parents=True)
    wsc.write(data / "a.wsc", 1000, [wsc.Channel("P0", wsc.pack(bytes([0, 1] * 8)), 16)])
    wsc.write(data / "run1" / "c0001.wsc", 1000, [wsc.Channel("P0", wsc.pack(bytes(8)), 8)])
    (data / "run1" / "run.json").write_text("{}")
    (tmp_path / "secret.wsc").write_bytes(b"outside")
    gui = Gui(data, web=web)
    gui.start()
    yield gui, data
    gui.stop()


def get(gui, path, host=None, cookie=None):
    c = http.client.HTTPConnection("127.0.0.1", gui.port, timeout=5)
    headers = {"Host": host or f"127.0.0.1:{gui.port}"}
    if cookie:
        headers["Cookie"] = cookie
    c.request("GET", path, headers=headers)
    r = c.getresponse()
    return r.status, dict(r.getheaders()), r.read()


def cookie(gui):
    return f"wireskein_token={gui.token}"


def test_token_is_required_and_becomes_a_cookie(served):
    gui, _ = served
    assert get(gui, "/")[0] == 403
    status, headers, _ = get(gui, f"/?file=/files/a.wsc&token={gui.token}")
    assert status == 303 and headers["Location"] == "/?file=%2Ffiles%2Fa.wsc"
    assert "HttpOnly" in headers["Set-Cookie"] and "SameSite=Strict" in headers["Set-Cookie"]
    assert get(gui, "/", cookie=cookie(gui))[0] == 200
    assert get(gui, "/", cookie="wireskein_token=wrong")[0] == 403


def test_only_localhost_hosts(served):
    gui, _ = served
    assert get(gui, "/", host=f"evil.example:{gui.port}", cookie=cookie(gui))[0] == 403    # DNS rebinding
    assert get(gui, "/", host=f"localhost:{gui.port}", cookie=cookie(gui))[0] == 200
    assert get(gui, "/", host=f"[::1]:{gui.port}", cookie=cookie(gui))[0] == 200


def test_viewer_and_captures(served):
    gui, data = served
    status, headers, body = get(gui, "/app.js", cookie=cookie(gui))
    assert status == 200 and headers["Content-Type"].startswith("text/javascript")
    status, _, body = get(gui, "/files/a.wsc", cookie=cookie(gui))
    assert status == 200 and body == (data / "a.wsc").read_bytes()
    assert get(gui, "/files/run1/c0001.wsc", cookie=cookie(gui))[0] == 200
    version = json.loads(get(gui, "/api/version", cookie=cookie(gui))[2])
    assert version["web"] == "9.9.9" and version["api"] == 1


@pytest.mark.parametrize("path", ["/files/../secret.wsc", "/files/%2e%2e/secret.wsc", "/../secret.wsc"])
def test_nothing_outside_the_served_directories(served, path):
    gui, _ = served
    assert get(gui, path, cookie=cookie(gui))[0] in (403, 404)


def test_only_capture_files(served):
    gui, _ = served
    assert get(gui, "/files/run1/run.json", cookie=cookie(gui))[0] == 404


def test_sr_is_served_as_wsc(served, tmp_path):
    gui, data = served
    from wireskein.analyze import load, save
    save(data / "b.sr", load(data / "a.wsc"))
    status, _, body = get(gui, "/files/b.sr", cookie=cookie(gui))
    assert status == 200
    (tmp_path / "back.wsc").write_bytes(body)
    head, chans = wsc.read(tmp_path / "back.wsc")
    assert [c.name for c in chans] == ["P0"] and wsc.unpack(chans[0]) == bytes([0, 1] * 8)


def test_browse_lists_captures_and_runs(served):
    gui, _ = served
    status, _, body = get(gui, "/browse", cookie=cookie(gui))
    text = body.decode()
    assert status == 200 and "a.wsc" in text and "run1/c0001.wsc" in text and "recorded run" in text
    assert urllib.parse.quote("/files/run1/c0001.wsc", safe="/") in text


def test_page_for(served):
    gui, data = served
    assert gui.page_for(data / "a.wsc") == "/?file=/files/a.wsc"
    assert gui.page_for(data) == "/browse"
    assert gui.page_for(data / "run1") == "/browse?dir=run1"
    assert gui.url("/browse").endswith(f"/browse?token={gui.token}")
