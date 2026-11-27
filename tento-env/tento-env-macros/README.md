# tento-env-macros

Implementation of `tento_env::env_kv!` and `#[derive(FromEnv)]`. Consumers should depend on `tento-env` and import macros from that facade. A direct Cargo dependency may use a different name, such as `env_alias = { package = "tento-env", path = "../tento/tento-env" }`. Import the macros and types through that alias.

When an alias is a Rust keyword, use its raw identifier in Rust code. For example, a Cargo dependency named `gen` is imported as `r#gen` in Rust 2024.

```rust,ignore
use tento_env::{env_kv, Env, FromEnv};
env_kv!("PORT", u16, default = 8080);
#[derive(FromEnv)]
struct Config { port: Env<Port> }
```

`env_kv!` accepts either `"KEY", Type` or `Name, "KEY", Type`, followed by each of `default = expression` and `secret` at most once. Keys must be nonempty and contain neither NUL nor `=`. Automatic names convert underscore-separated keys to PascalCase and must be valid Rust identifiers; use the named form for other keys.

For a key used by one struct, declare it on the field and keep its plain Rust type.

```rust
use tento_env::FromEnv;

#[derive(FromEnv)]
struct ServerConfig {
    #[env(from = "PORT", default = 8080)]
    port: u16,
}
```

`ServerConfig::from_env().into_result()` loads `port` as a `u16`. An absent key uses the default, and an invalid supplied value reports an error for `PORT`. Omitting `default` requires the key to be present. The key still appears in diagnostics, templates and preflight. Default-aware preflight reports a declared fallback without evaluating it. Static templates continue to show a type placeholder.

Use field declarations for plain values with `ParseEnvValue`, Serde serialization and diagnostic formatting. Use `env_kv!` and `Env<T>` for reusable key types or fields that retain the key alongside the value. The `secret` modifier marks provisioning metadata. Use `env_kv!("API_TOKEN", Redacted<String>, secret)` when the value also needs redacted diagnostics.

`FromEnv` supports named, nongeneric structs without a `where` clause, with plain annotated fields and the wrappers `Env<T>`, `Option<Env<T>>`, `EnvResult<T>`, `EnvGroup<T>`, `Option<EnvGroup<T>>`, and `EnvGroupResult<T>`. See the `tento-env` facade for backend loading, secret handling and template APIs.

`env_kv!` recognizes `PathBuf` and `String`, plus `std::path::PathBuf` and `std::string::String` with or without leading `::`, for unquoted `Display` output (and the path template hint). Recognition is syntactic: import those unqualified names only for their standard types. Arbitrary aliases, custom qualified types and other spellings use the generic Debug-based display fallback; the macro cannot resolve their underlying types.
