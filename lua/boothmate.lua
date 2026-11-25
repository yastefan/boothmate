-- Boothmate plugin for grandMA3.
--
-- Reports what OSC output does not carry: the executors of the current page with their assigned
-- objects, names, appearance colours and start state, and the grandMA3 version. Boothmate
-- connects to this plugin over TCP; the plugin never changes the show.
--
-- Start:  Plugin "boothmate"                       listen on 127.0.0.1:9002
--         Plugin "boothmate" "bind=0.0.0.0 token=x" listen on the network, Boothmate must send token x
--         Plugin "boothmate" "port=9010"           other port
--         Plugin "boothmate" "stop" | "status"
--
-- Protocol: one JSON object per line.
--   Boothmate -> plugin  {"op":"hello","token":"..."}  {"op":"resync"}
--   plugin -> Boothmate  {"ev":"hello","plugin":"0.1.0","ma":"2.5.1.1"}
--                        {"ev":"page","page":1,"executors":[{no,class,obj,name,addr,color,active,fader,cue}]}
--                        {"ev":"error","msg":"..."}
--
-- Change detection: HookObjectChange on every assigned object, its appearance and the pools
-- (labels, appearances, assignments); the current page is compared every frame (one local call).

local pluginName  = select(1, ...)
local componentName = select(2, ...)
local signalTable = select(3, ...)
local myHandle    = select(4, ...)

local socket = require("socket")
local json   = require("json")

local VERSION = "0.1.0"
local DEFAULT_PORT = 9002
local SNAPSHOT_MIN_INTERVAL = 0.1 -- seconds between page snapshots
local EXEC_ROWS = { 1, 2, 3, 4 }   -- executor numbers <row>01 .. <row>99
local EXEC_COLS = 99

_G.__boothmate = _G.__boothmate or { running = false }
local S = _G.__boothmate

local function log(fmt, ...)
  local msg = "[boothmate] " .. string.format(fmt, ...)
  pcall(Printf, msg)
end

local function field(h, k)
  if h == nil then return nil end
  local ok, v = pcall(function() return h[k] end)
  if ok then return v end
  return nil
end

local function call(h, m, ...)
  if h == nil then return nil end
  local args = { ... }
  local ok, v = pcall(function() return h[m](h, table.unpack(args)) end)
  if ok then return v end
  return nil
end

-- A property in its display role ("FF0000FF" for BackRGBA on 2.5.1).
local function display(h, prop)
  local ok, v = pcall(function() return h:Get(prop, Enums.Roles.Display) end)
  if ok and v ~= nil then return tostring(v) end
  ok, v = pcall(function() return h:Get(prop) end)
  if ok and v ~= nil then return tostring(v) end
  return nil
end

-- "RRGGBBAA" hex or "r,g,b[,a]" (0..1 or 0..255) -> "#rrggbb".
local function color(text)
  if text == nil then return nil end
  local s = tostring(text):gsub("^%s+", ""):gsub("%s+$", ""):gsub("^#", "")
  if s:match("^%x%x%x%x%x%x") and (#s == 6 or #s == 8) then return "#" .. s:sub(1, 6):lower() end
  local n = {}
  for v in s:gmatch("[^,]+") do n[#n + 1] = tonumber(v) end
  if #n < 3 then return nil end
  local scale = (n[1] > 1 or n[2] > 1 or n[3] > 1) and 1 or 255
  local function c(x) return math.max(0, math.min(255, math.floor(x * scale + 0.5))) end
  return string.format("#%02x%02x%02x", c(n[1]), c(n[2]), c(n[3]))
end

local function pageNumber(page)
  return tonumber(field(page, "no")) or tonumber(call(page, "Index"))
end

local function currentCue(obj)
  local cue = call(obj, "CurrentChild")
  if cue == nil then return nil end
  local no, name = field(cue, "no"), field(cue, "name")
  if no == nil and name == nil then return nil end
  -- Cue numbers are stored times 1000 on some versions.
  local n = tonumber(no)
  if n and n >= 1000 and n % 1000 == 0 then n = n / 1000 end
  local num = n and (n % 1 == 0 and string.format("%d", n) or tostring(n)) or ""
  return (num .. " " .. (name or "")):gsub("%s+$", "")
end

---------------------------------------------------------------------------------------------
-- Hooks
---------------------------------------------------------------------------------------------

local function unhookAll()
  for _, id in ipairs(S.hooks or {}) do pcall(Unhook, id) end
  S.hooks = {}
  S.hooked = {}
end

local function hook(obj)
  if obj == nil then return end
  local key = tostring(call(obj, "Addr") or obj)
  if S.hooked[key] then return end
  S.hooked[key] = true
  -- One hook per callback function, so every hook gets its own closure.
  local ok, id = pcall(HookObjectChange, function() S.dirty = true end, obj, S.pluginObject)
  if ok and id ~= nil and id ~= -1 then S.hooks[#S.hooks + 1] = id end
end

---------------------------------------------------------------------------------------------
-- Snapshot
---------------------------------------------------------------------------------------------

local function readExecutor(n)
  local ok, exec = pcall(GetExecutor, n)
  if not ok or exec == nil then return nil end
  local obj = field(exec, "Object")
  if obj == nil then return nil end
  local e = {
    no = n,
    class = call(obj, "GetClass"),
    obj = tonumber(field(obj, "no")),
    name = field(obj, "name") or "",
    addr = call(obj, "Addr"),
  }
  local app = field(obj, "Appearance")
  if app ~= nil then
    e.color = color(display(app, "BackRGBA"))
    hook(app)
  end
  local okA, act = pcall(function() return obj:HasActivePlayback() end)
  if okA and type(act) == "boolean" then e.active = act end
  local okF, f = pcall(function() return obj:GetFader({ token = "FaderMaster" }) end)
  if okF and type(f) == "number" then e.fader = f end
  if e.class == "Sequence" then e.cue = currentCue(obj) end
  hook(obj)
  return e
end

local function snapshot()
  unhookAll()
  local page = CurrentExecPage()
  local execs = {}
  for _, row in ipairs(EXEC_ROWS) do
    for col = 1, EXEC_COLS do
      local e = readExecutor(row * 100 + col)
      if e then execs[#execs + 1] = e end
    end
  end
  hook(page)
  local pools = DataPool()
  hook(field(pools, "Sequences"))
  hook(field(ShowData(), "Appearances"))
  local msg = { ev = "page", page = pageNumber(page) }
  if #execs > 0 then msg.executors = execs end
  S.lastPage = call(page, "Addr")
  S.dirty = false
  S.lastSnapshot = socket.gettime()
  return msg
end

---------------------------------------------------------------------------------------------
-- Network
---------------------------------------------------------------------------------------------

local function send(cl, msg)
  cl.out = cl.out .. json.encode(msg) .. "\n"
end

local function flush(cl)
  if cl.out == "" then return true end
  local last, err, partial = cl.sock:send(cl.out)
  local sent = last or partial or 0
  cl.out = cl.out:sub(sent + 1)
  return err == nil or err == "timeout"
end

local function handle(cl, line)
  local ok, msg = pcall(json.decode, line)
  if not ok or type(msg) ~= "table" then return true end
  if msg.op == "hello" then
    if (S.token or "") ~= "" and msg.token ~= S.token then
      send(cl, { ev = "error", msg = "wrong token" })
      log("refused client %s: wrong token", tostring(cl.peer))
      return false
    end
    cl.ready = true
    send(cl, { ev = "hello", plugin = VERSION, ma = Version() })
    send(cl, snapshot())
    log("Boothmate connected from %s", tostring(cl.peer))
  elseif msg.op == "resync" and cl.ready then
    S.dirty = true
  end
  return true
end

local function serviceClient(cl)
  local data, err, partial = cl.sock:receive(8192)
  local chunk = data or partial
  if chunk and chunk ~= "" then cl.buf = cl.buf .. chunk end
  while true do
    local line, rest = cl.buf:match("^([^\n]*)\n(.*)$")
    if not line then break end
    cl.buf = rest
    if not handle(cl, line) then flush(cl); return false end
  end
  if #cl.buf > 65536 then return false end
  if err == "closed" then return false end
  return flush(cl)
end

local function loop()
  local server, err = socket.bind(S.bind, S.port)
  if not server then
    log("cannot listen on %s:%d: %s", S.bind, S.port, tostring(err))
    S.running = false
    return
  end
  server:settimeout(0)
  S.server = server
  log("v%s listening on %s:%d%s", VERSION, S.bind, S.port, (S.token or "") ~= "" and " (token required)" or "")
  while S.running and not S.stop do
    local c = server:accept()
    if c then
      c:settimeout(0)
      pcall(function() c:setoption("tcp-nodelay", true) end)
      local peer = select(1, c:getpeername())
      S.clients[#S.clients + 1] = { sock = c, buf = "", out = "", peer = peer }
    end
    local anyReady = false
    for i = #S.clients, 1, -1 do
      local cl = S.clients[i]
      local ok, alive = pcall(serviceClient, cl)
      if not ok or not alive then
        pcall(function() cl.sock:close() end)
        table.remove(S.clients, i)
        if cl.ready then log("Boothmate disconnected") end
      elseif cl.ready then
        anyReady = true
      end
    end
    if anyReady then
      -- Handles are new userdata on every call: compare addresses.
      if call(CurrentExecPage(), "Addr") ~= S.lastPage then S.dirty = true end
      if S.dirty and socket.gettime() - (S.lastSnapshot or 0) >= SNAPSHOT_MIN_INTERVAL then
        local okS, msg = pcall(snapshot)
        if not okS then msg = { ev = "error", msg = "snapshot failed: " .. tostring(msg) }; S.dirty = false end
        for _, cl in ipairs(S.clients) do if cl.ready then send(cl, msg) end end
      end
    end
    coroutine.yield()
  end
  for _, cl in ipairs(S.clients) do pcall(function() cl.sock:close() end) end
  S.clients = {}
  pcall(function() server:close() end)
  S.server = nil
  unhookAll()
  S.running = false
  S.stop = false
  log("stopped")
end

---------------------------------------------------------------------------------------------
-- Entry points
---------------------------------------------------------------------------------------------

local function Main(display_handle, argument)
  local opts = { bind = "127.0.0.1", port = DEFAULT_PORT, token = "" }
  for tok in tostring(argument or ""):gmatch("%S+") do
    local k, v = tok:match("^(%w+)=(.*)$")
    if tok == "stop" then
      if S.running then S.stop = true; log("stopping") else log("not running") end
      S.ignoreCleanup = S.running
      return
    elseif tok == "status" then
      log("v%s %s on %s:%s, %d client(s)", VERSION, S.running and "running" or "stopped", tostring(S.bind), tostring(S.port), #(S.clients or {}))
      S.ignoreCleanup = S.running
      return
    elseif k == "bind" then opts.bind = v
    elseif k == "port" then opts.port = tonumber(v) or DEFAULT_PORT
    elseif k == "token" then opts.token = v
    else log("unknown argument %s", tok); S.ignoreCleanup = S.running; return end
  end
  if S.running then
    log("already running on %s:%d (use \"stop\" first)", S.bind, S.port)
    S.ignoreCleanup = true
    return
  end
  if opts.bind ~= "127.0.0.1" and opts.token == "" then
    log("refusing bind=%s without a token", opts.bind)
    return
  end
  S.bind, S.port, S.token = opts.bind, opts.port, opts.token
  S.running, S.stop, S.clients, S.hooks, S.hooked, S.dirty = true, false, {}, {}, {}, false
  -- HookObjectChange needs the plugin object, not this component.
  S.pluginObject = call(myHandle, "Parent") or myHandle
  -- The loop runs inside this call and yields every frame; onPC calls Cleanup when it returns.
  local ok, err = xpcall(loop, debug.traceback)
  if not ok then log("error: %s", tostring(err)); S.running = false end
end

local function Cleanup()
  if S.ignoreCleanup then S.ignoreCleanup = false; return end
  S.stop = true
  for _, cl in ipairs(S.clients or {}) do pcall(function() cl.sock:close() end) end
  S.clients = {}
  if S.server then pcall(function() S.server:close() end); S.server = nil end
  unhookAll()
  S.running = false
end

return Main, Cleanup
