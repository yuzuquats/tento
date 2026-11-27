//! Process capture regression: malformed bytes never become an absent optional value.
//! Runs in an isolated Unix child to preserve the captured bytes.
#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::process::Command;
use std::time::{Duration, Instant};
use tento_env::*;

const KEY: &str = "TENTO_CAPTURE_NON_UNICODE";
const CHILD: &str = "TENTO_CAPTURE_CHILD";
env_kv!(Captured, "TENTO_CAPTURE_NON_UNICODE", String);
#[derive(Debug, FromEnv)]
struct Required { value: Env<Captured> }
#[derive(Debug, FromEnv)]
struct Optional { value: Option<Env<Captured>> }
#[derive(Debug, FromEnv)]
struct Defaulted {
  #[env(from = "TENTO_CAPTURE_NON_UNICODE", default = 8080)]
  port: u16,
}

fn malformed(error: &EnvError) {
  assert!(matches!(error, EnvError::Parse { key: KEY, .. }));
  for diagnostic in [format!("{error:?}"), error.to_string(), error.fmt_colored()] {
    assert!(diagnostic.contains(KEY));
    assert!(!diagnostic.contains("capture-private-marker"));
  }
}

#[test]
fn capture_child_entry() {
  // The harness also discovers this test normally; only the isolated child runs it.
  if std::env::var_os(CHILD).is_none() { return; }
  let raw = OsString::from_vec(b"capture-private-marker-\xff".to_vec());
  let map = EnvMap::from_process();
  assert!(map.snapshot_os().iter().any(|(key, value)| key == OsStr::new(KEY) && value == &raw));
  malformed(&Captured::from_env().unwrap_err());
  malformed(&Required::from_backend(map.clone()).into_err().unwrap().errors()[0]);
  malformed(&Optional::from_backend(map.clone()).into_err().unwrap().errors()[0]);
  malformed(&Defaulted::from_backend(map.clone()).into_err().unwrap().errors()[0]);
  let report = with_backend(map.clone(), preflight_with_defaults::<Defaulted>);
  assert_eq!(report.vars[0].availability, VarAvailability::LookupFailed);
  assert_eq!(Defaulted::from_backend(EnvMap::new()).unwrap().port, 8080);
  // The original and replacement are both supplied. Each independent read
  // may see either one while a shared handle switches between them.
  let shared = map.fork();
  let start = std::sync::Barrier::new(2);
  std::thread::scope(|scope| {
    let writer = shared.clone();
    let start = &start;
    let source = &map;
    scope.spawn(move || {
      start.wait();
      for _ in 0..20_000 {
        writer.set(KEY, "replacement");
        writer.replace(source);
      }
    });
    start.wait();
    for _ in 0..20_000 {
      match Optional::from_backend(shared.clone()).into_result() {
        Ok(config) => assert_eq!(config.value.unwrap().value().0, "replacement"),
        Err(error) => malformed(&error.errors()[0]),
      }
      for snapshot in [shared.snapshot_os(), shared.fork().snapshot_os()] {
        let mut values = snapshot.iter().filter(|(key, _)| key == OsStr::new(KEY));
        let (_, value) = values.next().expect("supplied key disappeared from snapshot");
        assert!(value == &raw || value == OsStr::new("replacement"));
        assert!(values.next().is_none(), "supplied key duplicated in snapshot");
      }
    }
  });

  let result: EnvResult<Captured> = with_backend(map, Captured::from_env).into();
  assert!(matches!(result, EnvResult::Malformed(_)));
}

#[test]
fn process_capture_preserves_bytes_and_rejects_malformed_optional_values() {
  // Set bytes only in Command's child environment: no unsafe process mutation,
  // no dependence on harness ordering or a shared environment mutation lock.
  let mut child = Command::new(std::env::current_exe().unwrap())
    .args(["--exact", "capture_child_entry", "--nocapture"])
    .env_clear()
    .env(CHILD, "1")
    .env(KEY, OsString::from_vec(b"capture-private-marker-\xff".to_vec()))
    .spawn().unwrap();
  let deadline = Instant::now() + Duration::from_secs(10);
  loop {
    if let Some(status) = child.try_wait().unwrap() {
      assert!(status.success(), "capture child failed: {status}");
      break;
    }
    if Instant::now() >= deadline {
      let _ = child.kill();
      let _ = child.wait();
      panic!("capture child exceeded ten seconds");
    }
    std::thread::sleep(Duration::from_millis(10));
  }
}
