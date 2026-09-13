--!nonstrict
local snapshot = noctalia.state.get("railwatch.snapshot")
local function render()
    local s = snapshot and snapshot.snapshot or nil
    local t = s and s.telemetry or nil
    local active = s and s.active_incidents or {}
    local stale = snapshot == nil or snapshot.stale
    local color = #active > 0 and "#ff8096" or (stale and "#f4c570" or "#ffffff")
    local children = {ui.glyph({name = "bolt", size = 15, color = color})}
    if not barWidget.isVertical() then
        table.insert(children, ui.label({text = t and string.format("%.0f W", t.power_uw / 1000000) or "PSU", color = color}))
        if stale then table.insert(children, ui.label({text = "stale", color = "#f4c570"})) end
        if #active > 0 then table.insert(children, ui.label({text = tostring(#active) .. " alerts", color = color})) end
    end
    barWidget.render((barWidget.isVertical() and ui.column or ui.row)({gap = 6, align = "center"}, children))
    barWidget.setTooltip(stale and "PSU monitoring interrupted" or (#active > 0 and "PSU incidents need attention" or "Open PSU connector monitoring"))
end
function onClick() noctalia.togglePanel("gustav0ar/railwatch:dashboard") end
noctalia.state.watch("railwatch.snapshot", function(value) snapshot = value; render() end)
function update() noctalia.setUpdateInterval(1000); render() end
