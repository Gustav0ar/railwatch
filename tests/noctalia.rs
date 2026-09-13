//! Run the real Luau entries in the Luau VM without installing a daily-driver plugin.
#[test]
fn panel_widget_and_service_in_luau_vm() {
    for (mode, source) in [
        (
            "panel",
            include_str!("../clients/noctalia/railwatch/panel.luau"),
        ),
        (
            "widget",
            include_str!("../clients/noctalia/railwatch/widget.luau"),
        ),
        (
            "service",
            include_str!("../clients/noctalia/railwatch/service.luau"),
        ),
    ] {
        let lua = mlua::Lua::new();
        lua.globals().set("TEST_MODE", mode).unwrap();
        lua.globals()
            .set(
                "load_entry",
                lua.create_function(move |lua, ()| lua.load(source).set_name(mode).exec())
                    .unwrap(),
            )
            .unwrap();
        lua.load(include_str!("plugin_host.luau"))
            .set_name("host test")
            .exec()
            .unwrap();
    }
}
