//! Type-safe environment variable configuration with serde integration.
//!
//! This crate provides macros and types for type-safe environment variable
//! access with support for validation, redaction, and grouping.
//!
//! # Core Types
//!
//! ## `Env<T>`
//!
//! A wrapper that tracks the environment variable key alongside its value.
//! Use it for reusable keys when the loaded value should keep its key.
//! Use an inline `#[env(from = "KEY")]` field for a one-off value.
//!
//! ```ignore
//! pub struct Env<T> {
//!   key: &'static str,
//!   value: T,
//! }
//!
//! impl<T> Env<T> {
//!   pub fn key(&self) -> &'static str { self.key }
//!   pub fn value(&self) -> &T { &self.value }
//!   pub fn into_value(self) -> T { self.value }
//! }
//!
//! // Derefs to inner value for ergonomic access
//! impl<T> Deref for Env<T> {
//!   type Target = T;
//!   fn deref(&self) -> &T { &self.value }
//! }
//!
//! ```
//!
//! Format a displayable value directly. For an optional value, call
//! `display_opt()` to include the key and show whether a value is set.
//!
//! ```rust
//! use tento_env::Env;
//!
//! assert_eq!(Env::new("PORT", 8080).to_string(), "PORT: 8080");
//! assert_eq!(Env::new("PORT", None::<u16>).display_opt().to_string(), "PORT: <not set>");
//! ```
//!
//! ## `EnvGroup<T>`
//!
//! A wrapper for configuration structs loaded from environment variables.
//! Provides rich error reporting on load failure.
//!
//! ```ignore
//! pub struct EnvGroup<T> {
//!   config: T,
//! }
//!
//! impl<T> Deref for EnvGroup<T> {
//!   type Target = T;
//!   fn deref(&self) -> &T { &self.config }
//! }
//! ```
//!
//! ## `FromEnvVar` Trait
//!
//! [`FromEnvVar`] associates a reusable type with one environment key.
//! The `env_kv!` macro implements it. [`ParseEnvValue`] parses the underlying
//! value and is implemented for strings, paths, booleans, numbers and
//! compatible `Redacted<T>` wrappers.
//!
//! ## Error Types
//!
//! ### `EnvError`
//!
//! Represents a single environment variable error:
//!
//! ```ignore
//! pub enum EnvError {
//!   /// Environment variable not found
//!   Missing { key: &'static str },
//!
//!   /// Environment variable found but couldn't be parsed
//!   Parse {
//!     key: &'static str,
//!     value: String,
//!     expected_type: &'static str,
//!   },
//! }
//!
//! impl Display for EnvError {
//!   fn fmt(&self, f: &mut Formatter) -> fmt::Result {
//!     match self {
//!       EnvError::Missing { key } =>
//!         write!(f, "{}: missing", key),
//!       EnvError::Parse { key, value, expected_type } =>
//!         write!(f, "{}: \"{}\" (expected: {})", key, value, expected_type),
//!     }
//!   }
//! }
//! ```
//!
//! ### `EnvGroupError`
//!
//! Aggregates multiple errors with context about what was loaded:
//!
//! ```text
//! Environment Variable Errors:
//!
//! Loaded:
//!   [req] ACME_JS: "/path/to/acme-js"
//!   [req] TEST_STR: "hello"
//!   [opt] TEST_OPT: <not set>
//!
//! Errors:
//!   [req] DATABASE_URL: missing
//!   [req] PORT: "abc" (expected: u16)
//! ```
//!
//! # `env_kv!` Macro
//!
//! Creates a newtype struct for a single environment variable.
//!
//! ## Standard Form
//!
//! Derives the struct name from the environment variable key:
//!
//! ```ignore
//! env_kv!("ACME_JS", PathBuf);
//! env_kv!("DATABASE_URL", String);
//! env_kv!("PORT", u16, default = 8080);
//!
//! // Produces:
//! pub struct AcmeJs(pub PathBuf);      // ACME_JS -> AcmeJs
//! pub struct DatabaseUrl(pub String);  // DATABASE_URL -> DatabaseUrl
//! pub struct Port(pub u16);            // PORT -> Port (with default)
//!
//! impl FromEnvVar for AcmeJs { ... }
//! impl Deref for AcmeJs { type Target = PathBuf; ... }
//!
//! // Usage:
//! let acme_js: Env<AcmeJs> = AcmeJs::from_env()?;
//! let db: Env<DatabaseUrl> = DatabaseUrl::from_env()?;
//! let port: Env<Port> = Port::from_env()?; // Wraps Port(8080) if PORT is unset
//! ```
//!
//! ## Named Form (for conflicts)
//!
//! Explicitly specify the struct name when the derived name would conflict:
//!
//! ```ignore
//! // Both would derive to "ApiKey" from standard form
//! env_kv!(StripeApiKey, "API_KEY", String);
//! env_kv!(SendgridApiKey, "API_KEY", String);
//!
//! // Or when you want a different name than the derived one
//! env_kv!(DbConnection, "DATABASE_URL", String);
//! ```
//!
//! # Configuration Structs
//!
//! The derive supports reusable keys, one-off fields and nested groups.
//! Load from the active environment with `from_env`, or supply a backend
//! explicitly with `from_backend`.
//!
//! ```rust
//! use tento_env::{Env, EnvGroup, EnvMap, FromEnv, Redacted, env_kv};
//!
//! env_kv!("API_TOKEN", Redacted<String>, secret);
//!
//! #[derive(FromEnv)]
//! struct Server {
//!   #[env(from = "PORT", default = 8080)]
//!   port: u16,
//! }
//!
//! #[derive(FromEnv)]
//! struct Config {
//!   server: EnvGroup<Server>,
//!   token: Env<ApiToken>,
//! }
//!
//! let input = EnvMap::new();
//! input.set("API_TOKEN", "example-token");
//! let config = Config::from_backend(input).into_result()?;
//! assert_eq!(config.server.port, 8080);
//! assert_eq!(config.token.key(), "API_TOKEN");
//! assert!(!format!("{:?}", config.token).contains("example-token"));
//! # Ok::<(), tento_env::EnvGroupError>(())
//! ```
//!
//! The `secret` modifier supplies provisioning metadata. `Redacted<T>`
//! controls diagnostic display.
//!
//! # `FromEnv` Derive Macro
//!
//! Automatically generates a `from_env()` method for configuration structs.
//! This eliminates the need to manually implement the builder pattern.
//!
//! ```ignore
//! use tento_env::{env_kv, Env, FromEnv};
//!
//! env_kv!("DATABASE_URL", String);
//! env_kv!("PORT", u16, default = 8080);
//! env_kv!("API_KEY", String);
//!
//! #[derive(FromEnv)]
//! struct AppConfig {
//!   database_url: Env<DatabaseUrl>,   // Required field
//!   port: Env<Port>,                  // Defaults when absent
//!   api_key: Option<Env<ApiKey>>,     // Optional field
//! }
//!
//! // The derive generates:
//! // impl AppConfig {
//! //   pub fn from_env() -> EnvGroupResult<Self> { ... }
//! // }
//!
//! // For fallible contexts (can use ?):
//! let config = AppConfig::from_env().into_result()?;
//!
//! // For infallible contexts (panics with detailed error logging):
//! let config = AppConfig::from_env().assert_envars();
//! ```
//!
//! ## Field Types
//!
//! - `Env<T>` - Required field. Uses `T::KEY` and `T::from_env()` to load.
//! - `Option<Env<T>>` - Optional field. Missing values result in `None`.
//! - `EnvResult<T>` - Three-state field for explicit handling of missing vs malformed.
//!
//! The inner type `T` must implement `FromEnvVar`.
//!
//! Optional nested groups observe declared resolution during one synchronous
//! load. All observed keys absent means None, including defaults-only groups;
//! supplied values or lookup errors activate, and no observations is unknown.
//! Defaults and lookups run once. Direct Backend.lookup, raw std::env,
//! capture_template, hidden reads, other threads/futures and unrelated callback
//! reads are not a universal presence oracle. See __capture_env_presence.
//!
//! ## `EnvResult<T>` for Explicit Error Handling
//!
//! Use `EnvResult<T>` when you need to distinguish between:
//! - `Present(Env<T>)`: Variable loaded successfully
//! - `Missing { key }`: Variable not set (may be acceptable)
//! - `Malformed(EnvError)`: Variable set but invalid (always an error)
//!
//! ```ignore
//! #[derive(FromEnv)]
//! struct Config {
//!   // Required
//!   database_url: Env<DatabaseUrl>,
//!
//!   // Optional - missing is okay, but malformed is still an error
//!   cache_url: EnvResult<CacheUrl>,
//! }
//!
//! let config = Config::from_env().assert_envars();
//!
//! match &config.cache_url {
//!   EnvResult::Present(env) => println!("Cache: {}", env.value()),
//!   EnvResult::Missing { .. } => println!("Caching disabled"),
//!   EnvResult::Malformed(err) => unreachable!("caught by assert_envars"),
//! }
//! ```
//!
// Lets this crate use its own `env_kv!` macro, whose generated code references
// `tento_env::…` paths.
extern crate self as tento_env;

mod backend;
mod capability;
mod common;
mod context;
mod env;
mod env_result;
mod error;
mod from_env;
mod group;
mod manifest;
mod preflight;
mod redactable;
mod template;

pub use backend::{
  Backend, CompositeBackend, EnvBackend, EnvMap, effective_var, global_env_map, hermetic_child_env,
  hermetic_child_env_os, hermetic_child_env_os_with, hermetic_child_env_with, resolve_key, set_global_backend,
  set_global_env_map, with_backend, with_env_map, with_env_map_async,
};
#[doc(hidden)]
pub use backend::__capture_env_presence;
// Pre-EnvMap names, kept as deprecated shims until every consumer syncs.
#[allow(deprecated)]
pub use backend::{EnvOverlay, child_env_overlay, global_overlay, set_global_overlay};
pub use capability::{Capability, CapabilityInfo};
pub use common::{Home, XdgCacheHome, XdgConfigHome, XdgDataHome, XdgStateHome, app_data_dir};
pub use context::{
  FromEnvRoot, FromEnvTest, FromEnvVarRoot, FromEnvVarTest, RootContext, TestContext,
};
pub use env::Env;
pub use env_result::EnvResult;
pub use error::EnvError;
pub use from_env::{AssertEnvVar, FromEnvVar, ParseEnvValue, ValueTypeHint};
pub use group::{
  EnvGroup, EnvGroupBuilder, EnvGroupError, EnvGroupResult, EnvLoaded, EnvVars, FromEnv, PrintEnv,
};
pub use manifest::{EnvGroupManifest, EnvManifest, EnvVar};
pub use preflight::{
  DefaultAwareGroupReport, DefaultAwareVarReport, GroupReport, VarAvailability, VarReport,
  preflight, preflight_with_defaults,
};
pub use redactable::{Redactable, Redacted};
pub use template::{
  EnvFallbacks, EnvGroupMeta, EnvTemplate, EnvVarMeta, format_template, generate_env_template,
};

// Re-export colored for use in generated code
pub use colored::Colorize;

// Re-export serde and serde_json for use in generated code
pub use serde;
pub use serde_json;

// Re-export the macros
pub use tento_env_macros::FromEnv;
pub use tento_env_macros::env_kv;

// Shared implementations for the single-key impls emitted by `env_kv!`.
#[doc(hidden)]
pub fn __key_template<T: FromEnvVar>(name: &'static str) -> EnvGroupMeta {
  let mut meta = EnvGroupMeta::required(name);
  meta.vars.push(EnvVarMeta::required(T::KEY, T::VALUE_TYPE.as_str()).with_secret(T::SECRET));
  meta
}

#[doc(hidden)]
pub fn __key_fallbacks<T: FromEnvVar>() -> EnvFallbacks {
  EnvFallbacks {
    vars: vec![T::HAS_ABSENT_FALLBACK],
    nested: Vec::new(),
  }
}

#[doc(hidden)]
pub fn __format_key<T: FromEnvVar>(indent: usize, value: &dyn std::fmt::Debug) -> String {
  format!("{}{}: {}\n",
    " ".repeat(indent),
    T::KEY.green(),
    T::VALUE_TYPE.colorize(&format!("{value:?}")),
  )
}

/// Capture environment variables based on an EnvTemplate.
///
/// This is the only permitted way to dynamically query environment variables
/// outside of `tento_env`. It uses the template metadata to know which
/// variables to query.
///
/// Returns a list of (key, value) pairs for all env vars that are set.
pub fn capture_template<T: EnvTemplate>() -> Vec<(String, String)> {
  T::env_template()
    .all_vars()
    .into_iter()
    .filter_map(|var| {
      std::env::var(var.key)
        .ok()
        .map(|val| (var.key.to_string(), val))
    })
    .collect()
}

/// Read a single env var ad-hoc — the sanctioned escape hatch for **tests and
/// tooling** that legitimately need a value outside the typed `env_kv!` /
/// `FromEnv` path (so call sites don't reach for raw `std::env`, which acme/server-app
/// crates deny via clippy `disallowed_methods`). Resolved through the active
/// [`Backend`], so `with_backend` overrides apply in tests. Prefer `env_kv!` /
/// `FromEnv` for real configuration.
pub fn test_env(key: &'static str) -> Option<String> {
  resolve_key(key).ok().flatten()
}
