//! Terminal UI/UX layer for the Brain shell — rendering primitives and
//! per-verb renderers.
//!
//! Owns color, hyperlink, pager, and table conventions in one place so the
//! shell's output stays consistent across every verb. Vendored from the
//! server-side explore crate and repointed onto the SDK wire types.

#![forbid(unsafe_code)]
#![warn(clippy::all)]

pub mod format;
// The inner `render` submodule holds the per-verb card renderers, vendored as a
// unit; the name stutter is inherent to the vendored layout, not a smell.
#[allow(clippy::module_inception)]
pub mod render;
pub mod table;
pub mod term;
pub mod theme;
pub mod util;

pub use format::{dispatch, OutputFormat, Render, RenderCtx};
pub use render::{
    AdHocTable, AuditCard, AutoEdgeSummary, AutoEdgesDelta, ConnectionInfo, EncodeRendered,
    EntityCard, GraphNode, GraphTree, HelpFlagRow, HelpItem, HelpReference, HelpSection,
    HelpTopLevel, HelpUnknown, HelpVerb, InfoCard, LinkRendered, MemorySummary, ObjectRef,
    PlanSteps, ReasonSteps, RecallResults, RelationCard, RelationSummary, RenderableError,
    ServerInfo, ServerWelcomeFields, SessionInfo, SpaceInfo, StageKindLabel, StageOutcomeLabel,
    StageResult, StageResultsDelta, StatementCard, StatementSummary, SubscriptionEventList,
    SubscriptionEventRendered, TxnAbortRendered, TxnBeginRendered, TxnCommitRendered,
    UnlinkRendered, WelcomeBanner,
};
pub use term::policy::TermPolicy;
pub use theme::{Theme, Token};
