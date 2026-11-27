use tento_env::{Env, EnvError, EnvGroupBuilder, EnvMap, FromEnv, PrintEnv, Redacted, env_kv};

env_kv!(LogSecret, "PUBLIC_SMOKE_SECRET", Redacted<String>);

#[derive(Debug, FromEnv)]
struct SecretLogConfig {
  secret: Env<LogSecret>,
}

#[test]
fn string_secret_diagnostics_are_content_independent() {
  for value in ["", "xyz", "SYNTHETIC_LONG_SECRET_849273", "秘密🔑é"] {
    let map = EnvMap::new();
    map.set("PUBLIC_SMOKE_SECRET", value);
    let config = SecretLogConfig::from_backend(map).into_result().unwrap();
    let wrapped = Redacted::new(value.to_owned());
    assert_eq!(format!("{wrapped}"), "redacted[\"<redacted>\"]");
    assert_eq!(format!("{wrapped:?}"), "redacted[\"<redacted>\"]");

    let mut builder = EnvGroupBuilder::new();
    let _ = builder.require("PUBLIC_SMOKE_SECRET", Ok::<_, EnvError>(wrapped));
    let _missing: Option<String> =
      builder.require("PUBLIC_SMOKE_MISSING", Err(EnvError::missing("PUBLIC_SMOKE_MISSING")));
    let error = builder.finish(|| Some(())).into_err().unwrap();

    for text in [
      config.fmt_env(),
      format!("{config:?}"),
      error.to_string(),
      format!("{error:?}"),
    ] {
      assert!(text.contains("<redacted>"));
      if !value.is_empty() {
        assert!(!text.contains(value));
      }
    }
  }
}
