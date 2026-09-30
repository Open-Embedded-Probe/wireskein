"""decode/annotations.json: what analyze found, as rows of spans for the viewer."""

import numpy as np

from wireskein import annotate, fileformat
from wireskein._engine import fileio


def uart_file(tmp_path, text=b"Hi!\n"):
    rate, baud = 1_000_000, 115_200
    bits = [1] * 300
    for byte in text * 8:
        bits += [0] + [(byte >> k) & 1 for k in range(8)] + [1, 1]
    bits += [1] * 300
    per = rate / baud
    lv = np.array(bits, np.uint8)[(np.arange(int(len(bits) * per)) / per).astype(int)]
    p = tmp_path / "u.wireskein"
    fileformat.write(p, rate, [fileformat.Channel("TX", fileformat.pack(lv.tobytes()), len(lv))])
    return p


def test_uart_characters_become_a_row_under_their_line(tmp_path):
    p = uart_file(tmp_path)
    doc = annotate.build(fileio.load(p))
    assert doc["format"] == "wireskein-annotations/0"
    (row,) = [r for r in doc["rows"] if r["name"].startswith("uart")]
    assert row["near"] == "TX"
    assert "".join(i["text"] for i in row["items"][:3]) == "Hi!"
    first = row["items"][0]
    assert first["e"] > first["s"] and first["level"] == "ok"


def test_saved_in_the_file_and_read_back(tmp_path):
    p = uart_file(tmp_path)
    doc = annotate.build(fileio.load(p))
    annotate.save(p, doc)
    annotate.save(p, doc)                                    # replaces
    assert annotate.load(p) == doc
    assert fileformat.read(p)[1][0].name == "TX"             # the capture is untouched


def test_rows_from_an_i2c_document():
    doc = {"claims": [{"protocol": "i2c", "roles": {"scl": "D3", "sda": "D2"}, "layers": {
        "frames": [{"s": 1, "e": 2, "bits": 10}],
        "transactions": [{"s": 10, "e": 90, "addr": 0x44, "rw": "read", "addr_ack": True, "bytes": "0000", "acks": [True, False]},
                         {"s": 100, "e": 120, "addr": 0x45, "rw": "write", "addr_ack": False, "bytes": "", "acks": []}]}}]}
    (row,) = annotate.rows_of(doc)
    assert row["name"] == "i2c transactions" and row["near"] == "D2"
    assert [(i["text"], i["level"]) for i in row["items"]] == [("R 0x44 00 00", "ok"), ("W 0x45 NACK", "warn")]


def test_put_cannot_touch_the_capture(tmp_path):
    import pytest
    p = uart_file(tmp_path)
    for entry in ("capture.json", "wireskein.json", "ch/0.bits", "../x", "decode/"):
        with pytest.raises(ValueError):
            fileformat.put(p, entry, "x")
