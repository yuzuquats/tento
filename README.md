# Tento

Tento Env loads typed Rust configuration from environment variables. Declare
keys once, reuse them across config structs, and report missing or invalid
settings at startup.

The workspace contains [`tento-env`](tento-env), the runtime facade, and
[`tento-env-macros`](tento-env/tento-env-macros), its procedural macros.
Applications depend on `tento-env`, which re-exports `env_kv!` and
`#[derive(FromEnv)]`.

The workspace also includes [Tento Chrono](tento-chrono), which provides
Rust and TypeScript calendar arithmetic, recurrence rules, and date ranges.
Its bindings package builds the TypeScript modules for tests in Boa.

## Build and test

Clone recursively to fetch Chrono at the recorded revision. Install Deno
and the pinned Rust nightly for the full workspace:

```sh
git clone --recurse-submodules https://github.com/yuzuquats/tento.git
cd tento
(cd tento-chrono/bindings && deno cache --frozen build.deno.ts)
cargo +nightly-2026-06-24 test --workspace --locked
cargo +nightly-2026-06-24 check --workspace --all-targets --locked
```

The Env crates also support stable Rust. To work on them independently:

```sh
cargo +stable test -p tento-env -p tento-env-macros --locked
```

See the [Chrono guide](tento-chrono/README.md) for the TypeScript API and
its Deno tests.

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
