//! Resolving user/group specs (a name **or** a numeric id) into concrete OS
//! identities.
//!
//! A spec is the raw `String` a caller configures via
//! [`DaemonConfig::user`](crate::DaemonConfig::user) /
//! [`group`](crate::DaemonConfig::group). Both `chown_paths` and
//! `drop_privileges` need it resolved, so the parsing and `getpwnam`/`getgrnam`
//! lookups live here once rather than at each call site.

use std::ffi::CString;
use std::path::PathBuf;

use nix::unistd::{Gid, Group, Uid, User};

use crate::error::DaemonizeError;

/// A user resolved from a name or numeric UID spec.
pub(crate) struct ResolvedUser {
    pub(crate) name: String,
    pub(crate) uid: Uid,
    pub(crate) gid: Gid,
    pub(crate) dir: PathBuf,
}

impl ResolvedUser {
    /// The username as a `CString` for `initgroups`.
    ///
    /// # Errors
    ///
    /// Returns `DaemonizeError::UserNotFound` if the name contains a NUL byte.
    pub(crate) fn cname(&self) -> Result<CString, DaemonizeError> {
        CString::new(self.name.as_str())
            .map_err(|e| DaemonizeError::UserNotFound(format!("invalid username: {e}")))
    }
}

/// The target identity to chown to and drop privileges to, resolved once from
/// the configured user and group specs.
pub(crate) struct ResolvedIdentity {
    user: Option<ResolvedUser>,
    explicit_group: Option<Gid>,
}

impl ResolvedIdentity {
    /// Resolve the user and group specs. Each may be a name or a numeric id.
    ///
    /// # Errors
    ///
    /// Returns `DaemonizeError::UserNotFound` / `GroupNotFound` if a configured
    /// spec cannot be resolved.
    pub(crate) fn resolve(user: Option<&str>, group: Option<&str>) -> Result<Self, DaemonizeError> {
        let user = match user {
            Some(spec) => Some(resolve_user(spec)?),
            None => None,
        };
        let explicit_group = match group {
            Some(spec) => Some(resolve_group_gid(spec)?),
            None => None,
        };
        Ok(Self {
            user,
            explicit_group,
        })
    }

    /// The resolved user, if a user spec was configured.
    pub(crate) fn user(&self) -> Option<&ResolvedUser> {
        self.user.as_ref()
    }

    /// The gid to `setgid`/`chown` to: the explicit group if configured,
    /// otherwise the user's primary group. `None` when neither is set.
    pub(crate) fn effective_gid(&self) -> Option<Gid> {
        self.explicit_group
            .or_else(|| self.user.as_ref().map(|u| u.gid))
    }

    /// The `(owner, group)` pair for `chown`. `None` in either position means
    /// "leave unchanged" — [`nix::unistd::chown`] interprets `None` exactly so,
    /// which is why this replaces the former `u32::MAX` (`-1`) sentinel.
    pub(crate) fn chown_ids(&self) -> (Option<Uid>, Option<Gid>) {
        (self.user.as_ref().map(|u| u.uid), self.effective_gid())
    }
}

/// Resolve a user spec (name or numeric UID string).
fn resolve_user(spec: &str) -> Result<ResolvedUser, DaemonizeError> {
    let user = if let Ok(uid_num) = spec.parse::<u32>() {
        let uid = Uid::from_raw(uid_num);
        User::from_uid(uid)
            .map_err(|e| DaemonizeError::UserNotFound(format!("getpwuid({uid_num}): {e}")))?
            .ok_or_else(|| DaemonizeError::UserNotFound(format!("uid {uid_num}")))?
    } else {
        User::from_name(spec)
            .map_err(|e| DaemonizeError::UserNotFound(format!("getpwnam({spec}): {e}")))?
            .ok_or_else(|| DaemonizeError::UserNotFound(spec.to_string()))?
    };
    Ok(ResolvedUser {
        name: user.name,
        uid: user.uid,
        gid: user.gid,
        dir: user.dir,
    })
}

/// Resolve a group spec (name or numeric GID string) to a GID.
fn resolve_group_gid(spec: &str) -> Result<Gid, DaemonizeError> {
    if let Ok(gid_num) = spec.parse::<u32>() {
        Ok(Gid::from_raw(gid_num))
    } else {
        let group = Group::from_name(spec)
            .map_err(|e| DaemonizeError::GroupNotFound(format!("getgrnam({spec}): {e}")))?
            .ok_or_else(|| DaemonizeError::GroupNotFound(spec.to_string()))?;
        Ok(group.gid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Covers: R63
    #[test]
    fn resolve_user_numeric() {
        // UID 0 should resolve to root on all Unix systems
        let user = resolve_user("0").unwrap();
        assert_eq!(user.uid.as_raw(), 0);
        assert_eq!(user.name, "root");
    }

    // Covers: R59
    #[test]
    fn resolve_user_name() {
        let user = resolve_user("root").unwrap();
        assert_eq!(user.uid.as_raw(), 0);
    }

    #[test]
    fn resolve_user_nonexistent_name() {
        if std::env::var("CI").is_ok() {
            return;
        }
        let result = resolve_user("nonexistent_daemonize_test_user_xyz");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_group_gid_numeric() {
        let gid = resolve_group_gid("0").unwrap();
        assert_eq!(gid.as_raw(), 0);
    }

    #[test]
    fn resolve_group_gid_by_name() {
        // "root" on Linux, "wheel" on macOS/BSD — try root first to avoid
        // NSS lookup hangs for nonexistent groups in CI.
        let result = resolve_group_gid("root").or_else(|_| resolve_group_gid("wheel"));
        assert!(result.is_ok());
    }

    // Covers: R51
    #[test]
    fn resolve_group_gid_nonexistent_name() {
        if std::env::var("CI").is_ok() {
            return;
        }
        let result = resolve_group_gid("nonexistent_daemonize_test_group_xyz");
        assert!(matches!(result, Err(DaemonizeError::GroupNotFound(_))));
    }

    #[test]
    fn chown_ids_user_only() {
        // User only: owner is the user's UID, group is the user's primary GID.
        let id = ResolvedIdentity::resolve(Some("root"), None).unwrap();
        let (uid, gid) = id.chown_ids();
        assert_eq!(uid.unwrap().as_raw(), 0);
        assert_eq!(gid.unwrap().as_raw(), 0); // root's primary group
    }

    // Covers: R62
    #[test]
    fn chown_ids_neither_leaves_unchanged() {
        // Neither configured: both positions None (leave unchanged).
        let id = ResolvedIdentity::resolve(None, None).unwrap();
        assert_eq!(id.chown_ids(), (None, None));
    }

    // Covers: R61
    #[test]
    fn chown_ids_group_only_numeric() {
        // Group only: owner unchanged, group set to the numeric GID.
        let id = ResolvedIdentity::resolve(None, Some("0")).unwrap();
        let (uid, gid) = id.chown_ids();
        assert!(uid.is_none()); // no user change
        assert_eq!(gid.unwrap().as_raw(), 0);
    }

    #[test]
    fn effective_gid_prefers_explicit_group() {
        // Explicit group overrides the user's primary group.
        let id = ResolvedIdentity::resolve(Some("root"), Some("0")).unwrap();
        assert_eq!(id.effective_gid().unwrap().as_raw(), 0);
    }
}
