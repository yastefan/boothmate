-- Boothmate plugin for grandMA3.
--
-- Reports what OSC output does not carry: the executors of the current page with their assigned
-- objects, names, appearance colours and start state, and the grandMA3 version. Boothmate
-- connects to this plugin over TCP. The plugin changes only two things, and only when Boothmate
-- asks: the tempo of speed masters (Master 3.x) for the tempo link, and the OSC lines and the
-- Art-Net timecode input when the user presses "Set up in grandMA3". No other commands are accepted.
--
-- Start:  Plugin "boothmate"                       listen on 127.0.0.1:9002
--         Plugin "boothmate" "bind=0.0.0.0 token=x" listen on the network, Boothmate must send token x
--         Plugin "boothmate" "port=9010"           other port
--         Plugin "boothmate" "stop" | "status"
--
-- Protocol: one JSON object per line.
--   Boothmate -> plugin  {"op":"hello","token":"..."}  {"op":"resync"}
--                        {"op":"speed","master":1,"bpm":128.0}  speed master 3.1 to an exact BPM
--                        {"op":"phase","master":1}              re-sync its phasers (FastSync)
--                        {"op":"setup"}                         report OSC lines and Art-Net timecode input
--                        {"op":"setup_apply","osc":[{port,dest,send,receive,name,prefix}],
--                         "artnet_tc":{stream,slot}}             add or fix them, then report
--   plugin -> Boothmate  {"ev":"hello","plugin":"0.1.0","ma":"2.5.1.1"}
--                        {"ev":"page","page":1,"executors":[{no,class,obj,name,addr,color,active,fader,cue}]}
--                        {"ev":"setup","osc":{input,output,lines:[...]},"artnet":{input,timecode:[...]},
--                         "changes":["..."]}
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

local VERSION = "0.3.0"
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

-- Speed masters (see docs/spikes/m4-speedmaster.md). "At BPM" sets the effective tempo exactly
-- (also with SpeedScale), does not add to the oops list and costs microseconds. FastSync counts
-- as a Learn tap, so it is rate limited and followed by the exact tempo again.
local SPEED_MAX_MASTER = 99
local PHASE_MIN_INTERVAL = 4 -- seconds between FastSyncs on one master
S.speed = S.speed or {}

local function setSpeed(n, bpm)
  n, bpm = math.tointeger(n), tonumber(bpm)
  if not n or n < 1 or n > SPEED_MAX_MASTER or not bpm or bpm ~= bpm or bpm <= 0 or bpm > 450 then
    return false
  end
  local st = S.speed[n] or {}
  S.speed[n] = st
  st.bpm = bpm
  Cmd(string.format("Master 3.%d At BPM %.2f", n, bpm))
  return true
end

local function phase(n)
  n = math.tointeger(n)
  if not n or n < 1 or n > SPEED_MAX_MASTER then return false end
  local st = S.speed[n] or {}
  S.speed[n] = st
  local now = os.clock()
  if st.phased and now - st.phased < PHASE_MIN_INTERVAL then return true end
  st.phased = now
  Cmd(string.format("FastSync Master 3.%d", n))
  if st.bpm then
    -- The sync counts as a tap: restore the exact tempo.
    Cmd(string.format("Master 3.%d At BPM %.2f", n, st.bpm))
  end
  return true
end

---------------------------------------------------------------------------------------------
-- Setup: OSC lines (ShowData().OSCBase) and the Art-Net timecode input
-- (Root().DeviceConfigurations.DMXProtocols.ArtNet). See docs/spikes/m5-ma3-setup.md.
---------------------------------------------------------------------------------------------

local function yes(v)
  v = tostring(v or ""):lower()
  return v == "yes" or v == "on" or v == "true" or v == "1"
end

local function set(h, prop, value)
  return pcall(function() h:Set(prop, tostring(value)) end)
end

local function kids(h)
  local out = {}
  if h == nil then return out end
  local list = call(h, "Children")
  if type(list) == "table" then
    for _, c in ipairs(list) do out[#out + 1] = c end
    return out
  end
  local n = tonumber(call(h, "Count")) or 0
  for i = 1, n do
    local ok, c = pcall(function() return h[i] end)
    if ok and c ~= nil then out[#out + 1] = c end
  end
  return out
end

local function oscBase()
  local ok, b = pcall(function() return ShowData().OSCBase end)
  if ok then return b end
end

local function artNet()
  local ok, a = pcall(function() return Root().DeviceConfigurations.DMXProtocols.ArtNet end)
  if ok then return a end
end

local function tcRows(a)
  local ok, c = pcall(function() return a.ArtNetTimecodeCollect end)
  if ok then return c end
end

local function readSetup()
  local r = { ev = "setup" }
  local b = oscBase()
  if b ~= nil then
    local lines = {}
    for i, d in ipairs(kids(b)) do
      lines[#lines + 1] = {
        i = i, name = display(d, "Name"), port = tonumber(display(d, "Port")),
        dest = display(d, "DestinationIP"), mode = display(d, "Mode"), prefix = display(d, "Prefix") or "",
        send = yes(display(d, "Send")), receive = yes(display(d, "Receive")),
      }
    end
    r.osc = { input = yes(display(b, "EnableInput")), output = yes(display(b, "EnableOutput")) }
    if #lines > 0 then r.osc.lines = lines end
  end
  local a = artNet()
  if a ~= nil then
    local rows = {}
    for i, t in ipairs(kids(tcRows(a))) do
      rows[#rows + 1] = {
        i = i, enabled = yes(display(t, "Enabled")), mode = display(t, "Mode"),
        stream = tonumber(display(t, "StreamID")), slot = tonumber(display(t, "TimecodeSlot")),
      }
    end
    r.artnet = { input = yes(display(a, "In")) }
    if #rows > 0 then r.artnet.timecode = rows end
  end
  return r
end

-- Adds or fixes what Boothmate needs and never removes or switches off anything.
local function applySetup(want)
  local changes = {}
  local function note(fmt, ...) changes[#changes + 1] = string.format(fmt, ...) end
  local b = oscBase()
  if b ~= nil and type(want.osc) == "table" then
    local reopen = false
    for _, w in ipairs(want.osc) do
      local port = math.tointeger(tonumber(w.port))
      if port then
        local line
        for _, d in ipairs(kids(b)) do
          if tonumber(display(d, "Port")) == port then line = d; break end
        end
        if line == nil then
          line = call(b, "Append")
          if line ~= nil then
            set(line, "Name", w.name or "Boothmate")
            set(line, "Mode", "UDP")
            set(line, "Port", port)
            set(line, "DestinationIP", w.dest or "127.0.0.1")
            set(line, "Prefix", w.prefix or "")
            set(line, "Send", "No")
            set(line, "Receive", "No")
            note("OSC line %d added", port)
            reopen = true
          end
        end
        if line ~= nil then
          if w.dest and display(line, "DestinationIP") ~= w.dest then
            set(line, "DestinationIP", w.dest); note("OSC line %d: destination %s", port, w.dest)
          end
          if w.send and not yes(display(line, "Send")) then set(line, "Send", "Yes"); note("OSC line %d: Send = Yes", port) end
          if w.receive and not yes(display(line, "Receive")) then
            set(line, "Receive", "Yes"); note("OSC line %d: Receive = Yes", port); reopen = true
          end
        end
      end
    end
    if not yes(display(b, "EnableOutput")) then
      set(b, "EnableOutput", "Yes"); note("OSC: Enable Output")
    elseif want.restart_output then
      -- Nothing arrives although the lines are right: switch the output off and on again.
      set(b, "EnableOutput", "No"); set(b, "EnableOutput", "Yes"); note("OSC: Enable Output switched off and on")
    end
    if not yes(display(b, "EnableInput")) then
      set(b, "EnableInput", "Yes"); note("OSC: Enable Input")
    elseif reopen then
      -- A new input port is only opened when the input is switched on again.
      set(b, "EnableInput", "No"); set(b, "EnableInput", "Yes")
    end
  end
  local a = artNet()
  if a ~= nil and type(want.artnet_tc) == "table" then
    local stream = math.tointeger(tonumber(want.artnet_tc.stream)) or 0
    local slot = math.tointeger(tonumber(want.artnet_tc.slot)) or 1
    if not yes(display(a, "In")) then set(a, "In", "Yes"); note("Art-Net: Enable Input") end
    local rows, row = tcRows(a), nil
    for _, t in ipairs(kids(rows)) do
      if tostring(display(t, "Mode")) == "Input" and tonumber(display(t, "StreamID")) == stream then row = t; break end
    end
    if row == nil then
      row = call(rows, "Append")
      if row ~= nil then note("Art-Net timecode input added") end
    end
    if row ~= nil then
      if not yes(display(row, "Enabled")) then set(row, "Enabled", "Yes"); note("Art-Net timecode: enabled") end
      if tostring(display(row, "Mode")) ~= "Input" then set(row, "Mode", "Input"); note("Art-Net timecode: Mode = Input") end
      if tonumber(display(row, "StreamID")) ~= stream then set(row, "StreamID", stream); note("Art-Net timecode: stream %d", stream) end
      if tonumber(display(row, "TimecodeSlot")) ~= slot then set(row, "TimecodeSlot", slot); note("Art-Net timecode: slot %d", slot) end
    end
  end
  for _, c in ipairs(changes) do log("setup: %s", c) end
  local r = readSetup()
  if #changes > 0 then r.changes = changes end
  return r
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
  elseif msg.op == "speed" and cl.ready then
    if not setSpeed(msg.master, msg.bpm) then
      send(cl, { ev = "error", msg = "bad speed message" })
    end
  elseif msg.op == "phase" and cl.ready then
    phase(msg.master)
  elseif msg.op == "setup" and cl.ready then
    local ok, r = pcall(readSetup)
    send(cl, ok and r or { ev = "error", msg = "setup read failed: " .. tostring(r) })
  elseif msg.op == "setup_apply" and cl.ready then
    local ok, r = pcall(applySetup, msg)
    send(cl, ok and r or { ev = "error", msg = "setup failed: " .. tostring(r) })
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
