use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use herdr_fwd::registry::Ssh;

const SSH_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const SSH_FORWARD_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const SSH_DEPLOYMENT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub(crate) struct SshClient {
    pub(crate) target: String,
    pub(crate) control_path: std::path::PathBuf,
}

impl SshClient {
    pub(crate) fn invoke(&self, arguments: &[String]) -> Result<String, String> {
        self.invoke_with_timeout(arguments, SSH_CONTROL_TIMEOUT, "SSH control command")
    }

    fn invoke_with_timeout(
        &self,
        arguments: &[String],
        timeout: Duration,
        operation: &str,
    ) -> Result<String, String> {
        let child = isolated_command("ssh")
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to execute ssh: {error}"))?;
        let output = wait_with_output_timeout(child, timeout, operation)?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    fn start_master(&self, reconnecting: bool, stop: &AtomicBool) -> Result<Child, String> {
        let mut child = Command::new("ssh")
            .args([
                "-M",
                "-S",
                &self.control_path.display().to_string(),
                "-o",
                "ControlMaster=yes",
                "-o",
                "ControlPersist=no",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=3",
                "-o",
                "ServerAliveCountMax=2",
                "-o",
                if reconnecting {
                    "ConnectTimeout=3"
                } else {
                    "ConnectTimeout=10"
                },
                "-o",
                "ConnectionAttempts=1",
            ])
            .args(if reconnecting {
                &["-o", "BatchMode=yes"][..]
            } else {
                &[][..]
            })
            .args([self.target.as_str(), "cat >/dev/null"])
            // The remote `cat` is a lifetime guard. If the wrapper is killed
            // without running Drop, the OS closes this pipe, the primary SSH
            // session exits, and ControlPersist=no tears down every forward.
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to start SSH master: {error}"))?;
        let started = Instant::now();
        let timeout = Duration::from_secs(if reconnecting { 4 } else { 60 });
        while started.elapsed() < timeout && !stop.load(Ordering::SeqCst) {
            if self
                .invoke(&[
                    "-S".into(),
                    self.control_path.display().to_string(),
                    "-O".into(),
                    "check".into(),
                    self.target.clone(),
                ])
                .is_ok()
            {
                return Ok(child);
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|error| format!("failed to inspect SSH master: {error}"))?
            {
                return Err(format!("SSH master exited before it was ready: {status}"));
            }
            thread::sleep(Duration::from_millis(200));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err(if stop.load(Ordering::SeqCst) {
            "SSH connection cancelled".into()
        } else {
            format!(
                "SSH master did not become ready within {} seconds",
                timeout.as_secs()
            )
        })
    }

    pub(crate) fn reverse(&self, remote_port: u16, local_port: u16) -> Result<(), String> {
        let arguments = control_forward_arguments(
            &self.control_path,
            &self.target,
            "forward",
            "-R",
            &format!("127.0.0.1:{remote_port}:127.0.0.1:{local_port}"),
        );
        self.invoke_with_timeout(&arguments, SSH_FORWARD_TIMEOUT, "SSH reverse forward")?;
        Ok(())
    }

    pub(crate) fn remote_command(&self, command: &str) -> Result<String, String> {
        self.remote_command_with_timeout(command, SSH_CONTROL_TIMEOUT)
    }

    pub(crate) fn remote_command_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        self.invoke_with_timeout(
            &[
                "-S".into(),
                self.control_path.display().to_string(),
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=3".into(),
                self.target.clone(),
                command.into(),
            ],
            timeout,
            "SSH remote command",
        )
    }

    pub(crate) fn remote_command_with_stdin(
        &self,
        command: &str,
        input: &[u8],
    ) -> Result<String, String> {
        self.remote_command_with_stdin_timeout(command, input, SSH_CONTROL_TIMEOUT)
    }

    pub(crate) fn remote_command_with_stdin_timeout(
        &self,
        command: &str,
        input: &[u8],
        timeout: Duration,
    ) -> Result<String, String> {
        let mut child = isolated_command("ssh")
            .args([
                "-S",
                &self.control_path.display().to_string(),
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=3",
                &self.target,
                command,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to execute ssh: {error}"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open SSH stdin".to_string())?;
        let input = input.to_vec();
        let upload = thread::spawn(move || stdin.write_all(&input));
        let output_result = wait_with_output_timeout(child, timeout, "SSH session upload");
        let upload_result = upload
            .join()
            .map_err(|_| "SSH upload thread panicked".to_string())?;
        let output = output_result?;
        if output.status.success() {
            upload_result.map_err(|error| format!("failed to upload remote session: {error}"))?;
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    fn stop(&self) {
        let _ = self.invoke(&[
            "-S".into(),
            self.control_path.display().to_string(),
            "-O".into(),
            "exit".into(),
            self.target.clone(),
        ]);
    }
}

fn isolated_command(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

fn kill_process_group(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        // Every child created here owns a private process group, including
        // ProxyCommand descendants that can otherwise hold captured pipes open.
        let _ = libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.kill();
}

fn wait_with_output_timeout(
    mut child: Child,
    timeout: Duration,
    operation: &str,
) -> Result<std::process::Output, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture ssh stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture ssh stderr".to_string())?;
    let stdout = thread::spawn(move || read_stream(stdout));
    let stderr = thread::spawn(move || read_stream(stderr));
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                kill_process_group(&mut child);
                return collect_output(status, stdout, stderr);
            }
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                kill_process_group(&mut child);
                let _ = child.wait();
                let _ = stdout.join();
                let _ = stderr.join();
                return Err(format!(
                    "{operation} timed out after {}s",
                    timeout.as_secs()
                ));
            }
            Err(error) => {
                kill_process_group(&mut child);
                let _ = child.wait();
                let _ = stdout.join();
                let _ = stderr.join();
                return Err(format!("failed to inspect ssh process: {error}"));
            }
        }
    }
}

fn read_stream(mut stream: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn collect_output(
    status: std::process::ExitStatus,
    stdout: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stderr: thread::JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<std::process::Output, String> {
    let stdout = stdout
        .join()
        .map_err(|_| "failed to join ssh stdout reader".to_string())?
        .map_err(|error| format!("failed to read ssh stdout: {error}"))?;
    let stderr = stderr
        .join()
        .map_err(|_| "failed to join ssh stderr reader".to_string())?
        .map_err(|error| format!("failed to read ssh stderr: {error}"))?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

impl Ssh for SshClient {
    fn forward(&self, local: u16, host: &str, remote: u16) -> Result<(), String> {
        self.invoke_with_timeout(
            &control_forward_arguments(
                &self.control_path,
                &self.target,
                "forward",
                "-L",
                &format!("127.0.0.1:{local}:{}:{remote}", ssh_forward_host(host)),
            ),
            SSH_FORWARD_TIMEOUT,
            "SSH forward command",
        )?;
        Ok(())
    }

    fn cancel(&self, local: u16, host: &str, remote: u16) -> Result<(), String> {
        self.invoke_with_timeout(
            &control_forward_arguments(
                &self.control_path,
                &self.target,
                "cancel",
                "-L",
                &format!("127.0.0.1:{local}:{}:{remote}", ssh_forward_host(host)),
            ),
            SSH_FORWARD_TIMEOUT,
            "SSH cancel command",
        )?;
        Ok(())
    }
}

fn ssh_forward_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn control_forward_arguments(
    control_path: &Path,
    target: &str,
    operation: &str,
    direction: &str,
    specification: &str,
) -> Vec<String> {
    vec![
        "-S".into(),
        control_path.display().to_string(),
        "-O".into(),
        operation.into(),
        direction.into(),
        specification.into(),
        target.into(),
    ]
}

pub(crate) struct OwnedMaster {
    client: SshClient,
    child: Child,
}

impl OwnedMaster {
    pub(crate) fn start(client: SshClient) -> Result<Self, String> {
        let child = client.start_master(false, &AtomicBool::new(false))?;
        Ok(Self { client, child })
    }

    pub(crate) fn reconnect(client: SshClient, stop: &AtomicBool) -> Result<Self, String> {
        let child = client.start_master(true, stop)?;
        Ok(Self { client, child })
    }

    pub(crate) fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub(crate) fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.client.control_path);
    }
}

impl Drop for OwnedMaster {
    fn drop(&mut self) {
        self.client.stop();
        for _ in 0..10 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn remote_session_install_command(remote_path: &str) -> String {
    let temporary_path = format!("{remote_path}.tmp");
    let parent_directory = remote_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .expect("remote session path must have a parent directory");
    format!(
        "set -eu; install -d -m 700 \"{parent_directory}\"; umask 077; cat > \"{temporary_path}\"; chmod 600 \"{temporary_path}\"; mv -f \"{temporary_path}\" \"{remote_path}\""
    )
}

#[cfg(test)]
mod ssh_tests {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };

    use crate::local::support::RuntimeDirectory;

    use super::{control_forward_arguments, remote_session_install_command};

    #[test]
    fn forms_ssh_control_arguments_without_a_shell() {
        let arguments = control_forward_arguments(
            std::path::Path::new("/tmp/session/c"),
            "workbox",
            "forward",
            "-L",
            "127.0.0.1:5173:127.0.0.1:5173",
        );
        assert_eq!(
            arguments,
            [
                "-S",
                "/tmp/session/c",
                "-O",
                "forward",
                "-L",
                "127.0.0.1:5173:127.0.0.1:5173",
                "workbox"
            ]
        );
    }

    #[test]
    fn installs_the_session_payload_from_stdin_with_private_permissions() {
        let runtime = RuntimeDirectory::create("herdr-fwd-session-install-test-").unwrap();
        let command =
            remote_session_install_command("$HOME/.local/state/herdr-fwd/session-abc.json");
        let payload = br#"{"token":"sentinel"}"#;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("HOME", runtime.path())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(payload).unwrap();
        drop(stdin);

        assert!(child.wait().unwrap().success());
        let session = runtime
            .path()
            .join(".local/state/herdr-fwd/session-abc.json");
        assert_eq!(std::fs::read(&session).unwrap(), payload);
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&session).unwrap()) & 0o777,
            0o600
        );
        assert!(!session.with_extension("json.tmp").exists());
    }
}

#[test]
fn ssh_timeout_reaps_descendants_holding_pipes() {
    let child = isolated_command("sh")
        .args(["-c", "sleep 10 & wait"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    assert!(wait_with_output_timeout(child, Duration::from_millis(100), "test").is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn completed_ssh_leader_does_not_leave_pipe_readers_waiting() {
    let child = isolated_command("sh")
        .args(["-c", "sleep 10 & exit 0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    assert!(
        wait_with_output_timeout(child, Duration::from_secs(1), "test")
            .unwrap()
            .status
            .success()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}
