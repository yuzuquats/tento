use std::cell::Cell;

use tento_env::{
  Backend, Env, EnvError, EnvGroup, EnvGroupMeta, EnvGroupResult, EnvManifest, EnvMap, EnvResult,
  EnvTemplate, EnvVarMeta, EnvVars, FromEnv, FromEnvVar, GroupReport, PrintEnv, VarAvailability,
  VarReport, env_kv, generate_env_template, preflight, preflight_with_defaults, with_backend,
  with_env_map,
};

thread_local! {
  static DEFAULT_CALLS: Cell<u32> = const { Cell::new(0) };
}

fn runtime_default() -> u16 {
  DEFAULT_CALLS.set(DEFAULT_CALLS.get() + 1);
  8080
}

const PORT: &str = "Q_ENV_DEFAULT_PORT";
env_kv!(
  "Q_ENV_DEFAULT_PORT",
  u16,
  default = runtime_default(),
  secret
);

// An existing manual implementation can use the same key with different
// loading behavior and need not implement the new associated const.
#[derive(Debug, Clone, serde::Serialize)]
struct ManualPort(u16);

impl std::fmt::Display for ManualPort {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}

impl FromEnvVar for ManualPort {
  const KEY: &'static str = PORT;
  fn parse_value(value: &str) -> Result<Self, EnvError> {
    value
      .parse()
      .map(Self)
      .map_err(|_| EnvError::parse(Self::KEY, value, "u16"))
  }
}

#[derive(FromEnv)]
struct WithFallback {
  required: Env<QEnvDefaultPort>,
  optional: Option<Env<QEnvDefaultPort>>,
  result: EnvResult<QEnvDefaultPort>,
}

#[derive(FromEnv)]
struct Root {
  first: Env<ManualPort>,
  second: Env<ManualPort>,
  required: EnvGroup<WithFallback>,
  optional: Option<EnvGroup<WithFallback>>,
  deferred: EnvGroupResult<WithFallback>,
}

#[derive(FromEnv)]
struct OptionalOnly {
  optional: Option<EnvGroup<WithFallback>>,
  deferred: EnvGroupResult<WithFallback>,
}

#[derive(Debug, FromEnv)]
struct InlineConfig {
  #[env(from = "INLINE_PORT", default = runtime_default())]
  port: u16,
  #[env(from = "INLINE_NAME")]
  builder: String,
}

#[test]
fn inline_defaults_can_call_functions_named_like_earlier_fields() {
  fn default_port() -> u16 {
    8080
  }

  #[derive(FromEnv)]
  struct Config {
    #[env(from = "INLINE_UNUSED_PORT", default = 9000)]
    default_port: u16,
    #[env(from = "INLINE_PORT", default = default_port())]
    port: u16,
  }

  let config = Config::from_backend(EnvMap::new()).into_result().unwrap();
  assert_eq!((config.default_port, config.port), (9000, 8080));
}

#[test]
fn plain_fields_share_the_key_contract_and_preflight() {
  use VarAvailability::{Absent, AbsentWithFallback};

  let input = EnvMap::new();
  with_env_map(input.clone(), || {
    let report = preflight_with_defaults::<InlineConfig>();
    assert_eq!(report.vars[0].availability, AbsentWithFallback);
    assert_eq!(report.vars[1].availability, Absent);
    assert_eq!(report.unaccounted_required(), ["INLINE_NAME"]);
    assert_eq!(DEFAULT_CALLS.get(), 0);
  });
  let template = InlineConfig::env_template();
  assert_eq!(template.vars[0].key, "INLINE_PORT");
  assert_eq!(template.vars[0].type_hint, "number");
  assert!(template.vars[0].default.is_none());

  input.set("INLINE_NAME", "demo");
  let config = InlineConfig::from_backend(input.clone())
    .into_result()
    .unwrap();
  assert_eq!((config.port, config.builder.as_str()), (8080, "demo"));
  assert_eq!(DEFAULT_CALLS.get(), 1);
  assert_eq!(
    config.env_vars(),
    [
      ("INLINE_PORT", "8080".to_owned()),
      ("INLINE_NAME", "demo".to_owned())
    ],
  );
  assert!(config.fmt_env().contains("INLINE_PORT"));

  input.set("INLINE_PORT", "9000");
  assert_eq!(
    InlineConfig::from_backend(input.clone())
      .into_result()
      .unwrap()
      .port,
    9000
  );
  input.set("INLINE_PORT", "not a number");
  let malformed = InlineConfig::from_backend(input).into_result().unwrap_err();
  assert_eq!(malformed.error_count(), 1);
  assert_eq!(malformed.errors()[0].key(), "INLINE_PORT");
  assert!(!malformed.errors()[0].is_missing());
  assert_eq!(DEFAULT_CALLS.get(), 1);

  let missing = InlineConfig::from_backend(EnvMap::new())
    .into_result()
    .unwrap_err();
  assert_eq!(missing.errors()[0].key(), "INLINE_NAME");
  assert!(missing.errors()[0].is_missing());
}

#[test]
fn absent_fallbacks_are_reported_by_occurrence_without_running_the_loader() {
  use VarAvailability::{Absent, AbsentWithFallback, Present};

  with_env_map(EnvMap::new(), || {
    let legacy = preflight::<Root>();
    let current = preflight_with_defaults::<Root>();
    assert_eq!(DEFAULT_CALLS.get(), 0);
    assert!(!legacy.resolved);
    assert_eq!(legacy.missing_required(), [PORT, PORT, PORT]);
    assert_eq!(current.unaccounted_required(), [PORT, PORT]);
    assert_eq!(current.vars[0].availability, Absent);
    assert_eq!(current.vars[1].availability, Absent);
    assert_eq!(
      current
        .groups
        .iter()
        .map(|group| (group.name.as_str(), group.required))
        .collect::<Vec<_>>(),
      [("required", true), ("optional", false), ("deferred", false)],
    );
    for group in &current.groups {
      assert_eq!(group.vars.len(), 3);
      assert!(group.vars[0].required);
      assert!(!group.vars[1].required);
      assert!(!group.vars[2].required);
      assert!(
        group
          .vars
          .iter()
          .all(|var| var.secret && var.availability == AbsentWithFallback)
      );
    }

    // Probe the generated variable's direct EnvTemplate implementation too.
    let direct = preflight_with_defaults::<QEnvDefaultPort>();
    assert_eq!(direct.vars[0].availability, AbsentWithFallback);
    assert!(direct.unaccounted_required().is_empty());
    assert!(!preflight::<QEnvDefaultPort>().vars[0].present);

    assert_eq!(
      generate_env_template::<QEnvDefaultPort>(),
      "# Environment Variables Template\n# Generated from: QEnvDefaultPort\n\n# Q_ENV_DEFAULT_PORT={{number}}\n",
    );
    assert!(QEnvDefaultPort::env_template().vars[0].default.is_none());
    let manifest = EnvManifest::of::<QEnvDefaultPort>();
    assert_eq!(manifest.boot_critical_keys()[0].key, PORT);
    assert_eq!(manifest.boot_critical_secrets()[0].key, PORT);
    assert_eq!(manifest.key_fingerprint(), "163db2b0f7a4b29e");
    let root_manifest = EnvManifest::of::<Root>();
    assert_eq!(root_manifest.boot_critical_keys().len(), 3);
    assert_eq!(root_manifest.boot_critical_secrets().len(), 1);
    assert_eq!(root_manifest.boot_critical_secrets()[0].key, PORT);
    assert_eq!(root_manifest.key_fingerprint(), "8c4e4926e2abaad2");
    assert_eq!(
      EnvManifest::of::<OptionalOnly>().key_fingerprint(),
      root_manifest.key_fingerprint()
    );
    assert!(
      EnvManifest::of::<OptionalOnly>()
        .boot_critical_keys()
        .is_empty()
    );
    assert_eq!(DEFAULT_CALLS.get(), 0);

    let loaded = WithFallback::from_env().assert_envars();
    assert_eq!(loaded.required.value().0, 8080);
    assert_eq!(DEFAULT_CALLS.get(), 3);
    let optional = OptionalOnly::from_env().assert_envars();
    assert!(optional.optional.is_none());
    assert!(optional.deferred.is_ok());
    assert_eq!(DEFAULT_CALLS.get(), 9);
  });

  let malformed = EnvMap::new();
  malformed.set(PORT, "invalid-port");
  with_env_map(malformed, || {
    let report = preflight_with_defaults::<WithFallback>();
    assert!(report.vars.iter().all(|var| var.availability == Present));
    assert!(report.unaccounted_required().is_empty());
    assert!(preflight::<WithFallback>().resolved);
    let error = WithFallback::from_env().into_result().err().unwrap();
    assert_eq!(error.error_count(), 3);
    assert!(
      error
        .errors()
        .iter()
        .all(|e| e.key() == PORT && !e.is_missing())
    );
    assert_eq!(DEFAULT_CALLS.get(), 9);
  });
}

#[test]
fn a_lookup_failure_does_not_indicate_a_fallback() {
  struct BrokenBackend;
  impl Backend for BrokenBackend {
    fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
      Err(EnvError::parse(
        key,
        "backend-specific detail",
        "test value",
      ))
    }
  }

  let report = with_backend(BrokenBackend, preflight_with_defaults::<QEnvDefaultPort>);
  assert_eq!(report.vars[0].availability, VarAvailability::LookupFailed);
  assert_eq!(report.unaccounted_required(), [PORT]);
  assert!(
    !serde_json::to_string(&report)
      .unwrap()
      .contains("backend-specific detail")
  );
}

#[test]
fn legacy_struct_literals_and_template_display_defaults_keep_their_meaning() {
  struct LegacyTemplate;
  impl EnvTemplate for LegacyTemplate {
    fn env_template() -> EnvGroupMeta {
      EnvGroupMeta {
        name: "LegacyTemplate",
        required: true,
        version: None,
        secrets_version: None,
        vars: vec![EnvVarMeta {
          key: PORT,
          type_hint: "number",
          required: true,
          secret: false,
          default: Some("display-only"),
          description: None,
        }],
        nested: vec![],
      }
    }
  }

  with_env_map(EnvMap::new(), || {
    let literal = GroupReport {
      name: "LegacyTemplate".to_string(),
      required: true,
      resolved: false,
      vars: vec![VarReport {
        key: PORT.to_string(),
        required: true,
        secret: false,
        present: false,
      }],
      groups: vec![],
    };
    assert_eq!(
      serde_json::to_value(preflight::<LegacyTemplate>()).unwrap(),
      serde_json::to_value(literal).unwrap(),
    );
    let report = preflight_with_defaults::<LegacyTemplate>();
    assert_eq!(report.vars[0].availability, VarAvailability::Absent);
    assert_eq!(report.unaccounted_required(), [PORT]);
    assert!(
      !serde_json::to_string(&report)
        .unwrap()
        .contains("display-only")
    );
  });
}
