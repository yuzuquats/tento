use tento_env::{Env, EnvError, EnvMap, FromEnv, FromEnvVar, env_kv};

env_kv!(BuilderValue, "BUILDER_VALUE", String);
env_kv!(SecondValue, "SECOND_VALUE", u16);

#[derive(FromEnv)]
struct Config {
  builder: Env<BuilderValue>,
  second: Env<SecondValue>,
}

#[test]
fn builder_field_loads_and_reports_missing_keys() {
  let map = EnvMap::new();
  map.set(BuilderValue::KEY, "value");
  map.set(SecondValue::KEY, "42");
  let config = Config::from_backend(map).unwrap();
  assert_eq!(config.builder.value().0, "value");
  assert_eq!(config.second.value().0, 42);

  let error = Config::from_backend(EnvMap::new()).into_err().unwrap();
  assert_eq!(
    error.errors(),
    &[
      EnvError::missing(BuilderValue::KEY),
      EnvError::missing(SecondValue::KEY),
    ]
  );
}

#[allow(dead_code)]
mod caller_names {
  use tento_env::{Env, EnvVars, FromEnv, PrintEnv, env_kv};

  type Result<T> = ::std::result::Result<T, ::std::io::Error>;
  struct String;
  struct Vec;
  struct Sized;
  #[allow(non_camel_case_types)]
  struct str;
  #[allow(non_camel_case_types)]
  struct bool;
  #[allow(non_camel_case_types)]
  struct usize;
  mod std {}

  type D = u16;
  env_kv!("PORT", D, default = 8080);

  #[derive(FromEnv)]
  struct Config {
    port: Env<Port>,
    #[env(from = "LABEL", default = ::std::string::String::from("service"))]
    label: ::std::string::String,
  }

  fn load() -> Result<()> {
    let config = Config::from_backend(tento_env::EnvMap::new()).unwrap();
    assert_eq!(config.port.value().0, 8080);
    assert_eq!(tento_env::serde_json::from_str::<Port>("9000").unwrap().0, 9000);
    assert_eq!(config.label, "service");
    assert_eq!(config.env_vars()[0].0, "PORT");
    assert!(config.fmt_env().contains("service"));
    Ok(())
  }

  #[test]
  fn generated_paths_bypass_caller_names() {
    load().unwrap();
  }
}
