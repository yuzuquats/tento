# Contributing

Clone recursively so Cargo and Deno use the pinned Chrono source. Run
the relevant locked checks from [the README](README.md#build-and-test).
Include a test when changing behavior.

Open changes to the Env crates and the workspace configuration here.
Chrono and the TypeScript bindings live in
[yuzuquats/tento-chrono](https://github.com/yuzuquats/tento-chrono).
After a Chrono change lands there, a workspace change can advance its pin.

By contributing, you agree to license your contribution under the
[MIT license](LICENSE).
