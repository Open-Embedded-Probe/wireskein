-- LuaJIT plugin (Lua 5.1 syntax: no bitwise operators, use the `bit` library).
-- *_ffi variants take raw addresses (lightuserdata) and read through FFI pointers
-- (0-based, zero-copy); the plain variants take 1-based Lua tables.
local ffi = require("ffi")
local bit = require("bit")
local band, bor, bxor, rshift, lshift = bit.band, bit.bor, bit.bxor, bit.rshift, bit.lshift
local floor = math.floor

function scan_ffi(addr, n, ch)
  local s = ffi.cast("const uint16_t*", addr)
  local c, p = 0, band(rshift(s[0], ch), 1)
  for i = 1, n - 1 do
    local v = band(rshift(s[i], ch), 1)
    if v ~= p then c = c + 1; p = v end
  end
  return c
end

function scan(s, n, ch)
  local c, p = 0, band(rshift(s[1], ch), 1)
  for i = 2, n do
    local v = band(rshift(s[i], ch), 1)
    if v ~= p then c = c + 1; p = v end
  end
  return c
end

local function frame(bits, t, a, b, off, recs, st)
  local nw = floor((b - a) / 9)
  if nw == 0 then return end
  local bytes, first, firstAck = {}, 0, false
  for w = 0, nw - 1 do
    local v, base = 0, a + w * 9 + off
    for k = 0, 8 do v = bor(lshift(v, 1), bits[base + k]) end
    st.x = bxor(st.x, v)
    if w == 0 then first = rshift(v, 1); firstAck = band(v, 1) == 0
    else bytes[w] = rshift(v, 1) end
  end
  st.tx = st.tx + 1; st.nb = st.nb + nw - 1
  recs[st.tx] = { addr = rshift(first, 1), rw = band(first, 1) == 1 and "read" or "write", addr_ack = firstAck,
                  bytes = bytes, start = t[a + off], ["end"] = t[b - 1 + off] }
end

function i2c_ffi(bits_addr, t_addr, bounds_addr, nbounds)
  local bits = ffi.cast("const uint8_t*", bits_addr)
  local t = ffi.cast("const uint32_t*", t_addr)
  local bounds = ffi.cast("const uint32_t*", bounds_addr)
  local recs, st = {}, { tx = 0, nb = 0, x = 0 }
  for f = 0, nbounds - 2, 2 do frame(bits, t, bounds[f], bounds[f + 1], 0, recs, st) end
  return recs, st.tx, st.nb, st.x
end

function i2c(bits, t, bounds)
  local recs, st = {}, { tx = 0, nb = 0, x = 0 }
  for f = 1, #bounds - 1, 2 do frame(bits, t, bounds[f], bounds[f + 1], 1, recs, st) end
  return recs, st.tx, st.nb, st.x
end

function inc(x) return x + 1 end
