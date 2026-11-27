//! Regression assets for explicit loads and per-poll map restoration.
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::task::{Context, Poll, Waker};
use tento_env::*;

env_kv!(ExplicitValue, "STATIC_ENV_EXPLICIT_VALUE", String);
#[derive(Debug, FromEnv)]
struct Config {
  value: Env<ExplicitValue>,
}
fn map(value: &str) -> EnvMap {
  let map = EnvMap::new();
  map.set(ExplicitValue::KEY, value);
  map
}
fn current() -> String {
  resolve_key(ExplicitValue::KEY).unwrap().unwrap()
}
struct Failure;
impl Backend for Failure {
  fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
    Err(EnvError::parse(key, "synthetic failure", "backend"))
  }
}
struct Panic;
impl Backend for Panic {
  fn lookup(&self, _: &'static str) -> Result<Option<String>, EnvError> {
    panic!("synthetic backend panic")
  }
}
#[test]
fn explicit_load_bypasses_map_and_restores_both_scopes() {
  with_backend(map("outer backend"), || {
    with_env_map(map("outer map"), || {
      assert_eq!(
        Config::from_backend(map("explicit"))
          .unwrap()
          .value
          .value()
          .0,
        "explicit"
      );
      assert_eq!(current(), "outer map");
      assert!(
        Config::from_backend(EnvMap::new())
          .into_err()
          .unwrap()
          .errors()[0]
          .is_missing()
      );
      assert_eq!(
        Config::from_backend(Failure).into_err().unwrap().errors(),
        &[EnvError::parse(
          ExplicitValue::KEY,
          "synthetic failure",
          "backend"
        )]
      );
      assert!(catch_unwind(AssertUnwindSafe(|| Config::from_backend(Panic))).is_err());
      assert_eq!(current(), "outer map");
      with_backend(map("compatible backend"), || {
        assert_eq!(current(), "outer map")
      });
      with_env_map(map("nested map"), || {
        assert_eq!(
          Config::from_backend(map("nested explicit"))
            .unwrap()
            .value
            .value()
            .0,
          "nested explicit"
        );
        assert_eq!(current(), "nested map");
      });
      assert_eq!(current(), "outer map");
    });
    assert_eq!(current(), "outer backend");
  });
}

async fn command(value: &'static str) {
  assert_eq!(current(), value);
  assert_eq!(
    Config::from_backend(map("explicit"))
      .unwrap()
      .value
      .value()
      .0,
    "explicit"
  );
  assert_eq!(current(), value);
  let mut yielded = false;
  std::future::poll_fn(|_| {
    if std::mem::replace(&mut yielded, true) {
      Poll::Ready(())
    } else {
      Poll::Pending
    }
  })
  .await;
  assert_eq!(current(), value);
}
#[test]
fn deterministic_interleaving_cancellation_and_poll_panic_restore_map() {
  with_env_map(map("parent"), || {
    let mut context = Context::from_waker(Waker::noop());
    let mut a = Box::pin(with_env_map_async(map("a"), command("a")));
    let mut b = Box::pin(with_env_map_async(map("b"), command("b")));
    assert!(a.as_mut().poll(&mut context).is_pending());
    assert_eq!(current(), "parent");
    assert!(b.as_mut().poll(&mut context).is_pending());
    assert_eq!(current(), "parent");
    drop(a); // Cancellation between polls must not install any map.
    assert_eq!(current(), "parent");
    assert!(b.as_mut().poll(&mut context).is_ready());
    assert_eq!(current(), "parent");
    let mut panicking = Box::pin(with_env_map_async(map("panic map"), async {
      assert_eq!(current(), "panic map");
      Config::from_backend(Panic);
    }));
    assert!(catch_unwind(AssertUnwindSafe(|| panicking.as_mut().poll(&mut context))).is_err());
    assert_eq!(current(), "parent");
    drop(panicking);
    assert_eq!(current(), "parent");
  });
}

env_kv!(NestedValue, "STATIC_ENV_NESTED_VALUE", String);
#[derive(Debug, FromEnv)]
struct NestedConfig {
  value: Env<NestedValue>,
}

struct NestedLookup {
  inner_panics: bool,
  drop_results: std::rc::Rc<std::cell::RefCell<Vec<Option<String>>>>,
}

impl Backend for NestedLookup {
  fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
    if key != ExplicitValue::KEY {
      return Ok(Some("still in outer lookup".into()));
    }
    assert_eq!(effective_var(ExplicitValue::KEY).as_deref(), Some("outer map"));
    assert_eq!(
      hermetic_child_env_os().unwrap(),
      vec![(ExplicitValue::KEY.into(), "outer map".into())]
    );
    with_env_map(map("inner command"), || {
      assert_eq!(current(), "inner command");
      assert_eq!(effective_var(ExplicitValue::KEY).as_deref(), Some("inner command"));
    });
    assert_eq!(effective_var(ExplicitValue::KEY).as_deref(), Some("outer map"));
    if self.inner_panics {
      NestedConfig::from_backend(Panic).unwrap();
    }
    let inner = EnvMap::new();
    inner.set(NestedValue::KEY, "nested provider");
    let value = NestedConfig::from_backend(inner).unwrap().value.value().0.clone();
    assert_eq!(resolve_key(NestedValue::KEY)?.as_deref(), Some("still in outer lookup"));
    Ok(Some(value))
  }
}

impl Drop for NestedLookup {
  fn drop(&mut self) {
    // Both enclosing scopes must already be visible to user destructors.
    let restored = effective_var(ExplicitValue::KEY);
    if restored.as_deref() == Some("outer map") {
      assert_eq!(
        Config::from_backend(map("drop provider")).unwrap().value.value().0,
        "drop provider"
      );
    }
    self.drop_results.borrow_mut().push(restored);
  }
}

#[test]
fn explicit_provider_callbacks_can_load_another_config() {
  with_backend(map("outer backend"), || {
    with_env_map(map("outer map"), || {
      let drop_results = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
      let provider = |inner_panics| NestedLookup { inner_panics, drop_results: drop_results.clone() };
      assert_eq!(
        Config::from_backend(provider(false)).unwrap().value.value().0,
        "nested provider"
      );
      let panic = catch_unwind(AssertUnwindSafe(|| Config::from_backend(provider(true))));
      assert_eq!(panic.unwrap_err().downcast_ref::<&str>(), Some(&"synthetic backend panic"));
      assert_eq!(*drop_results.borrow(), vec![Some("outer map".into()); 2]);
      assert_eq!(current(), "outer map");
    });
    assert_eq!(current(), "outer backend");
  });
}
