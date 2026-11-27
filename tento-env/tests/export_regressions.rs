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
