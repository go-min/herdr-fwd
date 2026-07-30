use std::{
    io::Write,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use herdr_fwd::registry::Ssh;

const SSH_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const SSH_FORWARD_TIMEOUT: Duration = Duration::from_secs(3);

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
        let child = Command::new("ssh")
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

    fn start_master(&self) -> Result<Child, String> {
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
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                &self.target,
                "cat >/dev/null",
            ])
            // The remote `cat` is a lifetime guard. If the wrapper is killed
            // without running Drop, the OS closes this pipe, the primary SSH
            // session exits, and ControlPersist=no tears down every forward.
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to start SSH master: {error}"))?;
        for _ in 0..300 {
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
        Err("SSH master did not become ready within 60 seconds".into())
    }

    pub(crate) fn reverse(&self, remote_port: u16, local_port: u16) -> Result<(), String> {
        let arguments = control_forward_arguments(
            &self.control_path,
            &self.target,
            "forward",
            "-R",
            &format!("127.0.0.1:{remote_port}:127.0.0.1:{local_port}"),
        );
        self.invoke(&arguments)?;
        Ok(())
    }

    pub(crate) fn remote_command(&self, command: &str) -> Result<String, String> {
        self.invoke(&[
            "-S".into(),
            self.control_path.display().to_string(),
            self.target.clone(),
            command.into(),
        ])
    }

    pub(crate) fn remote_command_with_stdin(
        &self,
        command: &str,
        input: &[u8],
    ) -> Result<String, String> {
        let mut child = Command::new("ssh")
            .args([
                "-S",
                &self.control_path.display().to_string(),
                &self.target,
                command,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to execute ssh: {error}"))?;
        let upload = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open SSH stdin".to_string())?
            .write_all(input);
        if let Err(error) = upload {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("failed to upload remote session: {error}"));
        }
        let output = wait_with_output_timeout(child, SSH_CONTROL_TIMEOUT, "SSH session upload")?;
        if output.status.success() {
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

fn wait_with_output_timeout(
    mut child: Child,
    timeout: Duration,
    operation: &str,
) -> Result<std::process::Output, String> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|error| format!("failed to collect ssh output: {error}"));
            }
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{operation} timed out after {}s",
                    timeout.as_secs()
                ));
            }
            Err(error) => return Err(format!("failed to inspect ssh process: {error}")),
        }
    }
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
        let child = client.start_master()?;
        Ok(Self { client, child })
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
    format!(
        "set -eu; install -d -m 700 \"$HOME/.cache/herdr-fwd\"; umask 077; cat > {temporary_path}; chmod 600 {temporary_path}; mv -f {temporary_path} {remote_path}"
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
        let command = remote_session_install_command("$HOME/.cache/herdr-fwd/session-abc.json");
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
        let session = runtime.path().join(".cache/herdr-fwd/session-abc.json");
        assert_eq!(std::fs::read(&session).unwrap(), payload);
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&session).unwrap()) & 0o777,
            0o600
        );
        assert!(!session.with_extension("json.tmp").exists());
    }
}
