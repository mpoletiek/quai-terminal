//! Terminal UI: a synchronous render/input loop on the main thread, with all wallet I/O
//! on background threads (see `worker`, `data`). The terminal itself is owned by `term`.

pub mod app;
pub mod browser;
pub mod clipboard;
pub mod data;
pub mod eco;
pub mod edge;
pub mod fx;
pub mod glossary;
pub mod hit;
pub mod icons;
pub mod images;
pub mod keymap;
pub mod links;
pub mod num;
pub mod onboarding;
pub mod order_ui;
pub mod palette;
pub mod persist;
pub mod placeholders;
pub mod pointer;
pub mod screen;
pub mod term;
pub mod terminal;
pub mod theme;
pub mod themes;
pub mod ui;
pub mod views;
pub mod widgets;
pub mod worker;

use crate::commands::Ctx;
use app::App;
use crossterm::event::Event;
use std::time::{Duration, Instant};
use wallet_core::{CoreError, Result};
use worker::{Cmd, Ev};

/// Give the terminal back as it was found (safe to call when not in the TUI, and from a panic).
pub fn restore_terminal() {
    term::modes::restore();
}

/// Whether the terminal puts a worker's notice on the desktop itself: with notifications on,
/// only while the window is in the background (in front, the toast says it), and not when the
/// notice is also in the wallet's list and a daemon is running, which forwards it once.
fn desktop_notice(enabled: bool, focused: bool, listed: bool, daemon_running: bool) -> bool {
    enabled && !focused && !(listed && daemon_running)
}

/// How long the loop may sleep: until the next thing that needs a frame (an animation step, the
/// half-second tick, a resize settling), capped so a missed wake is never noticed for long.
fn next_wait(app: &App, animating: bool, last_tick: Instant, resized_at: Option<Instant>) -> Duration {
    const IDLE: Duration = Duration::from_millis(500);
    let mut wait = IDLE.saturating_sub(last_tick.elapsed()).max(Duration::from_millis(1));
    if animating {
        wait = wait.min(Duration::from_millis(33));
    }
    if ui::spinning(app) {
        wait = wait.min(ui::until_next_spinner_step());
    }
    if let Some(beat) = app.fx.beat
        && beat.elapsed() < ui::BEAT_PULSE
    {
        wait = wait.min(ui::BEAT_PULSE - beat.elapsed());
    }
    if let Some(step) = app.eco.anim.step.get().filter(|_| app.term.focused) {
        wait = wait.min(Duration::from_millis(step - app.eco.anim.ms % step).max(Duration::from_millis(15)));
    }
    if let Some(at) = resized_at {
        wait = wait.min(RESEND_AFTER_RESIZE.saturating_sub(at.elapsed()).max(Duration::from_millis(1)));
    }
    wait
}

/// A resize destroys the terminal's placements, and the pictures behind them may go with them.
/// Re-sending costs a screenful, so it waits until the size stops changing rather than paying it
/// for every step of a drag.
const RESEND_AFTER_RESIZE: Duration = Duration::from_millis(180);

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
    // The engine runs in the daemon, which holds the keys; this process never does. Standalone
    // (asked for, a test harness, or the daemon off and not running) keeps it here.
    let standalone = ctx.global.standalone
        || std::env::var_os("QUAI_TERMINAL_NO_DAEMON").is_some()
        || (!ctx.config.daemon_autostart && !crate::daemon::daemon_running(&ctx.paths));
    if standalone
        && ctx.config.daemon_autostart
        && meta.is_some()
        && ctx.global.network.is_none()
        && std::env::var_os("QUAI_TERMINAL_NO_DAEMON").is_none()
    {
        crate::daemon::ensure_current_soon(ctx.paths.clone(), 20);
    }

    wallet_core::diag::timing("startup.registry+caps", startup);
    let mut term = term::Term::start(caps.truecolor, ctx.global.no_color).map_err(|e| CoreError::Invalid(format!("terminal: {e}")))?;
    let light = term.answers.light();
    wallet_core::diag::timing("startup.probe_background", startup);
    if wallet_core::diag::enabled() {
        wallet_core::diag::mark(&format!("startup.probe answered={} took_us={}", term.answers.answered, term.answers.took.as_micros()));
    }
    term::signals::listen(term::wake);
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
    // What the terminal said beats what the environment suggested.
    caps.truecolor |= term.answers.truecolor;
    caps.kitty_keyboard = term.answers.kitty_keyboard;
    // A multiplexer draws its own screen and passes none of this through.
    caps.text_sizing = term.answers.text_sizing && !caps.tmux;
    caps.ansi8 = term.answers.ansi8;
    // Pointer shapes (OSC 22) where sequences it doesn't know are ignored, outside a multiplexer.
    term.shapes = caps.hyperlinks && !caps.tmux;
    let mut theme = theme;
    theme.fit_to_terminal(term.answers.background, caps.ansi8, caps.truecolor);
    let mut app = App::new(ctx.paths.clone(), network_id.clone(), config, theme, caps, meta);
    // Large pictures go to the terminal as files it reads and deletes, when it is on this machine.
    app.term.kitty.file_dir = terminal::picture_dir(app.term.caps.ssh);
    wallet_core::diag::timing("startup.app_new", startup);
    app.term.light_hint = light.unwrap_or(false);
    app.term.no_color = no_color;
    app.term.theme_override = theme_override;
    // Block digits are glyph noise for screen readers and the Linux console. This is a
    // session fallback only: it must never be written back into the saved preferences.
    app.term.plain = ctx.global.plain || no_color || std::env::var("TERM").is_ok_and(|t| t == "linux");
    fx::probe_fonts();
    icons::probe();
    app.start_onboarding();
    wallet_core::diag::timing("startup.app", startup);
    let mut first_frame = true;
    // What the window's title and taskbar last showed.
    let mut shown_title = String::new();
    let mut shown_progress = 0u8;
    let mut input_started: Option<Instant> = None;
    let mut qr_cache: Option<(String, u64, std::sync::Arc<Vec<u8>>)> = None;
    let mut last_theme_check = Instant::now();
    // The spinner glyph and the minute last drawn (see the tick and the redraw rules below).
    let mut spinner_drawn = 0u128;
    let mut ages_minute = 0u64;
    let mut last_tick = Instant::now();
    let mut last_size = term.ui.size().map(|s| (s.width, s.height)).unwrap_or((0, 0));
    let mut resized_at: Option<Instant> = None;

    // The engine's lanes and data worker wake this loop when they have news.
    quai_engine::set_waker(term::wake);
    let result = loop {
        // Start the worker once a wallet exists (immediately, or after onboarding).
        if app.worker.is_none()
            && let Some(meta) = app.meta.clone()
        {
            match start_engine(&app, meta, standalone) {
                Ok(w) => {
                    app.worker = Some(w);
                    app.start_data_worker();
                    if let Some(p) = app.lock.pending.take() {
                        // A wallet just created unlocks itself: the screen says so meanwhile.
                        app.begin_unlock(p);
                    }
                    app.send(Cmd::Refresh { full: false });
                }
                Err(e) => break Err(CoreError::Invalid(format!("worker: {e}"))),
            }
        }

        app.poll_creation();
        app.poll_copy();
        app.poll_persist();
        // Pictures fitted and encoded off this thread: placed on the next frame.
        if images::poll_fitted(&app) {
            app.dirty = true;
        }
        app.poll_monitor_check();
        app.poll_ipfs_check();

        // Stopped from outside (SIGTERM, SIGHUP): leave as a quit does.
        if term::signals::stop_requested() {
            break Ok(());
        }
        // Continued after an outside stop: the screen may be anything now.
        if term::signals::take_continued() {
            let _ = term.repaint();
            app.term.kitty.forget_images(app.term.caps.tmux);
            app.dirty = true;
        }

        // Drain worker events.
        let size = term.ui.size().map(|s| (s.width, s.height)).unwrap_or((80, 24));
        let mut events = Vec::new();
        if let Some(w) = &app.worker {
            while let Some(ev) = w.try_recv() {
                events.push(ev);
            }
        }
        // Notices the UI raised itself go the same way as the worker's.
        for (title, body) in std::mem::take(&mut app.status.notices_out) {
            if app.config.notifications && !app.term.focused {
                match crate::notify::detect_terminal() {
                    Some(backend) => term.queue(crate::notify::sequence(backend, &title, &body).as_bytes()),
                    None => crate::notify::desktop(&title, &body),
                }
            }
        }
        // A running daemon forwards the wallet's notification list to the desktop, so a notice
        // that is also in the list goes there once, through it. Asked only when there is one.
        let daemon = events.iter().any(|ev| matches!(ev, Ev::Notify { listed: true, .. })) && crate::daemon::daemon_running(&app.paths);
        for ev in events {
            // A notice for the desktop only while the window is in the background: in front, the
            // toast on screen says it, and a second copy in the corner of the desktop is noise.
            if let Ev::Notify { title, body, listed } = &ev
                && desktop_notice(app.config.notifications, app.term.focused, *listed, daemon)
            {
                match crate::notify::detect_terminal() {
                    Some(backend) => term.queue(crate::notify::sequence(backend, title, body).as_bytes()),
                    None => crate::notify::desktop(title, body),
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
        if !app.lock.locked
            && app.onboarding.is_none()
            && app.config.layout_seen < wallet_core::config::LAYOUT_VERSION
            && app.meta.is_some()
            && matches!(app.modal, app::Modal::None)
            && app.dash.refreshed_at > 0
        {
            // Explain the new layout once after upgrading.
            if app.config.onboarded || app.config.layout_seen > 0 || !app.dash.ops.is_empty() || !app.dash.activity.is_empty() {
                app.modal = app::Modal::Help;
                app.nav.help_moved = true;
            }
            app.config.layout_seen = wallet_core::config::LAYOUT_VERSION;
            app.save_config();
        }

        // Live theme reload (Omarchy theme switch or settings change).
        if app.term.pending_theme_reload || last_theme_check.elapsed() >= Duration::from_secs(2) {
            last_theme_check = Instant::now();
            let sig = theme_signature(&theme_file);
            // The system theme is watched even when the wallet uses a built-in palette: the
            // terminal re-themes with it, and a kitty that reloaded has dropped its images.
            let system = theme_signature(&theme::omarchy_colors());
            let system_changed = system != omarchy_sig;
            omarchy_sig = system;
            if app.term.pending_theme_reload || sig != theme_sig || system_changed {
                let setting = app.term.theme_override.clone().unwrap_or_else(|| app.config.theme.clone());
                let (mut t, file) = theme::resolve(app.paths.root(), &setting, light.unwrap_or(false), no_color);
                t.fit_to_terminal(term.answers.background, app.term.caps.ansi8, app.term.caps.truecolor);
                app.theme = t;
                theme_file = file;
                theme_sig = theme_signature(&theme_file);
                app.term.pending_theme_reload = false;
                app.dirty = true;
                // Clearing the screen deletes kitty image placements, and a terminal that
                // re-themed with the system has dropped the image data as well: send both again.
                app.term.kitty.forget_images(app.term.caps.tmux);
                term.ui.clear().ok();
            }
        }

        if last_tick.elapsed() >= Duration::from_millis(500) {
            last_tick = Instant::now();
            app.tick(size);
            if app.lock.unlocking {
                app.dirty = true;
            }
            // Ages on screen redraw when they can have changed: Home's are in minutes, a quote's
            // freshness on Swap in seconds.
            let minute = wallet_core::registry::now() / 60;
            let ages_moved = match app.nav.screen {
                app::Screen::Home => std::mem::replace(&mut ages_minute, minute) != minute,
                app::Screen::Exchange if app.nav.card == app::Card::Swap => app.eco.swap.quote_read.settled(),
                _ => false,
            };
            if app.term.focused && ages_moved || app.eco.test.running {
                app.dirty = true;
            }
        }

        // Ambient clock: advances while focused and something animates; a new step is a redraw.
        let now = Instant::now();
        let step = app.eco.anim.step.get().filter(|_| app.term.focused);
        if let (Some(last), Some(_)) = (app.eco.anim.last, step) {
            app.eco.anim.ms += now.saturating_duration_since(last).as_millis() as u64;
        }
        app.eco.anim.last = Some(now);
        // A new step of the edge light alone is a decoration frame, not a redraw.
        let edge_due = step.is_some_and(|step| app.eco.anim.ms / step != app.eco.anim.drawn.get());
        // A spinner is content, redrawn when its glyph changes (every 80 ms), not at the frame
        // rate; the block heartbeat's pulse is two frames, on and off.
        if ui::spinning(&app) && ui::spinner_step() != spinner_drawn {
            app.dirty = true;
        }
        if let Some(beat) = app.fx.beat
            && beat.elapsed() >= ui::BEAT_PULSE
            && app.last_frame < beat + ui::BEAT_PULSE
        {
            app.dirty = true;
        }
        if let Some(at) = resized_at
            && at.elapsed() >= RESEND_AFTER_RESIZE
        {
            resized_at = None;
            app.term.kitty.forget_images(app.term.caps.tmux);
            app.dirty = true;
        }
        let animating = ui::wants_animation(&app);
        // A size change ratatui notices on its own (no resize event) clears the screen, which
        // deletes kitty placements: reset ours before drawing.
        if let Ok(now_size) = term.ui.size() {
            let now_size = (now_size.width, now_size.height);
            if now_size != last_size {
                last_size = now_size;
                app.term.kitty.clear(app.term.caps.tmux);
                resized_at = Some(Instant::now());
                app.dirty = true;
            }
        }
        // Whether the terminal's own background shows through the page: a change repaints
        // everything, since cells already sent carry the old background.
        app.term.background = term.answers.background;
        let see_through = app.see_through();
        if term.ui.backend().see_through != see_through {
            term.ui.backend_mut().see_through = see_through;
            app.term.kitty.forget_images(app.term.caps.tmux);
            term.ui.clear().ok();
            app.dirty = true;
        }
        if app.dirty || animating || edge_due {
            let edges_only = !app.dirty && !animating;
            let mut drawn = (0u16, 0u16);
            let frame_started = Instant::now();
            let mut render_us = 0u64;
            term.ui.backend_mut().begin();
            let res = term.ui.draw(|f| {
                drawn = (f.area().width, f.area().height);
                let t0 = Instant::now();
                if edges_only {
                    ui::draw_edges(f, &mut app);
                } else {
                    ui::draw(f, &mut app);
                }
                render_us = t0.elapsed().as_micros() as u64;
            });
            if std::mem::take(&mut first_frame) {
                wallet_core::diag::timing("startup.first_frame", startup);
            }
            if let Err(e) = res {
                break Err(CoreError::Invalid(format!("draw: {e}")));
            }
            // Headline text drawn larger than a cell goes over the cells that hold its place. A
            // decoration frame keeps the last full frame's.
            term.ui.backend_mut().big_text(&app.term.big_text.borrow());
            // The hidden cursor waits at the focus, for magnifiers and screen readers.
            if let Some((x, y)) = app.input.focus_at {
                term.queue(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
            }
            if wallet_core::diag::enabled() {
                // Render (our drawing) and the frame through ratatui's diff, in microseconds, per
                // screen: the budget is render ≤ 2 ms and frame ≤ 4 ms at p99.
                let place = match (app.lock.locked, edges_only) {
                    (true, _) => "Lock".to_string(),
                    (false, true) => "Edges".to_string(),
                    (false, false) => format!("{:?}", app.nav.screen),
                };
                wallet_core::diag::count(&format!("perf.render_us.{place}"), render_us);
                wallet_core::diag::count(&format!("perf.frame_us.{place}"), frame_started.elapsed().as_micros() as u64);
                wallet_core::diag::count(&format!("frame.screen.{:?}", app.nav.screen), app.nav.selected as u64);
                if app.nav.screen == app::Screen::Markets
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
                app.term.kitty.clear(app.term.caps.tmux);
                resized_at = Some(Instant::now());
            }
            app.last_frame = Instant::now();
            spinner_drawn = ui::spinner_step();
            app.dirty = false;
            if let Some(req) = app.tasks.clipboard.take() {
                // The desktop's own tool here, read back; OSC 52 over SSH (see `clipboard`).
                let (seq, rx) = clipboard::start(req, app.term.caps.tmux);
                if let Some(seq) = seq {
                    term.queue(seq.as_bytes());
                }
                app.tasks.copying = Some(rx);
            }
            if std::mem::take(&mut app.fx.bell) {
                term.queue(if app.term.caps.tmux { b"\x1bPtmux;\x07\x1b\\" } else { b"\x07" });
            }
            // Kitty bitmap placement after the cell frame is flushed. A decoration frame moved no
            // content, so the pictures placed last frame stay exactly where they are.
            if !edges_only {
                let mut placements = images::kitty_items(&app);
                // The QR bitmap is encoded once per address, not every frame.
                if let Some((_, data)) = &app.term.qr_rect
                    && app.term.caps.tier == terminal::Tier::Pixels
                    && qr_cache.as_ref().is_none_or(|(d, _, _)| d != data)
                    && let Some(png) = terminal::qr_png(data, 8)
                {
                    qr_cache = Some((data.clone(), images::png_key(&png), std::sync::Arc::new(png)));
                }
                if let (Some((rect, data)), Some((cached, key, png))) = (app.term.qr_rect.clone(), qr_cache.as_ref())
                    && app.term.caps.tier == terminal::Tier::Pixels
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
                    app.term.kitty.place_all(&placements, app.term.caps.tmux);
                } else {
                    app.term.kitty.clear(app.term.caps.tmux);
                }
                app.flush_image_wants();
                // Pictures go out with the cells they sit among, in the same synchronized frame.
            }
            let images = app.term.kitty.take_output();
            term.queue(&images);
            if let Err(e) = term.ui.backend_mut().end() {
                break Err(CoreError::Invalid(format!("draw: {e}")));
            }
            // What the terminal had to take in for it (the budget at idle is ≤ 5 KB/s).
            wallet_core::diag::count(
                if edges_only { "perf.bytes.Edges" } else { "perf.bytes.Full" },
                term.ui.backend().last_frame_bytes as u64,
            );
        } else {
            // Placements cleared between frames (a detail closed, a screen left) still go out.
            let images = app.term.kitty.take_output();
            if !images.is_empty() {
                term.queue(&images);
            }
        }

        if app.quit {
            break Ok(());
        }

        // The mouse follows the setting (and the session's release) from one frame to the next.
        let _ = term.set_pointer(app.pointer_mode());
        term.set_shape(app.pointer_shape());
        // The window's title and taskbar say what the wallet is doing (see `window_title`).
        let title = app.window_title();
        if title != shown_title {
            term.queue(format!("\x1b]2;{title}\x1b\\").as_bytes());
            shown_title = title;
        }
        let progress = if app.term.caps.taskbar_progress { app.taskbar_state() } else { 0 };
        if progress != shown_progress {
            term.queue(format!("\x1b]9;4;{progress};0\x1b\\").as_bytes());
            shown_progress = progress;
        }
        let wait = next_wait(&app, animating, last_tick, resized_at);
        let events = match term.events(wait) {
            Ok(events) => events,
            Err(e) => break Err(CoreError::Invalid(format!("input: {e}"))),
        };
        for event in events {
            match event {
                Event::Key(key) => {
                    if key.kind != crossterm::event::KeyEventKind::Release && wallet_core::diag::enabled() {
                        input_started.get_or_insert_with(Instant::now);
                    }
                    let ctrl = key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
                    let ctrl_c = ctrl && key.code == crossterm::event::KeyCode::Char('c');
                    // Ctrl-Z, as in any shell program: the wallet locks first (a suspended
                    // wallet is out of sight, not out of reach), then gives the terminal back.
                    if ctrl && key.code == crossterm::event::KeyCode::Char('z') && key.kind == crossterm::event::KeyEventKind::Press {
                        app.suspend_lock();
                        app.term.kitty.clear(app.term.caps.tmux);
                        let images = app.term.kitty.take_output();
                        term.queue(&images);
                        if term.suspend().is_ok() {
                            app.term.kitty.forget_images(app.term.caps.tmux);
                        }
                        app.dirty = true;
                        continue;
                    }
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
                Event::Mouse(m) => app.on_mouse(m, size),
                Event::Resize(..) => {
                    app.term.kitty.clear(app.term.caps.tmux);
                    resized_at = Some(Instant::now());
                    app.dirty = true;
                    // The effect was built for the old canvas; the lock screen rests rather than
                    // replaying it every time a tiling window manager moves the window.
                    if app.lock.locked && app.motion().effects() {
                        app.fx.ambient = None;
                        app.lock.rested = true;
                    }
                }
                Event::Paste(text) => app.on_paste(&text),
                // Motion waits while the window is in the background; repaint on return. Kitty
                // keeps image placements across focus changes (a resize is what drops them, and
                // has its own path above), so they are left alone: clearing them here made every
                // icon blink on each alt-tab.
                Event::FocusGained => {
                    app.term.focused = true;
                    app.term.focus_gained_at = Some(Instant::now());
                    app.dirty = true;
                }
                Event::FocusLost => app.term.focused = false,
            }
        }
    };

    app.term.kitty.free_all(app.term.caps.tmux);
    let images = app.term.kitty.take_output();
    term.queue(&images);
    if shown_progress != 0 {
        term.queue(b"\x1b]9;4;0;0\x1b\\");
    }
    if let Some(w) = &app.worker {
        w.send(Cmd::Shutdown);
    }
    app.send_data(data::DataCmd::Shutdown);
    // Preferences saved in the last moments reach the disk before the process ends.
    app.flush_config();
    if let Some(dir) = &app.term.kitty.file_dir {
        terminal::clean_picture_files(dir);
    }
    drop(term);
    if let Some(line) = app.quit_receipt() {
        println!("{line}");
    }
    if let Some(state) = crate::daemon::state(&ctx.paths) {
        let unlocked = state.wallets.iter().filter(|w| w.2).count();
        println!(
            "Quai Terminal keeps watching your {} in the background{} · to stop: quai-terminal daemon stop",
            wallet_core::amount::count(state.wallets.len(), "wallet"),
            if unlocked > 0 { format!(" ({unlocked} unlocked, for sealed chats and private payments)") } else { String::new() }
        );
    }
    result
}

/// TUI tests that read or change the process-wide IPFS gateway take this, so they do not race.
#[cfg(test)]
pub(crate) static IPFS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// This terminal's engine: in the daemon, which holds the keys, or in this process when
/// standalone.
fn start_engine(app: &App, meta: wallet_core::registry::WalletMeta, standalone: bool) -> std::io::Result<quai_engine::client::Engine> {
    use quai_engine::client::{Dialer, Engine, Remote};
    if standalone {
        let host =
            quai_engine::host::Host::attach(app.registry.clone(), app.config.clone(), "", false, meta, app.network_id.clone(), term::wake)?;
        return Ok(Engine::Local(Box::new(host)));
    }
    let (paths, pid_paths) = (app.paths.clone(), app.paths.clone());
    let dialer = Dialer {
        socket: app.paths.engine_socket(),
        daemon_pid: Box::new(move || crate::daemon::state(&pid_paths).map(|s| s.pid)),
        ensure_daemon: Box::new(move || crate::daemon::ensure_current(&paths, 20).map(|_| ()).map_err(|e| e.to_string())),
    };
    Ok(Engine::Remote(Remote::connect(dialer, meta.id, app.network_id.clone(), term::wake)?))
}

#[cfg(test)]
mod notice_tests {
    /// One event, one desktop notice: the daemon's copy of a listed notice is the one shown.
    #[test]
    fn a_listed_notice_reaches_the_desktop_once() {
        use super::desktop_notice;
        assert!(desktop_notice(true, false, true, false), "no daemon: the terminal says it");
        assert!(!desktop_notice(true, false, true, true), "a daemon forwards the list: the terminal stays quiet");
        assert!(desktop_notice(true, false, false, true), "not in the list: only the terminal can say it");
        assert!(!desktop_notice(true, true, false, false), "in front, the toast is enough");
        assert!(!desktop_notice(false, false, false, false), "notifications off");
    }
}
