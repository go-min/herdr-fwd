use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::local::{
    companion::{establish_reverse_rpc_until, CompanionState},
    ssh::{remote_session_install_command, OwnedMaster, SshClient},
};
use herdr_fwd::{shell::quote, RemoteSessionConfig};

const LEASE_TIMEOUT: Duration = Duration::from_secs(10);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Owns the transport on a worker so retrying SSH never blocks the attached
/// Herdr client's exit handling. No credentials are requested during recovery.
pub(crate) struct TransportSupervisor {
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<Option<OwnedMaster>>>,
}

impl TransportSupervisor {
    pub(crate) fn start(
        master: OwnedMaster,
        client: SshClient,
        state: Arc<CompanionState<SshClient>>,
        local_port: u16,
        remote_path: String,
        mut config: RemoteSessionConfig,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let worker_stop = stop.clone();
        let worker_failure = failure.clone();
        let thread = thread::spawn(move || {
            let mut master = Some(master);
            while !worker_stop.load(Ordering::SeqCst) {
                let expired = state
                    .last_heartbeat
                    .lock()
                    .ok()
                    .and_then(|heartbeat| *heartbeat)
                    .is_some_and(|heartbeat| heartbeat.elapsed() > LEASE_TIMEOUT);
                if master.as_mut().is_some_and(OwnedMaster::is_alive) && !expired {
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }
                state.reconnecting.store(true, Ordering::SeqCst);
                eprintln!("SSH connection lost; restoring port forwards…");
                if let Some(mut previous) = master.take() {
                    previous.terminate();
                }
                let deadline = Instant::now() + RECOVERY_TIMEOUT;
                let mut last_error = "SSH connection unavailable".to_string();
                while !worker_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                    let attempt = (|| {
                        let attempt_started = Instant::now();
                        let mut next = OwnedMaster::reconnect(client.clone(), &worker_stop)?;
                        let restored = (|| {
                            let port = establish_reverse_rpc_until(&client, local_port, || {
                                worker_stop.load(Ordering::SeqCst) || Instant::now() >= deadline
                            })?;
                            config.rpc_url = format!("http://127.0.0.1:{port}");
                            let mut registry = state
                                .registry
                                .lock()
                                .map_err(|_| "forward registry lock poisoned")?;
                            registry.restore_tunnels(|| {
                                worker_stop.load(Ordering::SeqCst) || Instant::now() >= deadline
                            })?;
                            state.persist_locked(&registry)?;
                            drop(registry);
                            if worker_stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
                                return Err("SSH recovery cancelled".into());
                            }
                            config.validate()?;
                            let payload =
                                serde_json::to_vec(&config).map_err(|error| error.to_string())?;
                            client.remote_command_with_stdin_timeout(
                                &remote_session_install_command(&remote_path),
                                &payload,
                                Duration::from_secs(3),
                            )?;
                            let selector = if config.herdr_session == "default" {
                                String::new()
                            } else {
                                format!("--session {} ", quote(&config.herdr_session))
                            };
                            client.remote_command_with_timeout(&format!("export PATH=\"$HOME/.local/bin:$PATH\"; herdr {selector}plugin action invoke herdr.fwd.wake"), Duration::from_secs(3))?;
                            // A live SSH socket is insufficient: wait until the remote
                            // watcher reaches this companion through the new reverse tunnel.
                            loop {
                                if worker_stop.load(Ordering::SeqCst) || Instant::now() >= deadline
                                {
                                    return Err("remote watcher heartbeat did not resume during SSH recovery".into());
                                }
                                let resumed = state
                                    .last_heartbeat
                                    .lock()
                                    .map_err(|_| "heartbeat lock poisoned")?
                                    .is_some_and(|heartbeat| heartbeat >= attempt_started);
                                if resumed {
                                    break;
                                }
                                if !next.is_alive() {
                                    return Err(
                                        "SSH exited while waiting for the remote watcher".into()
                                    );
                                }
                                thread::sleep(Duration::from_millis(200));
                            }
                            Ok::<(), String>(())
                        })();
                        match restored {
                            Ok(()) => Ok(next),
                            Err(error) => {
                                next.terminate();
                                Err(error)
                            }
                        }
                    })();
                    match attempt {
                        Ok(next) => {
                            master = Some(next);
                            state.reconnecting.store(false, Ordering::SeqCst);
                            eprintln!("SSH connection restored; port forwards are ready.");
                            break;
                        }
                        Err(error) => last_error = error,
                    }
                    for _ in 0..5 {
                        if worker_stop.load(Ordering::SeqCst) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(200));
                    }
                }
                if master.is_none() && !worker_stop.load(Ordering::SeqCst) {
                    if let Ok(mut failure) = worker_failure.lock() {
                        *failure = Some(format!("could not restore SSH forwards: {last_error}"));
                    }
                    return None;
                }
            }
            master
        });
        Self {
            stop,
            failure,
            thread: Some(thread),
        }
    }

    pub(crate) fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .ok()
            .and_then(|failure| failure.clone())
            .or_else(|| {
                self.thread
                    .as_ref()
                    .filter(|thread| thread.is_finished())
                    .map(|_| "SSH transport supervisor stopped unexpectedly".to_string())
            })
    }

    pub(crate) fn finish(mut self) -> Option<OwnedMaster> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take()?.join().ok().flatten()
    }
}

impl Drop for TransportSupervisor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
