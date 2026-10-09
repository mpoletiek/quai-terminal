//! The boot card: what the wallet is doing while it starts, each line a real step that turns `✓`
//! as the app's own state reaches it. It is drawn only while those steps are still running (until
//! the first dashboard lands), never waits for anything, and goes at any key, after eight seconds,
//! at Motion Off and in plain mode. It never draws over the lock screen or a modal.

use super::*;

/// The longest the card stays, whatever is still running.
pub const BOOT_MAX: std::time::Duration = std::time::Duration::from_secs(8);

/// Whether the card is up this frame.
pub(crate) fn showing(app: &App) -> bool {
    app.fx.boot.is_some_and(|at| at.elapsed() < BOOT_MAX)
        && app.dash.refreshed_at == 0
        && app.motion() != Motion::Off
        && !app.term.plain
        && !app.lock.locked
        && matches!(app.modal, app::Modal::None)
}

/// One step: done (`Some(true)`), failed (`Some(false)`) or running (`None`), and what it says.
fn steps(app: &App) -> Vec<(&'static str, Option<bool>, String)> {
    use super::super::terminal::Tier;
    let caps = &app.term.caps;
    let tier = match caps.tier {
        Tier::Pixels => "pixels",
        Tier::Cells => "cells",
        Tier::Text => "text",
    };
    let terminal = format!(
        "{} · {tier} · {}",
        if caps.terminal.is_empty() { "terminal" } else { caps.terminal.as_str() },
        if caps.truecolor { "truecolor" } else { "256 colours" }
    );
    let wallet = match &app.meta {
        Some(m) if m.kind == wallet_core::registry::WalletKind::Watch => (Some(true), format!("{} · watch-only", m.name)),
        Some(m) => (Some(true), m.name.clone()),
        None => (None, "none yet".into()),
    };
    let engine = match &app.worker {
        Some(quai_engine::client::Engine::Local(_)) => (Some(true), "in this window".to_string()),
        Some(quai_engine::client::Engine::Remote(_)) => (Some(true), "the daemon".to_string()),
        None => (None, "starting".into()),
    };
    let d = &app.dash;
    let node = match (&d.health, &d.node_error) {
        (_, Some(e)) => (Some(false), truncate(&app::friendly_error(e), 40)),
        (Some(h), None) => {
            (Some(true), format!("{} · #{} · {} ms", d.network_name, amount::group_thousands(&h.height.to_string()), h.latency_ms))
        }
        (None, None) => (None, format!("{} · connecting", if d.network_name.is_empty() { &app.network_id } else { &d.network_name })),
    };
    let identity = match &d.health {
        Some(h) if h.identity_ok => (Some(true), "chain id and genesis match".to_string()),
        Some(_) => (Some(false), "MISMATCH, do not transact".to_string()),
        None => (None, "waiting for the node".into()),
    };
    let accounts = match d.refreshed_at {
        0 => (None, "reading".to_string()),
        _ => (Some(true), amount::count(d.accounts.len(), "account")),
    };
    vec![
        ("terminal", Some(true), terminal),
        ("wallet", wallet.0, wallet.1),
        ("engine", engine.0, engine.1),
        ("node", node.0, node.1),
        ("identity", identity.0, identity.1),
        ("accounts", accounts.0, accounts.1),
    ]
}

/// The card, centred on `area`.
pub(crate) fn draw(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    if !showing(app) {
        return;
    }
    let steps = steps(app);
    let w = 56.min(area.width.saturating_sub(4));
    let h = (steps.len() as u16 + 4).min(area.height);
    let card =
        Rect { x: area.x + (area.width.saturating_sub(w)) / 2, y: area.y + (area.height.saturating_sub(h)) / 3, width: w, height: h };
    let mut lines = vec![Line::from("")];
    for (name, state, text) in steps {
        let (mark, style) = match state {
            Some(true) => (t.icon(Icon::Ok).to_string(), Style::default().fg(t.ok)),
            Some(false) => (t.icon(Icon::Danger).to_string(), Style::default().fg(t.danger)),
            None => (spinner().to_string(), Style::default().fg(t.pending)),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {mark} "), style),
            Span::styled(format!("{name:<10}"), t.dim_style()),
            Span::styled(text, if state == Some(true) { t.text_style() } else { t.dim_style() }),
        ]));
    }
    f.render_widget(Clear, card);
    let block = panel(t, &format!("{}starting", t.lead(Icon::Wallet)), true).style(Style::default().bg(t.raised));
    f.render_widget(Paragraph::new(lines).block(block), card);
}
