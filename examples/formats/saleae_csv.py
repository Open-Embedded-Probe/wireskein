"""saleae_csv: a file format of WireSkein's examples, added from outside (wireskein.formats): the CSV a Saleae Logic 2
exports ("File > Export Data", CSV), of digital channels or of analog ones (Logic 2 writes them to two files).

    digital.csv                          analog.csv
    Time [s],Channel 0,Channel 1         Time [s],Channel 0,Channel 1
    0.000000000,0,0                      0.000000292,3.272,2.578
    0.000151900,0,1                      0.000001092,3.457,2.597
    ...                                  ...

Digital: the first row is every channel's level at the start, each row after it the levels after a change (one or
more channels changed at that time), and the last row the end of the capture (the levels unchanged; its time need not
fall on the sample grid). The file gives no sample rate: a tick is the largest step every change's time is a
multiple of (100 ns at 10 MS/s), read from the times to the nanosecond.

Analog: one row a sample, at a steady rate (the step between rows), in volts; the first row's time is where the
samples start (Logic 2 shifts the analog samples by their filter's delay). A tick is 1 ns.

Columns other than "Time [s]" are lines named by their headers. Which of the two a file is, its second row tells:
only 0 and 1 is digital.
"""

from __future__ import annotations

import csv
from fractions import Fraction
from math import gcd

from wireskein import capture

NS = 10**9


def _rows(path):
    with open(path, newline="", encoding="utf-8-sig") as f:
        rows = csv.reader(f)
        names = [n.strip() for n in next(rows)[1:]]
        return names, [r for r in rows if r]


def _is_digital(row) -> bool:
    return all(x.strip() in ("0", "1") for x in row[1:])


class SaleaeCsv:
    name = "saleae_csv"

    def sniff(self, head: bytes, path) -> bool:
        return head.lstrip(b"\xef\xbb\xbf").startswith(b"Time [s],")

    def read(self, path) -> capture.Capture:
        names, rows = _rows(path)
        if not rows:
            raise ValueError(f"{path}: no rows")
        if _is_digital(rows[0]) and (len(rows) < 2 or _is_digital(rows[1])):
            return self._digital(path, names, rows)
        return self._analog(path, names, rows)

    def _digital(self, path, names, rows) -> capture.Capture:
        ns = [round(Fraction(r[0]) * NS) for r in rows]
        t0 = ns[0]
        step = 0
        for t in ns[:-1]:                    # the last row is the end, which need not fall on the sample grid
            step = gcd(step, t - t0)
        step = step or 1
        times = [(t - t0) // step for t in ns[:-1]] + [-(-(ns[-1] - t0) // step)]
        n = times[-1] + 1
        chans = []
        for k, name in enumerate(names):
            lv = int(rows[0][k + 1])
            init, edges = lv, []
            for t, r in zip(times[1:], rows[1:]):
                v = int(r[k + 1])
                if v != lv:
                    edges.append(t)
                    lv = v
            chans.append(capture.logic(name, init, edges, n))
        return capture.Capture(Fraction(NS, step), [capture.Part(n, chans, {"start_s": float(Fraction(t0, NS))})],
                               meta={"source": "saleae_csv"})

    def _analog(self, path, names, rows) -> capture.Capture:
        ns = [round(Fraction(r[0]) * NS) for r in rows[:2]]
        if len(rows) < 2 or ns[1] <= ns[0]:
            raise ValueError(f"{path}: an analog export needs two rows or more, in time order")
        dt = ns[1] - ns[0]
        last = round(Fraction(rows[-1][0]) * NS)
        if last - ns[0] != dt * (len(rows) - 1):
            raise ValueError(f"{path}: the analog samples are not at one rate")
        rate = Fraction(NS, dt)
        chans = [capture.analog_volts(name, [float(r[k + 1]) for r in rows], float(rate), t0_ticks=ns[0])
                 for k, name in enumerate(names)]
        n = ns[0] + dt * len(rows)
        return capture.Capture(Fraction(NS), [capture.Part(n, chans, {"start_s": 0.0})],
                               meta={"source": "saleae_csv"})


FORMAT = SaleaeCsv()
