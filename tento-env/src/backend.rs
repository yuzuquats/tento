//! Pluggable resolution backend for typed env loading.
//!
//! By default `tento-env` reads every key from the process environment. A
//! [`Backend`] lets a caller resolve a whole config from a different source
//! instead — passed explicitly via [`crate::FromEnv::from_backend`]. A load
//! resolves **entirely** through one backend; there is no layering or
//! env/store fallback.
//!
//! The disk-backed implementation lives in the separate `tento-env-file-backend`
//! crate so this core crate stays dependency-light.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ffi::{OsStr, OsString};
use std::rc::Rc;
use std::sync::{Arc, RwLock};

use crate::error::EnvError;

/// A source of raw env values keyed by name. Keys are the `&'static str`
/// `FromEnvVar::KEY` consts.
pub trait Backend {
  /// Return the raw string for `key`, or `None` if this backend has no value
  /// for it. Errors are reserved for unrecoverable conditions (e.g. a value
  /// that isn't valid UTF-8).
  fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError>;
}

/// The process environment — the default when no backend is in scope.
pub struct EnvBackend;

impl Backend for EnvBackend {
  fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
    env_lookup(key)
  }
}

/// A [`Backend`] that resolves each key through an ordered list of inner
/// backends, returning the first that has a value. The explicit opt-in to
/// *layering* (the default single-backend path has none): a caller puts one or
/// more secret-store buckets ahead of the process env so secrets resolve from
/// the store while structural keys fall through to env/cargo-`[env]`.
///
/// Example: `CompositeBackend::new(vec![Box::new(store_server-app), Box::new(EnvBackend)])`.
///
/// Inner backends are `Send + Sync` so a composite can be installed as the
/// process-global default ([`set_global_backend`]).
pub struct CompositeBackend {
  backends: Vec<Box<dyn Backend + Send + Sync>>,
}

impl CompositeBackend {
  pub fn new(backends: Vec<Box<dyn Backend + Send + Sync>>) -> Self {
    Self { backends }
  }
}

impl Backend for CompositeBackend {
  fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
    for backend in &self.backends {
      if let Some(value) = backend.lookup(key)? {
        return Ok(Some(value));
      }
    }
    Ok(None)
  }
}

/// The **owned session environment** — env as declared state. Starts EMPTY:
/// no process-env capture, no `.env` file. Mechanism seeds exactly one entry
/// ([`EnvMap::seed_home`]); everything else enters via rc declarations
/// (`env.set` / `env.default` / `env.inherit`) evaluated at load. Installed
/// process-globally via [`set_global_env_map`], it becomes BOTH the typed
/// resolution backend (every `env_kv!`/[`resolve_key`] read is a map read with
/// no process-env fallback) and the snapshot source for **hermetic child
/// spawns** (`Command::env_clear().envs(map.snapshot())`). Cheap to clone —
/// one `Arc` backs every handle.
///
/// # Key case
///
/// Variable names follow the platform's own rule, so the declared map and the
/// child env block agree with what the OS would do:
///
/// * **unix** — names are bytes. `Path` and `PATH` are two variables.
/// * **windows** — names are case-INSENSITIVE (`Path`, `PATH`, `path` are one
///   variable; so are `TEMP`/`Temp`). Every entry is keyed by its
///   ASCII-uppercase fold ([`fold_key`], the Win32 rule), so `set`, `get`,
///   `set_default`, `inherit` and the [`Backend`] impl all agree, a second
///   declaration that differs only in case OVERWRITES the first rather than
///   adding a twin, and [`snapshot`](EnvMap::snapshot) can never hand
///   `CreateProcess` two names differing only by case (which resolves
///   inconsistently). The FIRST-seen spelling is what's stored and exported —
///   `(env.inherit "Path")` then `(env.set "PATH" …)` yields one entry
///   displayed and exported as `Path`.
#[derive(Clone, Default)]
pub struct EnvMap(Arc<RwLock<EnvEntries>>);

// A name belongs to one map. Both representations are observed and changed
// under one lock so a text replacement cannot expose an intermediate absence.
#[derive(Clone, Default)]
struct EnvEntries {
  text: BTreeMap<String, EnvEntry>,
  raw: BTreeMap<OsString, (OsString, OsString)>,
}

impl EnvEntries {
  fn text_snapshot(&self) -> Vec<(String, String)> {
    self
      .text
      .values()
      .map(|entry| (entry.name.clone(), entry.value.clone()))
      .collect()
  }
}

/// One declared variable: its value plus the spelling it was first declared
/// with. The map is keyed by [`fold_key`] of the name (identity on unix), so
/// `name` is the only place the original spelling survives — it is what
/// [`EnvMap::snapshot`] exports to a child.
#[derive(Clone, Debug)]
struct EnvEntry {
  name: String,
  value: String,
}

/// The map key for an env var name: the platform's identity rule for env
/// names, and the ONE place that rule is spelled out.
///
/// Windows compares env names case-insensitively — `Path` and `PATH` name the
/// same variable — so the key is the ASCII uppercase fold, borrowed unless a
/// fold is actually needed. ASCII is the whole practical alphabet for env
/// names (and is what `std::process::Command`'s own windows key folds); a
/// non-ASCII name that Windows' uppercase table would collapse (`über` vs
/// `Über`) stays two entries here — accepted, since no such variable exists
/// in the spawn protocol or in any rc we ship. On unix a name is bytes:
/// identity, always borrowed, so nothing about unix behavior changes.
#[cfg(windows)]
fn fold_key(key: &str) -> Cow<'_, str> {
  if key.bytes().any(|b| b.is_ascii_lowercase()) {
    Cow::Owned(key.to_ascii_uppercase())
  } else {
    Cow::Borrowed(key)
  }
}

/// See the windows arm: on unix an env name is compared byte-for-byte, so the
/// fold is the identity.
#[cfg(not(windows))]
fn fold_key(key: &str) -> Cow<'_, str> {
  Cow::Borrowed(key)
}

/// The pre-EnvMap name for the rc-defaults layer. The overlay semantics
/// (process env wins, overlay fills gaps) are gone — this is now the whole
/// declared environment.
#[deprecated(note = "renamed to EnvMap — env is the declared session state now")]
pub type EnvOverlay = EnvMap;

impl EnvMap {
  /// An EMPTY map. A variable is in the map because a declaration put it
  /// there — nothing is captured from the process environment.
  pub fn new() -> Self {
    Self::default()
  }

  /// Capture the caller's complete environment for a program entrypoint.
  #[allow(clippy::disallowed_methods)]
  pub fn from_process() -> Self {
    let map = Self::new();
    for (key, value) in std::env::vars_os() {
      if let (Some(name), Some(text)) = (key.to_str(), value.to_str()) {
        map.set(name, text);
      } else {
        map.insert_raw(key, value);
      }
    }
    map
  }

  fn insert_raw(&self, name: OsString, value: OsString) {
    let mut entries = self.0.write().unwrap_or_else(|e| e.into_inner());
    #[cfg(windows)]
    let identity = name.to_ascii_uppercase();
    #[cfg(not(windows))]
    let identity = name.clone();
    let prior = identity.to_str().and_then(|key| entries.text.remove(key));
    match entries.raw.entry(identity) {
      Entry::Occupied(mut slot) => slot.get_mut().1 = value,
      Entry::Vacant(slot) => {
        let name = prior.map_or(name, |entry| entry.name.into());
        slot.insert((name, value));
      }
    }
  }

  pub fn remove(&self, key: &str) {
    let identity = fold_key(key);
    let mut entries = self.0.write().unwrap_or_else(|e| e.into_inner());
    entries.text.remove(identity.as_ref());
    entries.raw.remove(OsStr::new(identity.as_ref()));
  }

  /// Replace a boot seed before startup runs, preserving installed handles.
  pub fn replace(&self, other: &Self) {
    if Arc::ptr_eq(&self.0, &other.0) {
      return;
    }
    let entries = other.0.read().unwrap_or_else(|e| e.into_inner()).clone();
    *self.0.write().unwrap_or_else(|e| e.into_inner()) = entries;
  }

  /// An independent copy for a scoped command; clone() shares session state.
  pub fn fork(&self) -> Self {
    let entries = self.0.read().unwrap_or_else(|e| e.into_inner()).clone();
    Self(Arc::new(RwLock::new(entries)))
  }

  /// Child environment bytes, including non-UTF-8 Unix values and names.
  pub fn snapshot_os(&self) -> Vec<(OsString, OsString)> {
    let entries = self.0.read().unwrap_or_else(|e| e.into_inner());
    let mut values = Vec::with_capacity(entries.text.len() + entries.raw.len());
    values.extend(entries.text.values().map(|entry| {
      (entry.name.as_str().into(), entry.value.as_str().into())
    }));
    values.extend(entries.raw.values().cloned());
    values
  }

  /// Set `key` unconditionally (rc `env.set`, spawn-protocol vars). On
  /// windows a name that folds onto an existing entry replaces that entry's
  /// VALUE and keeps its first-seen spelling (see the type's "Key case").
  pub fn set(&self, key: impl Into<String>, value: impl Into<String>) {
    let name = key.into();
    let identity = fold_key(&name).into_owned();
    let value = value.into();
    let mut entries = self.0.write().unwrap_or_else(|e| e.into_inner());
    let raw = entries.raw.remove(OsStr::new(&identity));
    match entries.text.entry(identity) {
      Entry::Occupied(mut slot) => slot.get_mut().value = value,
      Entry::Vacant(slot) => {
        let name = raw.map_or(name, |(name, _)| {
          name.into_string().expect("raw entry shares a textual key")
        });
        slot.insert(EnvEntry { name, value });
      }
    }
  }

  /// Set `key` **if unset in the map** (rc `env.default` — first writer wins).
  pub fn set_default(&self, key: impl Into<String>, value: impl Into<String>) {
    let name = key.into();
    let identity = fold_key(&name);
    {
      let entries = self.0.read().unwrap_or_else(|e| e.into_inner());
      if entries.text.contains_key(identity.as_ref())
        || entries.raw.contains_key(OsStr::new(identity.as_ref()))
      {
        return;
      }
    }
    // User-provided conversion may call this map. Recheck under the write lock.
    let value = value.into();
    let mut entries = self.0.write().unwrap_or_else(|e| e.into_inner());
    if !entries.raw.contains_key(OsStr::new(identity.as_ref())) {
      entries
        .text
        .entry(identity.into_owned())
        .or_insert(EnvEntry { name, value });
    }
  }

  /// A single map value by (dynamically-named) key — the string-keyed
  /// sibling of the [`Backend`] impl's `&'static str` lookup.
  pub fn get(&self, key: &str) -> Option<String> {
    self
      .0
      .read()
      .unwrap_or_else(|e| e.into_inner())
      .text
      .get(fold_key(key).as_ref())
      .map(|entry| entry.value.clone())
  }

  /// All map pairs, for materializing into a child process's environment
  /// (`env_clear().envs(map.snapshot())` — the child's environment IS the
  /// declared session env). Each name is its first-seen spelling, and no two
  /// names can differ only by case on windows — one entry per variable, as
  /// the OS sees variables.
  pub fn snapshot(&self) -> Vec<(String, String)> {
    self
      .0
      .read()
      .unwrap_or_else(|e| e.into_inner())
      .text_snapshot()
  }

  /// `(env.inherit "K" …)` — copy the NAMED keys from the REAL process
  /// environment into the map, when present there. This is the single
  /// sanctioned process-env read in the system: the *declaration* lives in an
  /// rc file, only the per-key *value* crosses from the launch env, at
  /// rc-load time. Nothing crosses undeclared. Absent keys are skipped
  /// (never set to empty). Last-wins like `set` — a later `env.set` can
  /// still override an inherited value. Windows resolves the named key
  /// case-insensitively on both sides: `std::env::var("PATH")` finds a
  /// process `Path`, and the value lands on the single folded map entry.
  #[allow(clippy::disallowed_methods)]
  pub fn inherit(&self, keys: &[&str]) {
    for key in keys {
      if let Ok(value) = std::env::var(key) {
        self.set(*key, value);
      }
    }
  }

  /// Seed `HOME` — the one documented mechanism entry (user identity, not
  /// configuration; locating `~/.tshrc` precedes any rc).
  ///
  /// The launch env's `HOME` is honoured when it is an absolute directory
  /// **owned by the calling uid** — that is the user's own home by any
  /// reasonable test, and one `stat` costs microseconds. Otherwise the answer
  /// comes from the user database (`getpwuid_r`), whose first call in a fresh
  /// process is a directory-services round trip (~0.5 ms on macOS, measured)
  /// that every tsh invocation used to pay; an env `HOME` that fails the
  /// ownership test (unset, relative, missing, another user's directory) is
  /// ignored exactly as before. The userdb-miss fallback to `$HOME` remains
  /// the documented secondary inside the same single-allow site (e.g. an
  /// NSS-less container).
  pub fn seed_home(&self) {
    if let Some(home) = owned_home_from_env().or_else(home_from_userdb) {
      self.set("HOME", home);
    }
  }
}

/// The launch env's `HOME` when it names an absolute directory owned by the
/// calling uid — see [`EnvMap::seed_home`]. Anything else is `None`.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
fn owned_home_from_env() -> Option<String> {
  let home = std::env::var_os("HOME")?;
  let uid = unsafe { libc::getuid() };
  owned_home(&home, uid)
}

#[cfg(unix)]
fn owned_home(home: &OsStr, uid: libc::uid_t) -> Option<String> {
  use std::os::unix::fs::MetadataExt;
  let path = std::path::Path::new(home);
  if !path.is_absolute() {
    return None;
  }
  let meta = std::fs::metadata(path).ok()?;
  (meta.is_dir() && meta.uid() == uid).then(|| home.to_string_lossy().into_owned())
}

#[cfg(not(unix))]
fn owned_home_from_env() -> Option<String> {
  None
}

/// `HOME` from `getpwuid_r` for the current uid; process-env `HOME` as the
/// documented secondary when the userdb misses. See [`EnvMap::seed_home`].
#[cfg(unix)]
#[allow(clippy::disallowed_methods)]
fn home_from_userdb() -> Option<String> {
  unsafe {
    let uid = libc::getuid();
    let mut pw: libc::passwd = std::mem::zeroed();
    let mut buf = vec![0u8; 4096];
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    if libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut out) == 0
      && !out.is_null()
      && !pw.pw_dir.is_null()
    {
      let dir = std::ffi::CStr::from_ptr(pw.pw_dir)
        .to_string_lossy()
        .into_owned();
      if !dir.is_empty() {
        return Some(dir);
      }
    }
  }
  // Secondary (userdb miss only): the launch env's HOME. Still inside the
  // one sanctioned process-env read site.
  std::env::var_os("HOME").map(|v| v.to_string_lossy().into_owned())
}

/// Windows: the profile service's `USERPROFILE` plays the userdb's role — OS
/// identity, not user configuration — with `HOME` as the secondary for
/// environments (MSYS, CI) that set it deliberately.
#[cfg(not(unix))]
#[allow(clippy::disallowed_methods)]
fn home_from_userdb() -> Option<String> {
  std::env::var_os("USERPROFILE")
    .or_else(|| std::env::var_os("HOME"))
    .map(|v| v.to_string_lossy().into_owned())
}

impl Backend for EnvMap {
  fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
    let identity = fold_key(key);
    let entries = self.0.read().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = entries.text.get(identity.as_ref()) {
      return Ok(Some(entry.value.clone()));
    }
    // A captured non-Unicode value is supplied but malformed, not absent.
    // Never format the raw value: it may contain a secret.
    if entries.raw.contains_key(OsStr::new(identity.as_ref())) {
      return Err(EnvError::parse(key, "<invalid unicode>", "String"));
    }
    Ok(None)
  }
}

fn env_lookup(key: &'static str) -> Result<Option<String>, EnvError> {
  match std::env::var(key) {
    Ok(s) => Ok(Some(s)),
    Err(std::env::VarError::NotPresent) => Ok(None),
    Err(std::env::VarError::NotUnicode(_)) => {
      Err(EnvError::parse(key, "<invalid unicode>", "String"))
    }
  }
}

thread_local! {
  static ACTIVE: RefCell<Option<Rc<dyn Backend>>> = const { RefCell::new(None) };
}

// One publication point for both process-global handles. Independent public
// getter calls may straddle installation; they are not a multi-call snapshot.
type GlobalBackend = Arc<dyn Backend + Send + Sync>;

struct GlobalState {
  backend: Option<GlobalBackend>,
  map: Option<EnvMap>,
}

impl GlobalState {
  const fn new() -> Self {
    Self { backend: None, map: None }
  }
}

static GLOBAL: RwLock<GlobalState> = RwLock::new(GlobalState::new());

// Poison recovery relies on this PRIVATE invariant: transitions prepare all
// candidates before locking, call no user code, and move into verified empty
// slots. There is no callback, fallible operation, assertion, panic or arbitrary
// destructor between the two infallible assignments of a dual installation.
// A rejected candidate is retained until AFTER the write guard is released.
fn global_handles(state: &RwLock<GlobalState>) -> (Option<GlobalBackend>, Option<EnvMap>) {
  let guard = state.read().unwrap_or_else(|e| e.into_inner());
  (guard.backend.clone(), guard.map.clone())
}

fn install_backend(state: &RwLock<GlobalState>, backend: GlobalBackend) -> bool {
  let mut candidate = Some(backend);
  let installed = {
    let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
    if guard.backend.is_none() {
      guard.backend = candidate.take();
      true
    } else {
      false
    }
  };
  drop(candidate);
  installed
}

fn install_env_map(state: &RwLock<GlobalState>, map: EnvMap) -> bool {
  // These are handles to the SAME shared map, prepared before the write guard.
  let mut candidate_map = Some(map.clone());
  let mut candidate_backend: Option<GlobalBackend> = Some(Arc::new(map));
  let installed = {
    let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
    if guard.backend.is_none() && guard.map.is_none() {
      guard.backend = candidate_backend.take();
      guard.map = candidate_map.take();
      true
    } else {
      false
    }
  };
  drop(candidate_backend);
  drop(candidate_map);
  installed
}

fn install_overlay(state: &RwLock<GlobalState>, map: EnvMap) -> bool {
  let mut candidate = Some(map);
  let installed = {
    let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
    if guard.map.is_none() {
      guard.map = candidate.take();
      true
    } else {
      false
    }
  };
  drop(candidate);
  installed
}

/// Install the process-global default backend. The first backend wins,
/// independently of a deprecated map-only overlay. Rejected values are dropped
/// after unlocking; no backend callback or destructor runs under the state lock.
pub fn set_global_backend<B: Backend + Send + Sync + 'static>(backend: B) -> bool {
  install_backend(&GLOBAL, Arc::new(backend))
}

/// Install the SAME shared map as both the typed resolution backend and the
/// hermetic child snapshot source. Succeeds only when BOTH slots are empty.
/// A previous custom backend or map-only overlay causes false with no mutation.
pub fn set_global_env_map(map: EnvMap) -> bool {
  install_env_map(&GLOBAL, map)
}

/// The scoped or process-global map. Handles are cloned under the state lock;
/// lookup, snapshot, fork and other caller code run only after unlocking.
pub fn global_env_map() -> Option<EnvMap> {
  SCOPED_MAP
    .with(|scope| scope.borrow().declared.clone())
    .or_else(|| global_handles(&GLOBAL).1)
}

struct ScopedMap {
  declared: Option<EnvMap>,
  lookup: Option<EnvMap>,
}

thread_local! {
  static SCOPED_MAP: RefCell<ScopedMap> = const {
    RefCell::new(ScopedMap { declared: None, lookup: None })
  };
}

/// A command-local environment, restored even on panic. Does not mutate the
/// process environment or the session's map.
pub fn with_env_map<R>(map: EnvMap, run: impl FnOnce() -> R) -> R {
  struct Restore(Option<ScopedMap>);
  impl Drop for Restore {
    fn drop(&mut self) {
      if let Some(previous) = self.0.take() {
        SCOPED_MAP.with(|scope| scope.replace(previous));
      }
    }
  }
  let scope = ScopedMap {
    declared: Some(map.clone()),
    lookup: Some(map),
  };
  let _restore = Restore(Some(SCOPED_MAP.with(|slot| slot.replace(scope))));
  run()
}

/// Install only for each poll: awaiting a child cannot expose its environment
/// to another task on the same executor thread.
pub async fn with_env_map_async<F: std::future::Future>(map: EnvMap, future: F) -> F::Output {
  let mut future = std::pin::pin!(future);
  std::future::poll_fn(|context| with_env_map(map.clone(), || future.as_mut().poll(context))).await
}

/// Pre-EnvMap name: register the global map WITHOUT making it the resolution
/// backend (the old composite carried that). Prefer [`set_global_env_map`].
#[deprecated(note = "use set_global_env_map — it also installs the map as the resolution backend")]
pub fn set_global_overlay(map: EnvMap) -> bool {
  install_overlay(&GLOBAL, map)
}

/// Pre-EnvMap name for [`global_env_map`].
#[deprecated(note = "renamed to global_env_map")]
pub fn global_overlay() -> Option<EnvMap> {
  global_env_map()
}

/// The effective value of a **dynamically-named** env var: a MAP-ONLY read of
/// the installed [`EnvMap`] — never the process environment. For surfaces
/// where the name is data (a lisp `(env NAME)` form) and the typed `env_kv!`
/// resolution can't apply. Without an installed map there is no declared
/// state, so this is `None` — a variable exists because a declaration put it
/// in the map, not because the launching shell exported it.
pub fn effective_var(name: &str) -> Option<String> {
  global_env_map().and_then(|m| m.get(name))
}

/// The env pairs for a spawned child: `Some(snapshot)` when the session
/// [`EnvMap`] is installed — the caller does `cmd.env_clear()` then
/// `cmd.envs(...)`, so the child's environment IS the declared session env.
/// `None` when no map is installed (a host that never adopted the declared
/// model): the caller leaves the child inheriting, preserving legacy hosts.
pub fn hermetic_child_env() -> Option<Vec<(String, String)>> {
  global_env_map().map(|m| m.snapshot())
}

pub fn hermetic_child_env_os() -> Option<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
  global_env_map().map(|m| m.snapshot_os())
}

pub fn hermetic_child_env_os_with(
  overlay: &[(String, String)],
) -> Option<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
  let base = global_env_map();
  if base.is_none() && overlay.is_empty() {
    return None;
  }
  let map = base.map(|map| map.fork()).unwrap_or_default();
  for (key, value) in overlay {
    map.set(key.clone(), value.clone());
  }
  Some(map.snapshot_os())
}

/// [`hermetic_child_env`] with a per-child `overlay` applied LAST (overlay
/// wins over the declared map) — the seam a lane's `:env` binding spawns
/// through: the session map stays untouched, one child sees its own
/// additions. `None` only when there is neither an installed map nor an
/// overlay entry; a non-empty overlay on a map-less host yields exactly the
/// overlay (the child is then hermetic to it — the declared model, not
/// inheritance, is what a lane's `:env` opted into).
pub fn hermetic_child_env_with(overlay: &[(String, String)]) -> Option<Vec<(String, String)>> {
  let Some(map) = global_env_map() else {
    return merge_overlay(None, overlay);
  };
  let entries = map.0.read().unwrap_or_else(|e| e.into_inner());
  let base = entries.text_snapshot();
  if entries.raw.is_empty() || overlay.is_empty() {
    drop(entries);
    return merge_overlay(Some(base), overlay);
  }
  // A text replacement of a raw value can still use its captured name.
  let overlay: Vec<_> = overlay
    .iter()
    .map(|(name, value)| {
      let identity = fold_key(name);
      let spelling = entries
        .raw
        .get(OsStr::new(identity.as_ref()))
        .and_then(|(name, _)| name.to_str())
        .unwrap_or(name);
      (spelling.to_owned(), value.clone())
    })
    .collect();
  drop(entries);
  merge_overlay(Some(base), &overlay)
}

/// The pure half of [`hermetic_child_env_with`]: `overlay` applied last over
/// an optional base snapshot, preserving base order (an overridden key keeps
/// its position; new keys append in overlay order).
fn merge_overlay(
  base: Option<Vec<(String, String)>>,
  overlay: &[(String, String)],
) -> Option<Vec<(String, String)>> {
  if overlay.is_empty() {
    return base;
  }
  let mut out = base.unwrap_or_default();
  // Keep ordinary small overlays free of index allocation.
  if overlay.len() <= 4 {
    for (key, value) in overlay {
      let folded = fold_key(key);
      match out.iter_mut().find(|(existing, _)| fold_key(existing) == folded) {
        Some(slot) => slot.1 = value.clone(),
        None => out.push((key.clone(), value.clone())),
      }
    }
    return Some(out);
  }

  // Index only overlay names: auxiliary storage is bounded by the overlay,
  // even for a large base. A single base pass records the FIRST occurrence,
  // preserving the linear merge's behavior for arbitrary duplicate bases.
  let mut positions: BTreeMap<_, Option<usize>> =
    overlay.iter().map(|(key, _)| (fold_key(key), None)).collect();
  for (index, (key, _)) in out.iter().enumerate() {
    if let Some(position) = positions.get_mut(fold_key(key).as_ref()) {
      position.get_or_insert(index);
    }
  }
  for (key, value) in overlay {
    let position = positions.get_mut(fold_key(key).as_ref()).expect("indexed overlay key");
    match *position {
      Some(index) => out[index].1 = value.clone(),
      None => {
        *position = Some(out.len());
        out.push((key.clone(), value.clone()));
      }
    }
  }
  Some(out)
}

/// Deprecated shim: the whole installed map (children get the full declared
/// environment; the old "filtered to keys absent from the process env"
/// overlay semantics are gone). Empty when no map is installed.
#[deprecated(note = "use hermetic_child_env() with env_clear() — children are hermetic now")]
pub fn child_env_overlay() -> Vec<(String, String)> {
  hermetic_child_env().unwrap_or_default()
}

/// Bypass the enclosing map for typed lookups during one configuration load.
/// Keep this separate from `with_backend`, whose scoped-map precedence is public.
pub(crate) fn from_explicit_backend<B: Backend + 'static, R>(
  backend: B,
  run: impl FnOnce() -> R,
) -> R {
  run_with_backend(backend, true, run)
}

/// Run `f` with `backend` as the active resolution source, restoring the
/// previous backend (if any) on return — including on panic. Nests correctly.
/// An enclosing `with_env_map` still takes precedence. For an explicit config
/// load that bypasses that map, use [`crate::FromEnv::from_backend`].
pub fn with_backend<B: Backend + 'static, R>(backend: B, f: impl FnOnce() -> R) -> R {
  run_with_backend(backend, false, f)
}

fn run_with_backend<B: Backend + 'static, R>(
  backend: B,
  suspend_lookup_map: bool,
  run: impl FnOnce() -> R,
) -> R {
  struct Restore {
    backend: Option<Rc<dyn Backend>>,
    suspended_lookup_map: Option<Option<EnvMap>>,
  }

  impl Drop for Restore {
    fn drop(&mut self) {
      let outgoing = ACTIVE.with(|slot| slot.replace(self.backend.take()));
      if let Some(map) = self.suspended_lookup_map.take() {
        SCOPED_MAP.with(|scope| scope.borrow_mut().lookup = map);
      }
      // A backend destructor can resolve keys or enter another scope.
      drop(outgoing);
    }
  }

  let backend = Rc::new(backend);
  let _restore = Restore {
    backend: ACTIVE.with(|slot| slot.replace(Some(backend))),
    suspended_lookup_map: suspend_lookup_map
      .then(|| SCOPED_MAP.with(|scope| scope.borrow_mut().lookup.take())),
  };
  run()
}

thread_local! {
  // Booleans only: no keys, values, errors or authority are retained.
  static ENV_PRESENCE: RefCell<Vec<Option<bool>>> = const { RefCell::new(Vec::new()) };
}

struct EnvPresenceScope;
impl Drop for EnvPresenceScope {
  fn drop(&mut self) {
    ENV_PRESENCE.with(|stack| { stack.borrow_mut().pop(); });
  }
}

/// Observe declared env loading through resolve_key during ONE synchronous call.
/// Some(false) requires at least one return, all Ok(None); any supplied value
/// or lookup error activates. None is unknown, never evidence of absence.
/// Nested observations reach every enclosing scope; unwinding restores scopes.
/// No observer borrow is held across run, backend/default/formatter callbacks.
///
/// This is data, never admission. Direct Backend.lookup, raw std::env,
/// capture_template, other threads/futures and mixed hidden reads are outside
/// this finite boundary. Unrelated callback resolutions are observed too, so
/// this is not a universal presence oracle for arbitrary handwritten loaders.
#[doc(hidden)]
pub fn __capture_env_presence<R>(run: impl FnOnce() -> R) -> (R, Option<bool>) {
  ENV_PRESENCE.with(|stack| stack.borrow_mut().push(None));
  let scope = EnvPresenceScope;
  let result = run();
  let observed = ENV_PRESENCE.with(|stack| stack.borrow().last().copied().flatten());
  drop(scope);
  (result, observed)
}

fn record_env_presence(result: &Result<Option<String>, EnvError>) {
  let supplied_or_error = !matches!(result, Ok(None));
  ENV_PRESENCE.with(|stack| {
    for observed in stack.borrow_mut().iter_mut() {
      *observed = Some(observed.unwrap_or(false) || supplied_or_error);
    }
  });
}

/// Resolve a key through the active backend, or the process environment when
/// none is in scope. The single resolution primitive used by `FromEnvVar` and
/// by `env_kv!`-generated code (hence `pub` + hidden).
#[doc(hidden)]
pub fn resolve_key(key: &'static str) -> Result<Option<String>, EnvError> {
  let result = if let Some(map) = SCOPED_MAP.with(|scope| scope.borrow().lookup.clone()) {
    map.lookup(key)
  } else if let Some(backend) = ACTIVE.with(|slot| slot.borrow().clone()) {
    backend.lookup(key)
  } else if let Some(global) = global_handles(&GLOBAL).0 {
    global.lookup(key)
  } else {
    env_lookup(key)
  };
  // Record this actual return exactly once, after callbacks and guard release.
  record_env_presence(&result);
  result
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::HashMap;

  #[cfg(unix)]
  #[test]
  fn captured_non_unicode_value_is_malformed_and_preserves_os_bytes() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let map = EnvMap::new();
    let raw = OsString::from_vec(vec![b's', 0xff]);
    map.insert_raw("RAW_SYNTHETIC".into(), raw.clone());
    let error = map.lookup("RAW_SYNTHETIC").unwrap_err();
    assert!(!error.is_missing());
    assert_eq!(error, EnvError::parse("RAW_SYNTHETIC", "<invalid unicode>", "String"));
    assert_eq!(map.lookup("ABSENT_SYNTHETIC").unwrap(), None);
    assert_eq!(map.snapshot_os(), vec![(OsString::from("RAW_SYNTHETIC"), raw)]);
    with_env_map(map.clone(), || {
      assert!(hermetic_child_env_with(&[]).unwrap().is_empty());
      let overlay = [("RAW_SYNTHETIC".into(), "child".into())];
      assert_eq!(hermetic_child_env_with(&overlay), Some(overlay.to_vec()));
    });
    map.set("RAW_SYNTHETIC", "replacement");
    assert_eq!(map.lookup("RAW_SYNTHETIC").unwrap().as_deref(), Some("replacement"));
  }

  #[cfg(windows)]
  #[test]
  fn text_child_overlay_uses_map_case_identity_and_preserves_first_spelling() {
    let result = merge_overlay(Some(vec![("Path".into(), "base".into())]),
      &[("PATH".into(), "child".into()), ("path".into(), "last".into())]);
    assert_eq!(result, Some(vec![("Path".into(), "last".into())]));
  }

  #[cfg(windows)]
  #[test]
  fn raw_windows_values_share_text_keys_without_losing_first_spelling() {
    use std::os::windows::ffi::OsStringExt;

    let raw = OsString::from_wide(&[0xd800]);
    let map = EnvMap::new();
    map.insert_raw("Raw_Key".into(), raw.clone());
    map.set_default("RAW_KEY", "not a replacement");
    assert!(map.lookup("raw_key").is_err());
    assert_eq!(map.snapshot_os(), vec![("Raw_Key".into(), raw)]);

    let (text_child, child) = with_env_map(map.clone(), || {
      let overlay = [("RAW_KEY".into(), "child".into())];
      assert!(hermetic_child_env_with(&[]).unwrap().is_empty());
      (hermetic_child_env_with(&overlay).unwrap(), hermetic_child_env_os_with(&overlay).unwrap())
    });
    assert_eq!(text_child, vec![("Raw_Key".into(), "child".into())]);
    assert_eq!(child, vec![("Raw_Key".into(), "child".into())]);
    assert!(map.lookup("raw_key").is_err());

    let removed = map.fork();
    removed.remove("raw_key");
    assert!(removed.snapshot_os().is_empty());
    assert_eq!(removed.lookup("RAW_KEY").unwrap(), None);

    map.set("RAW_key", "replacement");
    assert_eq!(map.snapshot_os(), vec![("Raw_Key".into(), "replacement".into())]);
    assert_eq!(map.lookup("RAW_KEY").unwrap().as_deref(), Some("replacement"));
  }

  #[test]
  fn inherited_environment_copies_all_keys_and_can_remove_them() {
    let map = EnvMap::from_process();
    #[allow(clippy::disallowed_methods)]
    let path = std::env::var("PATH").ok();
    assert_eq!(map.get("PATH"), path);
    map.remove("PATH");
    assert_eq!(map.get("PATH"), None);
  }

  #[cfg(unix)]
  #[test]
  fn os_snapshot_and_scoped_copy_keep_non_utf8_values() {
    use std::os::unix::ffi::OsStringExt;
    let map = EnvMap::new();
    let raw = std::ffi::OsString::from_vec(vec![0xff]);
    map.insert_raw("RAW".into(), raw.clone());
    let scoped = map.fork();
    scoped.set("OTHER", "value");
    assert!(scoped.snapshot_os().contains(&("RAW".into(), raw)));
    assert_eq!(map.get("OTHER"), None);
    scoped.set_default("RAW", "must not replace bytes");
    assert_eq!(scoped.get("RAW"), None);
    scoped.remove("RAW");
    assert!(scoped.snapshot_os().iter().all(|(key, _)| key != "RAW"));
  }

  #[test]
  fn scoped_environment_is_installed_only_while_a_future_is_polled() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let map = EnvMap::new();
    map.set("SCOPED_ENV", "inside");
    let before = effective_var("SCOPED_ENV");
    let mut polls = 0;
    let future = std::future::poll_fn(|_| {
      assert_eq!(effective_var("SCOPED_ENV").as_deref(), Some("inside"));
      assert_eq!(
        resolve_key("SCOPED_ENV").unwrap().as_deref(),
        Some("inside")
      );
      assert!(
        hermetic_child_env()
          .unwrap()
          .contains(&("SCOPED_ENV".into(), "inside".into()))
      );
      polls += 1;
      if polls == 1 {
        Poll::Pending
      } else {
        Poll::Ready(())
      }
    });
    let mut future = Box::pin(with_env_map_async(map, future));
    let mut context = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut context).is_pending());
    assert_eq!(effective_var("SCOPED_ENV"), before);
    assert!(future.as_mut().poll(&mut context).is_ready());
    assert_eq!(effective_var("SCOPED_ENV"), before);
  }

  struct MapBackend(HashMap<String, String>);
  impl Backend for MapBackend {
    fn lookup(&self, key: &'static str) -> Result<Option<String>, EnvError> {
      Ok(self.0.get(key).cloned())
    }
  }

  #[test]
  fn merge_overlay_wins_and_appends_in_order() {
    let base = Some(vec![
      ("PATH".to_string(), "/usr/bin".to_string()),
      ("HOME".to_string(), "/home/a".to_string()),
    ]);
    let overlay = vec![
      ("HOME".to_string(), "/lane/own".to_string()),
      ("API_DIR".to_string(), "/lane/dir".to_string()),
    ];
    let merged = merge_overlay(base, &overlay).expect("some");
    // Overridden key keeps its base position; new key appends.
    assert_eq!(
      merged,
      vec![
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("HOME".to_string(), "/lane/own".to_string()),
        ("API_DIR".to_string(), "/lane/dir".to_string()),
      ]
    );
    // Empty overlay is a no-op passthrough — including None (legacy inherit).
    assert_eq!(merge_overlay(None, &[]), None);
    // A map-less host with an overlay: the child is hermetic to the overlay.
    assert_eq!(merge_overlay(None, &overlay).as_deref(), Some(&overlay[..]));
  }

  #[test]
  fn merge_overlay_matches_linear_for_small_and_bulk_inputs() {
    // Include duplicate base names, platform-dependent identities, repeated
    // overlay names, and new keys whose first spelling/order must survive.
    let base = vec![
      ("Path".to_string(), "first".to_string()),
      ("Path".to_string(), "duplicate".to_string()),
      ("PATH".to_string(), "case variant".to_string()),
      ("UNCHANGED".to_string(), "keep".to_string()),
    ];
    let mut large_base = base.clone();
    large_base.extend((0..1024).map(|i| (format!("KEY_{i}"), format!("base_{i}"))));
    for count in [0, 1, 4, 5, 128, 1024] {
      let overlay: Vec<_> = (0..count)
        .map(|i| {
          let key = match i % 5 {
            0 => "path".to_string(),
            1 => "Path".to_string(),
            2 => "NEW".to_string(),
            _ => format!("KEY_{i}"),
          };
          (key, format!("value_{i}"))
        })
        .collect();
      for initial in [None, Some(vec![]), Some(base.clone()), Some(large_base.clone())] {
        let expected = if overlay.is_empty() {
          initial.clone()
        } else {
          let mut expected = initial.clone().unwrap_or_default();
          for (key, value) in &overlay {
            match expected.iter_mut().find(|(name, _)| fold_key(name) == fold_key(key)) {
              Some(slot) => slot.1 = value.clone(),
              None => expected.push((key.clone(), value.clone())),
            }
          }
          Some(expected)
        };
        assert_eq!(merge_overlay(initial, &overlay), expected, "overlay count {count}");
      }
    }
  }

  #[test]
  fn composite_backend_layers_in_order() {
    let mut store = HashMap::new();
    store.insert("COMPOSITE_SECRET".to_string(), "from-store".to_string());
    let mut env = HashMap::new();
    env.insert("COMPOSITE_STRUCTURAL".to_string(), "from-env".to_string());
    // Same key in both → first backend (store) wins.
    env.insert("COMPOSITE_SECRET".to_string(), "from-env".to_string());

    let composite =
      CompositeBackend::new(vec![Box::new(MapBackend(store)), Box::new(MapBackend(env))]);
    // Secret resolves from the first (store) backend.
    assert_eq!(
      composite.lookup("COMPOSITE_SECRET").unwrap().as_deref(),
      Some("from-store")
    );
    // Structural key falls through to the second (env) backend.
    assert_eq!(
      composite.lookup("COMPOSITE_STRUCTURAL").unwrap().as_deref(),
      Some("from-env")
    );
    // Absent in both → None.
    assert_eq!(composite.lookup("COMPOSITE_ABSENT").unwrap(), None);
  }

  #[test]
  fn env_map_starts_empty_and_set_vs_set_default() {
    // Empty by default: no process-env capture, no dotfile.
    let map = EnvMap::new();
    assert!(map.snapshot().is_empty(), "a fresh EnvMap must be EMPTY");
    assert_eq!(map.get("PATH"), None, "not even PATH leaks in");

    // set_default = set-if-unset; set = last-wins.
    map.set_default("K", "first");
    map.set_default("K", "second");
    assert_eq!(map.get("K").as_deref(), Some("first"));
    map.set("K", "explicit");
    assert_eq!(map.get("K").as_deref(), Some("explicit"));
    map.set_default("K", "late-default-loses");
    assert_eq!(map.get("K").as_deref(), Some("explicit"));
    assert_eq!(
      map.snapshot(),
      vec![("K".to_string(), "explicit".to_string())]
    );
  }

  #[test]
  fn inherit_copies_only_named_present_keys() {
    let map = EnvMap::new();
    // PATH is guaranteed present in the test process env; the bogus key is not.
    map.inherit(&["PATH", "TENTO_ENV_TEST_DEFINITELY_ABSENT"]);
    #[allow(clippy::disallowed_methods)]
    let real_path = std::env::var("PATH").ok();
    assert_eq!(map.get("PATH"), real_path, "named+present key copied");
    assert_eq!(
      map.get("TENTO_ENV_TEST_DEFINITELY_ABSENT"),
      None,
      "absent key is skipped, never set to empty"
    );
    // ONLY the named keys crossed — HOME (present in the process env) did not.
    assert_eq!(map.get("HOME"), None, "unnamed keys never cross");
    assert_eq!(map.snapshot().len(), 1);
  }

  #[test]
  fn seed_home_resolves_from_the_user_database() {
    let map = EnvMap::new();
    map.seed_home();
    let home = map.get("HOME").expect("userdb yields a home dir");
    assert!(
      std::path::Path::new(&home).is_absolute(),
      "home is an absolute path: {home}"
    );
    assert_eq!(map.snapshot().len(), 1, "seed_home adds exactly HOME");
  }

  #[cfg(unix)]
  #[test]
  fn home_hint_requires_an_owned_absolute_directory() {
    use std::os::unix::fs::MetadataExt;

    let home = std::env::temp_dir().join(format!("tsh-home-{}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    let uid = std::fs::metadata(&home).unwrap().uid();
    assert_eq!(owned_home(home.as_os_str(), uid).as_deref(), home.to_str());
    assert_eq!(owned_home(home.as_os_str(), uid.wrapping_add(1)), None);
    assert_eq!(owned_home(home.join("missing").as_os_str(), uid), None);
    assert_eq!(owned_home(OsStr::new("relative/home"), uid), None);
    std::fs::remove_dir(home).unwrap();
  }

  /// Map-as-backend resolution is map-only (no process-env fallthrough) —
  /// exercised via the thread-local scope here; the process-GLOBAL install
  /// (`set_global_env_map` → `resolve_key` / `effective_var` /
  /// `hermetic_child_env`) is proven in `tests/global_map.rs`, its own test
  /// process, because installing the global map would break this binary's
  /// process-env-resolution tests.
  #[test]
  fn map_backend_resolution_is_map_only() {
    let map = EnvMap::new();
    with_backend(map.clone(), || {
      // PATH is guaranteed set in the process env; the MAP's absence wins.
      assert_eq!(
        resolve_key("PATH").unwrap(),
        None,
        "no process-env leak through an installed map"
      );
    });
    map.set("PATH", "/declared/bin");
    with_backend(map.clone(), || {
      assert_eq!(
        resolve_key("PATH").unwrap().as_deref(),
        Some("/declared/bin")
      );
    });
  }

  /// The env-name identity rule, asserted per platform so the WINDOWS rule is
  /// checked by the same test everywhere: on unix the fold is the identity
  /// (behavior byte-identical to before this rule existed), on windows it is
  /// the ASCII uppercase fold Win32 applies to env names.
  #[test]
  fn fold_key_is_the_platform_env_name_rule() {
    // Already-folded / caseless names borrow on every platform.
    assert_eq!(fold_key("PATH"), "PATH");
    assert!(matches!(fold_key("PATH"), Cow::Borrowed(_)));
    assert_eq!(fold_key("TENTO_SESSION_DIR_2"), "TENTO_SESSION_DIR_2");

    #[cfg(windows)]
    {
      assert_eq!(fold_key("Path"), "PATH");
      assert_eq!(fold_key("path"), "PATH");
      assert_eq!(fold_key("Temp"), "TEMP");
      // ASCII-only fold: the non-ASCII head is left exactly as written.
      assert_eq!(fold_key("Ünicode_kEy"), "ÜNICODE_KEY");
      assert_eq!(fold_key("Path"), fold_key("PATH"));
    }
    #[cfg(not(windows))]
    {
      assert_eq!(fold_key("Path"), "Path");
      assert_eq!(fold_key("path"), "path");
      assert!(matches!(fold_key("Path"), Cow::Borrowed(_)));
      assert_ne!(fold_key("Path"), fold_key("PATH"));
    }
  }

  /// `Path` vs `PATH` in the MAP: one variable on windows, two on unix. The
  /// windows arm also pins the first-seen spelling — the map keeps `Path`
  /// even though the later write said `PATH`.
  #[test]
  fn map_keys_follow_the_platform_case_rule() {
    let map = EnvMap::new();
    map.set("Path", "/first");
    map.set("PATH", "/second");

    #[cfg(windows)]
    {
      assert_eq!(map.get("path").as_deref(), Some("/second"), "one variable");
      assert_eq!(map.get("PATH").as_deref(), Some("/second"));
      assert_eq!(
        map.snapshot(),
        vec![("Path".to_string(), "/second".to_string())],
        "first-seen spelling survives; the child env block has ONE Path"
      );
    }
    #[cfg(not(windows))]
    {
      assert_eq!(map.get("path"), None, "unix names are bytes");
      assert_eq!(map.get("Path").as_deref(), Some("/first"));
      assert_eq!(map.get("PATH").as_deref(), Some("/second"));
      assert_eq!(map.snapshot().len(), 2, "two distinct unix variables");
    }
  }

  /// `env.default` sees a case-variant declaration as ALREADY SET on windows
  /// (first writer wins across spellings), and the typed [`Backend`] read
  /// resolves through the same fold as `get`.
  #[test]
  fn set_default_and_backend_lookup_share_the_case_rule() {
    let map = EnvMap::new();
    map.set("Temp", "/first");
    map.set_default("TEMP", "/default-loses-on-windows");

    #[cfg(windows)]
    {
      assert_eq!(map.get("TEMP").as_deref(), Some("/first"));
      assert_eq!(map.lookup("TEMP").unwrap().as_deref(), Some("/first"));
      assert_eq!(map.snapshot().len(), 1);
    }
    #[cfg(not(windows))]
    {
      assert_eq!(map.get("Temp").as_deref(), Some("/first"));
      assert_eq!(
        map.get("TEMP").as_deref(),
        Some("/default-loses-on-windows"),
        "a distinct unix variable"
      );
      assert_eq!(
        map.lookup("TEMP").unwrap().as_deref(),
        Some("/default-loses-on-windows"),
        "the Backend read resolves the same byte-exact key as get"
      );
      assert_eq!(map.snapshot().len(), 2);
    }
  }

  /// The export invariant the child spawn depends on: `snapshot()` never
  /// yields two names that `CreateProcess` would treat as the same variable.
  #[test]
  fn snapshot_has_no_case_colliding_names_on_windows() {
    let map = EnvMap::new();
    map.set("Path", "/p");
    map.set("PATH", "/p2");
    map.set("path", "/p3");
    map.set("Temp", "/t");
    map.set("HOME", "/h");

    let mut folded: Vec<String> = map
      .snapshot()
      .into_iter()
      .map(|(k, _)| fold_key(&k).into_owned())
      .collect();
    let before = folded.len();
    folded.sort();
    folded.dedup();
    assert_eq!(
      folded.len(),
      before,
      "no two exported names may differ only by case"
    );
    #[cfg(windows)]
    assert_eq!(before, 3, "Path/PATH/path collapse to one entry");
    #[cfg(not(windows))]
    assert_eq!(before, 5, "unix keeps every distinct byte-spelling");
  }

  #[test]
  fn active_backend_resolves_then_restores() {
    // No backend in scope -> process env path (key absent -> None).
    assert_eq!(resolve_key("TENTO_ENV_BACKEND_TEST_KEY").unwrap(), None);

    let mut m = HashMap::new();
    m.insert(
      "TENTO_ENV_BACKEND_TEST_KEY".to_string(),
      "from-backend".to_string(),
    );

    let got = with_backend(MapBackend(m), || {
      resolve_key("TENTO_ENV_BACKEND_TEST_KEY").unwrap()
    });
    assert_eq!(got.as_deref(), Some("from-backend"));

    // Scope restored: back to process env (absent).
    assert_eq!(resolve_key("TENTO_ENV_BACKEND_TEST_KEY").unwrap(), None);
  }
}


#[cfg(test)]
mod global_state_regressions {
  use super::*;
  use std::sync::{Weak, mpsc};
  use std::thread::{self, JoinHandle};
  use std::time::{Duration, Instant};

  // Qualification MUST also impose an outer test-process deadline. A channel
  // timeout cannot cancel a deadlocked worker. Join only a completed worker.
  fn finish<T>(worker: JoinHandle<T>) -> thread::Result<T> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !worker.is_finished() {
      assert!(Instant::now() < deadline, "worker completion deadline");
      thread::yield_now();
    }
    worker.join()
  }

  fn map(value: &str) -> EnvMap {
    let map = EnvMap::new();
    map.set("GLOBAL_STATE_KEY", value);
    map
  }

  fn answer(state: &RwLock<GlobalState>) -> Option<String> {
    let backend = global_handles(state).0;
    backend.unwrap().lookup("GLOBAL_STATE_KEY").unwrap()
  }

  #[test]
  fn global_state_custom_map_and_map_map_orderings() {
    let custom_first = RwLock::new(GlobalState::new());
    assert!(install_backend(&custom_first, Arc::new(map("custom"))));
    assert!(!install_env_map(&custom_first, map("rejected")));
    assert_eq!(answer(&custom_first).as_deref(), Some("custom"));
    assert!(global_handles(&custom_first).1.is_none());

    let map_first = RwLock::new(GlobalState::new());
    let installed = map("first");
    assert!(install_env_map(&map_first, installed.clone()));
    assert!(!install_backend(&map_first, Arc::new(map("custom"))));
    assert!(!install_env_map(&map_first, map("second")));
    installed.set("GLOBAL_STATE_KEY", "shared mutation");
    let (backend, map) = global_handles(&map_first);
    assert_eq!(backend.unwrap().lookup("GLOBAL_STATE_KEY").unwrap().as_deref(), Some("shared mutation"));
    assert_eq!(map.unwrap().get("GLOBAL_STATE_KEY").as_deref(), Some("shared mutation"));
  }

  #[test]
  fn global_state_legacy_overlay_orders_remain_independent() {
    for overlay_first in [false, true] {
      let state = RwLock::new(GlobalState::new());
      if overlay_first {
        assert!(install_overlay(&state, map("overlay")));
        assert!(!install_env_map(&state, map("dual")));
        assert!(global_handles(&state).0.is_none());
      }
      assert!(install_backend(&state, Arc::new(map("custom"))));
      if !overlay_first {
        assert!(install_overlay(&state, map("overlay")));
      }
      assert!(!install_overlay(&state, map("replacement")));
      assert!(!install_env_map(&state, map("dual")));
      assert_eq!(answer(&state).as_deref(), Some("custom"));
      assert_eq!(global_handles(&state).1.unwrap().get("GLOBAL_STATE_KEY").as_deref(), Some("overlay"));
    }
    let state = RwLock::new(GlobalState::new());
    assert!(install_env_map(&state, map("dual")));
    assert!(!install_overlay(&state, map("overlay")));
    assert_eq!(answer(&state).as_deref(), Some("dual"));
  }

  struct ReentrantLookup {
    state: Weak<RwLock<GlobalState>>,
    done: mpsc::Sender<()>,
  }
  impl Backend for ReentrantLookup {
    fn lookup(&self, _: &'static str) -> Result<Option<String>, EnvError> {
      let state = self.state.upgrade().unwrap();
      assert!(global_handles(&state).0.is_some());
      assert!(!install_env_map(&state, map("rejected")));
      self.done.send(()).unwrap();
      Ok(Some("reentered".into()))
    }
  }

  #[test]
  fn global_state_lookup_reenters_after_read_guard_release() {
    let state = Arc::new(RwLock::new(GlobalState::new()));
    let (tx, rx) = mpsc::channel();
    assert!(install_backend(&state, Arc::new(ReentrantLookup {
      state: Arc::downgrade(&state), done: tx,
    })));
    let worker = thread::spawn(move || answer(&state));
    rx.recv_timeout(Duration::from_secs(2)).expect("lookup reentry deadline");
    assert_eq!(finish(worker).unwrap().as_deref(), Some("reentered"));
  }

  struct RejectedDrop {
    state: Weak<RwLock<GlobalState>>,
    done: mpsc::Sender<()>,
  }
  impl Backend for RejectedDrop {
    fn lookup(&self, _: &'static str) -> Result<Option<String>, EnvError> { Ok(None) }
  }
  impl Drop for RejectedDrop {
    fn drop(&mut self) {
      let state = self.state.upgrade().unwrap();
      assert!(global_handles(&state).0.is_some());
      assert!(!install_backend(&state, Arc::new(EnvMap::new())));
      self.done.send(()).unwrap();
    }
  }

  #[test]
  fn global_state_rejected_backend_drop_reenters_after_write_guard_release() {
    let state = Arc::new(RwLock::new(GlobalState::new()));
    assert!(install_backend(&state, Arc::new(map("winner"))));
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
      install_backend(&state, Arc::new(RejectedDrop {
        state: Arc::downgrade(&state), done: tx,
      }))
    });
    rx.recv_timeout(Duration::from_secs(2)).expect("rejected Drop reentry deadline");
    assert!(!finish(worker).unwrap());
  }

  #[test]
  fn global_state_poison_before_or_after_whole_transition_recovers() {
    for installed_before_poison in [false, true] {
      let state = Arc::new(RwLock::new(GlobalState::new()));
      if installed_before_poison {
        assert!(install_env_map(&state, map("whole")));
      }
      let worker_state = state.clone();
      let worker = thread::spawn(move || {
        let _guard = worker_state.write().unwrap();
        // No manufactured half-transition: state is empty OR wholly installed.
        panic!("synthetic poison at valid boundary");
      });
      assert!(finish(worker).is_err());
      assert!(state.is_poisoned());
      let (backend, current_map) = global_handles(&state);
      assert_eq!(backend.is_some(), installed_before_poison);
      assert_eq!(current_map.is_some(), installed_before_poison);
      assert_eq!(install_env_map(&state, map("whole")), !installed_before_poison);
      let (backend, current_map) = global_handles(&state);
      assert_eq!(backend.unwrap().lookup("GLOBAL_STATE_KEY").unwrap(), current_map.unwrap().get("GLOBAL_STATE_KEY"));
    }
  }

  #[test]
  fn global_state_map_race_single_snapshot_never_sees_half_publication() {
    let state = Arc::new(RwLock::new(GlobalState::new()));
    let (ready_tx, ready_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let mut starts = Vec::new();
    let mut workers = Vec::new();
    for value in ["left", "right"] {
      let (start_tx, start_rx) = mpsc::channel();
      starts.push(start_tx);
      let state = state.clone();
      let ready = ready_tx.clone();
      let result = result_tx.clone();
      workers.push(thread::spawn(move || {
        ready.send(()).unwrap();
        start_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        result.send(install_env_map(&state, map(value))).unwrap();
      }));
    }
    for _ in 0..2 { ready_rx.recv_timeout(Duration::from_secs(2)).unwrap(); }
    for start in starts { start.send(()).unwrap(); }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut results = Vec::new();
    while results.len() < 2 {
      // This ONE production read-lock snapshot is the coherence assertion.
      let (backend, map) = global_handles(&state);
      assert_eq!(backend.is_some(), map.is_some());
      if let (Some(backend), Some(map)) = (backend, map) {
        assert_eq!(backend.lookup("GLOBAL_STATE_KEY").unwrap(), map.get("GLOBAL_STATE_KEY"));
      }
      if let Ok(result) = result_rx.try_recv() { results.push(result); }
      assert!(Instant::now() < deadline, "map race deadline");
      thread::yield_now();
    }
    assert_eq!(results.into_iter().filter(|v| *v).count(), 1);
    for worker in workers { finish(worker).unwrap(); }
  }
}
