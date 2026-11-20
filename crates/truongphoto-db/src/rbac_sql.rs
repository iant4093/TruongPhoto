use rusqlite::types::Value;
use truongphoto_core::rbac::RbacScope;

/// Builds a parameterized SQL predicate restricting `alias.folder_id` to `scope`.
///
/// Never interpolates folder IDs into SQL strings; pushes `Value::Text` entries into
/// `params` and emits 1-based positional `?N` placeholders aligned with `params`.
pub fn folder_clause(alias: &str, scope: &RbacScope, params: &mut Vec<Value>) -> String {
    folder_column_clause(alias, "folder_id", scope, params)
}

/// Builds a parameterized SQL predicate restricting `alias.column` to `scope`.
pub fn folder_column_clause(
    alias: &str,
    column: &str,
    scope: &RbacScope,
    params: &mut Vec<Value>,
) -> String {
    match scope {
        RbacScope::AdminAll => "1 = 1".to_string(),
        RbacScope::RestrictedFolders(ids) => {
            if ids.is_empty() {
                return "1 = 0".to_string();
            }

            let start = params.len() + 1;
            let mut placeholders = Vec::with_capacity(ids.len());
            for (offset, id) in ids.iter().enumerate() {
                params.push(Value::Text(id.clone()));
                placeholders.push(format!("?{}", start + offset));
            }
            format!("{alias}.{column} IN ({})", placeholders.join(", "))
        }
    }
}
