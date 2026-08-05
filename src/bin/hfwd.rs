mod local {
    pub(crate) mod cli;
    pub(crate) mod companion;
    pub(crate) mod management;
    pub(crate) mod release;
    pub(crate) mod remote_management;
    pub(crate) mod ssh;
    pub(crate) mod support;
}

fn main() {
    local::cli::main();
}
