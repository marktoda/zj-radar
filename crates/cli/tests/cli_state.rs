//! `zj-radar state` end to end against a fake Zellij cache root
//! (`ZJ_RADAR_CACHE_DIR`), laid out like the real one: plugin URL spelled as
//! nested dirs under `file:`, plus per-server UUID noise.
use assert_cmd::Command;
use std::time::{Duration, SystemTime};

fn write_presence(root: &std::path::Path, url_path: &str, pid: u32, json: &str, age: Duration) {
    let dir = root.join("file:").join(url_path.trim_start_matches('/')).join("plugin_cache");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("zj-radar.presence.{pid}.json"));
    std::fs::write(&path, json).unwrap();
    let f = std::fs::File::options().write(true).open(&path).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
}

fn state(root: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("zj-radar").unwrap();
    cmd.env("ZJ_RADAR_CACHE_DIR", root).env_remove("ZELLIJ_SESSION_NAME").arg("state");
    cmd
}

const WAITING: &str = r#"{"v":1,"session_name":"work","running":0,"attention":1,"attention_tab_position":2,"updated_epoch_s":100,
  "tabs":[{"position":2,"name":"api","panes":[{"pane_id":14,"origin":"status","kind":"claude","status":"pending","label":"fix login"}]}]}"#;
const FAILED_BUILD: &str = r#"{"v":1,"session_name":"ci","running":0,"attention":0,"updated_epoch_s":100,
  "tabs":[{"position":0,"name":"b","panes":[{"pane_id":3,"origin":"command","kind":"test","status":"error","label":"cargo test"}]}]}"#;

#[test]
fn json_lists_live_sessions_across_plugin_urls_and_drops_dead_ones() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("0b1c2d3e-uuid")).unwrap();
    write_presence(root.path(), "/home/u/.config/zellij/plugins/zj_radar.wasm", 10, WAITING, Duration::ZERO);
    write_presence(root.path(), "/home/u/dev/target/zj_radar.wasm", 11, FAILED_BUILD, Duration::from_secs(5));
    write_presence(
        root.path(),
        "/home/u/dev/old/zj_radar.wasm",
        12,
        r#"{"session_name":"ghost","running":0,"attention":0}"#,
        Duration::from_secs(600),
    );

    let out = state(root.path()).arg("--json").assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let names: Vec<&str> = v["sessions"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["ci", "work"]);
    assert_eq!(v["sessions"][1]["tabs"][0]["panes"][0]["pane_id"], 14);
}

#[test]
fn needs_attention_is_a_shell_predicate() {
    let root = tempfile::tempdir().unwrap();
    write_presence(root.path(), "/p/zj_radar.wasm", 11, FAILED_BUILD, Duration::ZERO);
    state(root.path()).arg("--needs-attention").assert().code(1).stdout("");

    write_presence(root.path(), "/p/zj_radar.wasm", 10, WAITING, Duration::ZERO);
    let out = state(root.path()).arg("--needs-attention").assert().code(0);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("fix login") && !stdout.contains("cargo test"), "{stdout}");
}

#[test]
fn missing_cache_root_is_an_empty_answer() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("nope");
    state(&missing).arg("--json").assert().success().stdout("{\"v\":1,\"sessions\":[]}\n");
}

#[test]
fn include_stale_requires_needs_attention() {
    let root = tempfile::tempdir().unwrap();
    state(root.path()).arg("--include-stale").assert().code(2);
    state(root.path()).args(["--needs-attention", "--include-stale"]).assert().code(1);
}

const CALM: &str = r#"{"v":1,"session_name":"alpha","running":1,"attention":0,"updated_epoch_s":100,"tabs":[]}"#;

#[test]
fn current_session_is_flagged_and_listed_first() {
    let root = tempfile::tempdir().unwrap();
    write_presence(root.path(), "/p/zj_radar.wasm", 10, WAITING, Duration::ZERO);
    write_presence(root.path(), "/p/zj_radar.wasm", 11, CALM, Duration::ZERO);
    let out = state(root.path()).env("ZELLIJ_SESSION_NAME", "work").arg("--json").assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let got: Vec<(&str, bool)> = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), s["current"].as_bool().unwrap()))
        .collect();
    assert_eq!(got, vec![("work", true), ("alpha", false)], "current first, then by name");
}

#[test]
fn include_stale_returns_a_stale_match() {
    let root = tempfile::tempdir().unwrap();
    write_presence(root.path(), "/p/zj_radar.wasm", 10, WAITING, Duration::from_secs(120));
    state(root.path()).arg("--needs-attention").assert().code(1);
    let out = state(root.path()).args(["--needs-attention", "--include-stale", "--json"]).assert().code(0);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["sessions"][0]["name"], "work");
    assert_eq!(v["sessions"][0]["stale"], true);
    assert!(v["sessions"][0]["age_s"].as_u64().unwrap() >= 120);
}

#[cfg(unix)]
#[test]
fn a_fifo_named_like_a_presence_file_is_skipped_promptly() {
    // A FIFO would block `read_to_string` forever (and a symlink to
    // /dev/zero would grow without bound): only regular files are read.
    let root = tempfile::tempdir().unwrap();
    write_presence(root.path(), "/p/zj_radar.wasm", 10, WAITING, Duration::ZERO);
    let fifo = root.path().join("file:/p/zj_radar.wasm/plugin_cache/zj-radar.presence.66.json");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(made.success());
    let out = state(root.path()).arg("--json").timeout(Duration::from_secs(10)).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["sessions"].as_array().unwrap().len(), 1, "the real file is still listed");
}
