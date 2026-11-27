# iana-time-zone 0.1.65, vendored (tsh, 2026-09-12)

`src/tz_darwin.rs` reads the `/etc/localtime` symlink instead of CoreFoundation's
`CFTimeZoneCopySystem`, and the apple `core-foundation-sys` dependency is dropped, so an
executable using chrono's `Local` links no CoreFoundation. This patch is self-contained: retain the local `tz_darwin.rs` implementation
and the removed Apple dependency when updating the vendored crate. Requalify
macOS local-time behavior after an update; other platforms use upstream paths.
