"""`wireskein gui`: the local server around the viewer (wireskein-web)."""

import http.client
import json
import urllib.parse

import pytest

from wireskein import fileformat
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
    fileformat.write(data / "a.wireskein", 1000, [fileformat.Channel("P0", fileformat.pack(bytes([0, 1] * 8)), 16)])
    fileformat.write(data / "run1" / "c0001.wireskein", 1000, [fileformat.Channel("P0", fileformat.pack(bytes(8)), 8)])
    (data / "run1" / "run.json").write_text("{}")
    (tmp_path / "secret.wireskein").write_bytes(b"outside")
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
    status, headers, _ = get(gui, f"/?file=/files/a.wireskein&token={gui.token}")
    assert status == 303 and headers["Location"] == "/?file=%2Ffiles%2Fa.wireskein"
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
    status, _, body = get(gui, "/files/a.wireskein", cookie=cookie(gui))
    assert status == 200 and body == (data / "a.wireskein").read_bytes()
    assert get(gui, "/files/run1/c0001.wireskein", cookie=cookie(gui))[0] == 200
    version = json.loads(get(gui, "/api/version", cookie=cookie(gui))[2])
    assert version["web"] == "9.9.9" and version["api"] == 1


@pytest.mark.parametrize("path", ["/files/../secret.wireskein", "/files/%2e%2e/secret.wireskein", "/../secret.wireskein"])
def test_nothing_outside_the_served_directories(served, path):
    gui, _ = served
    assert get(gui, path, cookie=cookie(gui))[0] in (403, 404)


def test_only_capture_files(served):
    gui, _ = served
    assert get(gui, "/files/run1/run.json", cookie=cookie(gui))[0] == 404


def test_sr_is_served_as_wsc(served, tmp_path):
    gui, data = served
    from wireskein.analyze import load, save
    save(data / "b.sr", load(data / "a.wireskein"))
    status, _, body = get(gui, "/files/b.sr", cookie=cookie(gui))
    assert status == 200
    (tmp_path / "back.wireskein").write_bytes(body)
    head, chans = fileformat.read(tmp_path / "back.wireskein")
    assert [c.name for c in chans] == ["P0"] and fileformat.unpack(chans[0]) == bytes([0, 1] * 8)


def test_browse_lists_captures_and_runs(served):
    gui, _ = served
    status, _, body = get(gui, "/browse", cookie=cookie(gui))
    text = body.decode()
    assert status == 200 and "a.wireskein" in text and "run1/c0001.wireskein" in text and "recorded run" in text
    assert urllib.parse.quote("/files/run1/c0001.wireskein", safe="/") in text


def test_page_for(served):
    gui, data = served
    assert gui.page_for(data / "a.wireskein") == "/?file=/files/a.wireskein"
    assert gui.page_for(data) == "/browse"
    assert gui.page_for(data / "run1") == "/browse?dir=run1"
    assert gui.url("/browse").endswith(f"/browse?token={gui.token}")


def test_captures_are_found_by_content(served):
    gui, data = served
    import shutil
    import zipfile
    shutil.copy(data / "a.wireskein", data / "renamed.dat")
    with zipfile.ZipFile(data / "other.wireskein", "w") as z:          # a zip, but not a WireSkein file
        z.writestr("readme.txt", "hi")
    text = get(gui, "/browse", cookie=cookie(gui))[2].decode()
    assert "renamed.dat" in text and "other.wireskein" not in text
    assert get(gui, "/files/renamed.dat", cookie=cookie(gui))[0] == 200
    assert get(gui, "/files/other.wireskein", cookie=cookie(gui))[0] == 404


def call(gui, method, path, body, origin=None, ctype="application/json"):
    c = http.client.HTTPConnection("127.0.0.1", gui.port, timeout=30)
    headers = {"Host": f"127.0.0.1:{gui.port}", "Cookie": cookie(gui), "Content-Type": ctype,
               "Origin": origin or f"http://127.0.0.1:{gui.port}"}
    c.request(method, path, body=json.dumps(body), headers=headers)
    r = c.getresponse()
    return r.status, r.read()


def test_notes_and_markers_from_the_viewer(served):
    gui, data = served
    assert call(gui, "POST", "/api/note?file=a.wireskein", {"text": "seen in the viewer"})[0] == 200
    assert fileformat.notes(data / "a.wireskein")[-1]["content"] == "seen in the viewer"
    assert fileformat.notes(data / "a.wireskein")[-1]["by"] == "viewer"
    marks = [{"t": 3, "label": "start"}, {"t": 5, "end": 9, "label": "burst", "by": "viewer"}]
    assert call(gui, "PUT", "/api/markers?file=a.wireskein", {"markers": marks})[0] == 200
    assert fileformat.markers(data / "a.wireskein") == marks
    assert call(gui, "PUT", "/api/markers?file=a.wireskein", {"markers": [{"t": "x"}]})[0] == 422


def test_writes_need_the_page_itself(served):
    gui, data = served
    body = {"text": "x"}
    assert call(gui, "POST", "/api/note?file=a.wireskein", body, origin="http://evil.example")[0] == 403
    assert call(gui, "POST", "/api/note?file=a.wireskein", body, ctype="text/plain")[0] == 415
    assert call(gui, "POST", "/api/note?file=../secret.wireskein", body)[0] in (403, 404)
    assert fileformat.notes(data / "a.wireskein") == []


def test_annotations_and_checks(served, tmp_path):
    gui, data = served
    from wireskein.runlog import Recorder, level
    rec = Recorder(data / "run2")
    with rec.section(1, "t", expect=[level("P0", 0), level("P0", 1)]):
        rec.capture(rec.armed(), 1000, channels=[fileformat.Channel("P0", fileformat.pack(bytes(8)), 8)])
    rec.close()
    doc = json.loads(get(gui, "/api/checks?file=run2/c0001.wireskein", cookie=cookie(gui))[2])
    assert [(r["check"], r["ok"]) for r in doc["results"]] == [("level", True), ("level", False)]
    assert json.loads(get(gui, "/api/checks?file=a.wireskein", cookie=cookie(gui))[2]) == {"run": None, "results": []}
    ann = json.loads(get(gui, "/api/annotations?file=a.wireskein", cookie=cookie(gui))[2])
    assert ann["format"] == "wireskein-annotations/0" and ann["stored"] is False
    assert call(gui, "POST", "/api/annotations?file=a.wireskein", {})[0] == 200
    assert json.loads(get(gui, "/api/annotations?file=a.wireskein", cookie=cookie(gui))[2])["stored"] is True


def test_vcd_is_served_as_wireskein(served, tmp_path):
    gui, data = served
    from wireskein.analyze import load, save
    save(data / "v.vcd", load(data / "a.wireskein"))
    assert "v.vcd" in get(gui, "/browse", cookie=cookie(gui))[2].decode()
    status, _, body = get(gui, "/files/v.vcd", cookie=cookie(gui))
    assert status == 200
    (tmp_path / "back.wireskein").write_bytes(body)
    _, chans = fileformat.read(tmp_path / "back.wireskein")
    assert fileformat.unpack(chans[0]) == bytes([0, 1] * 8)
