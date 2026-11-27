//! Example demonstrating the FromEnv derive macro.
//!
//! Run with: cargo run -p tento-env --example derive_from_env

use std::path::PathBuf;

use tento_env::{Env, EnvMap, FromEnv, Redacted, env_kv};

env_kv!("DATABASE_URL", String);
env_kv!("ACME_JS", PathBuf);
env_kv!("PORT", u16, default = 8080);
env_kv!("DEBUG_MODE", bool, default = false);
env_kv!(StripeSecretKey, "API_KEY", Redacted<String>, secret);

// Use the derive macro to generate from_env()
#[derive(Debug, FromEnv)]
struct AppConfig {
  database_url: Env<DatabaseUrl>,
  acme_js: Env<AcmeJs>,
  port: Env<Port>,
  debug_mode: Env<DebugMode>,
  stripe_key: Option<Env<StripeSecretKey>>,
}

fn main() {
  let input = EnvMap::new();
  input.set("DATABASE_URL", "postgres://localhost/mydb");
  input.set("ACME_JS", "/home/user/acme-js");
  input.set("PORT", "3000");
  input.set("DEBUG_MODE", "true");
  input.set("API_KEY", "example-token");

  let config = AppConfig::from_backend(input).assert_envars();
  eprintln!("{config:?}");
}
