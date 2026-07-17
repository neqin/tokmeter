use super::config::{RefreshConfig, SshSourceConfig};
use super::protocol::{
    decode_json, DecodedSnapshot, ProtocolErrorKind, ProtocolRetention, MAX_STDOUT_BYTES,
};
use super::remote_store::{AttemptFailureKind, RemoteStore};
use super::timeutil::now_epoch;
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const STDERR_BYTES: usize = 4096;
const WORKER_LIMIT: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteErrorKind {
    Spawn,
    Transport,
    Timeout,
    Oversized,
    InvalidProtocol,
    Incompatible,
}

#[derive(Debug)]
pub struct RemoteError {
    pub kind: RemoteErrorKind,
    pub message: String,
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

pub struct RemoteCollectResult {
    pub source_id: String,
    pub label: String,
    pub attempt: i64,
    pub generation: u64,
    pub duration_ms: u64,
    pub result: Result<DecodedSnapshot, RemoteError>,
}

impl RemoteCollectResult {
    fn apply_to_store(self, store: &mut RemoteStore) {
        match self.result {
            Ok(decoded) => store.apply_success(
                &self.source_id,
                &self.label,
                decoded.snapshot,
                decoded.warnings,
                self.attempt,
                self.duration_ms,
            ),
            Err(error) => store.apply_failure(
                &self.source_id,
                &self.label,
                if error.kind == RemoteErrorKind::Incompatible {
                    AttemptFailureKind::Incompatible
                } else {
                    AttemptFailureKind::Error
                },
                error.message,
                self.attempt,
                self.duration_ms,
            ),
        }
    }
}

#[derive(Default)]
pub struct RemoteCoordinator {
    generation: u64,
    latest: HashMap<String, u64>,
}

impl RemoteCoordinator {
    pub fn start(
        &mut self,
        sources: Vec<SshSourceConfig>,
        refresh: RefreshConfig,
        retention: ProtocolRetention,
        store: &mut RemoteStore,
    ) -> mpsc::Receiver<RemoteCollectResult> {
        self.start_with_program(sources, refresh, retention, store, PathBuf::from("ssh"))
    }

    pub fn apply(&self, result: RemoteCollectResult, store: &mut RemoteStore) -> bool {
        if self.latest.get(&result.source_id) != Some(&result.generation) {
            return false;
        }
        result.apply_to_store(store);
        true
    }

    fn start_with_program(
        &mut self,
        sources: Vec<SshSourceConfig>,
        refresh: RefreshConfig,
        retention: ProtocolRetention,
        store: &mut RemoteStore,
        program: PathBuf,
    ) -> mpsc::Receiver<RemoteCollectResult> {
        self.generation = self.generation.wrapping_add(1).max(1);
        let generation = self.generation;
        let attempt = now_epoch();
        for source in &sources {
            self.latest.insert(source.id.clone(), generation);
            store.set_connecting(&source.id, &source.label, attempt);
        }
        spawn_batch(sources, move |source| {
            collect_with_program(&source, refresh, retention, &program, attempt, generation)
        })
    }
}

fn spawn_batch<F>(
    sources: Vec<SshSourceConfig>,
    collector: F,
) -> mpsc::Receiver<RemoteCollectResult>
where
    F: Fn(SshSourceConfig) -> RemoteCollectResult + Send + Sync + 'static,
{
    let (jobs_tx, jobs_rx) = mpsc::channel();
    let (results_tx, results_rx) = mpsc::channel();
    let jobs_rx = Arc::new(Mutex::new(jobs_rx));
    let collector = Arc::new(collector);
    let workers = sources.len().min(WORKER_LIMIT);

    for _ in 0..workers {
        let jobs_rx = jobs_rx.clone();
        let results_tx = results_tx.clone();
        let collector = collector.clone();
        thread::spawn(move || loop {
            let source = match jobs_rx.lock().unwrap().recv() {
                Ok(source) => source,
                Err(_) => break,
            };
            if results_tx.send(collector(source)).is_err() {
                break;
            }
        });
    }
    drop(results_tx);
    for source in sources {
        if jobs_tx.send(source).is_err() {
            break;
        }
    }
    drop(jobs_tx);
    results_rx
}

fn collect_with_program(
    source: &SshSourceConfig,
    refresh: RefreshConfig,
    retention: ProtocolRetention,
    program: &Path,
    attempt: i64,
    generation: u64,
) -> RemoteCollectResult {
    let started = Instant::now();
    let result = run_ssh(source, refresh, retention, program);
    RemoteCollectResult {
        source_id: source.id.clone(),
        label: source.label.clone(),
        attempt,
        generation,
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        result,
    }
}

fn run_ssh(
    source: &SshSourceConfig,
    refresh: RefreshConfig,
    retention: ProtocolRetention,
    program: &Path,
) -> Result<DecodedSnapshot, RemoteError> {
    let connect_timeout = format!("ConnectTimeout={}", refresh.ssh_connect_timeout_secs);
    let mut command = Command::new(program);
    command
        .args(["-o", "BatchMode=yes", "-o", &connect_timeout, "--"])
        .arg(&source.host)
        .arg(&source.binary)
        .arg("--export-source-json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(|error| RemoteError {
        kind: RemoteErrorKind::Spawn,
        message: format!("ssh spawn failed: {error}"),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| RemoteError {
        kind: RemoteErrorKind::Spawn,
        message: "ssh stdout unavailable".to_string(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| RemoteError {
        kind: RemoteErrorKind::Spawn,
        message: "ssh stderr unavailable".to_string(),
    })?;
    let stdout_reader = thread::spawn(move || drain(stdout, MAX_STDOUT_BYTES));
    let stderr_reader = thread::spawn(move || drain(stderr, STDERR_BYTES));
    let deadline = Duration::from_secs(refresh.ssh_command_timeout_secs as u64);
    let status = wait_until(&mut child, deadline);
    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    let stderr = sanitize(&stderr.bytes);

    let status = match status {
        Ok(Some(status)) => status,
        Ok(None) => {
            return Err(RemoteError {
                kind: RemoteErrorKind::Timeout,
                message: "ssh command timed out".to_string(),
            });
        }
        Err(error) => {
            return Err(RemoteError {
                kind: RemoteErrorKind::Transport,
                message: format!("ssh wait failed: {error}"),
            });
        }
    };
    if stdout.exceeded {
        return Err(RemoteError {
            kind: RemoteErrorKind::Oversized,
            message: "ssh snapshot exceeds 16 MiB".to_string(),
        });
    }
    if !status.success() {
        return Err(RemoteError {
            kind: RemoteErrorKind::Transport,
            message: exit_message(status, &stderr),
        });
    }
    let text = String::from_utf8(stdout.bytes).map_err(|_| RemoteError {
        kind: RemoteErrorKind::InvalidProtocol,
        message: "ssh snapshot is not utf-8".to_string(),
    })?;
    decode_json(&text, now_epoch(), retention).map_err(|error| RemoteError {
        kind: match error.kind {
            ProtocolErrorKind::Invalid => RemoteErrorKind::InvalidProtocol,
            ProtocolErrorKind::Incompatible => RemoteErrorKind::Incompatible,
        },
        message: error.message,
    })
}

fn wait_until(
    child: &mut std::process::Child,
    deadline: Duration,
) -> io::Result<Option<ExitStatus>> {
    let started = Instant::now();
    loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                kill_and_reap(child);
                return Err(error);
            }
        };
        if let Some(status) = status {
            return Ok(Some(status));
        }
        if started.elapsed() >= deadline {
            kill_and_reap(child);
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn kill_and_reap(child: &mut std::process::Child) {
    let pid = child.id() as i32;
    let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
    let _ = child.kill();
    let _ = child.wait();
}

struct DrainOutput {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn drain(mut reader: impl Read, cap: usize) -> io::Result<DrainOutput> {
    let mut bytes = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = cap.saturating_sub(bytes.len());
        let keep = remaining.min(read);
        bytes.extend_from_slice(&buffer[..keep]);
        exceeded |= keep < read;
    }
    Ok(DrainOutput { bytes, exceeded })
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<DrainOutput>>,
) -> Result<DrainOutput, RemoteError> {
    reader
        .join()
        .map_err(|_| RemoteError {
            kind: RemoteErrorKind::Transport,
            message: "ssh output reader panicked".to_string(),
        })?
        .map_err(|error| RemoteError {
            kind: RemoteErrorKind::Transport,
            message: format!("ssh output read failed: {error}"),
        })
}

fn sanitize(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut output = String::new();
    let mut chars = 0;
    let mut whitespace = false;
    for c in text.chars() {
        if c.is_control() || c.is_whitespace() {
            whitespace = !output.is_empty();
            continue;
        }
        if whitespace && chars < 240 {
            output.push(' ');
            chars += 1;
        }
        whitespace = false;
        if chars >= 240 {
            break;
        }
        output.push(c);
        chars += 1;
    }
    output
}

fn exit_message(status: ExitStatus, stderr: &str) -> String {
    let code = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    if stderr.is_empty() {
        format!("ssh exited with {code}")
    } else {
        format!("ssh exited with {code}: {stderr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::cache::{CompactData, CACHE_VERSION};
    use crate::data::protocol::{ExportRefreshStatus, SourceSnapshot};
    use crate::data::timeutil::local_offset;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    const ID: &str = "123e4567-e89b-12d3-a456-426614174000";
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn source(id: &str, host: &str) -> SshSourceConfig {
        SshSourceConfig {
            id: id.into(),
            label: id.into(),
            host: host.into(),
            enabled: true,
            binary: "/opt/tokmeter".into(),
        }
    }

    fn refresh() -> RefreshConfig {
        RefreshConfig {
            ui_secs: 3,
            limits_ttl_secs: 300,
            remote_secs: 60,
            ssh_connect_timeout_secs: 5,
            ssh_command_timeout_secs: 1,
        }
    }

    fn retention() -> ProtocolRetention {
        ProtocolRetention {
            history_days: 120,
            hours_days: 8,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tokmeter-remote-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn snapshot_json() -> String {
        let now = now_epoch();
        SourceSnapshot {
            app_version: "0.1.8".into(),
            cache_version: CACHE_VERSION,
            instance_id: ID.into(),
            generated_at: now,
            utc_offset_secs: local_offset(now),
            refresh_status: ExportRefreshStatus::Fresh,
            retention: retention(),
            data: CompactData::default(),
        }
        .to_json()
        .unwrap()
    }

    fn script(root: &Path) -> PathBuf {
        let path = root.join("ssh");
        fs::write(
            &path,
            r#"#!/bin/sh
printf '%s\n' "$@" > "$TOKMETER_TEST_ARGS"
printf '%s' "$$" > "$TOKMETER_TEST_PID"
if IFS= read -r line; then exit 91; fi
case "$TOKMETER_TEST_MODE" in
  success) /usr/bin/cat "$TOKMETER_TEST_JSON" ;;
  fail) printf 'auth\nfailed\t' >&2; exit 7 ;;
  sleep) /usr/bin/sleep 2 ;;
  large) /usr/bin/python3 -c 'import sys; sys.stdout.write("x" * (17 * 1024 * 1024))' ;;
  both) /usr/bin/python3 -c 'import sys; sys.stdout.write("x" * (17 * 1024 * 1024)); sys.stderr.write("e" * (1024 * 1024))' ;;
  malformed) printf '{' ;;
  incompatible) /usr/bin/sed 's/"protocol_version":1/"protocol_version":2/' "$TOKMETER_TEST_JSON" ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn with_script(mode: &str, f: impl FnOnce(&Path, &Path, &Path)) {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = temp_dir(mode);
        let program = script(&root);
        let json = root.join("snapshot.json");
        let args = root.join("args.txt");
        let pid = root.join("pid.txt");
        fs::write(&json, snapshot_json()).unwrap();
        std::env::set_var("TOKMETER_TEST_MODE", mode);
        std::env::set_var("TOKMETER_TEST_JSON", &json);
        std::env::set_var("TOKMETER_TEST_ARGS", &args);
        std::env::set_var("TOKMETER_TEST_PID", &pid);
        f(&program, &args, &pid);
        std::env::remove_var("TOKMETER_TEST_MODE");
        std::env::remove_var("TOKMETER_TEST_JSON");
        std::env::remove_var("TOKMETER_TEST_ARGS");
        std::env::remove_var("TOKMETER_TEST_PID");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn collects_valid_snapshot_with_exact_argv_and_closed_stdin() {
        with_script("success", |program, args, _| {
            let result = collect_with_program(
                &source("lxc", "user@lxc"),
                refresh(),
                retention(),
                program,
                now_epoch(),
                1,
            );
            assert!(result.result.is_ok());
            let args = fs::read_to_string(args).unwrap();
            assert_eq!(
                args.lines().collect::<Vec<_>>(),
                vec![
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=5",
                    "--",
                    "user@lxc",
                    "/opt/tokmeter",
                    "--export-source-json"
                ]
            );
        });
    }

    #[test]
    fn classifies_exit_timeout_oversize_and_protocol_errors() {
        for (mode, kind) in [
            ("fail", RemoteErrorKind::Transport),
            ("sleep", RemoteErrorKind::Timeout),
            ("large", RemoteErrorKind::Oversized),
            ("both", RemoteErrorKind::Oversized),
            ("malformed", RemoteErrorKind::InvalidProtocol),
            ("incompatible", RemoteErrorKind::Incompatible),
        ] {
            with_script(mode, |program, _, _| {
                let result = collect_with_program(
                    &source("lxc", "lxc"),
                    refresh(),
                    retention(),
                    program,
                    now_epoch(),
                    1,
                );
                assert_eq!(result.result.err().unwrap().kind, kind, "{mode}");
            });
        }
    }

    #[test]
    fn timeout_kills_and_reaps_child() {
        with_script("sleep", |program, _, pid_path| {
            let result = collect_with_program(
                &source("lxc", "lxc"),
                refresh(),
                retention(),
                program,
                now_epoch(),
                1,
            );
            assert_eq!(result.result.err().unwrap().kind, RemoteErrorKind::Timeout);
            let pid = fs::read_to_string(pid_path).unwrap();
            assert!(!Path::new("/proc").join(pid).exists());
        });
    }

    #[test]
    fn sanitizes_and_bounds_stderr() {
        with_script("fail", |program, _, _| {
            let error = collect_with_program(
                &source("lxc", "lxc"),
                refresh(),
                retention(),
                program,
                now_epoch(),
                1,
            )
            .result
            .err()
            .unwrap();
            assert!(error.message.contains("auth failed"));
            assert!(!error.message.contains('\n'));
        });
    }

    #[test]
    fn worker_pool_is_bounded_and_returns_completion_order() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let collector = {
            let active = active.clone();
            let max_active = max_active.clone();
            move |source: SshSourceConfig| {
                let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now_active, Ordering::SeqCst);
                if source.id == "slow" {
                    thread::sleep(Duration::from_millis(100));
                } else {
                    thread::sleep(Duration::from_millis(20));
                }
                active.fetch_sub(1, Ordering::SeqCst);
                RemoteCollectResult {
                    source_id: source.id.clone(),
                    label: source.label,
                    attempt: now_epoch(),
                    generation: 1,
                    duration_ms: 1,
                    result: Err(RemoteError {
                        kind: RemoteErrorKind::Transport,
                        message: "test".into(),
                    }),
                }
            }
        };
        let mut sources = vec![source("slow", "slow")];
        for index in 0..7 {
            sources.push(source(&format!("fast-{index}"), "fast"));
        }
        let results: Vec<_> = spawn_batch(sources, collector).into_iter().collect();
        assert_eq!(results.len(), 8);
        assert!(max_active.load(Ordering::SeqCst) <= WORKER_LIMIT);
        assert_ne!(results.first().unwrap().source_id, "slow");
    }

    #[test]
    fn older_result_does_not_overwrite_store() {
        let home = temp_dir("store");
        let mut store = RemoteStore::empty(home.to_str().unwrap());
        let mut coordinator = RemoteCoordinator::default();
        coordinator.latest.insert("lxc".into(), 2);
        let newer = RemoteCollectResult {
            source_id: "lxc".into(),
            label: "LXC".into(),
            attempt: 20,
            generation: 2,
            duration_ms: 1,
            result: Err(RemoteError {
                kind: RemoteErrorKind::Transport,
                message: "new".into(),
            }),
        };
        assert!(coordinator.apply(newer, &mut store));
        let older = RemoteCollectResult {
            source_id: "lxc".into(),
            label: "LXC".into(),
            attempt: 10,
            generation: 1,
            duration_ms: 1,
            result: Err(RemoteError {
                kind: RemoteErrorKind::Transport,
                message: "old".into(),
            }),
        };
        assert!(!coordinator.apply(older, &mut store));
        assert_eq!(store.sources["lxc"].last_attempt, 20);
        assert_eq!(store.sources["lxc"].error, "new");
        let _ = fs::remove_dir_all(home);
    }
}
