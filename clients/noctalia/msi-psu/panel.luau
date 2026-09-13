--!nonstrict
local snapshot = noctalia.state.get("msi-psu.snapshot")
local history = noctalia.state.get("msi-psu.history")
local series = noctalia.state.get("msi-psu.series") or {}
local historyError = noctalia.state.get("msi-psu.history-error")
local opened = false
local function label(text, size, color) return ui.label({text = text, color = color or "#ffffff", fontSize = size or 14, maxLines = 3}) end
local function connector(currents, index)
    local total, low, high = 0, math.huge, 0
    local pins = {}
    for pin, current in ipairs(currents) do
        total = total + current; low = math.min(low, current); high = math.max(high, current)
        table.insert(pins, ui.column({gap = 7, flexGrow = 1}, {
            label(string.format("%.3f A", current / 1000), 14),
            ui.progress({progress = math.min(1, current / 8000), height = 8, fill = "#70b7ff", track = "#272727", radius = 0}),
            label(tostring(pin), 12, "#b0b0b0"),
        }))
    end
    local deviation = 0
    if total > 0 then for _, current in ipairs(currents) do deviation = math.max(deviation, math.abs(current * 6 - total) * 100 / total) end end
    return ui.column({gap = 14, flexGrow = 1}, {label("Connector " .. tostring(index), 20), ui.row({gap = 10}, pins), label(total == 0 and "No load observed" or string.format("%.3f A total · %.1f%% deviation", total / 1000, deviation), 14)})
end
local function openGui(incident)
    local command = "msi-psu-gui"
    if incident then command = command .. " --page 2 --incident '" .. tostring(incident):gsub("'", "'\\''") .. "'" end
    noctalia.runAsync(command)
end
local function render()
    if not opened then return end
    local s = snapshot and snapshot.snapshot or nil
    local t = s and s.telemetry or nil
    local stale = snapshot == nil or snapshot.stale
    local children = {ui.row({justify = "space_between", align = "center"}, {
        label(s and s.device and s.device.model or "MSI PSU", 22),
        ui.button({text = "Open full view", variant = "outline", onClick = function() openGui(nil) end}),
    }), label(stale and "Monitoring interrupted. Readings are frozen." or "Monitoring", 14, stale and "#f4c570" or "#79c5a3")}
    if s and s.storage_error then table.insert(children, label("History recording failed: " .. s.storage_error, 15, "#ff8096")) end
    if historyError then table.insert(children, label("History unavailable. Previous totals may be outdated: " .. historyError, 14, "#f4c570")) end
    if s and ((tonumber(s.dropped_incident_updates) or 0) > 0 or (tonumber(s.dropped_evidence_samples) or 0) > 0 or (tonumber(s.truncated_evidence_windows) or 0) > 0) then
        table.insert(children, label("Incident evidence is incomplete after a storage backlog or alert storm.", 14, "#f4c570"))
    end
    if t then
        table.insert(children, ui.row({gap = 28}, {label(string.format("%.1f W output", t.power_uw / 1000000), 22), label(string.format("%.1f%% efficiency", t.efficiency_millipercent / 1000), 20), label(string.format("%.1f °C", t.temperature_mc / 1000), 20), label(tostring(t.fan.rpm) .. " RPM", 20)}))
        table.insert(children, ui.separator({color = "#272727"}))
        local connectors = {}
        for index, currents in ipairs(t.connector_currents_ma or {}) do table.insert(connectors, connector(currents, index)) end
        table.insert(children, ui.row({gap = 28}, connectors))
        table.insert(children, ui.graph({values = series, color = stale and "#777777" or "#70b7ff", height = 80, fillOpacity = 0, lineWidth = 2}))
    end
    if history then
        local buckets = history.energy and history.energy.buckets or {}
        for _, b in ipairs(buckets) do
            table.insert(children, label(string.format("Today  %.4f kWh estimated wall · %s %s", b.estimated_input_kwh, b.currency or "", b.cost or "Add a price per kWh"), 20, "#e8cb83"))
            table.insert(children, label(string.format("%.1f minutes recorded%s", b.output_observed_ms / 60000, b.incomplete_pricing and " · partial pricing" or ""), 13, "#b0b0b0"))
        end
    end
    table.insert(children, ui.separator({color = "#272727"}))
    table.insert(children, label("Recent incidents", 20))
    local incidents = history and history.incidents and history.incidents.incidents or (s and s.active_incidents or {})
    if #incidents == 0 then table.insert(children, label("No incidents recorded.", 14)) end
    for index, incident in ipairs(incidents) do
        if index > 4 then break end
        table.insert(children, ui.row({justify = "space_between", gap = 12}, {label(incident.message, 14, incident.severity == "critical" and "#ff8096" or "#f4c570"), ui.button({text = incident.recovered_at_ms and "Recovered" or "View incident", controlSize = "sm", onClick = function() openGui(incident.id) end})}))
    end
    panel.render(ui.scroll({padding = 24, gap = 18, fill = "#000000", flexGrow = 1}, children))
end
function onOpen() opened = true; render() end
function onClose() opened = false end
noctalia.state.watch("msi-psu.snapshot", function(v) snapshot = v; render() end)
noctalia.state.watch("msi-psu.history", function(v) history = v; render() end)
noctalia.state.watch("msi-psu.series", function(v) series = v or {}; render() end)
noctalia.state.watch("msi-psu.history-error", function(v) historyError = v; render() end)
