//! Binary self-verification: walk a config type's [`crate::EnvTemplate`] and
//! probe each key's presence through the active [`crate::Backend`]. Reports
//! discard the supplied values without parsing or printing them.

use serde::{Deserialize, Serialize};

use crate::template::{EnvFallbacks, EnvGroupMeta, EnvTemplate};

/// Presence of a single env var.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VarReport {
  pub key: String,
  pub required: bool,
  pub secret: bool,
  pub present: bool,
}

/// Presence report for a group and its subtree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupReport {
  pub name: String,
  pub required: bool,
  /// All required vars and required sub-groups satisfy the presence check.
  pub resolved: bool,
  pub vars: Vec<VarReport>,
  pub groups: Vec<GroupReport>,
}

impl GroupReport {
  /// Keys whose presence check failed within required group ancestry.
  pub fn missing_required(&self) -> Vec<String> {
    let mut out = Vec::new();
    self.collect_missing(true, &mut out);
    out
  }

  fn collect_missing(&self, ancestors_required: bool, out: &mut Vec<String>) {
    let on_boot_path = ancestors_required && self.required;
    if on_boot_path {
      out.extend(
        self
          .vars
          .iter()
          .filter(|v| v.required && !v.present)
          .map(|v| v.key.clone()),
      );
    }
    for g in &self.groups {
      g.collect_missing(on_boot_path, out);
    }
  }
}

/// Build a presence report for `T` against the active backend (value-safe).
pub fn preflight<T: EnvTemplate>() -> GroupReport {
  walk(&T::env_template())
}

fn walk(meta: &EnvGroupMeta) -> GroupReport {
  let vars: Vec<VarReport> = meta
    .vars
    .iter()
    .map(|v| VarReport {
      key: v.key.to_string(),
      required: v.required,
      secret: v.secret,
      present: crate::resolve_key(v.key)
        .map(|o| o.is_some())
        .unwrap_or(false),
    })
    .collect();
  let groups: Vec<GroupReport> = meta.nested.iter().map(walk).collect();
  let resolved = vars.iter().all(|v| !v.required || v.present)
    && groups.iter().all(|g| !g.required || g.resolved);
  GroupReport {
    name: meta.name.to_string(),
    required: meta.required,
    resolved,
    vars,
    groups,
  }
}

/// Backend presence and declared fallback status for one field occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VarAvailability {
  /// The backend supplied a value. The report does not parse it.
  Present,
  /// The backend reported absence and the loader declares a fallback branch.
  AbsentWithFallback,
  /// The backend reported absence and the loader declares no fallback.
  Absent,
  /// The backend returned an error. Its details are discarded.
  LookupFailed,
}

/// Default-aware report for one variable occurrence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultAwareVarReport {
  pub key: String,
  pub required: bool,
  pub secret: bool,
  pub availability: VarAvailability,
}

/// Default-aware report for a group and its subtree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultAwareGroupReport {
  pub name: String,
  pub required: bool,
  pub vars: Vec<DefaultAwareVarReport>,
  pub groups: Vec<DefaultAwareGroupReport>,
}

impl DefaultAwareGroupReport {
  /// Required occurrences with neither supplied input nor an applicable
  /// declared fallback, along paths consisting entirely of required groups.
  /// Preserves occurrence order and repeated keys.
  pub fn unaccounted_required(&self) -> Vec<String> {
    let mut out = Vec::new();
    self.collect_unaccounted(&mut out);
    out
  }

  fn collect_unaccounted(&self, out: &mut Vec<String>) {
    if !self.required {
      return;
    }
    out.extend(
      self
        .vars
        .iter()
        .filter(|v| {
          v.required
            && matches!(
              v.availability,
              VarAvailability::Absent | VarAvailability::LookupFailed
            )
        })
        .map(|v| v.key.clone()),
    );
    for group in &self.groups {
      group.collect_unaccounted(out);
    }
  }
}

/// Report supplied inputs and declared fallbacks for absent keys.
///
/// This report neither parses supplied values nor evaluates fallback
/// expressions. It describes the fields of optional groups without predicting
/// whether those groups activate or any configuration loads successfully.
pub fn preflight_with_defaults<T: EnvTemplate>() -> DefaultAwareGroupReport {
  walk_with_defaults(&T::env_template(), Some(&T::env_fallbacks()))
}

fn walk_with_defaults(
  meta: &EnvGroupMeta,
  fallbacks: Option<&EnvFallbacks>,
) -> DefaultAwareGroupReport {
  let vars = meta
    .vars
    .iter()
    .enumerate()
    .map(|(index, var)| {
      let has_fallback = fallbacks.and_then(|f| f.vars.get(index)) == Some(&true);
      let availability = match crate::resolve_key(var.key) {
        Ok(Some(_)) => VarAvailability::Present,
        Ok(None) if has_fallback => VarAvailability::AbsentWithFallback,
        Ok(None) => VarAvailability::Absent,
        Err(_) => VarAvailability::LookupFailed,
      };
      DefaultAwareVarReport {
        key: var.key.to_string(),
        required: var.required,
        secret: var.secret,
        availability,
      }
    })
    .collect();
  let groups = meta
    .nested
    .iter()
    .enumerate()
    .map(|(index, group)| walk_with_defaults(group, fallbacks.and_then(|f| f.nested.get(index))))
    .collect();
  DefaultAwareGroupReport {
    name: meta.name.to_string(),
    required: meta.required,
    vars,
    groups,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Backend, Env, EnvError, EnvGroup, EnvGroupResult, FromEnv, env_kv, with_backend};

  env_kv!("PREFLIGHT_TEST_REQ", String, secret);

  #[derive(FromEnv)]
  struct ReqGroup {
    req: Env<PreflightTestReq>,
  }

  #[derive(FromEnv)]
  struct Root {
    required_group: EnvGroup<ReqGroup>,
    optional_group: EnvGroupResult<ReqGroup>,
  }

  struct PresentBackend;
  impl Backend for PresentBackend {
    fn lookup(&self, _key: &'static str) -> Result<Option<String>, EnvError> {
      Ok(Some("x".to_string()))
    }
  }
  struct AbsentBackend;
  impl Backend for AbsentBackend {
    fn lookup(&self, _key: &'static str) -> Result<Option<String>, EnvError> {
      Ok(None)
    }
  }

  #[test]
  fn preflight_reflects_active_backend() {
    let ok = with_backend(PresentBackend, preflight::<Root>);
    assert!(ok.resolved, "all present → resolved");
    assert!(ok.missing_required().is_empty());

    let missing = with_backend(AbsentBackend, preflight::<Root>);
    assert!(!missing.resolved, "required var absent → not resolved");
    // The required group's var is boot-critical; the optional group's is not.
    assert_eq!(
      missing.missing_required(),
      vec!["PREFLIGHT_TEST_REQ".to_string()]
    );
  }

  #[test]
  fn group_report_json_roundtrips() {
    let report = with_backend(AbsentBackend, preflight::<Root>);
    let json = serde_json::to_string(&report).unwrap();
    let back: GroupReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back.missing_required(), report.missing_required());
  }
}
