"""Device level: identification where evidence exists, and no overreach where it does not.

    PYTHONPATH=. uv run python device_eval.py [n]
"""
import sys
from collections import Counter

import numpy as np

import corpus
from wireskein._engine import devices, fixture, staged, synth

staged.use_declarative(True)

n = int(sys.argv[1]) if len(sys.argv) > 1 else 60
packs, adb = devices.load_packs(), devices.address_table()
c = Counter()


def claims_devices(r):
    out = []
    for cl in r.claims:
        if cl.verdict not in ("confirmed", "likely") or cl.node.output is None:
            continue
        if cl.protocol == "i2c":
            out += [(cl, m) for m in devices.match_i2c(cl.node.output.items, packs, adb)]
        elif cl.protocol == "spi":
            out += [(cl, m) for m in devices.match_spi(cl.node.output.items["lines"], packs)]
    return out


# (a) real captures
for d in sorted(corpus.REAL.iterdir()):
    if "flash" in d.name and not d.name.startswith("i2cdb"):
        continue
    r = staged.analyze(fixture.load_capture(d))
    for cl, m in claims_devices(r):
        print(f"real {d.name:28} {cl.protocol}: {m.device:14} {m.level:12}", {k: v for k, v in m.evidence.items() if k != 'chips'},
              m.evidence.get("chips", ""))
# (b) random I2C/SPI traffic (tuning set): device claims are overreach
for d in sorted((corpus.ROOT / "corpus/fixtures/synth/tuning").iterdir())[:n]:
    r = staged.analyze(fixture.load_capture(d))
    for cl, m in claims_devices(r):
        c[f"random: {m.level}"] += 1
        if m.level in ("identified", "consistent"):
            print("  OVERREACH", d.name, cl.protocol, m.device, m.level, m.evidence)
# (c) SPI NOR traffic mixed with plain SPI
for s in range(n):
    cap, t = synth.scenario(s, "spinor")
    r = staged.analyze(cap)
    got = claims_devices(r)
    for b in t["buses"]:
        if b["protocol"] != "spi":
            continue
        want = b.get("device")
        hit = [m for cl, m in got if cl.roles.get("clk") == b["roles"]["clk"] and m.level in ("identified", "consistent")]
        if want:
            c["nor buses"] += 1
            c["nor identified"] += any(m.level == "identified" for m in hit)
            ok = [m for m in hit if m.level == "identified"]
            if ok:
                names = {"PP": "PAGE_PROGRAM", "SE": "SECTOR_ERASE_4K", "SFDP": "READ_SFDP"}
                cmds = [x["command"] for x in ok[0].messages]
                c["nor command sequence exact"] += cmds == [names.get(x, x) for x in want["commands"]]
                c["nor jedec right"] += any(want["jedec"] in i for i in ok[0].evidence["identity"])
        else:
            c["plain spi buses"] += 1
            c["plain spi overreach"] += bool(hit)
for k in sorted(c):
    print(f"{k:32} {c[k]}")
