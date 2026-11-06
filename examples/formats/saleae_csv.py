"""saleae_csv: a file format of WireSkein's examples, added from outside (wireskein.formats): the digital CSV a
Saleae Logic 2 exports ("File > Export Data", CSV, digital channels).

    Time [s],Channel 0,Channel 1
    0.000000000,1,0
    0.000104000,0,0
    ...

The first row is every channel's level at the start of the export, each row after it the levels after a change
(one or more channels changed at that time). The file has no sample rate: the times are read to the nanosecond, so
a tick is 1 ns, and the capture ends at the last change (its length is not in the file). Columns other than "Time
[s]" are logic lines named by their headers.
"""

from __future__ import annotations

import csv
from fractions import Fraction

from wireskein import capture

TICK_HZ = 10**9


class SaleaeCsv:
    name = "saleae_csv"

    def sniff(self, head: bytes, path) -> bool:
        return head.lstrip(b"\xef\xbb\xbf").startswith(b"Time [s],")

    def read(self, path) -> capture.Capture:
        with open(path, newline="", encoding="utf-8-sig") as f:
            rows = csv.reader(f)
            names = next(rows)[1:]
            times, levels = [], []
            for r in rows:
                if not r:
                    continue
                times.append(round(Fraction(r[0]) * TICK_HZ))
                levels.append([int(x) for x in r[1:]])
        if not times:
            raise ValueError(f"{path}: no rows")
        t0 = times[0]
        n = times[-1] - t0 + 1
        chans = []
        for k, name in enumerate(names):
            init, edges, lv = levels[0][k], [], levels[0][k]
            for t, row in zip(times[1:], levels[1:]):
                if row[k] != lv:
                    edges.append(t - t0)
                    lv = row[k]
            chans.append(capture.logic(name.strip(), init, edges, n))
        return capture.Capture(Fraction(TICK_HZ), [capture.Part(n, chans, {"start_s": float(Fraction(t0, TICK_HZ))})],
                               meta={"source": "saleae_csv"})


FORMAT = SaleaeCsv()
