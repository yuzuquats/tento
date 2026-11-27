//! Own integration process: backend-v-map. Either legal winner is accepted.
//! Qualification must impose an outer process deadline; no worker is joined
//! before is_finished, and all channel receives have finite deadlines.
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tento_env::{Backend, EnvError, EnvMap};

struct Custom;
impl Backend for Custom {
  fn lookup(&self, _: &'static str) -> Result<Option<String>, EnvError> {
    Ok(Some("custom".into()))
  }
}

#[test]
fn global_backend_vs_map_has_one_coherent_winner() {
  let map = EnvMap::new();
  map.set("GLOBAL_RACE_KEY", "map");
  let (ready_tx, ready_rx) = mpsc::channel();
  let (result_tx, result_rx) = mpsc::channel();
  let mut starts = Vec::new();
  let mut workers = Vec::new();
  for installing_map in [false, true] {
    let (start_tx, start_rx) = mpsc::channel();
    starts.push(start_tx);
    let ready = ready_tx.clone();
    let result = result_tx.clone();
    let map = map.clone();
    workers.push(thread::spawn(move || {
      ready.send(()).unwrap();
      start_rx.recv_timeout(Duration::from_secs(2)).unwrap();
      let installed = if installing_map {
        tento_env::set_global_env_map(map)
      } else {
        tento_env::set_global_backend(Custom)
      };
      result.send((installing_map, installed)).unwrap();
    }));
  }
  for _ in 0..2 { ready_rx.recv_timeout(Duration::from_secs(2)).unwrap(); }
  for start in starts { start.send(()).unwrap(); }
  let results = [
    result_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
    result_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
  ];
  assert_eq!(results.iter().filter(|(_, ok)| *ok).count(), 1);
  for worker in workers {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !worker.is_finished() {
      assert!(Instant::now() < deadline, "installer completion deadline");
      thread::yield_now();
    }
    worker.join().unwrap();
  }
  // Post-completion comparisons do not claim atomicity across separate calls.
  let map_won = results.iter().any(|(is_map, ok)| *is_map && *ok);
  assert_eq!(tento_env::resolve_key("GLOBAL_RACE_KEY").unwrap().as_deref(), Some(if map_won { "map" } else { "custom" }));
  if map_won {
    assert_eq!(tento_env::effective_var("GLOBAL_RACE_KEY").as_deref(), Some("map"));
    assert_eq!(tento_env::hermetic_child_env(), Some(map.snapshot()));
    assert_eq!(tento_env::hermetic_child_env_os(), Some(map.snapshot_os()));
    map.set("GLOBAL_RACE_KEY", "shared mutation");
    assert_eq!(tento_env::resolve_key("GLOBAL_RACE_KEY").unwrap().as_deref(), Some("shared mutation"));
    assert_eq!(tento_env::effective_var("GLOBAL_RACE_KEY").as_deref(), Some("shared mutation"));
  } else {
    assert!(tento_env::global_env_map().is_none());
    assert_eq!(tento_env::effective_var("GLOBAL_RACE_KEY"), None);
    assert_eq!(tento_env::hermetic_child_env(), None);
    assert_eq!(tento_env::hermetic_child_env_os(), None);
  }
}
