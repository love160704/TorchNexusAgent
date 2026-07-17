use assert_cmd::cargo::cargo_bin;
use assert_cmd::Command;
use predicates::prelude::*;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::{
    fs,
    net::{SocketAddr, TcpListener, TcpStream},
};

const FOUNDATION_MESSAGE: &str = "proxy runtime initialized";

fn valid_config(temp_dir: &Path) -> String {
    valid_config_with_socks5_bind(temp_dir, "0.0.0.0:1080")
}

fn valid_config_with_socks5_bind(temp_dir: &Path, socks5_bind: &str) -> String {
    let capture_dir = yaml_path(&temp_dir.join("captures"));

    format!(
        r#"listen:
  socks5:
    enabled: true
    bind: "{socks5_bind}"
  tcp: []
capture:
  targets:
    - ip: "1.2.3.4"
      ports: [9000]
  save_dir: "{}"
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
        capture_dir
    )
}

fn read_startup_lines(child: &mut std::process::Child) -> (String, String) {
    let stdout = child.stdout.take().expect("stdout should be piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = line.expect("stdout line should be readable");
            tx.send(line).expect("line should be sent");
        }
    });

    let first = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("foundation message should be printed");
    let second = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("startup message should be printed");

    (first, second)
}

fn free_local_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("port probe should bind");
    let addr = listener
        .local_addr()
        .expect("port probe should have a local addr");
    drop(listener);
    addr
}

fn valid_config_with_tcp(temp_dir: &Path, socks5_bind: &str, bind: &str, remote: &str) -> String {
    let capture_dir = yaml_path(&temp_dir.join("captures"));

    format!(
        r#"listen:
  socks5:
    enabled: true
    bind: "{socks5_bind}"
  tcp:
    - name: "game-server-9000"
      bind: "{bind}"
      remote: "{remote}"
capture:
  targets:
    - ip: "1.2.3.4"
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
"#
    )
}

fn yaml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn write_config(temp_dir: &Path, yaml: &str) -> std::path::PathBuf {
    let config_path = temp_dir.join("config.yaml");
    std::fs::write(&config_path, yaml).expect("config should be written");
    config_path
}

fn write_bundle_state(temp_dir: &Path, bundle_id: &str, state_json: &str) {
    let state_dir = temp_dir.join("captures").join("state");
    fs::create_dir_all(&state_dir).expect("state dir should be created");
    fs::write(state_dir.join(format!("{bundle_id}.json")), state_json)
        .expect("bundle state should be written");
}

#[test]
fn check_config_accepts_valid_config() {
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let config_path = write_config(temp_dir.path(), &valid_config(temp_dir.path()));

    Command::cargo_bin("torchnexus-agent")
        .expect("binary should exist")
        .current_dir(temp_dir.path())
        .args(["check-config", "--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("config ok"));
}

#[test]
fn check_config_rejects_interval_below_sixty() {
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let yaml = valid_config(temp_dir.path())
        .replace("upload_interval_seconds: 60", "upload_interval_seconds: 59");
    let config_path = write_config(temp_dir.path(), &yaml);

    Command::cargo_bin("torchnexus-agent")
        .expect("binary should exist")
        .current_dir(temp_dir.path())
        .args(["check-config", "--config"])
        .arg(&config_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "upload interval must be at least 60 seconds",
        ));
}

#[test]
fn help_has_no_manual_upload_commands() {
    let assert = Command::cargo_bin("torchnexus-agent")
        .expect("binary should exist")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("check-config"))
        .stdout(predicate::str::contains("list-sessions"));

    let output = assert.get_output();
    let stdout = String::from_utf8(output.stdout.clone()).expect("help should be utf8");
    let command_lines = stdout
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("check-config") || line.starts_with("list-sessions"))
        .count();

    assert_eq!(
        command_lines, 2,
        "help should expose only required commands"
    );
    assert!(
        !stdout
            .lines()
            .map(str::trim_start)
            .any(|line| line.starts_with("upload ") || line.starts_with("upload-session")),
        "help exposed a manual upload command:\n{stdout}"
    );
}

#[test]
fn help_still_has_only_required_commands_after_socks5_runtime() {
    let output = Command::cargo_bin("torchnexus-agent")
        .expect("binary should exist")
        .arg("--help")
        .output()
        .expect("help should run");

    assert!(output.status.success(), "help command should succeed");

    let stdout = String::from_utf8(output.stdout).expect("help should be utf8");
    assert!(stdout.contains("run"));
    assert!(stdout.contains("check-config"));
    assert!(stdout.contains("list-sessions"));
    assert!(!stdout.contains("upload-session"));
}

#[test]
fn run_stays_alive_after_printing_startup_messages() {
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let socks5_bind = free_local_addr();
    let config_path = write_config(
        temp_dir.path(),
        &valid_config_with_socks5_bind(temp_dir.path(), &socks5_bind.to_string()),
    );

    let mut child = ProcessCommand::new(cargo_bin("torchnexus-agent"))
        .current_dir(temp_dir.path())
        .args(["run", "--config"])
        .arg(&config_path)
        .stdout(Stdio::piped())
        .spawn()
        .expect("run command should start");

    let (first, second) = read_startup_lines(&mut child);

    assert_eq!(first, FOUNDATION_MESSAGE);
    assert_eq!(second, "configured proxy listeners started");
    TcpStream::connect(socks5_bind).expect("socks5 listener should accept tcp connections");
    thread::sleep(Duration::from_millis(250));
    assert!(
        child
            .try_wait()
            .expect("process should be queryable")
            .is_none(),
        "run should keep waiting for ctrl-c after startup"
    );

    child.kill().expect("process should be terminable");
    child.wait().expect("process should exit after kill");
}

#[test]
fn run_stays_alive_after_starting_socks5_listener_without_tcp_forwards() {
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let socks5_bind = free_local_addr();
    let config_path = write_config(
        temp_dir.path(),
        &valid_config_with_socks5_bind(temp_dir.path(), &socks5_bind.to_string()),
    );

    let mut child = ProcessCommand::new(cargo_bin("torchnexus-agent"))
        .current_dir(temp_dir.path())
        .args(["run", "--config"])
        .arg(&config_path)
        .stdout(Stdio::piped())
        .spawn()
        .expect("run command should start");

    let (first, second) = read_startup_lines(&mut child);

    assert_eq!(first, FOUNDATION_MESSAGE);
    assert_eq!(second, "configured proxy listeners started");

    TcpStream::connect(socks5_bind).expect("socks5 listener should accept tcp connections");

    thread::sleep(Duration::from_millis(250));
    assert!(
        child
            .try_wait()
            .expect("process should be queryable")
            .is_none(),
        "run should keep waiting for ctrl-c after startup"
    );

    child.kill().expect("process should be terminable");
    child.wait().expect("process should exit after kill");
}

#[test]
fn run_fails_fast_when_tcp_bind_is_occupied() {
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let socks5_bind = free_local_addr();
    let occupied = TcpListener::bind("127.0.0.1:0").expect("port should bind");
    let bind = occupied.local_addr().expect("local addr should exist");
    let config_path = write_config(
        temp_dir.path(),
        &valid_config_with_tcp(
            temp_dir.path(),
            &socks5_bind.to_string(),
            &bind.to_string(),
            "127.0.0.1:9000",
        ),
    );

    Command::cargo_bin("torchnexus-agent")
        .expect("binary should exist")
        .current_dir(temp_dir.path())
        .args(["run", "--config"])
        .arg(&config_path)
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(format!("failed to bind {bind}")));
}

#[test]
fn list_sessions_missing_capture_dir_succeeds_with_empty_stdout() {
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let config_path = write_config(temp_dir.path(), &valid_config(temp_dir.path()));

    Command::cargo_bin("torchnexus-agent")
        .expect("binary should exist")
        .current_dir(temp_dir.path())
        .args(["list-sessions", "--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn list_sessions_empty_capture_dir_succeeds_with_empty_stdout() {
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    std::fs::create_dir(temp_dir.path().join("captures")).expect("capture dir should be created");
    let config_path = write_config(temp_dir.path(), &valid_config(temp_dir.path()));

    Command::cargo_bin("torchnexus-agent")
        .expect("binary should exist")
        .current_dir(temp_dir.path())
        .args(["list-sessions", "--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn list_sessions_prints_bundle_summary_fields() {
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let config_path = write_config(temp_dir.path(), &valid_config(temp_dir.path()));
    write_bundle_state(
        temp_dir.path(),
        "01A",
        r#"{
  "bundle_id": "01A",
  "created_ms": 1783000000000,
  "finalized_ms": 1783000001000,
  "status": "queued",
  "file_size": 123,
  "record_count": 2,
  "sha256": null
}"#,
    );

    Command::cargo_bin("torchnexus-agent")
        .expect("binary should exist")
        .current_dir(temp_dir.path())
        .args(["list-sessions", "--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::eq(
            "01A\t2026-07-02T13:46:40+00:00\tqueued\t123\n",
        ));
}
