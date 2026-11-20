use crate::error::CoreError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// User role governing workspace and library permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Contributor,
    Viewer,
}

impl Role {
    /// Returns true if the role grants full server and library administration rights.
    pub const fn is_admin(self) -> bool {
        matches!(self, Self::Admin)
    }

    /// Returns true if the role permits contributing content (uploads, albums, tags, overrides).
    pub const fn can_contribute(self) -> bool {
        matches!(self, Self::Admin | Self::Contributor)
    }

    /// Returns the canonical lowercase string representation stored in SQLite and JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Contributor => "contributor",
            Self::Viewer => "viewer",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "admin" => Ok(Self::Admin),
            "contributor" => Ok(Self::Contributor),
            "viewer" => Ok(Self::Viewer),
            other => Err(CoreError::InvalidRole(other.to_string())),
        }
    }
}

/// Resolved folder visibility scope for a request or database query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RbacScope {
    /// Admins implicitly access every watched folder.
    AdminAll,
    /// Contributors and Viewers access only their explicitly granted folder IDs.
    RestrictedFolders(Vec<String>),
}

impl RbacScope {
    /// Builds an `RbacScope` from a user's `Role` and ACL-granted folder IDs.
    pub fn from_role_and_folders(role: Role, allowed_folder_ids: Vec<String>) -> Self {
        if role.is_admin() {
            Self::AdminAll
        } else {
            Self::RestrictedFolders(allowed_folder_ids)
        }
    }

    /// Returns true if the given folder ID is visible within this scope.
    pub fn allows_folder(&self, folder_id: &str) -> bool {
        match self {
            Self::AdminAll => true,
            Self::RestrictedFolders(ids) => ids.iter().any(|id| id == folder_id),
        }
    }

    /// Returns `None` for `AdminAll` or `Some(&[String])` for `RestrictedFolders`.
    pub fn allowed_folder_ids(&self) -> Option<&[String]> {
        match self {
            Self::AdminAll => None,
            Self::RestrictedFolders(ids) => Some(ids.as_slice()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_capabilities_and_parsing() {
        assert!(Role::Admin.is_admin());
        assert!(Role::Admin.can_contribute());
        assert_eq!(Role::Admin.as_str(), "admin");

        assert!(!Role::Contributor.is_admin());
        assert!(Role::Contributor.can_contribute());
        assert_eq!(Role::Contributor.as_str(), "contributor");

        assert!(!Role::Viewer.is_admin());
        assert!(!Role::Viewer.can_contribute());
        assert_eq!(Role::Viewer.as_str(), "viewer");

        assert_eq!("admin".parse::<Role>().unwrap(), Role::Admin);
        assert_eq!("CONTRIBUTOR".parse::<Role>().unwrap(), Role::Contributor);
        assert_eq!("viewer".parse::<Role>().unwrap(), Role::Viewer);
        assert!("guest".parse::<Role>().is_err());
    }

    #[test]
    fn rbac_scope_checks_folder_visibility() {
        let admin_scope = RbacScope::from_role_and_folders(Role::Admin, vec![]);
        assert_eq!(admin_scope, RbacScope::AdminAll);
        assert!(admin_scope.allows_folder("f-1"));
        assert!(admin_scope.allowed_folder_ids().is_none());

        let viewer_scope = RbacScope::from_role_and_folders(Role::Viewer, vec!["f-1".to_string()]);
        assert!(viewer_scope.allows_folder("f-1"));
        assert!(!viewer_scope.allows_folder("f-2"));
        assert_eq!(
            viewer_scope.allowed_folder_ids(),
            Some(&["f-1".to_string()][..])
        );

        let empty_viewer = RbacScope::from_role_and_folders(Role::Viewer, vec![]);
        assert!(!empty_viewer.allows_folder("f-1"));
    }
}
