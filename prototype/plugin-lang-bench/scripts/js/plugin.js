function scan(samples, n, ch) {
  var c = 0, p = (samples[0] >> ch) & 1;
  for (var i = 1; i < n; i++) { var v = (samples[i] >> ch) & 1; if (v !== p) { c++; p = v; } }
  return c;
}
function i2c(bits, t, bounds) {
  var recs = [], tx = 0, nb = 0, x = 0;
  for (var f = 0; f + 1 < bounds.length; f += 2) {
    var a = bounds[f], b = bounds[f + 1], nw = Math.floor((b - a) / 9);
    if (nw === 0) continue;
    var bytes = [], firstAck = false;
    for (var w = 0; w < nw; w++) {
      var v = 0;
      for (var k = 0; k < 9; k++) v = (v << 1) | bits[a + w * 9 + k];
      x ^= v;
      if (w === 0) firstAck = (v & 1) === 0;
      bytes.push(v >> 1);
    }
    var first = bytes.shift();
    tx++; nb += nw - 1;
    recs.push({ addr: first >> 1, rw: (first & 1) ? "read" : "write", addr_ack: firstAck, bytes: bytes, start: t[a], end: t[b - 1] });
  }
  return [recs, tx, nb, x];
}
function inc(x) { return x + 1; }
