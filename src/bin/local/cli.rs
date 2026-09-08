use std::{
    env, fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use herdr_fwd::{
    herdr_session_storage_key, registry::Registry, shell::quote as shell_quote,
    RemoteSessionConfig, PROTOCOL_VERSION,
};
use tiny_http::Server;

use crate::local::{
    companion::{
        cleanup_registry, establish_reverse_rpc, install_remote_session, manual_forwards_path,
        paused_forwards_path, spawn_server, CompanionState,
    },
    management::{
        close_all_forwards, close_forward, create_manual_forward, list_forwards, watch_forwards,
        MANUAL_FORWARD_USAGE,
    },
    remote_management::{ensure_remote_plugin, manage_remote, validate_ssh_target},
    ssh::{OwnedMaster, SshClient},
    support::{
        doctor, require_herdr_compatibility, require_local_herdr_compatibility, require_ssh,
        secure_random_hex, state_directory, RuntimeDirectory,
    },
    transport::TransportSupervisor,
};

const HEARTBEAT_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

struct Cli {
    target: String,
    open: bool,
    verbose: bool,
    auto_detect: bool,
    herdr_arguments: Vec<String>,
}

pub(crate) fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match arguments.first().map(String::as_str) {
        Some("help" | "--help" | "-h") => {
            print_help();
            return Ok(());
        }
        Some("version" | "--version" | "-V") => {
            println!("hfwd {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("list") => return list_forwards(),
        Some("status") => return watch_forwards(),
        Some("close") => return close_forward(arguments.get(1)),
        Some("close-all") => return close_all_forwards(),
        Some("forward") => return create_manual_forward(&arguments[1..]),
        Some("doctor") => return doctor(arguments.get(1)),
        Some("hook") => return run_hook(&arguments[1..]),
        Some("remote") => return manage_remote(&arguments[1..]),
        _ => {}
    }

    let cli = parse_cli(&arguments)?;
    require_ssh()?;
    require_local_herdr_compatibility()?;
    let stop_requested = Arc::new(AtomicBool::new(false));
    let stop_for_signal = stop_requested.clone();
    ctrlc::set_handler(move || stop_for_signal.store(true, Ordering::SeqCst))
        .map_err(|error| format!("failed to install signal handler: {error}"))?;

    let temporary = RuntimeDirectory::create("herdr-fwd-")?;
    let client = SshClient {
        target: cli.target.clone(),
        control_path: temporary.path().join("c"),
    };
    let master = OwnedMaster::start(client.clone())?;
    let remote_version = client
        .remote_command("export PATH=\"$HOME/.local/bin:/usr/local/bin:$PATH\"; herdr --version")?;
    require_herdr_compatibility(&remote_version, "remote")?;
    ensure_remote_plugin(&client)?;
    let host_key = client
        .remote_command("hostname -f 2>/dev/null || hostname")
        .unwrap_or_else(|_| cli.target.clone())
        .trim()
        .to_string();
    let herdr_session = remote_herdr_session_name(&cli);

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("failed to bind local companion: {error}"))?;
    let companion_port = listener
        .local_addr()
        .map_err(|error| format!("failed to inspect companion address: {error}"))?
        .port();
    let server = Server::from_listener(listener, None)
        .map_err(|error| format!("failed to start companion HTTP server: {error}"))?;
    let remote_rpc_port = establish_reverse_rpc(&client, companion_port)?;
    let token = secure_random_hex(32)?;
    let session_id = secure_random_hex(12)?;
    let remote_path = remote_session_path(&herdr_session, &session_id)?;
    let remote_config = RemoteSessionConfig {
        protocol_version: PROTOCOL_VERSION,
        session_id: session_id.clone(),
        herdr_session: herdr_session.clone(),
        token: token.clone(),
        rpc_url: format!("http://127.0.0.1:{remote_rpc_port}"),
        auto_detect: cli.auto_detect,
    };
    let state_path = state_directory()?.join(format!("session-{session_id}.json"));
    let companion_url = format!("http://127.0.0.1:{companion_port}");
    let companion = Arc::new(CompanionState {
        session_id,
        target: cli.target.clone(),
        companion_url,
        token,
        wrapper_pid: std::process::id(),
        registry: Mutex::new(Registry::new(client.clone())),
        state_path: state_path.clone(),
        manual_path: manual_forwards_path(&host_key, &herdr_session)?,
        paused_path: paused_forwards_path(&host_key, &herdr_session)?,
        last_heartbeat: Mutex::new(None),
        open_new: cli.open,
        reconnecting: AtomicBool::new(false),
    });
    companion.restore_manual()?;
    companion.persist()?;
    let server_stop = Arc::new(AtomicBool::new(false));
    let server_thread = spawn_server(server, companion.clone(), server_stop.clone());
    if let Err(error) = install_remote_session(&client, &remote_path, &remote_config) {
        server_stop.store(true, Ordering::SeqCst);
        let _ = server_thread.join();
        let _ = fs::remove_file(&state_path);
        return Err(error);
    }
    // Startup hooks do not run when a client attaches. Wake the action on an
    // already-running server; a newly-created server will run [[startup]].
    wake_remote_watcher(&client, &cli);

    println!(
        "Connecting to {}…\nRemote port discovery enabled.",
        cli.target
    );
    if cli.verbose {
        eprintln!(
            "session={} companion=127.0.0.1:{companion_port}",
            companion.session_id
        );
    }
    let transport = TransportSupervisor::start(
        master,
        client.clone(),
        companion.clone(),
        companion_port,
        remote_path.clone(),
        remote_config,
    );
    let attach_result = run_attach(&cli, &companion, &stop_requested, &transport);
    let master = transport.finish();

    server_stop.store(true, Ordering::SeqCst);
    let _ = server_thread.join();
    cleanup_registry(&companion);
    let _ = fs::remove_file(&state_path);
    let _ = client.remote_command(&format!("rm -f -- {remote_path}"));
    drop(master);
    drop(temporary);
    attach_result
}

fn wake_remote_watcher(client: &SshClient, cli: &Cli) {
    let selector = explicit_remote_herdr_session_name(cli)
        .map(|session| format!("--session {} ", shell_quote(session)))
        .unwrap_or_default();
    let command = format!(
        "export PATH=\"$HOME/.local/bin:$PATH\"; herdr {selector}plugin action invoke herdr.fwd.wake >/dev/null 2>&1 || true"
    );
    let _ = client.remote_command(&command);
}

fn remote_session_path(herdr_session: &str, session_id: &str) -> Result<String, String> {
    let scope = herdr_session_storage_key(herdr_session)?;
    Ok(format!(
        "$HOME/.cache/herdr-fwd/sessions/{scope}/session-{session_id}.json"
    ))
}

fn remote_herdr_session_name(cli: &Cli) -> String {
    explicit_remote_herdr_session_name(cli)
        .unwrap_or("default")
        .to_owned()
}

fn explicit_remote_herdr_session_name(cli: &Cli) -> Option<&str> {
    cli.herdr_arguments
        .windows(2)
        .find(|arguments| arguments[0] == "--session")
        .map(|arguments| arguments[1].as_str())
        .or_else(|| {
            cli.herdr_arguments.iter().find_map(|argument| {
                argument
                    .strip_prefix("--session=")
                    .filter(|session| !session.is_empty())
            })
        })
}

fn parse_cli(arguments: &[String]) -> Result<Cli, String> {
    let Some(target) = arguments.first() else {
        return Err(
            "usage: hfwd <target> [--open] [--verbose] [--no-auto-detect] -- [herdr arguments]"
                .into(),
        );
    };
    if target.starts_with('-') {
        return Err("remote target must be the first argument".into());
    }
    validate_ssh_target(target)?;
    let separator = arguments.iter().position(|argument| argument == "--");
    let wrapper_arguments = &arguments[1..separator.unwrap_or(arguments.len())];
    let mut open = false;
    let mut verbose = env::var("HERDR_FWD_LOG").as_deref() == Ok("debug");
    let mut auto_detect = true;
    let mut index = 0;
    while index < wrapper_arguments.len() {
        match wrapper_arguments[index].as_str() {
            "--open" => {
                open = true;
                index += 1;
            }
            "--verbose" => {
                verbose = true;
                index += 1;
            }
            "--no-auto-detect" => {
                auto_detect = false;
                index += 1;
            }
            "--local-bind" => {
                let value = wrapper_arguments
                    .get(index + 1)
                    .ok_or_else(|| "--local-bind requires 127.0.0.1".to_string())?;
                if value != "127.0.0.1" {
                    return Err("--local-bind only supports 127.0.0.1".into());
                }
                index += 2;
            }
            other => return Err(format!("unknown wrapper option: {other}")),
        }
    }
    let herdr_arguments = separator
        .map(|index| arguments[index + 1..].to_vec())
        .unwrap_or_default();
    Ok(Cli {
        target: target.clone(),
        open,
        verbose,
        auto_detect,
        herdr_arguments,
    })
}

fn print_help() {
    println!(
        "hfwd {version}\n\n\
Usage:\n  hfwd <target> [options] -- [herdr arguments]\n  \
hfwd <command> [arguments]\n\n\
Session options:\n  --open                 Open each newly forwarded URL once\n  --verbose              Show session diagnostics (never the token)\n  --no-auto-detect       Disable pane-output discovery\n  --local-bind 127.0.0.1 Local bind (loopback is the only supported value)\n\n\
Commands:\n  list                       List forwards in active local sessions\n  status                     Continuously refresh the forward list\n  {MANUAL_FORWARD_USAGE}\n                             Create a manual forward; start it first with hfwd <target>\n  close <id|local-port>      Close one forward\n  close-all                  Close every active forward\n  doctor [target]            Validate local and optional remote prerequisites\n  remote install <target>    Install and enable the managed remote plugin\n  remote update <target>     Reinstall the managed remote plugin from GitHub\n  remote status <target>     Show remote plugin registration state\n  remote uninstall <target>  Disable and remove the managed remote plugin\n  hook [zsh|bash|fish]       Print an interceptor for herdr --remote\n  hook install [shell]       Install it in the current shell config\n  help                        Show this help\n  version                     Show the wrapper version",
        version = env!("CARGO_PKG_VERSION")
    );
}

fn run_hook(arguments: &[String]) -> Result<(), String> {
    if arguments.first().map(String::as_str) == Some("install") {
        if arguments.len() > 2 {
            return Err("usage: hfwd hook install [zsh|bash|fish]".into());
        }
        let shell_environment = env::var("SHELL").ok();
        let shell = match arguments.get(1) {
            Some(shell) => hook_source(shell).map(|_| shell.as_str())?,
            None => detect_shell(shell_environment.as_deref())?,
        };
        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is not set; cannot install the shell hook".to_string())?;
        let path = install_hook(shell, &home)?;
        println!(
            "Installed {shell} hook in {}. Restart the shell or source that file.",
            path.display()
        );
        return Ok(());
    }
    if arguments.len() > 1 {
        return Err("usage: hfwd hook [zsh|bash|fish]".into());
    }
    let shell = arguments.first().map(String::as_str).unwrap_or("zsh");
    println!("{}", hook_source(shell)?);
    Ok(())
}

fn hook_source(shell: &str) -> Result<&'static str, String> {
    match shell {
        "bash" | "zsh" => Ok(
            "herdr() {\n  if [ \"${1:-}\" = --remote ] && [ \"$#\" -ge 2 ]; then\n    command hfwd \"$2\" -- \"${@:3}\"\n  else\n    command herdr \"$@\"\n  fi\n}",
        ),
        "fish" => Ok(
            "function herdr\n  if test (count $argv) -ge 2; and test \"$argv[1]\" = --remote\n    command hfwd $argv[2] -- $argv[3..-1]\n  else\n    command herdr $argv\n  end\nend\n",
        ),
        other => Err(format!(
            "unsupported shell for hook: {other} (use zsh, bash, or fish)"
        )),
    }
}

fn detect_shell(shell: Option<&str>) -> Result<&str, String> {
    let shell = shell
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "SHELL is not set; pass zsh, bash, or fish explicitly".to_string())?;
    let name = Path::new(shell)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "SHELL does not contain a valid shell name".to_string())?;
    hook_source(name)?;
    Ok(name)
}

fn install_hook(shell: &str, home: &Path) -> Result<PathBuf, String> {
    let source = hook_source(shell)?;
    if shell == "fish" {
        let path = home.join(".config/fish/conf.d/herdr-fwd.fish");
        if path.exists() {
            let existing = fs::read_to_string(&path)
                .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
            if existing == source {
                return Ok(path);
            }
            return Err(format!(
                "{} already exists and was not created by this hook installer",
                path.display()
            ));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        fs::write(&path, source)
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        return Ok(path);
    }

    let path = home.join(if shell == "zsh" { ".zshrc" } else { ".bashrc" });
    let mut existing = match fs::read_to_string(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    const START: &str = "# >>> herdr-fwd shell hook >>>";
    const END: &str = "# <<< herdr-fwd shell hook <<<";
    if existing.contains(START) && existing.contains(END) {
        return Ok(path);
    }
    if existing.contains(START) || existing.contains(END) {
        return Err(format!(
            "{} contains an incomplete herdr-fwd hook block",
            path.display()
        ));
    }
    if !existing.is_empty() {
        if !existing.ends_with('\n') {
            existing.push('\n');
        }
        existing.push('\n');
    }
    existing.push_str(START);
    existing.push('\n');
    existing.push_str(&format!("eval \"$(hfwd hook {shell})\"\n"));
    existing.push_str(END);
    existing.push('\n');
    fs::write(&path, existing)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    Ok(path)
}

fn run_attach(
    cli: &Cli,
    companion: &Arc<CompanionState<SshClient>>,
    stop_requested: &AtomicBool,
    transport: &TransportSupervisor,
) -> Result<(), String> {
    let mut arguments = vec!["--remote".to_string(), cli.target.clone()];
    arguments.extend(cli.herdr_arguments.iter().cloned());
    let mut child = Command::new("herdr")
        .args(arguments)
        // A remote attach is a client transport, not a nested local Herdr
        // session. Permit launching the wrapper from an existing local pane.
        .env_remove("HERDR_ENV")
        .spawn()
        .map_err(|error| format!("failed to start herdr: {error}"))?;
    let attach_started = Instant::now();
    loop {
        if stop_requested.load(Ordering::SeqCst) {
            terminate_child(&mut child);
            return Ok(());
        }
        let last_heartbeat = *companion
            .last_heartbeat
            .lock()
            .map_err(|_| "heartbeat lock poisoned".to_string())?;
        if let Some(error) = transport.failure() {
            terminate_child(&mut child);
            return Err(error);
        }
        if last_heartbeat.is_none()
            && !companion.reconnecting.load(Ordering::SeqCst)
            && attach_started.elapsed() > HEARTBEAT_STARTUP_TIMEOUT
        {
            terminate_child(&mut child);
            return Err(
                "remote plugin did not start; run doctor and verify the plugin is enabled".into(),
            );
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("herdr remote attach exited with {status}")),
            Ok(None) => thread::sleep(Duration::from_millis(200)),
            Err(error) => return Err(format!("failed while waiting for herdr: {error}")),
        }
    }
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        for _ in 0..20 {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod cli_tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{
        explicit_remote_herdr_session_name, hook_source, install_hook, parse_cli,
        remote_herdr_session_name, remote_session_path,
    };

    static TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            let id = TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("herdr-fwd-hook-test-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_wrapper_and_passthrough_arguments_separately() {
        let arguments = vec![
            "workbox".into(),
            "--open".into(),
            "--verbose".into(),
            "--".into(),
            "--session".into(),
            "agents".into(),
        ];
        let cli = parse_cli(&arguments).unwrap();
        assert_eq!(cli.target, "workbox");
        assert!(cli.open);
        assert!(cli.verbose);
        assert_eq!(cli.herdr_arguments, ["--session", "agents"]);
    }

    #[test]
    fn writes_remote_session_into_the_plugin_runtime_cache_directory() {
        assert_eq!(
            remote_session_path("review", "0123456789abcdef").unwrap(),
            "$HOME/.cache/herdr-fwd/sessions/session-726576696577/session-0123456789abcdef.json"
        );
    }

    #[test]
    fn selects_the_remote_herdr_session_for_persistent_forwards() {
        let default = parse_cli(&["workbox".into()]).unwrap();
        let named = parse_cli(&[
            "workbox".into(),
            "--".into(),
            "--session".into(),
            "review".into(),
        ])
        .unwrap();
        let equals =
            parse_cli(&["workbox".into(), "--".into(), "--session=preview".into()]).unwrap();
        let empty = parse_cli(&["workbox".into(), "--".into(), "--session=".into()]).unwrap();

        assert_eq!(remote_herdr_session_name(&default), "default");
        assert_eq!(remote_herdr_session_name(&named), "review");
        assert_eq!(remote_herdr_session_name(&equals), "preview");
        assert_eq!(remote_herdr_session_name(&empty), "default");
        assert_eq!(explicit_remote_herdr_session_name(&default), None);
        assert_eq!(explicit_remote_herdr_session_name(&named), Some("review"));
        assert_eq!(explicit_remote_herdr_session_name(&equals), Some("preview"));
        assert_eq!(explicit_remote_herdr_session_name(&empty), None);
    }

    #[test]
    fn rejects_unknown_or_misplaced_wrapper_options() {
        assert!(parse_cli(&["--open".into(), "workbox".into()]).is_err());
        assert!(parse_cli(&["workbox".into(), "--unsafe-bind".into()]).is_err());
        assert!(parse_cli(&["work box".into()]).is_err());
    }

    #[test]
    fn generates_interceptors_for_zsh_bash_and_fish() {
        for shell in ["zsh", "bash"] {
            let hook = hook_source(shell).unwrap();
            assert!(hook.contains("herdr()"));
            assert!(hook.contains("command hfwd \"$2\" -- \"${@:3}\""));
            assert!(hook.contains("command herdr \"$@\""));
        }

        let fish = hook_source("fish").unwrap();
        assert!(fish.contains("function herdr"));
        assert!(fish.contains("command hfwd $argv[2] -- $argv[3..-1]"));
        assert!(fish.contains("command herdr $argv"));
        assert!(hook_source("nu")
            .unwrap_err()
            .contains("zsh, bash, or fish"));
    }

    #[test]
    fn installs_each_shell_hook_once_and_preserves_existing_configuration() {
        let temporary = TestDirectory::create();
        let zshrc = temporary.path().join(".zshrc");
        fs::write(&zshrc, "export KEEP_ME=1").unwrap();

        assert_eq!(install_hook("zsh", temporary.path()).unwrap(), zshrc);
        assert_eq!(install_hook("zsh", temporary.path()).unwrap(), zshrc);
        assert_eq!(
            fs::read_to_string(&zshrc).unwrap(),
            "export KEEP_ME=1\n\n# >>> herdr-fwd shell hook >>>\n\
eval \"$(hfwd hook zsh)\"\n\
# <<< herdr-fwd shell hook <<<\n"
        );

        let bashrc = install_hook("bash", temporary.path()).unwrap();
        assert_eq!(bashrc, temporary.path().join(".bashrc"));
        assert!(fs::read_to_string(bashrc)
            .unwrap()
            .contains("hfwd hook bash"));

        let fish = install_hook("fish", temporary.path()).unwrap();
        assert_eq!(
            fish,
            temporary.path().join(".config/fish/conf.d/herdr-fwd.fish")
        );
        assert_eq!(
            fs::read_to_string(fish).unwrap(),
            hook_source("fish").unwrap()
        );
    }
}
