//! REPL connect-banner card.
//!
//! A single Render impl that matches the card style used by the encode
//! renderer: framed top + bottom, dot brand icon + version + server on
//! the title line, an identity block below, footer hint.
//!
//! The identity shown is what the server assigned from the connection's
//! API-key credential: the space id and tenant namespace. The local
//! credential profile (if one was used) is annotated underneath.

use std::io::{self, Write};

use serde_json::{json, Value};

use crate::render::render::fmt_uuid;
use crate::render::theme::Token;
use crate::render::{Render, RenderCtx};

/// Welcome banner rendered once at REPL start. All values are owned
/// strings so the shell can build the struct before any I/O happens
/// and pass it to `dispatch` like any other render payload.
pub struct WelcomeBanner {
    /// Display name like `"brain-shell"` or `"brain"`. Shown right
    /// after the brand icon.
    pub product_name: String,
    /// Cargo package version string.
    pub version: String,
    /// Server address the SDK is bound to, formatted as `host:port`.
    pub server_addr: String,
    /// Server-assigned space UUID bytes — rendered canonical dashed form.
    pub space_id: [u8; 16],
    /// Server-derived tenant namespace. Empty == the reserved `brain`
    /// system namespace (no tenant).
    pub namespace: String,
    /// Local credential profile the connection authenticated through, if
    /// any. `None` for the bare `--token` / `BRAIN_TOKEN` path.
    pub profile: Option<String>,
}

impl Render for WelcomeBanner {
    fn render_table(&self, ctx: &RenderCtx, w: &mut dyn Write) -> io::Result<()> {
        let policy = ctx.policy;
        let theme = &ctx.theme;
        let lbl = |s: &str| theme.paint(Token::Label, s, policy).into_owned();
        let val = |s: &str| theme.paint(Token::Value, s, policy).into_owned();
        let muted = |s: &str| theme.paint(Token::Muted, s, policy).into_owned();
        let accent = |s: &str| theme.paint(Token::Accent, s, policy).into_owned();

        // ── Top rule ──────────────────────────────────────────────
        let width = policy.width.clamp(40, 80);
        let rule: String = "─".repeat(width);
        writeln!(w, "{}", muted(&rule))?;

        // ── Title line ────────────────────────────────────────────
        // ◉ brand icon + product name + version + · + connection.
        let icon = accent("◉");
        let product = val(&self.product_name);
        let ver = muted(&format!("v{}", self.version));
        let sep = muted("·");
        let conn_label = muted("connected to");
        let server = val(&self.server_addr);
        writeln!(w, "  {icon} {product}  {ver}  {sep}  {conn_label} {server}")?;
        writeln!(w)?;

        // ── Identity block ───────────────────────────────────────
        // The server assigns the space id + namespace from the API key;
        // we reflect them. The local credential profile is annotated
        // underneath the namespace.
        let space_label = lbl(&format!("{:<10}", "space"));
        let uuid = val(&fmt_uuid(&self.space_id));
        writeln!(w, "  {space_label}  {uuid}")?;

        let blank_label: String = " ".repeat(10);
        let ns_label = lbl(&format!("{:<10}", "namespace"));
        let ns_value = if self.namespace.is_empty() {
            muted("— (system)")
        } else {
            val(&self.namespace)
        };
        writeln!(w, "  {ns_label}  {ns_value}")?;

        let profile_text = match &self.profile {
            Some(name) => format!("profile: {name}"),
            None => "credential: --token".to_string(),
        };
        writeln!(w, "  {blank_label}  {}", muted(&profile_text))?;

        writeln!(w)?;

        // ── Footer hint ──────────────────────────────────────────
        let hint = muted("Type `help` for commands, `quit` to exit.");
        writeln!(w, "  {hint}")?;

        // ── Bottom rule ──────────────────────────────────────────
        writeln!(w, "{}", muted(&rule))?;
        Ok(())
    }

    fn render_json(&self, _ctx: &RenderCtx) -> Value {
        json!({
            "product_name": self.product_name,
            "version": self.version,
            "server_addr": self.server_addr,
            "space": {
                "id": fmt_uuid(&self.space_id),
                "namespace": self.namespace,
                "profile": self.profile,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::format::OutputFormat;
    use crate::render::theme::Theme;
    use crate::render::TermPolicy;

    fn render(banner: &WelcomeBanner, format: OutputFormat) -> String {
        let ctx = RenderCtx {
            policy: TermPolicy::plain(),
            theme: Theme::default(),
            format,
        };
        let mut buf = Vec::new();
        crate::render::dispatch(banner, &ctx, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn sample() -> WelcomeBanner {
        WelcomeBanner {
            product_name: "brain-shell".into(),
            version: "1.0.0".into(),
            server_addr: "127.0.0.1:9090".into(),
            space_id: [
                0x01, 0x9e, 0x43, 0x3e, 0x92, 0x72, 0x7e, 0x70, 0xb0, 0x71, 0x4a, 0x4f, 0xd6, 0x13,
                0x5d, 0x1e,
            ],
            namespace: "acme".into(),
            profile: Some("work".into()),
        }
    }

    #[test]
    fn render_table_includes_brand_icon_product_and_server() {
        let out = render(&sample(), OutputFormat::Table);
        assert!(out.contains("◉"), "missing brand icon: {out}");
        assert!(out.contains("brain-shell"), "missing product name: {out}");
        assert!(out.contains("v1.0.0"), "missing version: {out}");
        assert!(
            out.contains("connected to 127.0.0.1:9090"),
            "missing server: {out}"
        );
    }

    #[test]
    fn render_table_shows_full_uuid_namespace_and_profile() {
        let out = render(&sample(), OutputFormat::Table);
        assert!(
            out.contains("019e433e-9272-7e70-b071-4a4fd6135d1e"),
            "missing canonical UUID: {out}"
        );
        assert!(out.contains("acme"), "missing namespace: {out}");
        assert!(out.contains("profile: work"), "missing profile: {out}");
    }

    #[test]
    fn render_table_system_namespace_and_token_path() {
        let mut banner = sample();
        banner.namespace = String::new();
        banner.profile = None;
        let out = render(&banner, OutputFormat::Table);
        assert!(out.contains("— (system)"), "missing system marker: {out}");
        assert!(
            out.contains("credential: --token"),
            "missing token-path annotation: {out}"
        );
    }

    #[test]
    fn render_table_has_top_and_bottom_rules() {
        let out = render(&sample(), OutputFormat::Table);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines.first().unwrap_or(&"").contains("─"),
            "missing top rule"
        );
        assert!(
            lines.last().unwrap_or(&"").contains("─"),
            "missing bottom rule"
        );
    }

    #[test]
    fn render_table_includes_help_hint() {
        let out = render(&sample(), OutputFormat::Table);
        assert!(
            out.contains("Type `help` for commands"),
            "missing help hint: {out}"
        );
    }

    #[test]
    fn render_json_envelope_shape() {
        let out = render(&sample(), OutputFormat::Json);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["product_name"], "brain-shell");
        assert_eq!(v["space"]["namespace"], "acme");
        assert_eq!(v["space"]["profile"], "work");
    }
}
