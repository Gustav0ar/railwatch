--!nonstrict
local last = 0
local snapshot = nil
local series = {}
local previous = nil
local function publish(line)
    local event = noctalia.json.decode(line)
    if type(event) ~= "table" then return end
    if event.kind == "snapshot" then
        snapshot = event.data
        last = os.time()
        local t = snapshot.snapshot.telemetry
        if t ~= nil then
            local key = tostring(t.session_id) .. ":" .. tostring(t.sequence)
            if key ~= previous then
                if snapshot.stale then series = {} else
                    table.insert(series, math.min(1, (tonumber(t.power_uw) or 0) / 1600000000))
                    if #series > 180 then table.remove(series, 1) end
                end
                previous = key
                local copy = {}
                for _, v in ipairs(series) do table.insert(copy, v) end
                noctalia.state.set("msi-psu.series", copy)
            end
        end
        noctalia.state.set("msi-psu.snapshot", snapshot)
        noctalia.state.set("msi-psu.error", nil)
    elseif event.kind == "history" then
        noctalia.state.set("msi-psu.history", event)
        noctalia.state.set("msi-psu.history-error", nil)
    elseif event.kind == "history_error" then
        noctalia.state.set("msi-psu.history-error", event.message)
    elseif event.kind == "offline" then
        noctalia.state.set("msi-psu.error", event.message)
        if snapshot then snapshot.stale = true; noctalia.state.set("msi-psu.snapshot", snapshot) end
    end
end
if not noctalia.runStream("msi-psu plugin-stream", publish) then
    noctalia.state.set("msi-psu.error", "Could not start the PSU monitor")
end
function update()
    noctalia.setUpdateInterval(1000)
    if os.time() - last > 5 and snapshot and not snapshot.stale then
        snapshot.stale = true
        noctalia.state.set("msi-psu.snapshot", snapshot)
        noctalia.state.set("msi-psu.error", "Monitoring interrupted. Readings are frozen.")
    end
end
