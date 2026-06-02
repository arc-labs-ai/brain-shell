//! `schema` management commands.
//!
//! Schema is a first-class additive expansion of the active vocabulary,
//! not a flag or mode. `schema upload` merges new declarations on top of
//! the system-default `brain:` namespace (plus any earlier user uploads).
//! `schema get` / `list` / `validate` are read/validate-side verbs that
//! drive the SCHEMA_GET / SCHEMA_LIST / SCHEMA_VALIDATE wire ops.

pub mod get;
pub mod list;
pub mod upload;
pub mod validate;

use brain_db_sdk::BrainError;

/// Read DSL source from a path, or stdin when the path is `-`.
pub(super) fn read_source(from_file: &str) -> Result<String, BrainError> {
    use std::io::Read as _;

    if from_file == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        return Ok(s);
    }
    Ok(std::fs::read_to_string(from_file)?)
}
