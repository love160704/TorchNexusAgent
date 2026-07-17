use assert_cmd::cargo::cargo_bin;
use socks5_proto::{
    handshake::{Method as HandshakeMethod, Request as HandshakeRequest},
    Address, Command as Socks5Command, Request as Socks5Request,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn yaml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn free_addr() -> SocketAddr {
    let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe should succeed");
    let addr = probe.local_addr().expect("bind probe should have addr");
    drop(probe);
    addr
}

fn socks5_config_text(root: &Path, socks5_bind: SocketAddr, remote: SocketAddr) -> String {
    format!(
        r#"listen:
  socks5:
    enabled: true
    bind: "{socks5_bind}"
  tcp: []
capture:
  targets:
    - ip: "{remote_ip}"
      ports: [{remote_port}]
  save_dir: "{capture_dir}"
  save_uncaptured_sessions: false
upload:
  enabled: true
  endpoint: "http://127.0.0.1:8080/api/client/capture/upload"
  basic_auth:
    username: "agent"
    password: "change-me"
  auto_package_on_disconnect: true
  upload_interval_seconds: 60
  retry:
    max_attempts: 5
    base_delay_seconds: 3
storage:
  flush_each_chunk: true
log:
  level: "info"
"#,
        remote_ip = remote.ip(),
        remote_port = remote.port(),
        capture_dir = yaml_path(&root.join("captures")),
    )
}

fn write_socks5_config(root: &Path, socks5_bind: SocketAddr, remote: SocketAddr) -> PathBuf {
    let config_path = root.join("config.yaml");
    std::fs::write(&config_path, socks5_config_text(root, socks5_bind, remote))
        .expect("config should be written");
    config_path
}

fn write_invalid_socks5_bind_config(root: &Path) -> PathBuf {
    let config_path = root.join("config.yaml");
    std::fs::write(
        &config_path,
        format!(
            r#"listen:
  socks5:
    enabled: true
    bind: "not-an-addr"
  tcp: []
capture:
  targets:
    - ip: "127.0.0.1"
      ports: [9000]
  save_dir: "{capture_dir}"
  save_uncaptured_sessions: false
upload:
  enabled: true
  endpoint: "http://127.0.0.1:8080/api/client/capture/upload"
  basic_auth:
    username: "agent"
    password: "change-me"
  auto_package_on_disconnect: true
  upload_interval_seconds: 60
  retry:
    max_attempts: 5
    base_delay_seconds: 3
storage:
  flush_each_chunk: true
log:
  level: "info"
"#,
            capture_dir = yaml_path(&root.join("captures")),
        ),
    )
    .expect("config should be written");
    config_path
}

struct EchoServer {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    addr: SocketAddr,
}

impl EchoServer {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("echo server should bind");
        listener
            .set_nonblocking(true)
            .expect("echo listener should become nonblocking");
        let addr = listener
            .local_addr()
            .expect("echo listener should have addr");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);

        let thread = thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        thread::spawn(move || {
                            let mut buf = [0_u8; 4096];
                            loop {
                                let Ok(n) = socket.read(&mut buf) else {
                                    break;
                                };
                                if n == 0 {
                                    break;
                                }
                                if socket.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            stop,
            thread: Some(thread),
            addr,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for EchoServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn queued_entries(root: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(root) {
        Ok(entries) => entries
            .map(|entry| entry.expect("queue entry should exist").path())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("failed to read pending dir: {error}"),
    }
}

fn queued_tlc_count(root: &Path) -> usize {
    queued_entries(root)
        .into_iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("tlc"))
        .count()
}

fn wait_for_queued_tlc(root: &Path) {
    for _ in 0..20 {
        if queued_tlc_count(root) == 1 {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for queued tlc");
}

struct AgentProcess {
    child: Option<Child>,
    stdout: mpsc::Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl AgentProcess {
    fn terminate(&mut self) -> ExitStatus {
        let mut child = self.child.take().expect("child should still be owned");
        if child
            .try_wait()
            .expect("child status should be queryable")
            .is_none()
        {
            child.kill().expect("child should terminate");
        }
        child.wait().expect("child should exit after kill")
    }

    fn stderr_snapshot(&self) -> String {
        self.stderr
            .lock()
            .expect("stderr collector should be lockable")
            .join("\n")
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn spawn_agent(config_path: &Path, current_dir: &Path) -> AgentProcess {
    let mut child = Command::new(cargo_bin("torchnexus-agent"))
        .current_dir(current_dir)
        .args(["run", "--config"])
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run command should start");

    let stdout = child.stdout.take().expect("stdout should be piped");
    let stderr = child.stderr.take().expect("stderr should be piped");
    let (tx, rx) = mpsc::channel();
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_collector = Arc::clone(&stderr_lines);
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = line.expect("stdout line should be readable");
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let line = line.expect("stderr line should be readable");
            stderr_collector
                .lock()
                .expect("stderr collector should be lockable")
                .push(line);
        }
    });

    AgentProcess {
        child: Some(child),
        stdout: rx,
        stderr: stderr_lines,
    }
}

fn wait_for_startup(process: &AgentProcess) {
    for _ in 0..2 {
        let line = process
            .stdout
            .recv_timeout(Duration::from_secs(5))
            .expect("startup line should be printed");
        if line == "configured proxy listeners started" {
            return;
        }
    }
    panic!("agent did not print startup readiness line");
}

fn encode_greeting() -> Vec<u8> {
    let request = HandshakeRequest::new(vec![HandshakeMethod::NONE]);
    let mut bytes = Vec::with_capacity(request.serialized_len());
    request.write_to_buf(&mut bytes);
    bytes
}

fn encode_connect_request(remote: SocketAddr) -> Vec<u8> {
    let request = Socks5Request::new(Socks5Command::Connect, Address::SocketAddress(remote));
    let mut bytes = Vec::with_capacity(request.serialized_len());
    request.write_to_buf(&mut bytes);
    bytes
}

fn socks5_connect(bind: SocketAddr, remote: SocketAddr) -> std::io::Result<TcpStream> {
    let mut client = TcpStream::connect(bind)?;
    client.write_all(&encode_greeting())?;

    let mut method = [0_u8; 2];
    client.read_exact(&mut method)?;
    if method != [0x05, 0x00] {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unexpected method selection reply: {method:?}"),
        ));
    }

    client.write_all(&encode_connect_request(remote))?;

    let mut response = [0_u8; 10];
    client.read_exact(&mut response)?;
    if response[0] != 0x05 || response[1] != 0x00 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unexpected connect response: {response:?}"),
        ));
    }

    Ok(client)
}

#[test]
fn run_starts_socks5_accepts_connect_and_finalizes_tlc() {
    let temp = TempDir::new().expect("temp dir should be created");
    let echo = EchoServer::spawn();
    let socks5_bind = free_addr();
    let config_path = write_socks5_config(temp.path(), socks5_bind, echo.addr());
    let mut process = spawn_agent(&config_path, temp.path());
    wait_for_startup(&process);

    if let Err(error) = socks5_connect(socks5_bind, echo.addr()) {
        panic!(
            "socks5 connect should succeed: {error}; agent stderr:\n{}",
            process.stderr_snapshot()
        );
    }

    let pending_dir = temp.path().join("captures").join("pending");
    wait_for_queued_tlc(&pending_dir);
    let queued = queued_entries(&pending_dir);
    assert_eq!(queued.len(), 1);
    assert!(
        queued[0]
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.ends_with(".tlc")),
        "captures/pending should contain a tlc bundle: {:?}",
        queued
    );

    let status = process.terminate();
    assert!(
        !status.success(),
        "forced shutdown should not look like clean exit"
    );
}

#[test]
fn run_fails_fast_when_socks5_bind_is_invalid() {
    let temp = TempDir::new().expect("temp dir should be created");
    let config_path = write_invalid_socks5_bind_config(temp.path());

    let output = Command::new(cargo_bin("torchnexus-agent"))
        .current_dir(temp.path())
        .args(["run", "--config"])
        .arg(&config_path)
        .output()
        .expect("run command should finish");

    assert!(!output.status.success(), "invalid bind should fail");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.trim().is_empty(),
        "invalid bind should fail before startup output: {stdout}"
    );
    assert!(
        stderr.contains("invalid socks5 bind not-an-addr"),
        "stderr did not mention invalid socks5 bind: {stderr}"
    );
}
