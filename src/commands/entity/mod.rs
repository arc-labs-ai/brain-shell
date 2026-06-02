//! `entity` read commands: `list`, `show`, `neighbors`.
//!
//! Each maps the SDK's flat `EntityView` / `RelationView` / `StatementView`
//! wire shapes onto the vendored render cards. The wire views carry raw ids
//! and no canonical-name backfill, so name columns fall back to the id where
//! the server gives us nothing better.

pub mod list;
pub mod neighbors;
pub mod resolve;
pub mod show;
