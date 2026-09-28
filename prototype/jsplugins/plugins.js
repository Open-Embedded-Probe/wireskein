// WireSkein prototype: protocol plugins in plain JavaScript (no Node APIs), so
// the same file runs in an embedded engine (V8 via mini-racer) and in a
// separate Node.js plugin host. Each plugin takes one typed stream and returns
// {nodes: [{analyzer, roles, params, metrics, score, output}]}.
// Numeric inputs may be plain arrays or typed arrays.

var WS = (function () {
  function q(n, scale) { return 1 - Math.exp(-n / (scale || 8)); }

  // I2C on START/STOP frames of a SyncBits stream (one data pin, sampled on SCL rising).
  function i2c(s) {
    if (s.sample_edge !== "rise" || s.n_data !== 1) return { nodes: [] };
    var bits = s.bits, t = s.t, b = s.bounds, nf = b.length / 2;
    var mod9n = 0, nlen = 0, acks = 0, nwords = 0, txs = [], nbytes = 0, addrAck = 0, nAddr = 0;
    for (var f = 0; f < nf; f++) {
      var a = b[2 * f], e = b[2 * f + 1], len = e - a;
      if (len <= 0) continue;
      nlen++;
      if (len % 9 === 0 || len % 9 === 1) mod9n++;
      var nw = Math.floor(len / 9), data = [], ackl = [];
      for (var w = 0; w < nw; w++) {
        var v = 0;
        for (var k = 0; k < 9; k++) v = (v << 1) | bits[a + w * 9 + k];
        nwords++;
        if ((v & 1) === 0) acks++;
        data.push(v >> 1); ackl.push((v & 1) === 0);
      }
      if (data.length) {
        nAddr++; if (ackl[0]) addrAck++;
        nbytes += data.length;
        txs.push({ addr: data[0] >> 1, rw: (data[0] & 1) ? "read" : "write", addr_ack: ackl[0],
                   bytes: data.slice(1), acks: ackl.slice(1), start: t[a], end: t[Math.max(a, e - 1)] });
      }
    }
    if (nlen === 0) return { nodes: [] };
    var mod9 = mod9n / nlen, ack = nwords ? acks / nwords : 0;
    var score = mod9 * (0.8 + 0.2 * ack) * q(nbytes) * (0.5 + 0.5 * s.clk_score) * (0.5 + 0.5 * s.pair_score);
    return { nodes: [{ analyzer: "i2c", roles: { scl: s.clock, sda: s.data[0] }, params: { sample_edge: "rise" },
      metrics: { mod9: mod9, ack_rate: ack, addr_ack: nAddr ? addrAck / nAddr : 0, transactions: txs.length, bytes: nbytes },
      score: score, output: txs }] };
  }

  // Characters between consecutive breaks.
  function splitOnBreaks(c) {
    var out = [], br = Array.prototype.slice.call(c.breaks), n = c.start.length;
    br.push(Number.MAX_SAFE_INTEGER);
    for (var i = 0; i + 1 < br.length; i++) {
      var v = [], ok = [];
      for (var j = 0; j < n; j++) if (c.start[j] > br[i] && c.start[j] < br[i + 1]) { v.push(c.values[j] & 0xFF); ok.push(!!c.ok[j]); }
      out.push({ v: v, ok: ok });
    }
    return out;
  }
  function linPid(id) {
    var b = []; for (var i = 0; i < 6; i++) b.push((id >> i) & 1);
    var p0 = b[0] ^ b[1] ^ b[2] ^ b[4], p1 = 1 - (b[1] ^ b[3] ^ b[4] ^ b[5]);
    return id | (p0 << 6) | (p1 << 7);
  }
  function linChecksum(data, pid) {
    var s = pid || 0;
    for (var i = 0; i < data.length; i++) { s += data[i]; s = (s & 0xFF) + (s >> 8); }
    return (~s) & 0xFF;
  }
  function hex(a) { var s = ""; for (var i = 0; i < a.length; i++) s += (a[i] < 16 ? "0" : "") + a[i].toString(16); return s; }

  function lin(c) {
    if (c.breaks.length < 2) return { nodes: [] };
    var frames = [], ck = { sync: 0, pid_parity: 0, checksum: 0, frames: 0 };
    splitOnBreaks(c).forEach(function (g) {
      var v = g.v; if (v.length < 3) return;
      ck.frames++;
      var pid = v[1], data = v.slice(2, v.length - 1), cs = v[v.length - 1];
      if (v[0] === 0x55) ck.sync++;
      if (linPid(pid & 0x3F) === pid) ck.pid_parity++;
      if (linChecksum(data, pid) === cs) ck.checksum++;
      frames.push([pid & 0x3F, hex(data)]);
    });
    if (ck.frames < 2) return { nodes: [] };
    var passed = (ck.sync + ck.pid_parity + ck.checksum) / (3 * ck.frames);
    return { nodes: [{ analyzer: "lin", roles: { data: c.pin }, params: { baud: c.baud, idle: c.idle },
      metrics: ck, score: passed * q(ck.frames, 3), output: frames }] };
  }

  function dmx512(c) {
    if (c.breaks.length < 2) return { nodes: [] };
    var packets = [], ck = { packets: 0, start_code_0: 0, framing_ok: 0, chars: 0 };
    splitOnBreaks(c).forEach(function (g) {
      if (g.v.length < 2) return;
      ck.packets++;
      if (g.v[0] === 0) ck.start_code_0++;
      ck.framing_ok += g.ok.filter(Boolean).length; ck.chars += g.ok.length;
      packets.push(hex(g.v.slice(1)));
    });
    if (ck.packets < 2) return { nodes: [] };
    var rateOk = Math.abs(c.baud / 250000 - 1) < 0.03 ? 1 : 0.3;
    var score = (ck.start_code_0 / ck.packets) * (ck.framing_ok / Math.max(1, ck.chars)) * rateOk * q(ck.packets, 2);
    return { nodes: [{ analyzer: "dmx512", roles: { data: c.pin }, params: { baud: c.baud, idle: c.idle },
      metrics: ck, score: score, output: packets }] };
  }

  // NMEA 0183 on a byte stream: $...*HH with XOR checksum.
  function nmea(bs) {
    var v = bs.values, s = "";
    for (var i = 0; i < v.length; i++) s += String.fromCharCode(v[i] & 0xFF);
    var re = /\$([^$*\r\n]{1,80})\*([0-9A-Fa-f]{2})/g, m, ok = 0, bad = 0, covered = 0, msgs = [];
    while ((m = re.exec(s)) !== null) {
      var x = 0; for (var k = 0; k < m[1].length; k++) x ^= m[1].charCodeAt(k);
      if (x === parseInt(m[2], 16)) { ok++; covered += m[0].length; msgs.push(m[1]); } else bad++;
    }
    return { nodes: [{ analyzer: "nmea", roles: bs.roles, params: {}, metrics: { checks_passed: ok, checks_failed: bad,
      coverage: v.length ? covered / v.length : 0 }, score: ok ? (1 - Math.pow(0.5, ok)) * ok / (ok + bad) : 0, output: msgs }] };
  }

  return { i2c: i2c, lin: lin, dmx512: dmx512, nmea: nmea };
})();
