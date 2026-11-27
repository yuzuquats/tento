# Tento

Tento Env loads typed Rust configuration from environment variables. Declare
keys once, reuse them across config structs, and report missing or invalid
settings at startup.

The workspace contains [`tento-env`](tento-env), the runtime facade, and
[`tento-env-macros`](tento-env/tento-env-macros), its procedural macros.
Applications depend on `tento-env`, which re-exports `env_kv!` and
`#[derive(FromEnv)]`.

## Build and test

Run these commands from the repository root with a current stable Rust toolchain.

```sh
cargo test --workspace --locked
cargo check --workspace --all-targets --locked
```

## Load a setting

In your application's `Cargo.toml`, add the facade as a path dependency. Adjust
the path to the location of your Tento checkout.

```toml
[dependencies]
tento-env = { path = "../tento/tento-env" }
```

```rust
use tento_env::FromEnv;

#[derive(FromEnv)]
struct Config {
    #[env(from = "PORT", default = 8080)]
    port: u16,
}

fn main() -> Result<(), tento_env::EnvGroupError> {
    let config = Config::from_env().into_result()?;
    println!("Configured port: {}", config.port);
    Ok(())
}
```

With `PORT` unset, the program prints `Configured port: 8080`. A valid
`PORT` overrides the default. An invalid value produces a keyed error.

Run `cargo doc --workspace --no-deps` for the runtime API. See the
[macro guide](tento-env/tento-env-macros/README.md) for reusable keys and
nested groups.

## License

[MIT](LICENSE).
