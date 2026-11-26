-- A fake grandMA3 for development without a console.
--
-- Runs lua/boothmate.lua against mocked grandMA3 objects and sends OSC output like onPC 2.5.1
-- (addresses and arguments from docs/spikes/m0-feedback.md).
--
--   lua5.4 tools/fake-ma3.lua [seconds]      needs LuaSocket and dkjson (or another "json" module)
--
-- Plugin self-update: Store/Delete/Go+ Macro work, the macro's Import and Plugin lines restart the
-- plugin from the library (FAKE_MA3_LIBRARY, default a temp folder).
--
-- Timeline: cues run on sequences 1 and 2, fader 2 moves, sequence 3 is relabelled (hook), the
-- page changes to 2 and back.

local socket = require("socket")
package.preload.json = package.preload.json or function()
  local ok, m = pcall(require, "dkjson")
  if ok then return m end
  return require("cjson")
end

local duration = tonumber(arg and arg[1]) or 30
local here = (arg and arg[0] or "tools/fake-ma3.lua"):match("^(.*)[/\\]") or "."

-- Mock objects --------------------------------------------------------------------------------

local hooks, nextHook = {}, 1
local function fire(obj) for _, h in pairs(hooks) do if h.obj == obj then h.fn(obj, 11) end end end

local function object(class, addr, props)
  local o = props or {}
  function o:GetClass() return class end
  function o:Addr() return addr end
  function o:Get(prop) return self[prop] end
  return o
end

local appearances = {
  object("Appearance", "14.6.1", { no = 1, name = "Red", BackRGBA = "FF0000FF" }),
  object("Appearance", "14.6.2", { no = 2, name = "Blue", BackRGBA = "0040FFFF" }),
}

local function sequence(no, name, cues, app)
  local s = object("Sequence", "14.14.1.7." .. no, { no = no, name = name, Appearance = app })
  s.cues, s.current, s.fader = cues, nil, 100
  function s:HasActivePlayback() return self.current ~= nil end
  function s:GetFader() return self.fader end
  function s:CurrentChild() if self.current then return { no = self.current * 1000, name = self.cues[self.current] } end end
  return s
end

local seqs = {
  sequence(1, "Warm Wash", { "Warm", "Amber" }, appearances[1]),
  sequence(2, "MCP Test", { "Warm", "Rot", "Blau" }, appearances[2]),
  sequence(3, "Strobe", { "Fast" }, nil),
}
local group = object("Group", "14.14.1.6.1", { no = 1, name = "Spots" })
local speed = object("Master", "14.13.3.1", { no = 1, name = "Speed1" })

local pages = {
  object("Page", "14.13.1.1", { no = 1, name = "Main" }),
  object("Page", "14.13.1.2", { no = 2, name = "Effects" }),
}
local assign = { [1] = { [201] = seqs[1], [202] = seqs[2], [203] = seqs[3], [101] = group, [102] = speed }, [2] = { [201] = seqs[3] } }
local currentPage = 1

GetExecutor = function(n)
  local obj = assign[currentPage][n]
  if obj then return { Object = obj }, pages[currentPage] end
end
CurrentExecPage = function() return pages[currentPage] end
-- Setup objects: a collection with Children/Count/Append/Remove; Get/Set by property name.
local function item(class, props)
  local o = props or {}
  function o:GetClass() return class end
  function o:Get(p) return self[p] end
  function o:Set(p, v) self[p] = tostring(v) end
  return o
end
local function collection(class, childClass, defaults, items)
  local c = item(class, {})
  c.items = items or {}
  function c:Children() local t = {}; for i, v in ipairs(self.items) do t[i] = v end; return t end
  function c:Count() return #self.items end
  function c:Append()
    local o = item(childClass, {}); for k, v in pairs(defaults) do o[k] = v end
    self.items[#self.items + 1] = o; return o
  end
  function c:Remove(i) table.remove(self.items, i) end
  return c
end
local oscDefaults = { Name = "OSCData", Mode = "UDP", Port = "8000", DestinationIP = "10.1.255.255", Prefix = "", Send = "No", Receive = "No" }
local oscBase = collection("OSCBase", "OSCData", oscDefaults, {})
oscBase.EnableInput, oscBase.EnableOutput = "Yes", "No"
local l1 = oscBase:Append(); l1.Name = "OSCData 1"; l1.Receive = "Yes"; l1.DestinationIP = "127.0.0.1"
local artTc = collection("ArtNetTimecodeCollect", "Art-Net-Timecode", { Enabled = "No", Mode = "Output", StreamID = "0", TimecodeSlot = "1" }, {})
local artNet = item("ArtNet", { In = "No", ArtNetTimecodeCollect = artTc })
Root = function() return { DeviceConfigurations = { DMXProtocols = { ArtNet = artNet } } } end

-- Macros and the plugin library, for the self-update (see selfUpdate in lua/boothmate.lua).
local library = os.getenv("FAKE_MA3_LIBRARY") or (os.tmpname() .. "-gma3_library")
os.execute('mkdir -p "' .. library .. '/datapools/plugins"')
package.path = library .. "/datapools/plugins/?.lua;" .. package.path
local macros = collection("Macros", "Macro", {}, {})
local function macroLine() return item("MacroLine", { Command = "", Wait = "Follow" }) end
local function macroNo(n)
  for i, m in ipairs(macros.items) do if m.no == n then return m, i end end
end
ObjectList = function(ref)
  local n = tonumber(ref:match("^Macro (%d+)$"))
  return { n and macroNo(n) or nil }
end
local runMacro -- set below; macros run after the plugin's frame, like on the console
Cmd = function(cmd)
  print("fake-ma3: Cmd " .. cmd)
  local n = tonumber(cmd:match("^Store Macro (%d+) /NoConfirmation$"))
  if n and not macroNo(n) then
    local m = item("Macro", { no = n, name = "Macro " .. n, lines = {} })
    function m:Set(p, v) if p == "Name" then self.name = v else self[p] = v end end
    function m:Count() return #self.lines end
    function m:Ptr(i) return self.lines[i] end
    macros.items[#macros.items + 1] = m
    return
  end
  local mn, ln = cmd:match("^Store Macro (%d+)%.(%d+) /NoConfirmation$")
  if mn then local m = macroNo(tonumber(mn)); m.lines[tonumber(ln)] = macroLine(); return end
  n = tonumber(cmd:match("^Delete Macro (%d+) /NoConfirmation$"))
  if n then local _, i = macroNo(n); if i then table.remove(macros.items, i) end; return end
  n = tonumber(cmd:match("^Go%+ Macro (%d+)$"))
  if n then runMacro(macroNo(n)) end
end
DataPool = function() return { Sequences = object("Pool", "14.14.1.7", {}), Macros = macros } end
ShowData = function() return { Appearances = object("Pool", "14.6", {}), OSCBase = oscBase } end
Version = function() return "2.5.1.1" end
Printf = function(s) print(s) end
Enums = { Roles = { Display = 1 } }
HookObjectChange = function(fn, obj) local id = nextHook; nextHook = id + 1; hooks[id] = { fn = fn, obj = obj }; return id end
Unhook = function(id) hooks[id] = nil end

-- OSC output -----------------------------------------------------------------------------------

local udp = socket.udp()
udp:setpeername("127.0.0.1", 9001)

local function pad(s) s = s .. "\0"; return s .. string.rep("\0", (4 - #s % 4) % 4) end
local function osc(addr, ...)
  local types, data = ",", {}
  for _, a in ipairs({ ... }) do
    if math.type(a) == "integer" then types = types .. "i"; data[#data + 1] = string.pack(">i4", a)
    elseif type(a) == "number" then types = types .. "f"; data[#data + 1] = string.pack(">f", a)
    else types = types .. "s"; data[#data + 1] = pad(a) end
  end
  udp:send(pad(addr) .. pad(types) .. table.concat(data))
end

local function go(s, cue)
  s.current = cue
  local text = s.name .. ";" .. cue .. " " .. s.cues[cue]
  osc("/" .. s:Addr(), "Go+", 1, text)
  return function() osc("/" .. s:Addr(), "Go+", 0, text) end
end

-- Run ------------------------------------------------------------------------------------------

local pluginObject = { no = 1, name = "boothmate" }
local Main, Cleanup, co
local function start(file, argument)
  local chunk = assert(loadfile(file))
  Main, Cleanup = chunk("boothmate", "boothmate", {}, { Parent = function() return pluginObject end })
  co = coroutine.create(function() Main(nil, argument) end)
end
start(here .. "/../lua/boothmate.lua", "")

-- Runs the macro lines this fake understands, each after the previous line's wait.
local pending = {}
runMacro = function(m)
  local t = socket.gettime()
  for _, l in ipairs(m.lines) do
    pending[#pending + 1] = { t, l.Command }
    t = t + (tonumber(l.Wait) or 0)
  end
end
local imported
local function macroCommand(cmd)
  print("fake-ma3: macro " .. cmd)
  local file = cmd:match('^Import Plugin Library "([^"]+)" At Plugin 1$')
  if file then
    local f = assert(io.open(library .. "/datapools/plugins/" .. file))
    local xml = f:read("a"); f:close()
    imported = library .. "/datapools/plugins/" .. xml:match('FileName="([^"]+)"')
    return
  end
  local arg = cmd:match('^Plugin "boothmate" "(.*)"$') or (cmd == 'Plugin "boothmate"' and "")
  if arg then
    if coroutine.status(co) ~= "dead" then error("the old plugin still runs") end
    start(assert(imported, "nothing imported"), arg)
  end
end

local t0 = socket.gettime()
local events = {
  { 1, function() return go(seqs[1], 1) end },
  { 2, function() return go(seqs[2], 2) end },
  { 3, function() osc("/14.13.2.1", "FaderMaster", 1, 80.0) end },
  { 3.5, function() osc("/14.14.1.6.1", "FaderMaster", 1, 60.0) end },
  { 3.8, function() osc("/14.13.3.1", "FaderMaster", 3, 50.0) end },
  { 4, function() seqs[3].name = "Strobe Hard"; fire(seqs[3]) end },
  { 7, function() currentPage = 2 end },
  { 9, function() currentPage = 1 end },
  { 10, function() seqs[1].current = nil; osc("/14.14.1.7.1", "Off", 1, "Warm Wash") end },
  { 11, function() osc("/14.14.1.7.2", "Pause", 2, "MCP Test") end },
}
local release = {}
local fadeFrom, fadeTo = 5, 6.5

while socket.gettime() - t0 < duration do
  local t = socket.gettime() - t0
  while events[1] and t >= events[1][1] do
    local r = table.remove(events, 1)[2]()
    if type(r) == "function" then release[#release + 1] = { t + 0.3, r } end
  end
  for i = #release, 1, -1 do if t >= release[i][1] then table.remove(release, i)[2]() end end
  if t >= fadeFrom and t <= fadeTo then
    seqs[2].fader = 100 - (t - fadeFrom) / (fadeTo - fadeFrom) * 100 * 0.6
    osc("/14.14.1.7.2", "FaderMaster", 3, seqs[2].fader, seqs[2].name)
  end
  while pending[1] and socket.gettime() >= pending[1][1] do macroCommand(table.remove(pending, 1)[2]) end
  if coroutine.status(co) ~= "dead" then
    local ok, err = coroutine.resume(co)
    if not ok then error(err) end
    if coroutine.status(co) == "dead" then Cleanup() end
  elseif not pending[1] then
    break
  end
  socket.sleep(1 / 30)
end
Cleanup()
print("fake-ma3: done")
