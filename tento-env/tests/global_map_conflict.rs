//! Own integration process: custom backend first. Qualification requires an
//! outer process deadline in addition to the finite worker receive deadline.
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tento_env::{Backend, Env, EnvError, EnvMap, FromEnv, env_kv};

env_kv!(NestedValue, "GLOBAL_BACKEND_NESTED_VALUE", String);

#[derive(FromEnv)]
struct NestedConfig {
  value: Env<NestedValue>,
}

struct Custom;
impl Backend for Custom {
  fn lookup(&self, _: &'static str) -> Result<Option<String>, EnvError> {
    // Actual public callback reentry must occur after the global read unlock.
    assert!(tento_env::global_env_map().is_none());
    assert!(!tento_env::set_global_env_map(EnvMap::new()));
    let nested = EnvMap::new();
    nested.set("GLOBAL_BACKEND_NESTED_VALUE", "custom");
    Ok(Some(NestedConfig::from_backend(nested).unwrap().value.value().0.clone()))
  }
}

#[test]
fn global_custom_backend_rejects_map_without_publication() {
  assert!(tento_env::set_global_backend(Custom));
  let rejected = EnvMap::new();
  rejected.set("GLOBAL_CONFLICT_KEY", "map");
  assert!(!tento_env::set_global_env_map(rejected));
  assert!(tento_env::global_env_map().is_none());
  assert_eq!(tento_env::effective_var("GLOBAL_CONFLICT_KEY"), None);
  assert_eq!(tento_env::hermetic_child_env(), None);
  assert_eq!(tento_env::hermetic_child_env_os(), None);
  let (tx, rx) = mpsc::channel();
  let worker = thread::spawn(move || {
    tx.send(tento_env::resolve_key("GLOBAL_CONFLICT_KEY")).unwrap();
  });
  assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap().as_deref(), Some("custom"));
  let deadline = Instant::now() + Duration::from_secs(2);
  while !worker.is_finished() {
    assert!(Instant::now() < deadline, "lookup worker completion deadline");
    thread::yield_now();
  }
  worker.join().unwrap();
}
