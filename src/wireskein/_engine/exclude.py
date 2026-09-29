"""Exclusion rules evaluated before any decoding, from the common survey only.

Each rule says a proposal is impossible. A rule is only usable if the audit
(`exclusion_audit.py`) shows it never removes a true hypothesis. Rules marked
`definitional` follow from the protocol definition; `heuristic` ones are
candidates whose safety must be measured.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable

import numpy as np

from .survey import Survey


@dataclass
class Rule:
    name: str
    protocol: str
    kind: str                      # definitional | heuristic
    test: Callable                 # (params, survey, capture) -> True if excluded
    doc: str


def _runs_p05(sv: Survey, cap, ch: str) -> float:
    key = ("p05", ch)
    cache = sv.__dict__.setdefault("_cache", {})
    if key not in cache:
        c = cap.channel(ch)
        _, length, _ = c.runs(cap.n_samples)
        inner = length[1:-1]
        inner = inner[inner > 1]  # 1-sample spikes are glitches, not bits
        cache[key] = float(np.quantile(inner, 0.05)) if len(inner) >= 20 else 0.0
    return cache[key]


def _uart_idle(p, sv, cap):
    return _surely_idles_at(sv, cap, p["ch"], 1 - p["idle"])


def _uart_bit_too_long(p, sv, cap):
    T = cap.rate / p["baud"]
    p05 = _runs_p05(sv, cap, p["ch"])
    return p05 > 0 and p05 < 0.4 * T


def _uart_char_len(p, sv, cap):
    a = sv.asyncs.get(p["ch"])
    if not a or not a.unit or not a.char_bits:
        return False
    T = cap.rate / p["baud"]
    if abs(T / a.unit - 1) > 0.03:
        return False
    L = 1 + p["data_bits"] + (0 if p["parity"] == "none" else 1) + int(p["stop_bits"])
    best = a.char_bits[0][1]
    tied = {l for l, r in a.char_bits if r >= best - 0.02}
    return best >= 0.9 and L not in tied and L <= 13


def _uart_pure_clock(p, sv, cap):
    f = sv.features[p["ch"]]
    return f.scores.get("clock", 0) >= 0.95 and f.distinct_runs <= 2


def _surely_idles_at(sv, cap, ch: str, level: int) -> bool:
    """Conservative idle evidence: the estimated idle level, the first and the
    last level of the capture all agree."""
    c = cap.channel(ch)
    final = c.initial ^ (len(c.edges) & 1)
    return sv.features[ch].idle_level == level and c.initial == level and final == level


def _i2c_idle_high(p, sv, cap):
    return _surely_idles_at(sv, cap, p["scl"], 0) or _surely_idles_at(sv, cap, p["sda"], 0)


def _i2c_scl_not_clock(p, sv, cap):
    return p["scl"] not in sv.clocks


def _i2c_no_coactivity(p, sv, cap):
    r = sv.pairs.get((p["scl"], p["sda"]))
    return r is None or r.co_activity < 0.5


def _i2c_sda_busier(p, sv, cap):
    return sv.features[p["sda"]].n_edges > 1.2 * sv.features[p["scl"]].n_edges + 20


def _spi_clk_not_clock(p, sv, cap):
    return p["clk"] not in sv.clocks


def _spi_no_data(p, sv, cap):
    rs = [r for (c, o), r in sv.pairs.items() if c == p["clk"] and o != p["cs"]]
    return not any(r.co_activity >= 0.3 for r in rs)


def _spi_cs_idle(p, sv, cap):
    return p["cs"] is not None and _surely_idles_at(sv, cap, p["cs"], p["cs_active"])


def _spi_cs_too_busy(p, sv, cap):
    if p["cs"] is None:
        return False
    rising = sv.features[p["clk"]].n_edges / 2
    return sv.features[p["cs"]].n_edges > rising / 2


def _spi_cs_no_boundary(p, sv, cap):
    if p["cs"] is None:
        return False
    r = sv.pairs.get((p["clk"], p["cs"]))
    return r is None or r.boundary < 0.3


RULES = [
    Rule("uart.idle_level", "uart", "definitional", _uart_idle,
         "UART の線はアイドル時にアイドルレベルにある。推定アイドル・最初・最後のレベルがそろって逆なら除く"),
    Rule("uart.bit_too_long", "uart", "definitional", _uart_bit_too_long,
         "1ビットより短いラン（グリッチ以外）はありえない。5パーセンタイルのランが 0.4 ビット未満なら除く"),
    Rule("uart.char_len", "uart", "heuristic", _uart_char_len,
         "共通の下調べで文字長がほぼ確定（停止ビット検査 ≥ 0.9）したとき、合わない形式を除く"),
    Rule("uart.pure_clock", "uart", "heuristic", _uart_pure_clock,
         "ラン幅が2種類だけの純粋なクロックは UART ではないとみなす"),
    Rule("i2c.idle_high", "i2c", "definitional", _i2c_idle_high,
         "I²C はオープンドレインで、SCL・SDA ともアイドルは HIGH。推定アイドル・最初・最後のレベルがそろって LOW なら除く"),
    Rule("i2c.scl_not_clock", "i2c", "heuristic", _i2c_scl_not_clock,
         "SCL は周期を持つ（共通の下調べでクロック情報が取れる）"),
    Rule("i2c.no_coactivity", "i2c", "definitional", _i2c_no_coactivity,
         "SDA の変化の半分以上は SCL のバースト中にある"),
    Rule("i2c.sda_busier", "i2c", "definitional", _i2c_sda_busier,
         "SDA の変化は SCL の変化より多くならない（1 ビットに高々 1 回＋START/STOP）"),
    Rule("spi.clk_not_clock", "spi", "heuristic", _spi_clk_not_clock,
         "SPI の CLK は周期を持つ"),
    Rule("spi.no_data", "spi", "definitional", _spi_no_data,
         "CLK のバースト中に変化する線が1本もなければ SPI ではない"),
    Rule("spi.cs_idle", "spi", "definitional", _spi_cs_idle,
         "CS は非選択レベルでアイドルする。推定アイドル・最初・最後のレベルがそろって有効レベルなら除く"),
    Rule("spi.cs_too_busy", "spi", "definitional", _spi_cs_too_busy,
         "CS の切り替えは CLK の立ち上がりの半分より多くならない"),
    Rule("spi.cs_no_boundary", "spi", "heuristic", _spi_cs_no_boundary,
         "CS の切り替わりはクロックのバーストの前後にある"),
]


def excluded_by(protocol: str, params: dict, sv: Survey, cap, rules=RULES) -> list[str]:
    return [r.name for r in rules if r.protocol == protocol and r.test(params, sv, cap)]
