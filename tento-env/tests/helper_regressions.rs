//! KU-F4 integration regressions. Authored under a no-execution contract; NOTRUN.
use std::collections::HashSet;

use tento_env::{
  Backend, Env, EnvError, EnvGroup, EnvGroupResult, EnvMap, EnvResult, FromEnv, FromEnvVar,
  ParseEnvValue, PrintEnv, Redactable, Redacted, env_kv, with_backend,
};

const KEY: &str = "R2LB_REDACTED_NUMBER";
const MARKER: &str = "SYNTHETIC_R2LB_PRIVATE_MARKER_NOT_A_NUMBER_7c92";

env_kv!(RedactedNumber, "R2LB_REDACTED_NUMBER", Redacted<u32>);
env_kv!(DefaultedNumber, "R2LB_REDACTED_NUMBER", Redacted<u32>, default = Redacted::new(42));
env_kv!(PlainNumber, "R2LB_REDACTED_NUMBER", u32);

struct FailingStore;

impl Backend for FailingStore {
  fn lookup(&self, _: &'static str) -> Result<Option<String>, EnvError> {
    Err(EnvError::parse(MARKER, MARKER, MARKER))
  }
}

#[derive(Debug, FromEnv)]
struct InlineConfig {
  #[env(from = "R2LB_REDACTED_NUMBER", default = Redacted::new(42))]
  _number: Redacted<u32>,
}

#[derive(Debug, FromEnv)]
struct NestedRedaction {
  _config: EnvGroup<InlineConfig>,
}

#[test]
fn backend_errors_are_sanitized_before_they_reach_redacted_loaders() {
  let expected = EnvError::parse(KEY, "<redacted>", "redacted value");
  for error in [
    with_backend(FailingStore, RedactedNumber::from_env).unwrap_err(),
    with_backend(FailingStore, DefaultedNumber::from_env).unwrap_err(),
    NumericConfig::from_backend(FailingStore).into_err().unwrap().errors()[0].clone(),
    NestedRedaction::from_backend(FailingStore).into_err().unwrap().errors()[0].clone(),
  ] {
    assert_eq!(error, expected);
    for text in [format!("{error:?}"), error.to_string(), error.fmt_colored()] {
      assert_no_marker(&text);
    }
  }
  let group = NestedRedaction::from_backend(FailingStore).into_err().unwrap();
  for text in [format!("{group:?}"), group.to_string(), group.fmt_indented(3)] {
    assert_no_marker(&text);
  }
  assert_eq!(
    with_backend(FailingStore, PlainNumber::from_env).unwrap_err(),
    EnvError::parse(MARKER, MARKER, MARKER)
  );
  let missing = <Redacted<u32> as ParseEnvValue>::__sanitize_lookup_error(KEY, EnvError::missing(MARKER));
  assert_eq!(missing, EnvError::missing(KEY));
}

#[derive(Debug, FromEnv)]
struct NumericConfig {
  number: Env<RedactedNumber>,
}

#[derive(Debug, FromEnv)]
struct DeferredConfig {
  numeric: EnvGroupResult<NumericConfig>,
}

fn assert_no_marker(text: &str) {
  assert!(!text.contains(MARKER), "diagnostic retained synthetic input");
  assert!(text.contains(KEY), "diagnostic lost the environment key");
}

fn assert_sanitized(error: &EnvError) {
  assert_eq!(
    error,
    &EnvError::Parse {
      key: KEY,
      value: "<redacted>".into(),
      expected_type: "u32",
    }
  );
  for text in [format!("{error:?}"), error.to_string(), error.fmt_colored()] {
    assert_no_marker(&text);
    assert!(text.contains("u32"));
  }
}

#[test]
fn redacted_parse_error_is_sanitized_at_boundary() {
  let error = Redacted::<u32>::parse_env_value(KEY, MARKER).unwrap_err();
  assert_sanitized(&error);

  // The real numeric parser still reports ordinary, unwrapped input normally.
  let plain = u32::parse_env_value(KEY, MARKER).unwrap_err();
  assert!(matches!(plain, EnvError::Parse { value, .. } if value == MARKER));
}

#[test]
fn derive_redacted_numeric_diagnostics_never_include_marker() {
  let map = EnvMap::new();
  map.set(KEY, MARKER);

  let result: EnvResult<RedactedNumber> =
    with_backend(map.clone(), RedactedNumber::from_env).into();
  let EnvResult::Malformed(error) = &result else {
    panic!("malformed numeric value must fail");
  };
  assert_sanitized(error);
  assert_no_marker(&format!("{result:?}"));
  assert_no_marker(&result.to_string());
  assert_sanitized(&result.into_result().unwrap_err());

  let group = NumericConfig::from_backend(map.clone()).into_err().unwrap();
  assert!(group.loaded().is_empty());
  assert_eq!(group.error_count(), 1);
  assert_sanitized(&group.errors()[0]);
  for text in [format!("{group:?}"), group.to_string(), group.fmt_indented(3)] {
    assert_no_marker(&text);
  }

  // A real derived deferred group retains the failure and exposes PrintEnv's
  // returned diagnostics, including the path used by its stderr printer.
  let deferred = DeferredConfig::from_backend(map).into_result().unwrap();
  assert!(deferred.numeric.is_err());
  for text in [
    deferred.fmt_env(),
    deferred.fmt_env_indented(2),
    deferred.fmt_env_with_seen(2, &mut HashSet::new()),
  ] {
    assert_no_marker(&text);
    assert!(text.contains("u32"));
  }
}

#[test]
fn redacted_valid_missing_and_expected_type_are_preserved() {
  let number = Redacted::<u32>::parse_env_value(KEY, "42").unwrap();
  assert_eq!(number.extract_secret(), &42);
  assert_eq!(format!("{number}"), "redacted[\"<u32>\"]");
  assert_eq!(format!("{number:?}"), "redacted[\"<u32>\"]");
  assert_eq!(number.into_inner(), 42);

  let map = EnvMap::new();
  map.set(KEY, "42");
  let config = NumericConfig::from_backend(map).into_result().unwrap();
  assert_eq!(config.number.key(), KEY);
  assert_eq!(config.number.value().0.extract_secret(), &42);

  let missing = with_backend(EnvMap::new(), RedactedNumber::from_env).unwrap_err();
  assert_eq!(missing, EnvError::Missing { key: KEY });
  let result: EnvResult<RedactedNumber> = Err(missing).into();
  assert!(matches!(result, EnvResult::Missing { key: KEY }));
  let group = NumericConfig::from_backend(EnvMap::new()).into_err().unwrap();
  assert_eq!(group.errors(), &[EnvError::Missing { key: KEY }]);

  let preview = Redacted::<String>::parse_env_value(KEY, "SYNTHETIC-preview").unwrap();
  assert_eq!(preview.extract_secret(), "SYNTHETIC-preview");
  assert_eq!(format!("{preview}"), "redacted[\"<redacted>\"]");
}

#[test]
fn redacted_parser_preserves_inner_missing_key() {
  struct MissingValue;
  impl std::fmt::Display for MissingValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.write_str("missing")
    }
  }
  impl Redactable for MissingValue {
    fn redacted_fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.write_str("<missing>")
    }
  }
  impl ParseEnvValue for MissingValue {
    fn parse_env_value(_: &'static str, _: &str) -> Result<Self, EnvError> {
      Err(EnvError::Missing { key: "R2LB_INNER_MISSING" })
    }
  }
  let error = Redacted::<MissingValue>::parse_env_value(KEY, MARKER).unwrap_err();
  assert_eq!(error, EnvError::Missing { key: "R2LB_INNER_MISSING" });
}


mod presence_regressions {
  use std::cell::{Cell, RefCell};
  use std::collections::BTreeMap;
  use std::panic::{AssertUnwindSafe, catch_unwind};
  use std::rc::Rc;
  use tento_env::*;

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
    struct SafePair { secret: Env<super::RedactedNumber>, b: Env<PresenceB> }
    #[derive(Debug, FromEnv)]
    struct SafeOuter { pair: Option<EnvGroup<SafePair>> }
    let map = EnvMap::new();
    map.set(super::KEY, super::MARKER);
    let error = SafeOuter::from_backend(map).into_err().unwrap();
    assert_eq!(error.errors(), &[
      EnvError::parse(super::KEY, "<redacted>", "u32"),
      EnvError::missing(PresenceB::KEY),
    ]);
    for text in [format!("{error:?}"), error.to_string(), error.fmt_indented(2)] {
      assert!(!text.contains(super::MARKER));
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


mod export_regressions {
  use std::cell::Cell;
  use std::panic::{AssertUnwindSafe, catch_unwind};
  use std::rc::Rc;
  use tento_env::*;
  use tento_env::serde;
  use serde::ser::{Error as _, SerializeMap};

  env_kv!(ExportRequired, "R2LN_EXPORT_REQUIRED", String);
  env_kv!(ExportOptional, "R2LN_EXPORT_OPTIONAL", String);
  env_kv!(ExportResult, "R2LN_EXPORT_RESULT", String);
  env_kv!(ExportSecret, "R2LN_EXPORT_SECRET", Redacted<String>);
  env_kv!(ExportNumber, "R2LN_EXPORT_NUMBER", u32);
  env_kv!(ExportBool, "R2LN_EXPORT_BOOL", bool);
  #[derive(Debug, FromEnv)]
  struct Strings {
    required: Env<ExportRequired>,
    optional: Option<Env<ExportOptional>>,
    result: EnvResult<ExportResult>,
  }
  fn strings(raw: &str) -> Strings {
    let map = EnvMap::new();
    for key in [ExportRequired::KEY, ExportOptional::KEY, ExportResult::KEY] {
      map.set(key, raw);
    }
    Strings::from_backend(map).unwrap()
  }
  #[test]
  fn derive_env_vars_preserves_exact_strings_all_branches() {
    for raw in [
      r#"C:\tmp\file"#, r#""embedded" quotes"#, "trailing\"", "\\",
      "line\nnext\tcolumn\rreturn", "\u{0001}\u{0000}\u{001f}", "", "雪🦀",
    ] {
      let vars = strings(raw).env_vars();
      assert_eq!(vars, vec![
        (ExportRequired::KEY, raw.to_string()),
        (ExportOptional::KEY, raw.to_string()),
        (ExportResult::KEY, raw.to_string()),
      ]);
    }
  }
  #[derive(Debug, FromEnv)]
  struct Nested {
    required: EnvGroup<Strings>,
    optional: Option<EnvGroup<Strings>>,
    deferred: EnvGroupResult<Strings>,
    secret: Env<ExportSecret>,
    number: Env<ExportNumber>,
    flag: Env<ExportBool>,
  }
  #[test]
  fn derive_env_vars_nested_redacted_and_heterogeneous_values() {
    let raw = "raw\\path\ntrailing\"雪";
    let config = Nested {
      required: EnvGroup::new(strings(raw)),
      optional: Some(EnvGroup::new(strings(raw))),
      deferred: EnvGroupResult::ok(strings(raw)),
      secret: ExportSecret::new(Redacted::new(raw.to_string())),
      number: ExportNumber::new(42),
      flag: ExportBool::new(true),
    };
    let vars = config.env_vars();
    assert_eq!(vars.len(), 12);
    for (_, value) in &vars[..9] { assert_eq!(value, raw); }
    assert_eq!(&vars[9..], &[
      (ExportSecret::KEY, raw.to_string()),
      (ExportNumber::KEY, "42".into()),
      (ExportBool::KEY, "true".into()),
    ]);
  }

  #[derive(Debug, Clone, Copy, serde::Deserialize)]
  enum WideNumber { Unsigned(u128), Signed(i128) }
  #[derive(Debug, Clone, serde::Deserialize)]
  pub struct Wide {
    number: WideNumber,
    #[serde(skip)]
    calls: Rc<Cell<usize>>,
  }
  impl serde::Serialize for Wide {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      self.calls.set(self.calls.get() + 1);
      match self.number {
        WideNumber::Unsigned(value) => serializer.serialize_u128(value),
        WideNumber::Signed(value) => serializer.serialize_i128(value),
      }
    }
  }
  #[derive(Debug, Clone, serde::Deserialize)]
  pub struct Compound {
    number: Wide,
    #[serde(skip)]
    calls: Rc<Cell<usize>>,
  }
  impl serde::Serialize for Compound {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      self.calls.set(self.calls.get() + 1);
      let mut map = serializer.serialize_map(Some(3))?;
      // Deliberate nonlexical order: nonstring JSON text must remain unchanged.
      map.serialize_entry("z", &[self.number.clone()])?;
      map.serialize_entry("a", &true)?;
      map.serialize_entry("none", &Option::<u8>::None)?;
      map.end()
    }
  }
  #[derive(Debug, Clone, serde::Deserialize)]
  pub struct Failing {
    #[serde(skip)]
    calls: Rc<Cell<usize>>,
  }
  impl serde::Serialize for Failing {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
      self.calls.set(self.calls.get() + 1);
      Err(S::Error::custom("SYNTHETIC_R2LN_SERIALIZER_ERROR_MARKER"))
    }
  }
  // env_kv! always emits a `pub` newtype whose Deref names the inner type, so
  // the inner fixture types must be `pub` too (E0446 otherwise).
  // These export-only custom values are constructed by the caller, as allowed
  // by env_kv::new. Loading is not a second serialization or a copied exporter.
  macro_rules! export_only {
    ($ty:ty) => {
      impl ParseEnvValue for $ty {
        fn parse_env_value(key: &'static str, _: &str) -> Result<Self, EnvError> {
          Err(EnvError::parse(key, "export-only fixture", "fixture"))
        }
      }
    }
  }
  export_only!(Wide);
  export_only!(Compound);
  export_only!(Failing);
  env_kv!(ExportWide, "R2LN_EXPORT_WIDE", Wide);
  env_kv!(ExportCompound, "R2LN_EXPORT_COMPOUND", Compound);
  env_kv!(ExportFailing, "R2LN_EXPORT_FAILING", Failing);
  #[derive(Debug, FromEnv)]
  struct Wides {
    required: Env<ExportWide>,
    optional: Option<Env<ExportWide>>,
    result: EnvResult<ExportWide>,
  }
  #[derive(Debug, FromEnv)]
  struct CompoundConfig { value: Env<ExportCompound> }

  #[test]
  fn derive_env_vars_wide_numbers_all_present_branches_and_nested_compound_once() {
    // Independent exact literals; no Value-based oracle or extra package feature.
    for (number, expected) in [
      (WideNumber::Unsigned(18446744073709551616), "18446744073709551616"),
      (WideNumber::Unsigned(u128::MAX), "340282366920938463463374607431768211455"),
      (WideNumber::Signed(i128::MIN), "-170141183460469231731687303715884105728"),
    ] {
      let counts = [Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0))];
      let value = |i: usize| Wide { number, calls: counts[i].clone() };
      let config = Wides {
        required: ExportWide::new(value(0)),
        optional: Some(ExportWide::new(value(1))),
        result: EnvResult::Present(ExportWide::new(value(2))),
      };
      assert_eq!(config.env_vars(), vec![(ExportWide::KEY, expected.to_string()); 3]);
      for count in counts { assert_eq!(count.get(), 1); }

      let inner_calls = Rc::new(Cell::new(0));
      let outer_calls = Rc::new(Cell::new(0));
      let compound = CompoundConfig {
        value: ExportCompound::new(Compound {
          number: Wide { number, calls: inner_calls.clone() },
          calls: outer_calls.clone(),
        }),
      };
      assert_eq!(compound.env_vars(), vec![(
        ExportCompound::KEY, format!("{{\"z\":[{}],\"a\":true,\"none\":null}}", expected)
      )]);
      assert_eq!(inner_calls.get(), 1);
      assert_eq!(outer_calls.get(), 1);
    }
  }

  #[derive(Debug, FromEnv)]
  struct FailingRequired { earlier: Env<ExportRequired>, bad: Env<ExportFailing> }
  #[derive(Debug, FromEnv)]
  struct FailingOptional { earlier: Env<ExportRequired>, bad: Option<Env<ExportFailing>> }
  #[derive(Debug, FromEnv)]
  struct FailingResult { earlier: Env<ExportRequired>, bad: EnvResult<ExportFailing> }
  fn assert_safe_failure(run: impl FnOnce() -> Vec<(&'static str, String)>, calls: &Cell<usize>) {
    let failure = catch_unwind(AssertUnwindSafe(run));
    // A successful first field was already appended; no partial/empty vector
    // may be returned as successful export when the later serializer fails.
    let payload = failure.expect_err("serialization failure returned successful vector");
    let text = payload.downcast_ref::<String>().map(String::as_str)
      .or_else(|| payload.downcast_ref::<&str>().copied()).unwrap();
    assert_eq!(text, "failed to serialize environment value for R2LN_EXPORT_FAILING");
    assert!(!text.contains("SYNTHETIC_R2LN_SERIALIZER_ERROR_MARKER"));
    assert_eq!(calls.get(), 1);
  }
  #[test]
  fn derive_env_vars_serialize_failure_fixed_key_only_all_present_branches() {
    let calls = Rc::new(Cell::new(0));
    let earlier = || ExportRequired::new("already exported".to_string());
    let bad = || ExportFailing::new(Failing { calls: calls.clone() });
    let required = FailingRequired { earlier: earlier(), bad: bad() };
    assert_safe_failure(|| required.env_vars(), &calls);
    calls.set(0);
    let optional = FailingOptional { earlier: earlier(), bad: Some(bad()) };
    assert_safe_failure(|| optional.env_vars(), &calls);
    calls.set(0);
    let result = FailingResult { earlier: earlier(), bad: EnvResult::Present(bad()) };
    assert_safe_failure(|| result.env_vars(), &calls);
  }
}
