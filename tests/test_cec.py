"""HDMI CEC on one line: start bits, blocks of 8 data bits, EOM and ACK; directed messages acknowledged by the
follower, broadcasts not refused. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen


def bit(w, t, b):
    w.set(t, 0)
    w.set(t + (0.6e-3 if b else 1.5e-3), 1)
    return t + 2.4e-3


def message(w, t, blocks, broadcast):
    w.set(t, 0)
    w.set(t + 3.7e-3, 1)
    t += 4.5e-3
    for k, x in enumerate(blocks):
        for i in range(8):
            t = bit(w, t, (x >> (7 - i)) & 1)
        t = bit(w, t, int(k == len(blocks) - 1))            # EOM
        t = bit(w, t, 1 if broadcast else 0)                # ACK as the follower leaves it
    return t + 7e-3


def test_messages():
    msgs = [[0x05, 0x70, 0x30, 0x00], [0x5F, 0x72, 0x01], [0x05, 0x71], [0x0F, 0x36], [0x40]]
    w = gen.Wave(1)
    t = 1e-3
    for m in msgs:
        t = message(w, t, m, (m[0] & 0xF) == 0xF)
    cap = gen.quantize({"CEC": w}, 200e3, t + 2e-3, np.random.default_rng(0))
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "cec"
    got = [[m["initiator"] << 4 | m["destination"]] + ([m["opcode"]] if "opcode" in m else [])
           + list(bytes.fromhex(m.get("data", ""))) for m in c["result"]["messages"]]
    assert got == msgs and all(m["acked"] and m["complete"] for m in c["result"]["messages"])


def test_short_traffic_is_no_uart():
    """Four short messages (a poll among them): read as a UART each bit is a 6-bit character of about 2000 baud;
    CEC's start bits and timing tell (2026-11-21, synth-protocols-54)."""
    msgs = [[0x34, 0x81], [0x1F, 0xC2], [0x15, 0x4D], [0xA4]]
    w = gen.Wave(1)
    t = gen.cec(w, 0.4e-3, msgs)
    cap = gen.quantize({"CEC": w}, 1e6, t + 1e-3, np.random.default_rng(1))
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "cec" and c["verdict"] == "confirmed"


def test_bounces_at_the_edges():
    """A tenth of the edges followed by a pulse of 1-3 samples back (a slow edge crossing the threshold again): the
    lone line is read with pulses far shorter than its runs taken out too (2026-11-21: CEC, DALI, DCC, S/PDIF and the
    other pulse-width readers read nothing under such bounces)."""
    rng = np.random.default_rng(3)
    msgs = [[0x05, 0x70, 0x30, 0x00], [0x5F, 0x72, 0x01], [0x05, 0x71], [0x0F, 0x36], [0x40]]
    w = gen.Wave(1)
    t = gen.cec(w, 1e-3, msgs)
    cap = gen.quantize({"CEC": w}, 200e3, t + 2e-3, rng)
    ch = cap.channels[0]
    e = np.asarray(ch.edges, np.int64)
    sel = e[rng.random(len(e)) < 0.1]
    a = sel + rng.integers(1, 3, len(sel))
    extra = np.concatenate([a, a + rng.integers(1, 3, len(sel))])
    allx = np.concatenate([e, extra])
    vals, cnt = np.unique(allx, return_counts=True)
    from dataclasses import replace
    from wireskein._engine.model import Capture
    cap = Capture(cap.rate, cap.n_samples, [replace(ch, edges=vals[cnt % 2 == 1])])
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "cec"
    got = [[m["initiator"] << 4 | m["destination"]] + ([m["opcode"]] if "opcode" in m else [])
           + list(bytes.fromhex(m.get("data", ""))) for m in c["result"]["messages"]]
    assert got == msgs
