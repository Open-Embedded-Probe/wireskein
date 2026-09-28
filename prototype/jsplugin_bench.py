"""Plugin boundary experiment: the same typed streams given to Python plugins
(in process), JavaScript in an embedded V8 (mini-racer, JSON), and JavaScript in
a separate Node.js process (JSON-only messages, or JSON header + binary arrays).

    PYTHONPATH=. uv run python jsplugin_bench.py [set] [limit]
"""
import json
import os
import struct
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

import numpy as np

from wsproto import corpus, fixture, staged

HERE = Path(__file__).parent / "jsplugins"


def jsonable(fields):
    out = {}
    for k, v in fields.items():
        out[k] = v.tolist() if isinstance(v, np.ndarray) else v
    return out


def binary_message(plugin, fields):
    bufs, meta, blobs = [], {}, []
    for k, v in fields.items():
        if isinstance(v, np.ndarray):
            if v.dtype == np.uint8 or v.dtype == bool:
                a, dt = v.astype("<u1"), "u8"
            elif v.dtype.kind in "iu" and (len(v) == 0 or (v.min() >= -2**31 and v.max() < 2**31)):
                a, dt = v.astype("<i4"), "i32"
            elif v.dtype.kind in "iu":
                a, dt = v.astype("<i8"), "i64"
            else:
                a, dt = v.astype("<f8"), "f64"
            bufs.append({"name": k, "dtype": dt, "n": int(a.size)})
            blobs.append(a.tobytes())
        else:
            meta[k] = v
    header = json.dumps({"plugin": plugin, "fields": meta, "buffers": bufs}).encode()
    return struct.pack("<I", len(header)) + header + b"".join(blobs)


class NodeHost:
    def __init__(self):
        self.p = subprocess.Popen(["node", str(HERE / "host.mjs")], stdin=subprocess.PIPE, stdout=subprocess.PIPE)

    def call_raw(self, msg: bytes):
        self.p.stdin.write(msg)
        self.p.stdin.flush()
        n = struct.unpack("<I", self.p.stdout.read(4))[0]
        return json.loads(self.p.stdout.read(n))

    def close(self):
        self.p.stdin.close()
        self.p.wait()


def same(py, js) -> bool:
    """Compare a Python plugin result with a JS one: scores and decoded output."""
    if not py and not js["nodes"]:
        return True
    if "nodes" not in js:
        print("JS error:", js.get("error"))
        return False
    if len(py) != len(js["nodes"]):
        return False
    (score, out), jn = py[0], js["nodes"][0]
    if abs(score - jn["score"]) > 1e-9:
        return False
    if isinstance(out, list) and out and isinstance(out[0], dict):  # i2c transactions
        a = [(x["addr"], x["rw"], tuple(x["bytes"]), int(x["start"]), int(x["end"])) for x in out if "addr" in x]
        b = [(x["addr"], x["rw"], tuple(x["bytes"]), int(x["start"]), int(x["end"])) for x in jn["output"]]
        return a == b
    if isinstance(out, list):
        return [list(x) if isinstance(x, (list, tuple)) else x for x in out] == jn["output"]
    return True


def main():
    set_name = sys.argv[1] if len(sys.argv) > 1 else "tuning"
    limit = int(sys.argv[2]) if len(sys.argv) > 2 else 400
    dirs = sorted((corpus.ROOT / "corpus/fixtures/synth" / set_name).iterdir())[:limit]
    if set_name == "tuning":
        dirs += sorted(corpus.REAL.iterdir())
    rec = []
    staged.RECORDER = rec
    for d in dirs:
        staged.analyze(fixture.load_capture(d))
    staged.RECORDER = None
    sizes = sorted(((sum(v.size for v in f.values() if isinstance(v, np.ndarray)), n) for n, f, _, _ in rec), reverse=True)
    print("largest streams (elements):", sizes[:5], flush=True)
    print(f"{len(rec)} plugin calls recorded from {len(dirs)} captures: "
          + str({k: sum(1 for r in rec if r[0] == k) for k in sorted({r[0] for r in rec})}))

    from py_mini_racer import MiniRacer
    v8 = MiniRacer()
    v8.eval((HERE / "plugins.js").read_text())
    host = NodeHost()
    res = defaultdict(lambda: defaultdict(float))
    mism = defaultdict(int)
    for i, (name, fields, py, py_dt) in enumerate(rec):
        if i % 200 == 0:
            print(f"  replay {i}/{len(rec)}", flush=True)
        r = res[name]
        r["calls"] += 1
        r["py_s"] += py_dt
        n_el = sum(v.size for v in fields.values() if isinstance(v, np.ndarray))
        big = n_el > 200_000
        r["elements"] += n_el
        # JSON variants on huge streams are very slow: measure a few and scale
        if not big or r["big_json_sampled"] < 3:
            js_in = jsonable(fields)
            t0 = time.perf_counter()
            payload = json.dumps(js_in)
            out = json.loads(v8.eval(f"JSON.stringify(WS.{name}({payload}))"))
            dt_v8 = time.perf_counter() - t0
            mism["v8"] += not same(py, out)
            t0 = time.perf_counter()
            hdr = json.dumps({"plugin": name, "fields": js_in, "buffers": []}).encode()
            out = host.call_raw(struct.pack("<I", len(hdr)) + hdr)
            dt_nj = time.perf_counter() - t0
            mism["node_json"] += not same(py, out)
            r["checked_json"] += 1
            if big:
                r["big_json_sampled"] += 1
                r["big_v8_per_el"] += dt_v8 / n_el
                r["big_nj_per_el"] += dt_nj / n_el
                r["big_json_bytes_per_el"] += len(hdr) / n_el
            else:
                r["v8_json_s"] += dt_v8
                r["node_json_s"] += dt_nj
                r["node_json_bytes"] += len(hdr)
        if big:
            r["big_elements"] += n_el
        t0 = time.perf_counter()
        msg = binary_message(name, fields)
        out = host.call_raw(msg)
        r["node_bin_s"] += time.perf_counter() - t0
        r["node_bin_bytes"] += len(msg)
        mism["node_bin"] += not same(py, out)
    host.close()
    v8.close()
    for r in res.values():  # extrapolate the JSON variants for huge streams
        k = max(1, r["big_json_sampled"])
        r["v8_json_s"] += r["big_v8_per_el"] / k * r["big_elements"]
        r["node_json_s"] += r["big_nj_per_el"] / k * r["big_elements"]
        r["node_json_bytes"] += r["big_json_bytes_per_el"] / k * r["big_elements"]
    print("mismatches vs Python:", dict(mism), "(JSON variants checked on",
          int(sum(r["checked_json"] for r in res.values())), "calls; binary on all)")
    print("huge streams (>200k elements):", {k: int(r["big_elements"]) for k, r in res.items() if r["big_elements"]})
    print(f"{'plugin':8} {'calls':>6} {'py ms':>8} {'v8(json) ms':>12} {'node json ms':>13} {'node bin ms':>12} {'json KB/call':>13} {'bin KB/call':>12}")
    tot = defaultdict(float)
    for name, r in sorted(res.items()):
        c = r["calls"]
        for k in ("py_s", "v8_json_s", "node_json_s", "node_bin_s"):
            tot[k] += r[k]
        print(f"{name:8} {int(c):6d} {r['py_s'] * 1e3:8.1f} {r['v8_json_s'] * 1e3:12.1f} {r['node_json_s'] * 1e3:13.1f} "
              f"{r['node_bin_s'] * 1e3:12.1f} {r['node_json_bytes'] / c / 1e3:13.1f} {r['node_bin_bytes'] / c / 1e3:12.1f}")
    print(f"{'total':8} {'':6} {tot['py_s'] * 1e3:8.1f} {tot['v8_json_s'] * 1e3:12.1f} {tot['node_json_s'] * 1e3:13.1f} {tot['node_bin_s'] * 1e3:12.1f}")
    sys.stdout.flush()
    os._exit(0)


if __name__ == "__main__":
    try:
        main()
    except BaseException:
        import traceback
        traceback.print_exc()
        sys.stdout.flush()
        os._exit(1)
