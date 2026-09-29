-- Lua 5.4 plugin. Containers are 1-based on the Lua side (host index i -> s[i+1]);
-- values in `bounds` stay 0-based host indices, so the script adds 1 when indexing.
function scan(s, n, ch)
  local c, p = 0, (s[1] >> ch) & 1
  for i = 2, n do
    local v = (s[i] >> ch) & 1
    if v ~= p then c = c + 1; p = v end
  end
  return c
end

function i2c(bits, t, bounds)
  local recs, tx, nb, x = {}, 0, 0, 0
  for f = 1, #bounds - 1, 2 do
    local a, b = bounds[f], bounds[f + 1]
    local nw = (b - a) // 9
    if nw > 0 then
      local bytes, first, firstAck = {}, 0, false
      for w = 0, nw - 1 do
        local v, base = 0, a + w * 9
        for k = 1, 9 do v = (v << 1) | bits[base + k] end
        x = x ~ v
        if w == 0 then first = v >> 1; firstAck = (v & 1) == 0
        else bytes[w] = v >> 1 end
      end
      tx = tx + 1; nb = nb + nw - 1
      recs[tx] = { addr = first >> 1, rw = (first & 1) == 1 and "read" or "write", addr_ack = firstAck,
                   bytes = bytes, start = t[a + 1], ["end"] = t[b] }
    end
  end
  return recs, tx, nb, x
end

function inc(x) return x + 1 end
