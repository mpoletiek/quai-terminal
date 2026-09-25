//! What happens to the app: worker events, locking and unlocking, onboarding, and the celebrations.

use super::*;
use wallet_core::journal::OpKind;

impl App {
    /// Ctrl-Z: a wallet suspended to the shell is locked first; it comes back to the lock screen.
    pub(crate) fn suspend_lock(&mut self) {
        self.lock_now(None);
    }

    /// Lock now: the screen locks immediately, and the worker drops the keys as soon as it
    /// reaches the command (it may still be finishing a network sync).
    pub(crate) fn lock_now(&mut self, size: Option<(u16, u16)>) {
        if self.can_sign() && !self.lock.locked {
            self.send(Cmd::Lock);
            self.enter_lock(size);
            // A wallet handed to the daemon is locked there too; off the UI thread, since the daemon
            // answers between polls.
            if let Some(wallet) = self.meta.as_ref().map(|m| m.id.clone())
                && crate::daemon::state(&self.paths).is_some_and(|d| d.unlocked(&wallet))
            {
                let paths = self.paths.clone();
                let _ = std::thread::Builder::new().name("daemon-lock".into()).spawn(move || {
                    let _ = crate::daemon::lock_wallet(&paths, &wallet);
                });
            }
        }
    }

    /// Forget every decrypted conversation and private draft, and make any read still in flight
    /// stale. Called at lock and whenever the wallet or network changes.
    pub(crate) fn forget_private(&mut self) {
        self.private_epoch += 1;
        let board = &mut self.eco.board;
        board.dms.clear();
        board.msg.clear();
        board.msg_lines.clear();
        board.msg_offered = false;
        if self.eco.board.pin.as_deref().is_some_and(|p| p.starts_with("dm:") || p.starts_with("msg:")) {
            self.dock.draft.clear();
        }
    }

    /// Switch the UI to the lock screen. Idempotent: the worker's `Locked` confirmation after a
    /// local lock changes nothing (and doesn't restart the animation).
    pub(crate) fn enter_lock(&mut self, size: Option<(u16, u16)>) {
        if self.lock.locked {
            return;
        }
        self.lock.locked = true;
        self.flow_on_lock();
        // A review open when the wallet locked goes with the keys; it is said after unlocking.
        if let Modal::Review(r) = &self.modal {
            self.lock.dropped_review = Some(r.review.title.clone());
        }
        // Keep a half-filled, non-secret form; everything else is dropped with the keys. A
        // private message is dropped too: it is exactly what a lock is meant to hide.
        if let Modal::Form(form) = std::mem::replace(&mut self.modal, Modal::None)
            && !form.fields.iter().any(|f| f.is_secret())
            && !form.kind.is_private()
        {
            let mut form = form;
            form.pending = false;
            self.parked = Some(form);
        }
        self.forget_private();
        self.dash.unlocked = false;
        self.dash.peers.clear();
        self.dash.offers.clear();
        self.lock.fade = None;
        self.lock.rested = false;
        self.lock.unlocked_at = None;
        self.lock.input.zeroize();
        // The lock screen offers the other wallets on this computer (ctrl-w).
        self.load_wallets();
        // Without a size the next tick starts the animation.
        if let Some(size) = size {
            self.start_lock_ceremony(size);
        }
    }

    /// Start (or loop) the lock screen animation on a canvas exactly the size of the art area.
    pub fn start_lock_ceremony(&mut self, size: (u16, u16)) {
        if self.effects_allowed() && self.lock.locked {
            let (w, h) = lock_art_size(size);
            let chosen = self.config.lock_effect.as_str();
            let effect = if chosen != "random" && super::super::fx::EFFECTS.iter().any(|(n, _)| *n == chosen) {
                chosen.to_string()
            } else {
                super::super::fx::random_lock_effect().to_string()
            };
            let args = super::super::fx::theme_args(&effect, &self.theme);
            // Public chain data only: the lock screen never shows wallet data.
            let text = match self.chain_weather() {
                Some(line) => format!("{}\n\n{line}", super::super::fx::wordmark_block()),
                None => super::super::fx::wordmark_block(),
            };
            // A safety cap only: every effect ends on its own well before it (the longest, swarm,
            // is about 2,000 frames). One cut short froze on whatever frame the cap landed on.
            self.fx.ambient = Ceremony::with_args(&effect, &args, &text, w, h, 2_400).map(|c| c.at_speed(super::super::fx::LOCK_SPEED));
        }
    }

    /// Lock-screen line from public chain data: height, head hash and local time (ASCII).
    pub fn chain_weather(&self) -> Option<String> {
        let h = self.dash.health.as_ref()?;
        let hash = h.head_hash.trim_start_matches("0x");
        let short = if hash.len() > 12 { format!("0x{}..{}", &hash[..6], &hash[hash.len() - 4..]) } else { format!("0x{hash}") };
        let kind = match h.order {
            Some(0) => "prime",
            Some(1) => "region",
            _ => "zone",
        };
        let time = chrono::Local::now().format("%H:%M");
        Some(format!("#{}  {short}  {kind}  {time}", wallet_core::amount::group_thousands(&h.height.to_string())))
    }

    /// Corner stamp for good news. Money values are shown in the toast, never animated.
    /// A moment worth marking. The corner stamps that played here drew over the value column and
    /// the swap quote, so they are gone; what remains is the bell, for those who asked for sound.
    /// The bell, for news that came from outside (money arrived, something sold), when the user
    /// asked for sound: only while the window is in the background or has sat untouched a
    /// minute, and at most once in ten seconds. Never for the user's own confirmations.
    pub(crate) fn ring(&mut self) {
        let away = !self.term.focused || self.input.last_input.elapsed().as_secs() >= 60;
        if self.config.sound && away && self.news.last_bell.is_none_or(|at| at.elapsed().as_secs() >= 10) {
            self.fx.bell = true;
            self.news.last_bell = Some(Instant::now());
        }
    }

    /// Value arrived: said in a toast naming the sender, marked on the rail until Activity is
    /// opened, and (in Full and Vivid) run in along the header, lit on the hero's gutter and
    /// swept across its new row.
    fn arrive(&mut self, arrivals: &[wallet_core::appdb::Activity]) {
        let now = Instant::now();
        let who = |app: &App, a: &wallet_core::appdb::Activity| {
            super::super::ui::activity_contact(app, a)
                .map(|name| format!("from {name}"))
                .unwrap_or_else(|| format!("· {}", wallet_core::session::short_address(&a.address)))
        };
        match arrivals {
            [] => return,
            [a] => {
                let text =
                    format!("Received {} {} {}", super::super::worker::incoming_amount(a), super::super::num::unit(&a.asset), who(self, a));
                self.toast(text, false);
            }
            many => self.toast(format!("Received {} payments · Activity", many.len()), false),
        }
        self.news.arrival_said = Some(now);
        if self.nav.screen != Screen::Activity {
            self.news.arrivals_unseen = true;
        }
        if self.motion().effects() {
            self.signal(super::super::edge::Signal::Arrival);
            self.fx.gutter_flash = Some(now);
            for a in arrivals {
                self.fx.row_flash.insert(a.key.clone(), now);
            }
        }
        self.ring();
    }

    /// Handle a worker event.
    pub fn on_event(&mut self, ev: Ev, size: (u16, u16)) {
        self.dirty = true;
        match ev {
            Ev::Head(height) => self.on_block(height),
            Ev::SplitQuote { key, result } => {
                if self.nav.screen != Screen::Swap || self.eco.requests.split != self.swap_input_key().map(|identity| (key, identity)) {
                    return;
                }
                self.eco.requests.split = None;
                match result {
                    Ok(result) => match result.plan {
                        Some(plan) => {
                            let Some(account) = self.dash.active_account().map(|a| a.address.clone()) else {
                                return;
                            };
                            let intent = wallet_core::execution::TradingIntent {
                                account,
                                max_fee: None,
                                action: wallet_core::execution::TradingAction::Split {
                                    plan: Box::new(plan),
                                    index: 0,
                                    deadline: self.eco.swap.deadline_minutes,
                                },
                            };
                            self.start_plan("split swap · separate allocations".into(), intent, None);
                        }
                        None => self.toast(result.reason, false),
                    },
                    Err(error) => self.toast(friendly_error(&error), true),
                }
            }
            Ev::Plan(view) => self.on_plan(*view),
            Ev::Pnl(result) => {
                self.eco.feeds.pnl.settle(result.map(|p| *p));
            }
            Ev::QiMax { key, result } => {
                if self.eco.requests.max != Some((key, self.max_identity())) {
                    return;
                }
                self.eco.requests.max = None;
                match result {
                    Ok(q) => {
                        if self.nav.screen == Screen::Convert {
                            self.eco.convert.amount = q.amount;
                            self.eco.convert.edited = Some(Instant::now());
                            self.eco.convert.quote = None;
                        } else if self.nav.screen == Screen::Wrap {
                            self.eco.wrap.amount = q.amount;
                        }
                        self.toast(
                            format!(
                                "amount quoted at one-qit resolution with {} qits fee; {} inputs excluded. Preparation refreshes fees",
                                q.fee_qits, q.excluded_inputs
                            ),
                            false,
                        );
                    }
                    Err(e) => self.toast(friendly_error(&e), true),
                }
            }
            // What a send destination turned out to be. A late answer for an address that has
            // since been edited is dropped rather than shown against the wrong one.
            Ev::Contract { address, found } => {
                if self.tasks.contract_probe.as_deref() == Some(address.as_str()) {
                    self.tasks.contract_probe = None;
                    self.tasks.contract_found = *found;
                    self.refresh_form_note();
                }
            }
            Ev::Dashboard(mut d) => {
                // Channels is chosen by payment code: keep the cursor on the same one when offers
                // arrive, leave or move, so a key never lands on a different sender.
                let anchor = (self.nav.screen == Screen::Channels)
                    .then(|| self.channel_offer().map(|o| o.code.clone()).or_else(|| self.channel_peer().map(|p| p.code.clone())))
                    .flatten();
                // The pending lane may have read the journal after this refresh did: keep its read.
                if d.ops_at < self.dash.ops_at
                    && d.network_id == self.dash.network_id
                    && d.meta.as_ref().map(|m| &m.id) == self.dash.meta.as_ref().map(|m| &m.id)
                {
                    d.ops = self.dash.ops.clone();
                    d.ops_at = self.dash.ops_at;
                }
                self.observe_changes(&d);
                self.dash = *d;
                if self.lock.locked {
                    // A refresh that finished after a local lock must not bring unlocked-only data back.
                    self.dash.unlocked = false;
                    self.dash.peers.clear();
                    self.dash.offers.clear();
                }
                if let Some(m) = &self.dash.meta {
                    self.meta = Some(m.clone());
                }
                // A wallet switch clears every cached view, but the screen the user is on was
                // never re-opened, so nothing asked for the new wallet's data — and at the moment
                // of the switch there were no accounts to ask about yet. The first dashboard that
                // brings them is when the open view can load, so it is re-opened here.
                if self.reload_view_on_accounts && !self.dash.accounts.is_empty() {
                    wallet_core::diag::end("ux.wallet_switch");
                    self.reload_view_on_accounts = false;
                    self.on_view_opened();
                }
                if let Some(code) = anchor
                    && let Some(i) =
                        self.dash.offers.iter().map(|o| &o.code).chain(self.dash.peers.iter().map(|p| &p.code)).position(|c| *c == code)
                {
                    self.nav.selected = i;
                }
                // Balances changed (a swap output, a claimed WQI, a transfer): rebuild the portfolio
                // on the views that show it, without waiting for the view to be reopened.
                if matches!(self.nav.screen, Screen::Home | Screen::Swap) || self.eco.feeds.portfolio.value().is_none() {
                    self.maybe_refresh_portfolio(false);
                }
                self.preload();
            }
            // Results the worker produced before it saw a pending lock are dropped with the keys.
            Ev::Orders { wallet, network, mut rows, announced } => {
                if self.meta.as_ref().is_some_and(|m| m.id == wallet) && self.dash.network_id == network {
                    // Active orders first; finished ones keep their order below them.
                    rows.sort_by_key(|p| !wallet_core::orders::details(p).is_ok_and(|v| v.state.active()));
                    if self.nav.screen == Screen::Orders {
                        self.nav.selected = self.nav.selected.min(rows.len().saturating_sub(1));
                    }
                    // A limit that became reachable since the last look is said on screen, whoever
                    // found it (this terminal, or the daemon between two of its checks).
                    let before = self.eco.feeds.orders.value().map(|rows| super::super::order_ui::reachable(rows)).unwrap_or_default();
                    let fresh: Vec<String> = super::super::order_ui::reachable(&rows)
                        .into_iter()
                        .filter(|(id, _)| !before.iter().any(|(b, _)| b == id))
                        .map(|(_, to)| to)
                        .collect();
                    if let Some(to) = fresh.first() {
                        self.toast_as(
                            format!("your limit on {to} is reachable · Trade › Orders, enter to review"),
                            Severity::Attention,
                            None,
                        );
                    }
                    // The desktop and the bell, only for what this terminal announced: an order the
                    // daemon found, it put on the desktop itself. With a daemon running, it also
                    // forwards this terminal's notification, so the terminal leaves the desktop to it.
                    if let Some(id) = announced.first()
                        && let Some(plan) = rows.iter().find(|p| &p.id == id)
                        && let Ok(value) = wallet_core::orders::details(plan)
                    {
                        if !crate::daemon::daemon_running(&self.paths) {
                            self.status.notices_out.push(wallet_core::orders::reachable_notice(&value));
                        }
                        self.ring();
                    }
                    self.eco.feeds.orders.settle(Ok(rows));
                }
            }
            Ev::OrderReview(r) => {
                if self.lock.locked || self.eco.plan.is_some() || matches!(self.modal, Modal::Review(_)) {
                    self.send(Cmd::Discard(r.op_id.clone()));
                    self.toast("order review discarded because the wallet locked or another review began", true);
                } else {
                    self.modal = Modal::Review(ReviewState {
                        review: *r,
                        scroll: 0,
                        content_lines: 1,
                        viewport: 1,
                        approve_focused: false,
                        opened: Instant::now(),
                        typed: String::new(),
                    });
                }
            }
            Ev::Review(r) if self.lock.locked => self.send(Cmd::Discard(r.op_id.clone())),
            Ev::Secret(_) | Ev::Quote(_) if self.lock.locked => {}
            Ev::Review(r) => {
                if !self.flow_on_review(&r.op_id) {
                    self.send(Cmd::Discard(r.op_id.clone()));
                    return;
                }
                wallet_core::diag::end("ux.review");
                self.modal = Modal::Review(ReviewState {
                    review: *r,
                    scroll: 0,
                    content_lines: 1,
                    viewport: 1,
                    approve_focused: false,
                    opened: Instant::now(),
                    typed: String::new(),
                });
            }
            Ev::Submitted(s) => {
                let kind = self.status.committing_kind.take();
                if let Some(kind) = &kind {
                    self.after_submit(kind);
                }
                // A step in a sequence continues on its own; only the last step shows the result.
                if !self.flow_on_submitted(&s.op_id, &kind.unwrap_or(OpKind::Other(String::new()))) && !self.lock.locked {
                    self.modal = Modal::Result(s);
                }
            }
            Ev::Quote(q) if self.nav.screen == Screen::Convert => {
                let card = &self.eco.convert;
                let direction = if card.qi_to_quai { "qi_to_quai" } else { "quai_to_qi" };
                let decimals = if card.qi_to_quai { wallet_core::amount::QI_DECIMALS } else { 18 };
                if q.direction != direction
                    || wallet_core::amount::parse_amount(&card.amount, decimals).ok().map(|v| v.to_string()).as_deref()
                        != Some(q.amount.as_str())
                {
                    return;
                }
                // The right tolerance depends on the size, and the card cannot know it before the
                // quote arrives: it starts unset, which ConversionSlippage rejects outright, and a
                // fixed 3% would be refunded at 250 QUAI and far more than needed at 50. Adopt the
                // quote's suggestion until the user picks their own.
                if !self.eco.convert.manual_slippage {
                    self.eco.convert.slippage_bps = q.suggested_slippage_bps;
                }
                self.eco.convert.quote = Some(*q);
            }
            Ev::Quote(q) => self.modal = Modal::Quote(q),
            Ev::Info(m) => self.toast(m, false),
            Ev::Ack(m) => {
                if matches!(&self.modal, Modal::Form(f) if f.pending) {
                    self.modal = Modal::None;
                }
                self.toast(m, false);
            }
            Ev::CommitError { op_id, message, ambiguous } => {
                // A plan's step: the engine says where the plan stands (`Ev::Plan`).
                self.status.committing_kind = None;
                self.send(Cmd::Journal);
                // Where the money is comes first. A toast is too small for this, and gone too soon.
                let activity = Screen::Activity.place();
                let body = if ambiguous {
                    vec![
                        "We can't confirm whether this was sent.".to_string(),
                        format!("Check {activity} before trying again: sending twice could pay twice."),
                        String::new(),
                        friendly_error(&message),
                    ]
                } else {
                    vec!["Nothing was sent.".to_string(), String::new(), friendly_error(&message)]
                };
                let title = if ambiguous { "Unconfirmed" } else { "Not sent" };
                self.modal = Modal::Notice { title: title.into(), body, error: true, detail: Some(op_id) };
            }
            Ev::PrepareError(m) => {
                // The prepare answered, if only with a refusal: that is the wait being measured.
                wallet_core::diag::end("ux.review");
                // Said with where the money is: preparing only reads and builds.
                let text = format!("{} · nothing was sent", friendly_error(&m).trim_end_matches('.'));
                self.on_event(Ev::Error(text), size);
            }
            Ev::Error(m) => {
                self.flow_on_error();
                let text = friendly_error(&m);
                if let Modal::Form(form) = &mut self.modal
                    && form.pending
                {
                    form.pending = false;
                    form.error_field = error_field(form, &text);
                    if let Some(i) = form.error_field {
                        form.focus = i;
                    }
                    form.error = Some(text);
                } else if self.lock.locked {
                    // Background work failing while locked (the node, a sync). A refused password
                    // is not reported here: the lock screen checks passwords itself.
                    self.status.log.push_front(Toast { text, level: Severity::Danger, at: Instant::now(), id: None });
                } else {
                    self.toast(text, true);
                }
            }
            Ev::Busy(b) => {
                if b != self.status.busy {
                    self.status.busy_at = b.as_ref().map(|_| Instant::now());
                }
                self.status.busy = b;
            }
            Ev::SignBusy(b) => {
                if b != self.status.signing {
                    self.status.signing_at = b.as_ref().map(|_| Instant::now());
                }
                self.status.signing = b;
            }
            // A watch-only wallet opened by a switch: nothing to unlock.
            Ev::Unlocked => self.unlocked_by_engine(),
            Ev::UnlockFailed(e) => self.unlock_failed(e),
            Ev::EngineLost(why) => {
                self.lock.unlocking = false;
                self.lock.unlocking_since = None;
                self.lock.handoff_pending = None;
                self.enter_lock(Some(size));
                // Short enough for the lock box; the reason goes in the log.
                self.lock.error = Some("The engine stopped; the keys went with it.".into());
                self.toast(format!("engine stopped ({why}) · reconnecting"), true);
            }
            Ev::EngineBack => {
                if self.lock.locked {
                    self.lock.error = Some("Reconnected. Unlock to sign again.".into());
                }
                self.toast("reconnected to the engine".to_string(), false);
            }
            Ev::Locked => {
                // The confirmation of a switch this screen already locked for. If that wallet was
                // unlocked while the worker was still getting there, it stays unlocked.
                if !(std::mem::take(&mut self.lock.switch_pending) && !self.lock.locked) {
                    self.enter_lock(Some(size));
                }
            }
            Ev::KeysRefused => {
                self.lock.switch_pending = false;
                self.enter_lock(Some(size));
                self.lock.error = Some("that wallet did not open — unlock it again".into());
            }
            Ev::Secret(text) => {
                self.modal = Modal::Secret { text, title: "Anyone with these words controls your funds".into() };
            }
            // Asked before a lock or a switch: whatever it says is no longer this screen's to show.
            Ev::Conversation { epoch, .. } if self.lock.locked || epoch != self.private_epoch => {}
            Ev::Messaging { epoch, .. } if self.lock.locked || epoch != self.private_epoch => {}
            Ev::Messaging { view, open, note, .. } => {
                use wallet_core::messaging::service::KeyNeed;
                let board = &mut self.eco.board;
                // This week's key is offered once per unlock, where the user will see it.
                let offer = matches!(&view, Ok(v) if v.status.need == KeyNeed::Publish) && !board.msg_offered;
                if offer {
                    board.msg_offered = true;
                }
                // A failed read keeps what was already shown and says so; the first one shows the error.
                let refused = match &view {
                    Err(e) if board.msg.value().is_some() => Some(e.clone()),
                    _ => None,
                };
                board.msg.settle(view);
                if let Some(e) = refused {
                    self.toast(format!("private messages: {}", super::friendly_error(&e)), true);
                }
                if let Some((peer, lines)) = open
                    && (lines.is_ok() || !matches!(self.eco.board.msg_lines.get(&peer), Some(Ok(_))))
                {
                    self.eco.board.msg_lines.insert(peer, lines);
                }
                if let Some(note) = note {
                    self.toast(note, false);
                }
                if offer {
                    self.toast("this week's messaging key is not published yet: Board › K", false);
                }
            }
            Ev::Conversation { peer, result, .. } => {
                // A failed read keeps the conversation on screen (`Resource::shown`).
                self.eco.board.dms.settle(peer, result);
            }
            // An arrival already said by name (`arrive`) is not said again in the worker's words.
            Ev::Notify { title, .. }
                if title.starts_with("Incoming payment") && self.news.arrival_said.is_some_and(|at| at.elapsed().as_secs() < 60) => {}
            Ev::Notify { title, body, .. } => self.toast(format!("{title}: {body}"), false),
            Ev::Chat { subs, pin, note } => {
                self.eco.board.subs = subs;
                self.eco.board.pin = pin;
                if let Some(note) = note {
                    self.toast(note, false);
                }
            }
            // A sent transaction was mined: show it now, through the same path as a refresh so a
            // confirmation celebrates and a sequence moves on to its next step.
            Ev::Ops { wallet, network, ops, at } => {
                if at > self.dash.ops_at && network == self.dash.network_id && self.dash.meta.as_ref().is_some_and(|m| m.id == wallet) {
                    let mut next = self.dash.clone();
                    next.ops = ops;
                    next.ops_at = at;
                    self.on_event(Ev::Dashboard(Box::new(next)), size);
                }
            }
            // News for a wallet since locked or switched away is not this screen's to say.
            Ev::ChatNews { epoch, .. } if self.lock.locked || epoch != self.private_epoch => {}
            Ev::ChatNews { news, .. } => {
                self.eco.board.news.confirm();
                // A chat already on screen (open on the Board, or pinned) is being read; the rest also
                // goes to the desktop. Notice titles are the chat's label (`#general`, `Alice · sealed`).
                let open = (self.nav.screen == Screen::Board).then(|| self.board_row().map(|r| App::chat_target(&r).0)).flatten();
                let on_screen: Vec<String> = open.iter().chain(self.eco.board.pin.iter()).map(|t| self.chat_label(t)).collect();
                for (title, body) in news {
                    let visible = on_screen.iter().any(|l| title == *l || title.starts_with(&format!("{l} ·")));
                    if self.config.notifications && !visible {
                        crate::notify::desktop(&title, &body);
                    }
                    self.toast(format!("{title} · {body}"), false);
                }
            }
        }
    }

    /// Heartbeat on new blocks; celebrate confirmations and new receipts (not on first load).
    pub(crate) fn observe_changes(&mut self, next: &Dashboard) {
        let old_height = self.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
        if let Some(h) = &next.health
            && h.height > old_height
        {
            if old_height > 0 {
                self.fx.beat = Some(Instant::now());
                self.fx.beat_order = h.order.unwrap_or(2);
            }
            // The session's entropy minimum: the smallest head hash seen (all the same length).
            if self.fx.lowest_hash.as_ref().is_none_or(|(low, _)| h.head_hash.to_lowercase() < low.to_lowercase()) {
                self.fx.lowest_hash = Some((h.head_hash.clone(), h.height));
            }
            if self.fx.recent_hashes.back() != Some(&h.head_hash) {
                self.fx.recent_hashes.push_back(h.head_hash.clone());
                while self.fx.recent_hashes.len() > 24 {
                    self.fx.recent_hashes.pop_front();
                }
            }
        }
        if self.dash.refreshed_at == 0 || self.dash.network_id != next.network_id {
            return;
        }
        let now = Instant::now();
        self.fx.row_flash.retain(|_, s| s.elapsed().as_millis() < super::super::edge::FLASH_MS);
        self.fx.drawer_flash.retain(|_, s| s.elapsed().as_millis() < super::super::edge::FLASH_MS);
        if self.motion().effects() {
            // Confirmations: a row lights once when it reaches the target.
            let new_height = next.health.as_ref().map(|h| h.height).unwrap_or(old_height);
            for op in &next.ops {
                let before = super::super::ui::confirmations(op, old_height).map(|(n, _)| n).unwrap_or(0);
                let after = super::super::ui::confirmations(op, new_height).map(|(n, _)| n).unwrap_or(0);
                if old_height > 0 && before < super::super::ui::CONFIRM_TARGET && after >= super::super::ui::CONFIRM_TARGET {
                    self.fx.row_flash.insert(op.id.clone(), now);
                    self.signal(super::super::edge::Signal::Settled);
                }
            }
            // Cash drawer: each newly arrived Qi coin lights its denomination slot, and so does
            // a coin whose lock just opened (it became money you can spend).
            if let (Some(old), Some(new)) = (&self.dash.qi, &next.qi) {
                let (was, is) = (wallet_core::sdk::U256::from(old_height), wallet_core::sdk::U256::from(new_height));
                for coin in &new.coins {
                    let arrived = !old.coins.iter().any(|o| o.outpoint == coin.outpoint);
                    let opened = old_height > 0 && coin.unlock_height > was && coin.unlock_height <= is;
                    if arrived || opened {
                        self.fx.drawer_flash.insert(coin.denomination, now);
                    }
                }
            }
        }
        // The static half of the drawer's news, at every motion level: which slots got a coin
        // (arrived or unlocked) since Qi was last on screen.
        if let (Some(old), Some(new)) = (&self.dash.qi, &next.qi)
            && self.nav.screen != Screen::Qi
        {
            let new_height = next.health.as_ref().map(|h| h.height).unwrap_or(old_height);
            let (was, is) = (wallet_core::sdk::U256::from(old_height), wallet_core::sdk::U256::from(new_height));
            for coin in &new.coins {
                let arrived = !old.coins.iter().any(|o| o.outpoint == coin.outpoint);
                let opened = old_height > 0 && coin.unlock_height > was && coin.unlock_height <= is;
                if arrived || opened {
                    self.fx.drawer_new.insert(coin.denomination);
                }
            }
        }
        // A time lock that opened: said until Accounts or Qi is looked at (every motion level).
        for l in next.locks.iter().filter(|l| l.unlocked) {
            let was_locked =
                self.dash.locks.iter().any(|o| !o.unlocked && o.source == l.source && o.amount == l.amount && o.asset == l.asset);
            if was_locked {
                self.news.unlocked_news.push(format!("{} {} unlocked · spendable now", l.amount, super::super::num::unit(&l.asset)));
            }
        }
        // A transaction leaving the pending pill is said where the pill was, for a moment.
        let head = next.health.as_ref().map(|h| h.height).unwrap_or(old_height);
        if let Some(op) = next.ops.iter().find(|op| {
            !matches!(op.status, OpStatus::Submitted | OpStatus::Unknown)
                && self.dash.ops.iter().any(|o| o.id == op.id && matches!(o.status, OpStatus::Submitted | OpStatus::Unknown))
        }) {
            use super::super::icons::Icon;
            use wallet_core::track::describe;
            let text = match op.status {
                OpStatus::Failed | OpStatus::Replaced => {
                    format!("{} {} · {}", self.theme.icon(Icon::Danger), op.status.as_str(), describe(op))
                }
                _ => {
                    let tally = super::super::ui::confirmations(op, head).map(|(n, target)| format!(" · {n}/{target}")).unwrap_or_default();
                    format!("{} mined · {}{tally}", self.theme.icon(Icon::Ok), describe(op))
                }
            };
            self.fx.pill_resolved = Some((text, now));
        }
        let confirmed: Vec<OpKind> = next
            .ops
            .iter()
            .filter(|op| {
                matches!(op.status, OpStatus::Confirmed | OpStatus::Settled)
                    && self.dash.ops.iter().any(|o| o.id == op.id && o.status != op.status && !o.status.is_terminal())
            })
            .map(|op| op.kind.clone())
            .collect();
        for kind in &confirmed {
            self.after_confirm(kind);
        }
        // A swap that landed reads like a fill ticket: what went in, what came out, and how that
        // compares with the quote. Better than quoted is a small gift worth saying; under it but
        // inside the tolerance is reassurance.
        let landed: Vec<String> = next
            .ops
            .iter()
            .filter(|op| {
                matches!(op.kind, OpKind::Swap | OpKind::SwapExactOutput)
                    && matches!(op.status, OpStatus::Confirmed | OpStatus::Settled)
                    && self.dash.ops.iter().any(|o| o.id == op.id && !o.status.is_terminal())
            })
            .map(swap_receipt)
            .collect();
        for said in landed {
            self.toast(said, false);
        }
        let newest_seen = self.dash.notifications.iter().map(|n| n.id).max().unwrap_or(0);
        if next.notifications.iter().any(|n| n.id > newest_seen && n.title == "NFT sold") {
            self.eco.nft.nfts.clear();
            self.load_my_listings();
            self.ring();
            return;
        }
        let arrivals: Vec<wallet_core::appdb::Activity> = next
            .activity
            .iter()
            .filter(|a| !self.dash.activity.iter().any(|o| o.key == a.key) && self.worth_celebrating(a))
            .cloned()
            .collect();
        if !arrivals.is_empty() && !self.config.first_receive_celebrated {
            self.config.first_receive_celebrated = true;
            self.news.first_payment = arrivals.first().map(|a| a.key.clone());
            self.save_config();
        }
        self.arrive(&arrivals);
    }

    /// Whether a new activity row is money that arrived for this wallet and deserves a moment:
    /// incoming, not dust or a zero-value transfer (the planted rows of address poisoning), in an
    /// asset this wallet trusts, and recent — turning explorer lookups on backfills old rows, and
    /// those are history, not news.
    pub(crate) fn worth_celebrating(&self, a: &wallet_core::appdb::Activity) -> bool {
        const RECENT_SECS: u64 = 15 * 60;
        if a.direction != "in" || wallet_core::recipient::is_dust(a) {
            return false;
        }
        if wallet_core::registry::now().saturating_sub(a.observed) > RECENT_SECS {
            return false;
        }
        match a.detail.token().as_str() {
            // Native QUAI and Qi carry no token contract.
            None => matches!(a.asset.as_str(), "QUAI" | "QI" | "Qi"),
            Some(token) => {
                let token = token.to_lowercase();
                let verified = self
                    .eco
                    .feeds
                    .portfolio
                    .value()
                    .and_then(|p| p.rows.iter().find(|r| r.key.id().to_lowercase() == token))
                    .is_some_and(|r| r.trust == wallet_core::portfolio::Trust::Verified);
                verified
                    || self
                        .config
                        .network(&self.network_id)
                        .ok()
                        .is_some_and(|n| wallet_core::portfolio::curated_addresses(&n).iter().any(|c| c.to_lowercase() == token))
            }
        }
    }

    /// Finish an IPFS gateway test.
    pub fn poll_ipfs_check(&mut self) {
        let Some(rx) = &self.tasks.ipfs_check else { return };
        let Ok((content, gateway, result)) = rx.try_recv() else { return };
        self.tasks.ipfs_check = None;
        self.status.busy = None;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => return self.toast(format!("IPFS gateway not saved: {e}"), true),
        };
        let stored = (!gateway.is_default_for(content)).then(|| gateway.display());
        match content {
            wallet_core::ipfs::Content::Abi => self.config.abi_ipfs_gateway = stored.clone(),
            wallet_core::ipfs::Content::Media => self.config.ipfs_gateway = stored.clone(),
        }
        let _ = wallet_core::ipfs::set_gateway(content, stored.as_deref());
        self.save_config();
        // Pictures that failed on the old gateway get another chance on this one.
        self.eco.media.images.retain(|_, slot| !matches!(slot, super::super::eco::ImageSlot::Failed(_)));
        match outcome {
            wallet_core::ipfs::TestOutcome::Verified(ms) => {
                self.toast(format!("IPFS via {} · test file verified against its CID in {ms} ms", gateway.display()), false)
            }
            wallet_core::ipfs::TestOutcome::Answered(why) => {
                self.toast(format!("IPFS via {} · saved; it answered but the test file did not arrive ({why})", gateway.display()), true)
            }
        }
    }

    /// Finish a monitoring endpoint check.
    pub fn poll_monitor_check(&mut self) {
        let Some(rx) = &self.tasks.monitor_check else { return };
        let Ok((network, endpoint, result)) = rx.try_recv() else { return };
        self.tasks.monitor_check = None;
        self.status.busy = None;
        match result {
            Ok(()) => {
                let url = endpoint.rpc_url.clone();
                self.config.monitor_endpoints.insert(network.clone(), endpoint);
                self.save_config();
                if network == self.network_id {
                    self.data_policy_changed();
                }
                self.toast(format!("{network}: monitoring via {url} (chain id and genesis verified)"), false);
            }
            Err(e) => self.toast(format!("monitoring endpoint not saved: {e}"), true),
        }
    }

    /// Add another wallet through the same flow the first one used: it shows and verifies a
    /// recovery phrase, or takes one being imported, and names it.
    pub fn begin_onboarding(&mut self, kind: OnboardKind) {
        self.onboarding = Some(super::super::onboarding::start(self, kind));
        self.dirty = true;
    }

    /// The wallets on this computer, newest last, for the Wallets screen.
    pub fn load_wallets(&mut self) {
        self.cockpit.list = self.registry.list().unwrap_or_default();
        // Read off this thread: one small file per wallet ([`App::poll_persist`] takes them).
        let network = self.network_id.clone();
        let wallets: Vec<String> = self.cockpit.list.iter().map(|w| w.id.clone()).collect();
        let paths = wallets.iter().map(|w| wallet_core::cockpit::summary_path(&self.paths, w, &network)).collect();
        self.persist.read(super::super::persist::Read::Summaries { network, wallets }, paths);
        let addresses: Vec<(String, Vec<String>)> = self.cockpit.list.iter().map(|w| (w.id.clone(), w.quai_owner_addresses())).collect();
        if !addresses.is_empty() {
            self.send_data(super::super::data::DataCmd::WalletQuai(addresses));
        }
    }

    /// Hand a password to the engine, which checks it where the keys will live (the daemon, or
    /// this process when standalone) off every UI path. The lock screen says it is unlocking
    /// meanwhile; [`Ev::Unlocked`] or [`Ev::UnlockFailed`] is the answer.
    pub fn begin_unlock(&mut self, password: Zeroizing<String>) {
        let Some(meta) = self.meta.clone() else { return };
        self.lock.error = None;
        self.dirty = true;
        let Some(engine) = &self.worker else {
            self.lock.error = Some("the wallet is still starting — try again in a moment".into());
            return;
        };
        wallet_core::diag::begin("ux.unlock");
        self.lock.unlocking = true;
        self.lock.unlocking_since = Some(Instant::now());
        // Standalone, the daemon gets the password too when the user shares unlocks with it
        // (once it opens the wallet here); a daemon-hosted engine shares the keys itself.
        let starting = crate::daemon::STARTING.load(std::sync::atomic::Ordering::SeqCst);
        self.lock.handoff_pending = (!engine.is_remote()
            && self.config.daemon_share_unlock
            && (starting || crate::daemon::state(&self.paths).is_some_and(|d| !d.unlocked(&meta.id))))
        .then(|| (meta.id.clone(), password.clone()));
        engine.unlock(meta.id, password);
    }

    /// The engine opened the wallet: hand a standalone unlock to the daemon if asked to.
    fn unlocked_by_engine(&mut self) {
        if let Some((wallet, password)) = self.lock.handoff_pending.take()
            && self.meta.as_ref().is_some_and(|m| m.id == wallet)
        {
            self.hand_to_daemon(wallet, password);
        }
        self.show_unlocked();
    }

    /// The password did not open the wallet: say why, and stay locked.
    fn unlock_failed(&mut self, error: String) {
        self.lock.handoff_pending = None;
        self.lock.unlocking = false;
        self.lock.unlocking_since = None;
        self.dirty = true;
        if !self.lock.locked {
            return;
        }
        let text = friendly_error(&error);
        self.lock.error = Some(text.clone());
        self.status.log.push_front(Toast { text, level: Severity::Danger, at: Instant::now(), id: None });
    }

    /// Give the running daemon this wallet's password, off the UI thread. The copy lives only in
    /// that task and is wiped when it ends; every check `daemon::hand_unlock` makes still applies.
    pub(crate) fn hand_to_daemon(&mut self, wallet: String, password: Zeroizing<String>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let paths = self.paths.clone();
        runtime.spawn(async move {
            // The daemon may still be starting: wait for it (bounded), then hand over only if it
            // is up and does not hold this wallet already.
            let since = std::time::Instant::now();
            while crate::daemon::STARTING.load(std::sync::atomic::Ordering::SeqCst) && since.elapsed().as_secs() < 15 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            if !crate::daemon::state(&paths).is_some_and(|d| !d.unlocked(&wallet)) {
                return;
            }
            let answer = crate::daemon::hand_unlock(&paths, &wallet, &password).await.map_err(|e| e.to_string());
            drop(password);
            let _ = tx.send(answer);
        });
        self.lock.handoff = Some(rx);
    }

    /// Say how a hand-off to the daemon went.
    pub(crate) fn poll_handoff(&mut self) {
        let Some(rx) = &self.lock.handoff else { return };
        match rx.try_recv() {
            Ok(Ok(name)) => {
                self.lock.handoff = None;
                self.toast(format!("{name} is unlocked in the daemon too · quai-terminal daemon lock"), false);
            }
            Ok(Err(e)) => {
                self.lock.handoff = None;
                self.toast(e, true);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.lock.handoff = None,
        }
    }

    /// Leave the lock screen for the dashboard.
    pub(crate) fn show_unlocked(&mut self) {
        wallet_core::diag::end("ux.unlock");
        self.lock.locked = false;
        self.lock.unlocking = false;
        self.lock.unlocking_since = None;
        self.lock.input.zeroize();
        self.lock.error = None;
        self.fx.ambient = None;
        if let Some(form) = self.parked.take() {
            self.modal = Modal::Form(form);
        }
        self.lock.unlocked_at = Some(Instant::now());
        if let Some(title) = self.lock.dropped_review.take() {
            self.toast_as(
                format!("the wallet locked with a review open ({title}); it was discarded and nothing was signed"),
                Severity::Info,
                None,
            );
        }
        // Borders draw themselves in; the balances are simply there. A fade here animated every
        // amount on the dashboard, which money never does. One light runs along the header.
        self.start_transition();
        self.signal(super::super::edge::Signal::Wake);
        self.send(Cmd::Refresh { full: false });
    }

    /// Finish background wallet creation.
    pub fn poll_creation(&mut self) {
        let Some(rx) = &self.tasks.creating else { return };
        match rx.try_recv() {
            Ok(Ok((meta, password))) => {
                self.tasks.creating = None;
                self.status.busy = None;
                if self.config.default_wallet.is_none() {
                    self.config.default_wallet = Some(meta.name.clone());
                }
                self.config.default_network = self.network_id.clone();
                self.config.onboarded = true;
                self.save_config();
                self.lock.locked = meta.kind != WalletKind::Watch;
                self.lock.pending = password;
                // The safety act is the one celebrated: a phrase verified is the only way back in.
                let said = match (meta.kind, meta.backed_up) {
                    (WalletKind::Hd, true) => {
                        format!(
                            "{} is ready · the recovery phrase you verified is the only way back in; nothing here can recover it",
                            meta.name
                        )
                    }
                    _ => format!("{} is ready", meta.name),
                };
                self.toast(said, false);
                self.onboarding = None;
                // A wallet added while one is already open: the session has to follow it, or the
                // screen would show the new name over the old wallet's balances and history.
                // `switch_wallet` reads the new wallet itself, so `meta` stays the old one until
                // it does — otherwise it would see no change and do nothing.
                if self.worker.is_some() {
                    let id = meta.id.clone();
                    self.switch_wallet(&id);
                    // They typed this password a moment ago; do not ask for it again.
                    if let Some(p) = self.lock.pending.take() {
                        self.begin_unlock(p);
                    }
                } else {
                    self.meta = Some(meta);
                }
                self.load_wallets();
                self.dirty = true;
            }
            Ok(Err(e)) => {
                self.tasks.creating = None;
                self.status.busy = None;
                self.toast(e, true);
                self.dirty = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.tasks.creating = None;
                self.status.busy = None;
            }
        }
    }
}

/// "Swap landed · 120 QUAI → 4,210.3 WQI · 0.12% better than quoted", from what the operation
/// recorded when it was prepared and what its receipt said it received.
pub(crate) fn swap_receipt(op: &wallet_core::appdb::Operation) -> String {
    use wallet_core::sdk::U256;
    let d = &op.detail;
    let atoms = |v: &serde_json::Value| v.as_str().and_then(|s| U256::from_str_radix(s, 10).ok()).or_else(|| v.as_u64().map(U256::from));
    let from_decimals = d.decimals().as_u64().unwrap_or(18) as u8;
    let to_decimals = d.to_decimals().as_u64().unwrap_or(18) as u8;
    let to = d.to_symbol().as_str().unwrap_or("?");
    let paid = U256::from_str_radix(&op.amount, 10).ok().map(|v| super::super::num::short(v, from_decimals, 6));
    let pair = match (paid, atoms(d.actual_out())) {
        (Some(paid), Some(out)) => {
            format!("{paid} {} → {} {to}", super::super::num::unit(&op.asset), super::super::num::short(out, to_decimals, 6))
        }
        _ => format!("{} → {to}", super::super::num::unit(&op.asset)),
    };
    let f = |v: U256| wallet_core::amount::to_f64(v, to_decimals);
    let versus = match (atoms(d.actual_out()), atoms(d.expected_out()), atoms(d.minimum_out())) {
        (Some(out), Some(expected), minimum) if !expected.is_zero() => {
            let delta = (f(out) - f(expected)) / f(expected) * 100.0;
            if delta >= 0.005 {
                format!(" · {delta:.2}% better than quoted")
            } else if delta <= -0.005 {
                let room = minimum.filter(|m| !m.is_zero()).map(|m| (f(expected) - f(m)) / f(expected) * 100.0);
                match room {
                    Some(room) => format!(" · {:.2}% under the quote, inside your {room:.1}%", -delta),
                    None => format!(" · {:.2}% under the quote", -delta),
                }
            } else {
                " · as quoted".to_string()
            }
        }
        _ => String::new(),
    };
    format!("Swap landed · {pair}{versus}")
}
