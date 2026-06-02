//! FORGET response renderer.

use std::io::{self, Write};

use brain_db_sdk::wire::types::ForgetResponse;
use serde_json::{json, Value};

use crate::render::render::fmt_id;
use crate::render::theme::Token;
use crate::render::{Render, RenderCtx};

impl Render for ForgetResponse {
    fn render_table(&self, ctx: &RenderCtx, w: &mut dyn Write) -> io::Result<()> {
        let ok = ctx.theme.paint(Token::Success, "ok", ctx.policy);
        writeln!(
            w,
            "{ok}  memory_id={}  was_already_forgotten={}  edges_removed={}",
            fmt_id(self.memory_id),
            self.was_already_forgotten,
            self.edges_removed,
        )
    }

    fn render_json(&self, _ctx: &RenderCtx) -> Value {
        json!({
            "memory_id": fmt_id(self.memory_id),
            "was_already_forgotten": self.was_already_forgotten,
            "edges_removed": self.edges_removed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::format::OutputFormat;
    use crate::render::theme::Theme;
    use crate::render::TermPolicy;

    fn ctx() -> RenderCtx {
        RenderCtx {
            policy: TermPolicy::plain(),
            theme: Theme::default(),
            format: OutputFormat::Table,
        }
    }

    #[test]
    fn table_shows_id_and_counters() {
        let r = ForgetResponse {
            memory_id: 0x1234,
            was_already_forgotten: false,
            edges_removed: 3,
        };
        let mut buf = Vec::new();
        r.render_table(&ctx(), &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("ok"));
        assert!(s.contains("edges_removed=3"));
        assert!(s.contains("was_already_forgotten=false"));
    }

    #[test]
    fn json_includes_all_fields() {
        let r = ForgetResponse {
            memory_id: 0xabcd,
            was_already_forgotten: true,
            edges_removed: 0,
        };
        let v = r.render_json(&ctx());
        assert_eq!(v["was_already_forgotten"], true);
        assert_eq!(v["edges_removed"], 0);
    }
}
