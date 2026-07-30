mod plugin {
    pub(crate) mod dashboard_actions;
    pub(crate) mod dashboard_render;
    pub(crate) mod dashboard_terminal;
    pub(crate) mod herdr;
    pub(crate) mod lifecycle;
    pub(crate) mod notifications;
    pub(crate) mod onboarding;
    pub(crate) mod preferences;
    pub(crate) mod rpc;
    pub(crate) mod session;
    pub(crate) mod sidebar_config;
}

fn main() {
    plugin::lifecycle::main();
}
