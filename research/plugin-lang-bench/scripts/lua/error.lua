local function inner(frame)
  local total = 0
  -- line 4 indexes a nil field
  total = total + frame.missing.length
  return total
end
local function outer() local r = inner({}) return r end
outer()
