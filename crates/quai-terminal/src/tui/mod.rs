//! Terminal UI: a synchronous render/input loop on the main thread, with all wallet I/O
//! on a background worker thread (see `worker`).

pub mod app;
pub mod data;
pub mod eco;
pub mod edge;
pub mod fx;
pub mod glossary;
pub mod images;
pub mod onboarding;
pub mod order_ui;
pub mod palette;
pub mod terminal;
pub mod theme;
pub mod themes;
pub mod ui;
pub mod views;
pub mod worker;

use crate::commands::Ctx;
use app::App;
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wallet_core::{CoreError, Result};
use worker::{Cmd, Ev, Worker};

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Leave raw mode / alternate screen (safe to call when not in the TUI).
pub fn restore_terminal() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let mut out = std::io::stdout();
        let _ = execute!(out, event::DisableBracketedPaste, event::DisableFocusChange, LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn theme_signature(path: &Option<std::path::PathBuf>) -> Option<(std::path::PathBuf, std::time::SystemTime)> {
    let p = path.as_ref()?;
    let real = std::fs::canonicalize(p).ok()?;
    let modified = std::fs::metadata(&real).and_then(|m| m.modified()).ok()?;
    Some((real, modified))
}

pub async fn run(ctx: Ctx) -> Result<()> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return Err(CoreError::Invalid("the TUI needs a terminal; use subcommands for scripting".into()));
    }
    let startup = Instant::now();
    let wallets = ctx.registry.list()?;
    let meta = if wallets.is_empty() {
        None
    } else {
        Some(ctx.registry.resolve(ctx.global.wallet.as_deref(), ctx.config.default_wallet.as_deref())?)
    };
    let network_id = ctx.global.network.clone().unwrap_or_else(|| ctx.config.default_network.clone());
    ctx.config.network(&network_id)?;
    let caps = terminal::detect(ctx.config.graphics);
    // Alerts, chats and notifications carry on after this window closes: start the daemon that
    // watches every wallet, unless it runs already or the user turned this off.
    // (`QUAI_TERMINAL_NO_DAEMON` keeps test harnesses from leaving one behind on a scratch copy.)
    if ctx.config.daemon_autostart
        && meta.is_some()
        && ctx.global.network.is_none()
        && std::env::var_os("QUAI_TERMINAL_NO_DAEMON").is_none()
        && let Err(e) = crate::daemon::ensure_current(&ctx.paths, 20)
    {
        wallet_core::diag::mark(&format!("daemon.autostart_failed {e}"));
    }

    enable_raw_mode().map_err(|e| CoreError::Invalid(format!("terminal: {e}")))?;
    ACTIVE.store(true, Ordering::SeqCst);
    let _guard = Guard;
    wallet_core::diag::timing("startup.registry+caps", startup);
    let mut input = terminal::Input::probe_background(Duration::from_millis(150));
    let light = input.light_background();
    wallet_core::diag::timing("startup.probe_background", startup);
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, event::EnableBracketedPaste, event::EnableFocusChange)
        .map_err(|e| CoreError::Invalid(format!("terminal: {e}")))?;
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout())).map_err(|e| CoreError::Invalid(format!("terminal: {e}")))?;
    term.clear().ok();
    wallet_core::diag::timing("startup.terminal", startup);

    let no_color = ctx.global.no_color;
    let config = ctx.config.clone();
    // A session-only theme override. Kept outside `config` so saving settings never persists it.
    let theme_override = std::env::var("QUAI_TERMINAL_THEME")
        .or_else(|_| std::env::var("QUAI_WALLET_THEME"))
        .ok()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    let (theme, mut theme_file) =
        theme::resolve(ctx.paths.root(), theme_override.as_deref().unwrap_or(&config.theme), light.unwrap_or(false), no_color);
    wallet_core::diag::timing("startup.theme", startup);
    let mut theme_sig = theme_signature(&theme_file);
    let mut omarchy_sig = theme_signature(&theme::omarchy_colors());
    let mut caps = caps;
    caps.light_background = light;
    let mut app = App::new(ctx.paths.clone(), network_id.clone(), config, theme, caps, meta);
    wallet_core::diag::timing("startup.app_new", startup);
    app.light_hint = light.unwrap_or(false);
    app.no_color = no_color;
    app.theme_override = theme_override;
    // Block digits are glyph noise for screen readers and the Linux console. This is a
    // session fallback only: it must never be written back into the saved preferences.
    app.plain = no_color || std::env::var("TERM").is_ok_and(|t| t == "linux");
    app.start_onboarding();
    wallet_core::diag::timing("startup.app", startup);
    let mut first_frame = true;
    let mut input_started: Option<Instant> = None;
    let mut qr_cache: Option<(String, u64, std::sync::Arc<Vec<u8>>)> = None;
    let mut last_theme_check = Instant::now();
    let mut last_tick = Instant::now();
    let mut last_size = term.size().map(|s| (s.width, s.height)).unwrap_or((0, 0));
    // A resize destroys the terminal's placements, and the pictures behind them may go with
    // them. Re-sending costs a screenful, so it waits until the size stops changing rather than
    // paying it for every step of a drag.
    let mut resized_at: Option<Instant> = None;
    const RESEND_AFTER_RESIZE: Duration = Duration::from_millis(180);

    let result = loop {
        // Start the worker once a wallet exists (immediately, or after onboarding).
        if app.worker.is_none()
            && let Some(meta) = app.meta.clone()
        {
            match Worker::spawn(app.registry.clone(), app.config.clone(), meta, app.network_id.clone(), || {}) {
                Ok(w) => {
                    app.worker = Some(w);
                    app.start_data_worker();
                    if let Some(p) = app.pending_unlock.take() {
                        // A wallet just created unlocks itself: the screen says so meanwhile.
                        app.begin_unlock(p);
                    }
                    app.send(Cmd::Refresh { full: false });
                }
                Err(e) => break Err(CoreError::Invalid(format!("worker: {e}"))),
            }
        }

        app.poll_creation();
        app.poll_unlock();
        app.poll_monitor_check();
        app.poll_ipfs_check();

        // Drain worker events.
        let size = term.size().map(|s| (s.width, s.height)).unwrap_or((80, 24));
        let mut events = Vec::new();
        if let Some(w) = &app.worker {
            while let Ok(ev) = w.rx.try_recv() {
                events.push(ev);
            }
        }
        for ev in events {
            if let Ev::Notify { title, body } = &ev
                && app.config.notifications
            {
                // Terminal notification between frames (no cursor movement), desktop as fallback.
                if !crate::notify::terminal(title, body) {
                    crate::notify::desktop(title, body);
                }
            }
            app.on_event(ev, size);
        }

        let mut data_events = Vec::new();
        if let Some(d) = &app.data {
            while let Ok(ev) = d.rx.try_recv() {
                data_events.push(ev);
            }
        }
        for ev in data_events {
            app.on_data_event(ev);
        }
        app.tick_eco();
        if !app.locked
            && app.onboarding.is_none()
            && app.config.layout_seen < wallet_core::config::LAYOUT_VERSION
            && app.meta.is_some()
            && matches!(app.modal, app::Modal::None)
            && app.dash.refreshed_at > 0
        {
            // Explain the new layout once after upgrading.
            if app.config.onboarded || app.config.layout_seen > 0 || !app.dash.ops.is_empty() || !app.dash.activity.is_empty() {
                app.modal = app::Modal::Help;
                app.help_moved = true;
            }
            app.config.layout_seen = wallet_core::config::LAYOUT_VERSION;
            app.save_config();
        }

        // Live theme reload (Omarchy theme switch or settings change).
        if app.pending_theme_reload || last_theme_check.elapsed() >= Duration::from_secs(2) {
            last_theme_check = Instant::now();
            let sig = theme_signature(&theme_file);
            // The system theme is watched even when the wallet uses a built-in palette: the
            // terminal re-themes with it, and a kitty that reloaded has dropped its images.
            let system = theme_signature(&theme::omarchy_colors());
            let system_changed = system != omarchy_sig;
            omarchy_sig = system;
            if app.pending_theme_reload || sig != theme_sig || system_changed {
                let setting = app.theme_override.clone().unwrap_or_else(|| app.config.theme.clone());
                let (t, file) = theme::resolve(app.paths.root(), &setting, light.unwrap_or(false), no_color);
                app.theme = t;
                theme_file = file;
                theme_sig = theme_signature(&theme_file);
                app.pending_theme_reload = false;
                app.dirty = true;
                // Clearing the screen deletes kitty image placements, and a terminal that
                // re-themed with the system has dropped the image data as well: send both again.
                app.kitty.forget_images(app.caps.tmux);
                term.clear().ok();
            }
        }

        if last_tick.elapsed() >= Duration::from_millis(500) {
            last_tick = Instant::now();
            app.tick(size);
            if app.busy.is_some() || app.unlocking || !app.toasts.is_empty() {
                app.dirty = true;
            }
            if matches!(app.screen, app::Screen::Swap | app::Screen::Home) || app.eco.testing {
                app.dirty = true;
            }
        }

        // Ambient clock: advances while focused and something animates; a new step is a redraw.
        let now = Instant::now();
        let step = app.eco.anim_step.get().filter(|_| app.focused);
        if let (Some(last), Some(_)) = (app.eco.anim_last, step) {
            app.eco.anim_ms += now.saturating_duration_since(last).as_millis() as u64;
        }
        app.eco.anim_last = Some(now);
        if let Some(step) = step
            && app.eco.anim_ms / step != app.eco.anim_drawn.get()
        {
            app.dirty = true;
        }
        if let Some(at) = resized_at
            && at.elapsed() >= RESEND_AFTER_RESIZE
        {
            resized_at = None;
            app.kitty.forget_images(app.caps.tmux);
            app.dirty = true;
        }
        let animating = ui::wants_animation(&app);
        // A size change ratatui notices on its own (no resize event) clears the screen, which
        // deletes kitty placements: reset ours before drawing.
        if let Ok(now_size) = term.size() {
            let now_size = (now_size.width, now_size.height);
            if now_size != last_size {
                last_size = now_size;
                app.kitty.clear(app.caps.tmux);
                resized_at = Some(Instant::now());
                app.dirty = true;
            }
        }
        if app.dirty || animating {
            let mut drawn = (0u16, 0u16);
            let res = term.draw(|f| {
                drawn = (f.area().width, f.area().height);
                ui::draw(f, &mut app)
            });
            if std::mem::take(&mut first_frame) {
                wallet_core::diag::timing("startup.first_frame", startup);
            }
            if let Err(e) = res {
                break Err(CoreError::Invalid(format!("draw: {e}")));
            }
            if wallet_core::diag::enabled() {
                wallet_core::diag::count(&format!("frame.screen.{:?}", app.screen), app.selected as u64);
                if app.screen == app::Screen::Markets
                    && let Some(pool) = app.selected_pool()
                {
                    wallet_core::diag::count(&format!("frame.pair.{}", pool.address.to_ascii_lowercase()), 1);
                }
                if let Some(at) = input_started.take() {
                    wallet_core::diag::timing("input.presented", at);
                }
            }
            // A resize that lands after the poll above is noticed by ratatui inside `draw`,
            // which clears the screen and so deletes every kitty placement. The size it drew
            // with is the one that tells us that happened, and our record has to be dropped
            // before this frame's pictures are placed.
            if drawn != last_size {
                last_size = drawn;
                app.kitty.clear(app.caps.tmux);
                resized_at = Some(Instant::now());
            }
            app.last_frame = Instant::now();
            app.dirty = false;
            if let Some(text) = app.clipboard.take() {
                // OSC 52: the terminal puts the text on the system clipboard (works over SSH/tmux too).
                use base64::Engine;
                use std::io::Write;
                let seq = format!("\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(text.as_bytes()));
                let seq = if app.caps.tmux { format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b")) } else { seq };
                let mut out = std::io::stdout();
                let _ = out.write_all(seq.as_bytes());
                let _ = out.flush();
            }
            if std::mem::take(&mut app.bell) {
                use std::io::Write;
                let mut out = std::io::stdout();
                let _ = out.write_all(if app.caps.tmux { b"\x1bPtmux;\x07\x1b\\" } else { b"\x07" });
                let _ = out.flush();
            }
            // Kitty bitmap placement after the cell frame is flushed.
            let mut placements = images::kitty_items(&app);
            // The QR bitmap is encoded once per address, not every frame.
            if let Some((_, data)) = &app.qr_rect
                && app.caps.tier == terminal::Tier::Pixels
                && qr_cache.as_ref().is_none_or(|(d, _, _)| d != data)
                && let Some(png) = terminal::qr_png(data, 8)
            {
                qr_cache = Some((data.clone(), images::png_key(&png), std::sync::Arc::new(png)));
            }
            if let (Some((rect, data)), Some((cached, key, png))) = (app.qr_rect.clone(), qr_cache.as_ref())
                && app.caps.tier == terminal::Tier::Pixels
                && *cached == data
            {
                placements.push(terminal::Placement {
                    png: png.clone(),
                    key: *key,
                    x: rect.x,
                    y: rect.y,
                    cols: rect.width,
                    rows: rect.height,
                    z: 0,
                });
            }
            if images::bitmaps(&app) && !placements.is_empty() {
                app.kitty.place_all(&placements, app.caps.tmux);
            } else {
                app.kitty.clear(app.caps.tmux);
            }
            app.flush_image_wants();
        }

        if app.quit {
            break Ok(());
        }

        let mut timeout = if animating { Duration::from_millis(33) } else { Duration::from_millis(100) };
        if let Some(step) = app.eco.anim_step.get().filter(|_| app.focused) {
            timeout = timeout.min(Duration::from_millis(step - app.eco.anim_ms % step).max(Duration::from_millis(15)));
        }
        match input.poll(timeout) {
            Ok(true) => match input.read() {
                Ok(Event::Key(key)) => {
                    if key.kind != crossterm::event::KeyEventKind::Release && wallet_core::diag::enabled() {
                        input_started.get_or_insert_with(Instant::now);
                    }
                    let ctrl_c =
                        key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) && key.code == crossterm::event::KeyCode::Char('c');
                    if app.onboarding.is_some() {
                        if ctrl_c {
                            app.quit = true;
                        } else if key.kind != crossterm::event::KeyEventKind::Release {
                            onboarding::on_key(&mut app, key);
                        }
                    } else {
                        app.on_key(key, size);
                    }
                }
                Ok(Event::Mouse(m)) => app.on_mouse(m),
                Ok(Event::Resize(..)) => {
                    app.kitty.clear(app.caps.tmux);
                    resized_at = Some(Instant::now());
                    app.dirty = true;
                    if app.locked && app.motion().effects() {
                        app.ambient = None;
                    }
                }
                Ok(Event::Paste(text)) => app.on_paste(&text),
                // Animations keep playing in the background; just repaint on return.
                Ok(Event::FocusGained) => {
                    app.focused = true;
                    app.dirty = true;
                    // The terminal may have redrawn or resized while away; place bitmaps afresh.
                    app.kitty.clear(app.caps.tmux);
                }
                // Ambient light freezes; ceremonies keep playing.
                Ok(Event::FocusLost) => app.focused = false,
                Err(e) => break Err(CoreError::Invalid(format!("input: {e}"))),
            },
            Ok(false) => {}
            Err(e) => break Err(CoreError::Invalid(format!("input: {e}"))),
        }
    };

    app.kitty.free_all(app.caps.tmux);
    if let Some(w) = &app.worker {
        w.send(Cmd::Shutdown);
    }
    app.send_data(data::DataCmd::Shutdown);
    drop(term);
    restore_terminal();
    if let Some(state) = crate::daemon::state(&ctx.paths) {
        let unlocked = state.wallets.iter().filter(|w| w.2).count();
        println!(
            "Quai Terminal keeps watching your {} wallet(s) in the background{} · to stop: quai-terminal daemon stop",
            state.wallets.len(),
            if unlocked > 0 { format!(" ({unlocked} unlocked, for sealed chats and private payments)") } else { String::new() }
        );
    }
    result
}

/// TUI tests that read or change the process-wide IPFS gateway take this, so they do not race.
#[cfg(test)]
pub(crate) static IPFS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
