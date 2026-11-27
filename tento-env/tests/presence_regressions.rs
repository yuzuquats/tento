mod presence_regressions {
  use std::cell::{Cell, RefCell};
  use std::collections::BTreeMap;
  use std::panic::{AssertUnwindSafe, catch_unwind};
  use std::rc::Rc;
  use tento_env::*;

  const KEY: &str = "R2LB_REDACTED_NUMBER";
  const MARKER: &str = "SYNTHETIC_R2LB_PRIVATE_MARKER_NOT_A_NUMBER_7c92";
  env_kv!(RedactedNumber, "R2LB_REDACTED_NUMBER", Redacted<u32>);

  env_kv!(PresenceA, "R2LN_PRESENCE_A", u32);
  env_kv!(PresenceB, "R2LN_PRESENCE_B", String);
  thread_local! { static DEFAULT_CALLS: Cell<usize> = const { Cell::new(0) }; }
  fn default_value() -> u32 {
    DEFAULT_CALLS.with(|calls| calls.set(calls.get() + 1));
    7
  }
  env_kv!(PresenceDefault, "R2LN_PRESENCE_DEFAULT", u32, default = default_value());

  #[derive(Debug, FromEnv)]
  struct Pair { a: Env<PresenceA>, b: Env<PresenceB> }
  #[derive(Debug, FromEnv)]
  struct Outer { pair: Option<EnvGroup<Pair>> }
  #[derive(Debug, FromEnv)]
  struct Defaults { value: Env<PresenceDefault> }
  #[derive(Debug, FromEnv)]
  struct OuterDefaults { group: Option<EnvGroup<Defaults>> }
  #[derive(Debug, FromEnv)]
  struct OptionalOnly { value: Option<Env<PresenceB>> }
  #[derive(Debug, FromEnv)]
  struct OuterOptional { group: Option<EnvGroup<OptionalOnly>> }
  #[derive(Debug, FromEnv)]
  struct DeferredOnly { value: EnvResult<PresenceA> }
  #[derive(Debug, FromEnv)]
  struct NumericOptional { value: Option<Env<PresenceA>> }
  #[derive(Debug, FromEnv)]
  struct OuterNumericOptional { group: Option<EnvGroup<NumericOptional>> }
  #[derive(Debug, FromEnv)]
  struct OuterDeferred { group: Option<EnvGroup<DeferredOnly>> }
  #[derive(Debug, FromEnv)]
  struct OuterOuter { middle: Option<EnvGroup<Outer>> }

  struct Counting {
    map: EnvMap,
    calls: Rc<RefCell<BTreeMap<&'static str, usize>>>,
  }
  impl Backend for Counting {
    fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
      *self.calls.borrow_mut().entry(key).or_default() += 1;
      self.map.lookup(key)
    }
  }
  fn counted<T: FromEnv>(map: EnvMap) -> (EnvGroupResult<T>, BTreeMap<&'static str, usize>) {
    // Rc deliberately proves that synchronous loading adds no Send requirement.
    let calls = Rc::new(RefCell::new(BTreeMap::new()));
    let loaded = T::from_backend(Counting { map, calls: calls.clone() });
    let observed = calls.borrow().clone();
    (loaded, observed)
  }
  fn pair_map(a: Option<&str>, b: Option<&str>) -> EnvMap {
    let map = EnvMap::new();
    if let Some(a) = a { map.set(PresenceA::KEY, a); }
    if let Some(b) = b { map.set(PresenceB::KEY, b); }
    map
  }
  fn pair_once(calls: &BTreeMap<&'static str, usize>) {
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[PresenceA::KEY], 1);
    assert_eq!(calls[PresenceB::KEY], 1);
  }

  #[test]
  fn derive_optional_group_absent_partial_complete() {
    let (absent, calls) = counted::<Outer>(pair_map(None, None));
    assert!(absent.unwrap().pair.is_none());
    pair_once(&calls);
    let (partial, calls) = counted::<Outer>(pair_map(Some("7"), None));
    let error = partial.into_err().unwrap();
    assert_eq!(error.errors(), &[EnvError::missing(PresenceB::KEY)]);
    assert_eq!(error.loaded().len(), 1);
    assert_eq!(error.loaded()[0].key, PresenceA::KEY);
    assert_eq!(error.loaded()[0].display_value, "7");
    assert!(error.loaded()[0].required);
    pair_once(&calls);
    let (complete, calls) = counted::<Outer>(pair_map(Some("7"), Some("present")));
    let pair = complete.unwrap().pair.unwrap();
    assert_eq!(pair.a.value().0, 7);
    assert_eq!(pair.b.value().0, "present");
    pair_once(&calls);
  }

  #[test]
  fn derive_optional_group_malformed_keeps_missing_and_sanitized_errors() {
    let error = Outer::from_backend(pair_map(Some("bad"), None)).into_err().unwrap();
    assert_eq!(error.errors(), &[
      EnvError::parse(PresenceA::KEY, "bad", "u32"),
      EnvError::missing(PresenceB::KEY),
    ]);
    #[derive(Debug, FromEnv)]
    struct SafePair { secret: Env<RedactedNumber>, b: Env<PresenceB> }
    #[derive(Debug, FromEnv)]
    struct SafeOuter { pair: Option<EnvGroup<SafePair>> }
    let map = EnvMap::new();
    map.set(KEY, MARKER);
    let error = SafeOuter::from_backend(map).into_err().unwrap();
    assert_eq!(error.errors(), &[
      EnvError::parse(KEY, "<redacted>", "u32"),
      EnvError::missing(PresenceB::KEY),
    ]);
    for text in [format!("{error:?}"), error.to_string(), error.fmt_indented(2)] {
      assert!(!text.contains(MARKER));
    }
  }

  #[test]
  fn derive_optional_group_defaults_and_optional_only_presence() {
    DEFAULT_CALLS.with(|calls| calls.set(0));
    let (absent, calls) = counted::<OuterDefaults>(EnvMap::new());
    assert!(absent.unwrap().group.is_none());
    assert_eq!(calls[PresenceDefault::KEY], 1);
    DEFAULT_CALLS.with(|calls| assert_eq!(calls.get(), 1));
    let map = EnvMap::new();
    map.set(PresenceDefault::KEY, "7"); // Equal to default still means supplied.
    let (present, calls) = counted::<OuterDefaults>(map);
    assert_eq!(present.unwrap().group.unwrap().value.value().0, 7);
    assert_eq!(calls[PresenceDefault::KEY], 1);
    DEFAULT_CALLS.with(|calls| assert_eq!(calls.get(), 1));
    DEFAULT_CALLS.with(|calls| calls.set(0));
    let direct = with_backend(EnvMap::new(), PresenceDefault::from_env).unwrap();
    assert_eq!(direct.value().0, 7);
    DEFAULT_CALLS.with(|calls| assert_eq!(calls.get(), 1));
    for supplied in [None, Some("<not set>"), Some("supplied")] {
      let map = EnvMap::new();
      if let Some(text) = supplied { map.set(PresenceB::KEY, text); }
      let (loaded, calls) = counted::<OuterOptional>(map);
      let group = loaded.unwrap().group;
      assert_eq!(group.is_some(), supplied.is_some());
      if let Some(group) = group {
        assert_eq!(group.value.as_ref().unwrap().value().0, supplied.unwrap());
      }
      assert_eq!(calls[PresenceB::KEY], 1);
    }
    let optional_error = OuterNumericOptional::from_backend(pair_map(Some("bad"), None)).into_err().unwrap();
    assert_eq!(optional_error.errors(), &[EnvError::parse(PresenceA::KEY, "bad", "u32")]);
    assert!(OuterDeferred::from_backend(EnvMap::new()).unwrap().group.is_none());
    let present = OuterDeferred::from_backend(pair_map(Some("0"), None)).unwrap();
    assert!(matches!(&present.group.unwrap().value, EnvResult::Present(_)));
    let malformed = OuterDeferred::from_backend(pair_map(Some("bad"), None)).into_err().unwrap();
    assert_eq!(malformed.errors(), &[EnvError::parse(PresenceA::KEY, "bad", "u32")]);
  }

  #[test]
  fn derive_nested_observers_propagate_without_repeating_lookups() {
    let (absent, calls) = counted::<OuterOuter>(EnvMap::new());
    assert!(absent.unwrap().middle.is_none());
    pair_once(&calls);
    let (partial, calls) = counted::<OuterOuter>(pair_map(Some("9"), None));
    let error = partial.into_err().unwrap();
    assert_eq!(error.errors(), &[EnvError::missing(PresenceB::KEY)]);
    assert_eq!(error.loaded()[0].key, PresenceA::KEY);
    pair_once(&calls);
    let (complete, calls) = counted::<OuterOuter>(pair_map(Some("9"), Some("b")));
    assert!(complete.unwrap().middle.unwrap().pair.is_some());
    pair_once(&calls);
  }

  struct LookupError(bool);
  impl Backend for LookupError {
    fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
      if key != PresenceA::KEY { return Ok(None); }
      Err(if self.0 { EnvError::missing(key) } else { EnvError::parse(key, "backend error", "backend") })
    }
  }
  #[test]
  fn derive_lookup_errors_activate_even_missing() {
    for missing in [true, false] {
      let error = Outer::from_backend(LookupError(missing)).into_err().unwrap();
      assert_eq!(error.errors().len(), 2);
      assert_eq!(error.errors()[0], if missing {
        EnvError::missing(PresenceA::KEY)
      } else {
        EnvError::parse(PresenceA::KEY, "backend error", "backend")
      });
      assert_eq!(error.errors()[1], EnvError::missing(PresenceB::KEY));
    }
  }

  macro_rules! manual {
    ($name:ident, $load:block) => {
      #[derive(Debug)]
      struct $name;
      impl FromEnv for $name { fn from_env() -> EnvGroupResult<Self> $load }
      impl PrintEnv for $name {
        fn print_env(&self) {}
        fn fmt_env(&self) -> String { String::new() }
        fn fmt_env_indented(&self, _: usize) -> String { String::new() }
        fn fmt_env_with_seen(&self, _: usize, _: &mut std::collections::HashSet<&'static str>) -> String { String::new() }
        fn type_name() -> &'static str { stringify!($name) }
      }
      impl EnvTemplate for $name {
        fn env_template() -> EnvGroupMeta { EnvGroupMeta::required(stringify!($name)) }
      }
      impl EnvVars for $name { fn env_vars(&self) -> Vec<(&'static str, String)> { Vec::new() } }
    }
  }
  manual!(UnknownSuccess, { EnvGroupResult::ok(Self) });
  manual!(UnknownMissing, {
    EnvGroupResult::err(vec![EnvLoaded::required("MANUAL", "context")], vec![EnvError::missing("MANUAL")])
  });
  manual!(UnknownParse, {
    EnvGroupResult::err(vec![], vec![EnvError::parse("MANUAL", "bad", "manual")])
  });
  manual!(ObservedAbsentParse, {
    assert_eq!(resolve_key("R2LN_MANUAL_ABSENT").unwrap(), None);
    EnvGroupResult::err(vec![], vec![EnvError::parse("MANUAL", "bad", "manual")])
  });
  #[derive(Debug, FromEnv)]
  struct ManualSuccessOuter { group: Option<EnvGroup<UnknownSuccess>> }
  #[derive(Debug, FromEnv)]
  struct ManualMissingOuter { group: Option<EnvGroup<UnknownMissing>> }
  #[derive(Debug, FromEnv)]
  struct ManualParseOuter { group: Option<EnvGroup<UnknownParse>> }
  #[derive(Debug, FromEnv)]
  struct ManualObservedParseOuter { group: Option<EnvGroup<ObservedAbsentParse>> }
  #[test]
  fn derive_zero_observation_is_unknown_and_parse_overrides_absence() {
    assert!(ManualSuccessOuter::from_backend(EnvMap::new()).unwrap().group.is_some());
    let missing = ManualMissingOuter::from_backend(EnvMap::new()).into_err().unwrap();
    assert_eq!(missing.errors(), &[EnvError::missing("MANUAL")]);
    assert_eq!(missing.loaded()[0].display_value, "context");
    assert_eq!(ManualParseOuter::from_backend(EnvMap::new()).into_err().unwrap().errors(),
      &[EnvError::parse("MANUAL", "bad", "manual")]);
    assert_eq!(ManualObservedParseOuter::from_backend(EnvMap::new()).into_err().unwrap().errors(),
      &[EnvError::parse("MANUAL", "bad", "manual")]);
  }

  struct Reentrant(Rc<Cell<usize>>);
  impl Backend for Reentrant {
    fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
      self.0.set(self.0.get() + 1);
      let (inner, presence) = __capture_env_presence(|| {
        with_env_map(EnvMap::new(), || resolve_key("R2LN_REENTRY_ABSENT"))
      });
      assert_eq!(inner.unwrap(), None);
      assert_eq!(presence, Some(false));
      Ok(Some(if key == PresenceA::KEY { "7" } else { "b" }.into()))
    }
  }
  struct Panicking;
  impl Backend for Panicking {
    fn lookup(&self, _: &'static str) -> Result<Option<String>, EnvError> {
      panic!("synthetic nested load panic")
    }
  }
  #[test]
  fn presence_observer_reentry_once_only_and_unwind_restoration() {
    let calls = Rc::new(Cell::new(0));
    assert!(Outer::from_backend(Reentrant(calls.clone())).unwrap().pair.is_some());
    assert_eq!(calls.get(), 2);
    let map = pair_map(Some("7"), Some("b"));
    let (_, outer) = __capture_env_presence(|| {
      with_env_map(map, || { resolve_key(PresenceA::KEY).unwrap(); });
      let failed = catch_unwind(AssertUnwindSafe(|| Outer::from_backend(Panicking)));
      assert!(failed.is_err());
      assert_eq!(__capture_env_presence(|| ()).1, None);
      assert!(Outer::from_backend(EnvMap::new()).unwrap().pair.is_none());
    });
    assert_eq!(outer, Some(true)); // Prior outer observation survives inner unwind.
    assert_eq!(__capture_env_presence(|| ()).1, None);
    assert!(Outer::from_backend(EnvMap::new()).unwrap().pair.is_none());
  }
}
