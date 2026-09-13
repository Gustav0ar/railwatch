-- Execute the real entry scripts against the documented host API without installing them.
local mode = TEST_MODE
local values, watchers, tree = {}, {}, nil
local callback
local mock = {snapshot = {connected = true, device = {model = "MPG Ai1600TS"}, active_incidents = {}, telemetry = {session_id="s",sequence=1,power_uw=180000000,temperature_mc=52000,efficiency_millipercent=81625,fan={rpm=400},connector_currents_ma={{0,0,0,0,0,0},{375,375,375,375,375,437}}}}, stale = false}
values["msi-psu.snapshot"] = mock
local function contains(node, text)
    if type(node) ~= "table" then return false end
    if node.props and type(node.props.text)=="string" and node.props.text:find(text,1,true) then return true end
    for _,child in ipairs(node.children or {}) do if contains(child,text) then return true end end
    return false
end
noctalia = {state = {get = function(k) return values[k] end, set = function(k,v) values[k]=v; if watchers[k] then watchers[k](v) end end, watch=function(k,f)watchers[k]=f end}, setUpdateInterval=function()end, togglePanel=function()end, pluginDir=function()return "/tmp/plugin"end, runStream=function(_,f)callback=f;return true end, runAsync=function()return true end, log=function()end,json={decode=function(v)return v end}}
ui = setmetatable({}, {__index=function(_,kind)return function(props,children)return {kind=kind,props=props or {},children=children or {}}end end})
barWidget = {isVertical=function()return false end,render=function(t)tree=t end,setTooltip=function()end}
panel = {render=function(t)tree=t end}
load_entry()
if mode=="panel" then
    onOpen();assert(contains(tree,"Connector 2"));assert(contains(tree,"No load observed"));assert(contains(tree,"180.0 W output"));assert(contains(tree,"No incidents recorded."))
    mock.stale=true;watchers["msi-psu.snapshot"](mock);assert(contains(tree,"Readings are frozen"))
    mock.snapshot.active_incidents={{id="incident",message="Current imbalance",severity="critical"}};watchers["msi-psu.snapshot"](mock);assert(contains(tree,"Current imbalance"));onClose()
elseif mode=="widget" then
    update();assert(contains(tree,"180 W"));mock.stale=true;watchers["msi-psu.snapshot"](mock);assert(contains(tree,"stale"));mock.snapshot.active_incidents={{}};watchers["msi-psu.snapshot"](mock);assert(contains(tree,"1 alerts"))
elseif mode=="service" then
    assert(callback);callback({kind="snapshot",data=mock});assert(values["msi-psu.snapshot"]==mock);assert(#values["msi-psu.series"]==1);callback({kind="snapshot",data=mock});assert(#values["msi-psu.series"]==1);callback({kind="offline",message="lost"});assert(values["msi-psu.snapshot"].stale==true)
else error("unknown test") end
print(mode .. " behavior passed")
