"""SD commands in SPI mode on the bus between a host (the DUT) and a card."""

import numpy as np

from wireskein import verify
from wireskein._engine import gen, periph
from wireskein.runlog import sd_spi


def _cmd(idx, arg, crc=None):
    b = bytes([0x40 | idx]) + arg.to_bytes(4, "big")
    return b + bytes([(periph.crc7(b) << 1 | 1) if crc is None else crc])


def _exchange(steps):
    """(mosi, miso) of the steps (host bytes, card bytes), the shorter side filled with 0xFF."""
    mo, mi = bytearray(), bytearray()
    for host, card in steps:
        n = max(len(host), len(card))
        mo += host.ljust(n, b"\xff")
        mi += card.ljust(n, b"\xff")
    return bytes(mo), bytes(mi)


def _cap(steps):
    clk, mosi, miso, cs = gen.Wave(0), gen.Wave(1), gen.Wave(1), gen.Wave(1)
    mo, mi = _exchange(steps)
    end = gen.spi(clk, mosi, miso, cs, 5e-6, [(mo, mi)], 2e6)
    return gen.quantize({"SCK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, 20e6, end + 5e-6,
                        np.random.default_rng(0))


BLOCK = bytes(range(256)) * 2
INIT = [(_cmd(0, 0), b""), (b"\xff", b"\x01"),
        (_cmd(8, 0x1AA), b""), (b"\xff" * 5, b"\x01\x00\x00\x01\xaa"),
        (_cmd(55, 0, 0x01), b""), (b"\xff", b"\x01"), (_cmd(41, 0x40000000, 0x01), b""), (b"\xff", b"\x00"),
        (_cmd(58, 0, 0x01), b""), (b"\xff" * 5, b"\x00\xc0\xff\x80\x00"),
        (_cmd(17, 0, 0x01), b""), (b"\xff" * 2, b"\xff\x00"), (b"\xff" * 3, b"\xff\xff\xfe"),
        (b"\xff" * 514, BLOCK + periph.crc16(BLOCK).to_bytes(2, "big"))]


def test_init_and_a_block_read():
    cap = _cap(INIT)
    want = [{"cmd": 0, "r1": 1}, {"cmd": 8, "arg": 0x1AA, "r1": 1, "resp": "000001aa"}, {"cmd": 55},
            {"acmd": 41, "r1": 0}, {"cmd": 58, "resp": "c0ff8000"}, {"cmd": 17, "r1": 0, "data": [BLOCK.hex()]}]
    ok, got, why = verify._judge(cap, sd_spi("SCK", "MOSI", "MISO", "CS", want), {"SCK", "MOSI", "MISO", "CS"})
    assert ok is True, why
    assert [c["cmd"] for c in got["commands"]] == [0, 8, 55, 41, 58, 17] and got["commands"][3]["acmd"]
    ok, _, why = verify._judge(cap, sd_spi("SCK", "MOSI", "MISO", "CS", mode=None), {"SCK", "MOSI", "MISO", "CS"})
    assert ok is True, why


def test_a_bad_cmd8_crc_and_no_answer():
    cap = _cap([(_cmd(0, 0), b""), (b"\xff", b"\x01"), (_cmd(8, 0x1AA, 0x01), b""), (b"\xff" * 12, b"")])
    ok, _, why = verify._judge(cap, sd_spi("SCK", "MOSI", "MISO", "CS"), {"SCK", "MOSI", "MISO", "CS"})
    assert ok is False and "CMD8" in why and "CRC7" in why and "no R1" in why, why


def test_a_block_write_with_its_data_response():
    blk = bytes(512)
    mo = _cmd(24, 8, 0x01) + b"\xff\xff" + b"\xff\xfe" + blk + periph.crc16(blk).to_bytes(2, "big") + b"\xff" * 5
    mi = b"\xff" * 6 + b"\xff\x00" + b"\xff" * (2 + 512 + 2) + b"\x05\x00\x00\x00\xff"
    rows = [(i, a, b, 0) for i, (a, b) in enumerate(zip(mo, mi))]
    (c,) = periph.sd_spi(rows)["commands"]
    (b,) = c["blocks"]
    assert (c["cmd"], c["r1"], b["token"], b["crc_ok"], b["data_response"], b["busy_bytes"]) == (24, 0, 0xFE, True, 5, 3)


def test_analyze_reads_sd_commands_on_spi():
    from wireskein._engine import export, staged
    res = staged.analyze(_cap(INIT))
    (c,) = [c for c in res.claims if c.protocol == "spi"]
    fin = export.final_of(c.node, export.layers_of(c.node))
    assert [x["cmd"] for x in fin["sd"]] == ["CMD0", "CMD8", "CMD55", "ACMD41", "CMD58", "CMD17"]
    assert fin["sd"][-1]["blocks"][0]["crc_ok"] is True
