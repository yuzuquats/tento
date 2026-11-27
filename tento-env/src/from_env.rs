use std::path::PathBuf;

use colored::{ColoredString, Colorize};

use crate::env::Env;
use crate::error::EnvError;
use crate::redactable::{Redactable, Redacted};

/// Hint for what type of value an environment variable holds.
/// Used to determine coloring in PrintEnv output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueTypeHint {
  /// String values (default) - displayed in yellow
  String,
  /// File system paths - displayed in cyan
  Path,
  /// Numeric values (integers, floats) - displayed in magenta
  Number,
  /// Boolean values - displayed in blue
  Bool,
  /// Redacted/sensitive values - displayed in red
  Redacted,
}

impl ValueTypeHint {
  /// Get the colored representation of a value based on its type hint.
  pub fn colorize(&self, value: &str) -> ColoredString {
    match self {
      ValueTypeHint::String => value.yellow(),
      ValueTypeHint::Path => value.cyan(),
      ValueTypeHint::Number => value.magenta(),
      ValueTypeHint::Bool => value.blue(),
      ValueTypeHint::Redacted => value.red(),
    }
  }

  /// Get a string representation of the value type for template generation.
  pub fn as_str(&self) -> &'static str {
    match self {
      ValueTypeHint::String => "String",
      ValueTypeHint::Path => "PathBuf",
      ValueTypeHint::Number => "number",
      ValueTypeHint::Bool => "bool",
      ValueTypeHint::Redacted => "Redacted",
    }
  }
}

/// Extension trait for asserting environment variable results.
///
/// Provides a more informative panic message than `.unwrap()` or `.expect()`.
pub trait AssertEnvVar<T> {
  /// Assert that the environment variable loaded successfully.
  ///
  /// On success, returns the `Env<T>` wrapper.
  /// On error, logs the error to stderr and panics with a detailed message.
  ///
  /// # Panics
  ///
  /// Panics if the environment variable is missing or invalid.
  ///
  /// # Example
  ///
  /// ```ignore
  /// let db_url = DatabaseUrl::from_env().assert_envars();
  /// // On failure, prints:
  /// // Environment Variable Error:
  /// //   DATABASE_URL: missing
  /// ```
  fn assert_envars(self) -> Env<T>;
}

impl<T> AssertEnvVar<T> for Result<Env<T>, EnvError> {
  fn assert_envars(self) -> Env<T> {
    match self {
      Ok(value) => value,
      Err(error) => {
        eprintln!("\nEnvironment Variable Error:\n  {}", error.fmt_colored());
        panic!("Failed to load environment variable: {}", error.key());
      }
    }
  }
}

/// Trait for types that can be parsed from an environment variable string.
pub trait FromEnvVar: Sized {
  /// The environment variable key this type is associated with.
  const KEY: &'static str;

  /// Hint for the value type, used for colored output.
  const VALUE_TYPE: ValueTypeHint = ValueTypeHint::String;

  /// Whether this variable is a deployable secret — provisioned via the secret
  /// store and synced to hosts (e.g. an API key or an OAuth client id), as
  /// opposed to a structural path or a runtime tunable. Independent of
  /// [`ValueTypeHint::Redacted`], which only controls display coloring: a value
  /// can be store-provisioned without being redacted (and vice versa). Drives
  /// the `secrets` fleet node's presence checks.
  const SECRET: bool = false;

  /// Whether `from_env` takes a declared fallback branch when the backend
  /// returns `Ok(None)`. Implementations with that behavior can opt in.
  const HAS_ABSENT_FALLBACK: bool = false;

  /// Parse from a string value.
  fn parse_value(s: &str) -> Result<Self, EnvError>;

  #[doc(hidden)]
  fn __sanitize_lookup_error(error: EnvError) -> EnvError {
    error
  }

  /// Load from the active resolution backend (the process environment by
  /// default; see [`crate::Backend`] / [`crate::FromEnv::from_backend`]).
  fn from_env() -> Result<Env<Self>, EnvError> {
    match crate::backend::resolve_key(Self::KEY).map_err(Self::__sanitize_lookup_error)? {
      Some(s) => {
        let value = Self::parse_value(&s)?;
        Ok(Env::new(Self::KEY, value))
      }
      None => Err(EnvError::missing(Self::KEY)),
    }
  }
}

/// Helper trait for parsing values from strings with type information.
pub trait ParseEnvValue: Sized {
  fn parse_env_value(key: &'static str, s: &str) -> Result<Self, EnvError>;

  #[doc(hidden)]
  fn __sanitize_lookup_error(_key: &'static str, error: EnvError) -> EnvError {
    error
  }
}

impl ParseEnvValue for String {
  fn parse_env_value(_key: &'static str, s: &str) -> Result<Self, EnvError> {
    Ok(s.to_string())
  }
}

impl ParseEnvValue for PathBuf {
  fn parse_env_value(_key: &'static str, s: &str) -> Result<Self, EnvError> {
    Ok(PathBuf::from(s))
  }
}

impl ParseEnvValue for bool {
  fn parse_env_value(key: &'static str, s: &str) -> Result<Self, EnvError> {
    match s.to_lowercase().as_str() {
      "true" | "1" | "yes" | "on" => Ok(true),
      "false" | "0" | "no" | "off" => Ok(false),
      _ => Err(EnvError::parse(key, s, "bool")),
    }
  }
}

macro_rules! impl_parse_env_value_for_int {
  ($($t:ty),*) => {
    $(
      impl ParseEnvValue for $t {
        fn parse_env_value(key: &'static str, s: &str) -> Result<Self, EnvError> {
          s.parse().map_err(|_| EnvError::parse(key, s, stringify!($t)))
        }
      }
    )*
  };
}

impl_parse_env_value_for_int!(
  u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64
);

impl<T: ParseEnvValue + Redactable> ParseEnvValue for Redacted<T> {
  fn __sanitize_lookup_error(key: &'static str, error: EnvError) -> EnvError {
    if error.is_missing() {
      EnvError::missing(key)
    } else {
      EnvError::parse(key, "<redacted>", "redacted value")
    }
  }

  fn parse_env_value(key: &'static str, s: &str) -> Result<Self, EnvError> {
    match T::parse_env_value(key, s) {
      Ok(value) => Ok(Redacted::new(value)),
      Err(error @ EnvError::Missing { .. }) => Err(error),
      // Discard the raw input without formatting the underlying error.
      Err(EnvError::Parse { key, expected_type, .. }) => Err(EnvError::Parse {
        key,
        value: "<redacted>".to_string(),
        expected_type,
      }),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{EnvMap, with_backend};

  fn load<T: FromEnvVar>(value: Option<&str>) -> Result<Env<T>, EnvError> {
    let map = EnvMap::new();
    if let Some(value) = value {
      map.set(T::KEY, value);
    }
    with_backend(map, T::from_env)
  }

  // Define a test newtype using the pattern env_kv! would generate
  #[derive(Debug, Clone, PartialEq)]
  pub struct AcmeJs(pub PathBuf);

  impl std::ops::Deref for AcmeJs {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
      &self.0
    }
  }

  impl FromEnvVar for AcmeJs {
    const KEY: &'static str = "ACME_JS";

    fn parse_value(s: &str) -> Result<Self, EnvError> {
      Ok(AcmeJs(PathBuf::from(s)))
    }
  }

  #[test]
  fn test_acme_js_from_env() {
    let result = load::<AcmeJs>(Some("/path/to/acme-js"));
    assert!(result.is_ok());

    let env_val = result.unwrap();
    assert_eq!(env_val.key(), "ACME_JS");
    assert_eq!(env_val.value().0, PathBuf::from("/path/to/acme-js"));

    // Test deref
    let path: &PathBuf = &env_val;
    assert_eq!(path, &PathBuf::from("/path/to/acme-js"));
  }

  #[test]
  fn test_missing_env_var() {
    #[derive(Debug)]
    #[allow(dead_code)]
    struct MissingTest(String);

    impl FromEnvVar for MissingTest {
      const KEY: &'static str = "ACME_JS_MISSING_TEST";

      fn parse_value(s: &str) -> Result<Self, EnvError> {
        Ok(MissingTest(s.to_string()))
      }
    }

    let result = load::<MissingTest>(None);
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert_eq!(err.key(), "ACME_JS_MISSING_TEST");
    assert_eq!(format!("{}", err), "ACME_JS_MISSING_TEST: missing");
  }

  // Test for Port with u16 parsing
  #[derive(Debug, Clone, PartialEq)]
  pub struct Port(pub u16);

  impl std::ops::Deref for Port {
    type Target = u16;
    fn deref(&self) -> &u16 {
      &self.0
    }
  }

  impl FromEnvVar for Port {
    const KEY: &'static str = "PORT";

    fn parse_value(s: &str) -> Result<Self, EnvError> {
      let val = u16::parse_env_value(Self::KEY, s)?;
      Ok(Port(val))
    }
  }

  #[test]
  fn test_port_from_env() {
    let result = load::<Port>(Some("8080"));
    assert!(result.is_ok());

    let env_val = result.unwrap();
    assert_eq!(env_val.key(), "PORT");
    assert_eq!(*env_val.value(), Port(8080));
    assert_eq!(**env_val, 8080u16);
  }

  #[test]
  fn test_port_parse_error() {
    let result = load::<Port>(Some("not_a_number"));
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert_eq!(format!("{}", err), "PORT: \"not_a_number\" (expected: u16)");

  }

  #[test]
  fn test_bool_parsing() {
    #[derive(Debug)]
    struct DebugMode(bool);

    impl FromEnvVar for DebugMode {
      const KEY: &'static str = "DEBUG_MODE";

      fn parse_value(s: &str) -> Result<Self, EnvError> {
        let val = bool::parse_env_value(Self::KEY, s)?;
        Ok(DebugMode(val))
      }
    }

    // Test various true values
    for val in &["true", "1", "yes", "on", "TRUE", "Yes", "ON"] {
      let result = load::<DebugMode>(Some(val)).unwrap();
      assert!(result.value().0, "Expected true for '{}'", val);
    }

    // Test various false values
    for val in &["false", "0", "no", "off", "FALSE", "No", "OFF"] {
      let result = load::<DebugMode>(Some(val)).unwrap();
      assert!(!result.value().0, "Expected false for '{}'", val);
    }

    let result = load::<DebugMode>(Some("maybe"));
    assert!(result.is_err());
    assert_eq!(
      format!("{}", result.unwrap_err()),
      "DEBUG_MODE: \"maybe\" (expected: bool)"
    );
  }

  #[test]
  fn test_assert_envars_success() {
    let env_val = load::<AcmeJs>(Some("/path/to/acme-js")).assert_envars();
    assert_eq!(env_val.key(), "ACME_JS");
    assert_eq!(env_val.value().0, PathBuf::from("/path/to/acme-js"));
  }

  #[test]
  #[should_panic(expected = "Failed to load environment variable: ASSERT_TEST_VAR")]
  fn test_assert_envars_failure() {
    #[derive(Debug)]
    #[allow(dead_code)]
    struct AssertTestVar(String);

    impl FromEnvVar for AssertTestVar {
      const KEY: &'static str = "ASSERT_TEST_VAR";

      fn parse_value(s: &str) -> Result<Self, EnvError> {
        Ok(AssertTestVar(s.to_string()))
      }
    }

    load::<AssertTestVar>(None).assert_envars();
  }
}
