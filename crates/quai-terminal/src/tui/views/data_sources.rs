//! System › Data sources: what the wallet reads from where, and whether it answered.

use super::*;

// ---------------------------------------------------------------- System › Data sources

pub fn draw_data_sources(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let [list, status] = Layout::vertical([Constraint::Length(app::DATA_SOURCES.len() as u16 + 2), Constraint::Min(4)]).areas(area);
    let c = &app.config;
    let offline = wallet_core::http::offline();
    let (on, off) = (t.icon(Icon::On), t.icon(Icon::Off));
    let on_off = |b: bool| {
        if offline {
            format!("{off} off (--offline-data)")
        } else if b {
            format!("{on} on")
        } else {
            format!("{off} off")
        }
    };
    let rows: Vec<Row> = app::DATA_SOURCES
        .iter()
        .enumerate()
        .map(|(i, (id, label))| {
            let value = match *id {
                "explorer_lookups" => on_off(c.explorer_lookups),
                "market_data" => on_off(c.fetch_prices),
                "images" => on_off(c.images),
                "token_icons" => on_off(c.token_icons),
                _ => {
                    if app.eco.test.running {
                        format!("{} testing…", spinner())
                    } else {
                        t.icon(Icon::Disclosure).into()
                    }
                }
            };
            let row = Row::new(vec![Cell::from(*label), Cell::from(value_line(t, value))]);
            if i == app.nav.selected { row.style(t.selected()) } else { row }
        })
        .collect();
    let block = panel(t, "data sources", true);
    let inner = block.inner(list);
    f.render_widget(block, list);
    app.input.hits.borrow_mut().rows(app.main_list(), inner, 0, rows.len(), |i| app::DATA_SOURCES.get(i).map(|s| s.0.to_string()));
    f.render_widget(Table::new(rows, [Constraint::Length(46), Constraint::Min(16)]), inner);
    let network = app.net();
    let mut lines = Vec::new();
    if let Some(n) = &network {
        let ex = wallet_core::explorer::Explorer::for_network(n);
        lines.push(kv(t, "network", Span::raw(n.name.clone())));
        // Node traffic first: it is the part every setting on this screen leaves alone.
        let host = |url: &str| wallet_core::http::host_of(url).unwrap_or_else(|_| url.to_string());
        lines.push(kv(
            t,
            "reads",
            match &n.monitor {
                Some(m) => Span::styled(
                    format!(
                        "{} · your node: every read, reviews too · {} stands in when it does not answer · reviews warn at {}+ blocks behind",
                        host(&m.rpc_url),
                        host(&n.rpc_url),
                        wallet_core::network::MONITOR_LAG_WARN
                    ),
                    Style::default().fg(t.ok),
                ),
                None => Span::raw(format!(
                    "{} · balances, quotes and reviews; it sees the addresses asked about · System › Settings sets your own node",
                    host(&n.rpc_url)
                )),
            },
        ));
        lines.push(kv(t, "sends", Span::raw(format!("{} · every transaction is broadcast here", host(&n.rpc_url)))));
        lines.push(kv(
            t,
            "backend",
            Span::raw(match ex.backend {
                wallet_core::explorer::Backend::Quai => format!("explorer.qu.ai API · {}", ex.base),
                wallet_core::explorer::Backend::Blockscout => format!("Blockscout · {}", ex.base),
                wallet_core::explorer::Backend::ChainOnly => "chain only (no explorer)".into(),
            }),
        ));
        lines.push(kv(
            t,
            "swaps",
            Span::raw(
                n.ecosystem
                    .quainance_router
                    .as_ref()
                    .map(|r| format!("Quainance {}", short_address(&r.address)))
                    .unwrap_or_else(|| "—".into()),
            ),
        ));
        lines.push(kv(t, "marketplace", Span::raw(n.ecosystem.bazarr_indexer.clone().unwrap_or_else(|| "—".into()))));
        for content in [wallet_core::ipfs::Content::Abi, wallet_core::ipfs::Content::Media] {
            let gateway = wallet_core::ipfs::gateway(content);
            let label = match content {
                wallet_core::ipfs::Content::Abi => "IPFS · ABIs",
                wallet_core::ipfs::Content::Media => "IPFS · images",
            };
            lines.push(kv(
                t,
                label,
                if gateway.is_local() {
                    Span::styled(format!("{} · your node, reached directly", gateway.display()), Style::default().fg(t.ok))
                } else if gateway.is_default_for(content) {
                    Span::styled(format!("{} · Quai's gateway, the default for {}", gateway.display(), content.label()), t.dim_style())
                } else {
                    Span::raw(format!(
                        "{} · {}",
                        gateway.display(),
                        if wallet_core::http::proxy().is_some() { "through the proxy" } else { "direct" }
                    ))
                },
            ));
        }
        lines.push(kv(
            t,
            "proxy",
            match wallet_core::http::proxy() {
                Some(p) => Span::styled(
                    format!("{p} · lookups and node RPC; a node on this machine or your network is reached directly"),
                    Style::default().fg(t.ok),
                ),
                None => Span::styled("none · `config set proxy socks5h://127.0.0.1:9050` for Tor", t.dim_style()),
            },
        ));
    }
    lines.push(Line::from(Span::styled(
        "Explorer lookups send your addresses (and IP) to the explorer. Market data and images do not include your addresses. None of these switches changes which node reads or sends.",
        t.dim_style(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("request budget · this process, and the server's shared window", t.strong_style())));
    let hosts = wallet_core::http::host_statuses();
    if hosts.is_empty() {
        lines.push(Line::from(Span::styled("  no requests yet", t.dim_style())));
    }
    for h in hosts {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<24}", h.host), t.text_style()),
            Span::styled(format!("{:>3} last min · {} total", h.last_minute, h.total), t.dim_style()),
            match h.server {
                Some((limit, remaining, reset)) => Span::styled(
                    format!("  · server {remaining}/{limit} left, resets {reset}s"),
                    if remaining * 5 < limit { Style::default().fg(t.attention) } else { t.dim_style() },
                ),
                None => Span::styled(format!("  · paced {}/min", h.per_minute), t.dim_style()),
            },
            Span::styled(
                if h.rate_limited > 0 { format!("  · {}× 429", h.rate_limited) } else { String::new() },
                Style::default().fg(t.attention),
            ),
            Span::styled(
                h.last_error.map(|(at, e)| format!("   last error {} · {}", ago(at), truncate(&e, 40))).unwrap_or_default(),
                Style::default().fg(t.danger),
            ),
        ]));
    }
    if let Some(results) = &app.eco.test.results {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("connection test", t.strong_style())));
        for (name, r, ms) in results {
            match r {
                Ok(d) => lines.push(Line::from(vec![
                    Span::styled(format!("  {} ", t.icon(Icon::Ok)), Style::default().fg(t.ok)),
                    Span::raw(format!("{name:<24} {d} · {ms} ms")),
                ])),
                Err(e) => lines.push(Line::from(vec![
                    Span::styled(format!("  {} ", t.icon(Icon::Danger)), Style::default().fg(t.danger)),
                    Span::raw(format!("{name:<24} {}", truncate(e, 60))),
                ])),
            }
        }
    }
    let _ = Screen::DataSources;
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "status", false)), status);
}
