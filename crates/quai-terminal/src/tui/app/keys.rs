//! Keys and the paste buffer: the top of the key path (`on_key`), a form's keys and the screen's own.

use super::*;

impl App {
    /// Bracketed paste into whichever text field has focus.
    pub fn on_paste(&mut self, text: &str) {
        let clean: String = text.trim().chars().filter(|c| !c.is_control()).take(1024).collect();
        match &mut self.modal {
            Modal::Form(form) if !form.pending => {
                if let Some(f) = form.fields.get_mut(form.focus)
                    && !matches!(f.kind, FieldKind::Choice(_))
                {
                    f.value.push_str(&clean);
                }
            }
            Modal::Palette { query, .. } => query.push_str(&clean),
            Modal::None if self.locked => self.lock_input.push_str(&clean),
            _ => {}
        }
        if let Some(Onboarding::Details { fields, focus, .. }) = &mut self.onboarding
            && let Some(f) = fields.get_mut(*focus)
        {
            f.value.push_str(&clean);
        }
        self.dirty = true;
    }

    pub fn move_selection(&mut self, delta: i64) {
        if let Modal::Review(r) = &mut self.modal {
            let max = r.content_lines.saturating_sub(r.viewport);
            r.scroll = (r.scroll as i64 + delta).clamp(0, max as i64) as u16;
            self.dirty = true;
            return;
        }
        if !self.detail.is_empty() {
            let len = self.detail_len();
            if len > 0 {
                self.detail_selected = (self.detail_selected as i64 + delta).rem_euclid(len as i64) as usize;
            }
            self.dirty = true;
            return;
        }
        let len = self.list_len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as i64 + delta).rem_euclid(len as i64) as usize;
        self.dirty = true;
    }

    /// Rows in the current screen's primary list.
    pub fn list_len(&self) -> usize {
        match self.screen {
            Screen::Home if self.pane == 1 => self.activity_rows().len().min(12),
            Screen::Home => self.eco.portfolio.as_ref().map_or(0, |p| p.rows.len()) + self.home_positions().len(),
            Screen::Pools => self.eco.pools_view.positions.as_ref().and_then(|r| r.as_ref().ok()).map_or(0, Vec::len),
            Screen::Accounts => self.dash.accounts.len(),
            Screen::Activity => self.activity_rows().len(),
            Screen::Qi => self.dash.qi.as_ref().map_or(0, |q| q.coins.len()),
            Screen::Board if self.pane == 1 => self.board_message_count(),
            Screen::Board => self.board_rows().len(),
            Screen::Wallets => self.wallets.len(),
            Screen::Channels => self.dash.offers.len() + self.dash.peers.len(),
            Screen::Contacts => self.dash.contacts.len(),
            Screen::Launches => self.launch_rows().len(),
            Screen::Pnl => self.pnl_positions().len(),
            Screen::Orders => self.eco.orders.as_ref().map_or(0, Vec::len),
            Screen::Network => self.dash.networks.len(),
            Screen::Settings => self.settings_rows().len(),
            Screen::DataSources => DATA_SOURCES.len(),
            Screen::Collected => self.eco.nft_len(),
            Screen::Explore => self.eco.collections_filtered().len(),
            Screen::Listings => self.eco.listings_len(),
            Screen::Markets if self.pane == 1 => self.flow_rows().len(),
            Screen::Markets => self.market_rows().len(),
            Screen::Swap | Screen::Convert | Screen::Wrap => 0,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent, size: (u16, u16)) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.last_input = Instant::now();
        self.dirty = true;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if matches!(self.modal, Modal::Review(_) | Modal::Form(_)) {
                self.modal = Modal::Confirm {
                    title: "Quit".into(),
                    body: "Quit with a form or review open? Nothing is signed.".into(),
                    action: ConfirmAction::Quit,
                };
            } else {
                self.quit = true;
            }
            return;
        }
        // A key in the last minute keeps the wallet open: the drained hairline fills again.
        if std::mem::take(&mut self.lock_warned) {
            self.signal(super::super::edge::Signal::Refill);
        }
        self.unpin_lists();
        // Nothing is drawn below the minimum size, so nothing may be acted on either: a review
        // armed and read before the window shrank would otherwise sign on an Enter nobody could
        // see the target of. Esc stays, because backing out is always safe.
        if super::super::ui::too_small(size) && key.code != KeyCode::Esc {
            return;
        }
        // A key skips a decorative effect (never while typing into a modal; celebrations just fade).
        if !self.locked && matches!(self.modal, Modal::None) && self.ambient.is_some() {
            self.ambient = None;
            return;
        }
        if self.onboarding.is_some() {
            return; // handled in the onboarding module
        }
        if self.locked && matches!(self.modal, Modal::None) {
            match key.code {
                // Typing during an unlock would land in the next attempt's password, so the
                // keyboard is ignored until this one answers.
                _ if self.unlocking => {}
                KeyCode::Enter if !self.lock_input.is_empty() => {
                    let password = Zeroizing::new(std::mem::take(&mut self.lock_input));
                    self.begin_unlock(password);
                }
                KeyCode::Backspace => {
                    self.lock_input.pop();
                }
                KeyCode::Esc => self.lock_input.zeroize(),
                // Another wallet on this computer, without unlocking this one first.
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.lock_input.zeroize();
                    self.lock_error = None;
                    if let Some(next) = self.next_wallet_id() {
                        self.switch_wallet(&next);
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.lock_input.push(c);
                    self.lock_error = None;
                }
                _ => {}
            }
            return;
        }
        // Any other key lets go of a hold to sign.
        if key.code != KeyCode::Enter {
            self.hold = None;
        }
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        self.modal = match modal {
            Modal::None => {
                self.on_screen_key(key, size);
                return;
            }
            Modal::Form(form) => self.form_key(form, key),
            Modal::Review(mut r) => match key.code {
                // A risky review: with Approve focused, the keyboard types its confirmation words.
                KeyCode::Char(c) if r.review.confirm.is_some() && r.approve_focused && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if r.typed.chars().count() < 64 {
                        r.typed.push(c);
                    }
                    Modal::Review(r)
                }
                KeyCode::Backspace if r.review.confirm.is_some() && r.approve_focused => {
                    r.typed.pop();
                    Modal::Review(r)
                }
                // The same send as a shell command; copying signs nothing and keeps the review open.
                KeyCode::Char('y') => {
                    match review_cli(&r.review) {
                        Some(cli) => self.copy(super::super::clipboard::PublicText::command(cli), "command · the review is still open"),
                        None => self.toast("only sends have a command-line form yet", true),
                    }
                    Modal::Review(r)
                }
                KeyCode::Esc => {
                    self.send(Cmd::Discard(r.review.op_id.clone()));
                    self.flow_on_rejected(&r.review.op_id);
                    Modal::None
                }
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                    r.approve_focused = !r.approve_focused;
                    Modal::Review(r)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    r.scroll = (r.scroll + 1).min(r.content_lines.saturating_sub(r.viewport));
                    Modal::Review(r)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    r.scroll = r.scroll.saturating_sub(1);
                    Modal::Review(r)
                }
                KeyCode::PageDown | KeyCode::Char(' ') | KeyCode::Char('G') | KeyCode::End => {
                    let jump = if matches!(key.code, KeyCode::Char('G') | KeyCode::End) { r.content_lines } else { r.viewport };
                    r.scroll = (r.scroll + jump).min(r.content_lines.saturating_sub(r.viewport));
                    Modal::Review(r)
                }
                KeyCode::PageUp => {
                    r.scroll = r.scroll.saturating_sub(r.viewport);
                    Modal::Review(r)
                }
                // Hold to sign: one press starts the bar; only Enter held until it fills signs.
                KeyCode::Enter
                    if r.approve_focused && r.can_approve() && self.config.hold_to_sign && !self.held_to_sign(&r.review.op_id) =>
                {
                    Modal::Review(r)
                }
                KeyCode::Enter => {
                    if r.approve_focused && r.can_approve() {
                        self.hold = None;
                        self.committing_kind = Some(r.review.kind.clone());
                        match &r.review.confirm {
                            Some(_) => self.send(Cmd::CommitConfirmed { op_id: r.review.op_id.clone(), words: r.typed.trim().to_string() }),
                            None => self.send(Cmd::Commit(r.review.op_id.clone())),
                        }
                        Modal::None
                    } else if r.approve_focused && !r.words_typed() {
                        let phrase = r.review.confirm.clone().unwrap_or_default();
                        self.toast(format!("type `{phrase}` to sign this review"), true);
                        Modal::Review(r)
                    } else if r.approve_focused {
                        self.toast("read to the end of the review first (space pages down)", true);
                        Modal::Review(r)
                    } else {
                        self.send(Cmd::Discard(r.review.op_id.clone()));
                        self.flow_on_rejected(&r.review.op_id);
                        Modal::None
                    }
                }
                _ => Modal::Review(r),
            },
            Modal::Palette { mut query, mut selected } => {
                let matches = self.palette_entries(&query);
                // ctrl-y: take the selected entry's shell command, for a script.
                if key.code == KeyCode::Char('y') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    match matches.get(selected).map(|e| e.cli.clone()).filter(|c| !c.is_empty()) {
                        Some(cli) => self.copy(super::super::clipboard::PublicText::command(cli), "command"),
                        None => self.toast("this one has no command-line form", true),
                    }
                    self.modal = Modal::Palette { query, selected };
                    return;
                }
                match key.code {
                    KeyCode::Esc => Modal::None,
                    KeyCode::Down | KeyCode::Tab => {
                        selected = (selected + 1).min(matches.len().saturating_sub(1));
                        Modal::Palette { query, selected }
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        selected = selected.saturating_sub(1);
                        Modal::Palette { query, selected }
                    }
                    KeyCode::Enter => {
                        if let Some(e) = matches.into_iter().nth(selected) {
                            self.run_palette(e);
                            return;
                        }
                        Modal::None
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        Modal::Palette { query, selected: 0 }
                    }
                    KeyCode::Char(c) => {
                        query.push(c);
                        Modal::Palette { query, selected: 0 }
                    }
                    _ => Modal::Palette { query, selected },
                }
            }
            Modal::Receive { asset_qi, account } => {
                self.kitty.clear(self.caps.tmux);
                match key.code {
                    KeyCode::Tab | KeyCode::Left | KeyCode::Right => Modal::Receive { asset_qi: !asset_qi, account },
                    KeyCode::Down | KeyCode::Char('j') => {
                        Modal::Receive { asset_qi, account: (account + 1) % self.dash.accounts.len().max(1) }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        Modal::Receive { asset_qi, account: account.checked_sub(1).unwrap_or(self.dash.accounts.len().saturating_sub(1)) }
                    }
                    KeyCode::Char('y') => {
                        if let Some(v) = self.receive_value(asset_qi, account) {
                            self.copy(
                                super::super::clipboard::PublicText::address(v),
                                if asset_qi { "Qi receive code" } else { "address" },
                            );
                        }
                        Modal::Receive { asset_qi, account }
                    }
                    KeyCode::Char('n') if asset_qi => {
                        self.send(Cmd::NewQiAddress(Some("receive".into())));
                        Modal::Receive { asset_qi, account }
                    }
                    _ => Modal::None,
                }
            }
            // Only a deliberate key hides the phrase (a stray one used to, before it was written
            // down); it is zeroized on drop.
            Modal::Secret { .. } if matches!(key.code, KeyCode::Esc | KeyCode::Enter) => Modal::None,
            m @ Modal::Secret { .. } => m,
            // Results carry a tx hash worth reading: only deliberate keys close them.
            m @ (Modal::Result(_) | Modal::Quote(_)) => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Modal::None,
                KeyCode::Char('c') if matches!(m, Modal::Quote(_)) => {
                    self.modal = Modal::None;
                    self.run_action("convert_quai_qi");
                    return;
                }
                KeyCode::Char('C') if matches!(m, Modal::Quote(_)) => {
                    self.modal = Modal::None;
                    self.run_action("convert_qi_quai");
                    return;
                }
                _ => m,
            },
            m @ Modal::Notice { .. } => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Modal::None,
                _ => m,
            },
            Modal::Confirm { title, body, action } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    match action {
                        ConfirmAction::Quit => self.quit = true,
                        ConfirmAction::RemoveContact(name) => self.send(Cmd::RemoveContact(name)),
                        ConfirmAction::SwitchNetwork(id) => self.switch_network(id),
                        ConfirmAction::AcceptOffer(code) => self.send(Cmd::AcceptOffer(code)),
                        ConfirmAction::DeclineOffer(code) => self.send(Cmd::DeclineOffer(code)),
                        ConfirmAction::BlockPeer(address) => self.messaging_op(super::super::worker::MsgOp::Block(address)),
                        ConfirmAction::TrustPeer(address) => self.messaging_op(super::super::worker::MsgOp::Trust(address)),
                        ConfirmAction::VerifyPeer(address) => self.messaging_op(super::super::worker::MsgOp::Verify(address)),
                        ConfirmAction::MoveMessaging(account) => {
                            self.messaging_op(super::super::worker::MsgOp::Setup { account, new_identity: true })
                        }
                        ConfirmAction::Unfollow(name) => {
                            self.config.board_channels.retain(|c| *c != name);
                            self.save_config();
                            self.selected = self.selected.min(self.list_len().saturating_sub(1));
                            self.toast(format!("unfollowed #{name}"), false);
                        }
                    }
                    Modal::None
                }
                KeyCode::Char('q') if matches!(action, ConfirmAction::Quit) => {
                    self.quit = true;
                    Modal::None
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Enter => Modal::None,
                _ => Modal::Confirm { title, body, action },
            },
            Modal::Themes(mut picker) => match picker.on_key(key, &mut self.theme) {
                PickerOutcome::Open => Modal::Themes(picker),
                PickerOutcome::Cancelled => Modal::None,
                PickerOutcome::Applied => {
                    if let Some(e) = picker.current() {
                        self.config.theme = e.id.clone();
                        self.theme_override = None;
                        let name = e.name.clone();
                        self.save_config();
                        self.pending_theme_reload = true;
                        self.toast(format!("theme · {name}"), false);
                    }
                    Modal::None
                }
            },
            Modal::Effects(mut g) => {
                let n = super::super::fx::EFFECTS.len() + 1;
                match key.code {
                    KeyCode::Esc => Modal::None,
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                        g.selected = (g.selected + 1) % n;
                        g.preview = None;
                        Modal::Effects(g)
                    }
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                        g.selected = (g.selected + n - 1) % n;
                        g.preview = None;
                        Modal::Effects(g)
                    }
                    KeyCode::Enter => {
                        self.config.lock_effect = g.value().to_string();
                        self.save_config();
                        self.toast(format!("lock screen · {}", g.value()), false);
                        Modal::None
                    }
                    _ => Modal::Effects(g),
                }
            }
            Modal::TokenPicker { pay, query, selected } => self.picker_key(pay, query, selected, key),
            // The action sheet: its letters are its own; the arrows and Enter choose; Esc closes.
            Modal::Sheet { items, mut selected } => {
                let n = items.len().max(1);
                let chosen = match key.code {
                    KeyCode::Esc => return self.modal_closed(),
                    KeyCode::Down | KeyCode::Tab => {
                        selected = (selected + 1) % n;
                        None
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        selected = (selected + n - 1) % n;
                        None
                    }
                    KeyCode::Enter => items.get(selected).cloned(),
                    KeyCode::Char(c) => items.iter().find(|i| i.key == c).cloned(),
                    _ => None,
                };
                match chosen {
                    Some(item) => {
                        self.modal = Modal::None;
                        self.run_do(item.how);
                        return;
                    }
                    None => Modal::Sheet { items, selected },
                }
            }
            // `g` then a letter: straight to a screen; `g g` is the top of the list.
            Modal::Wallets { mut selected } => {
                let n = self.wallets.len();
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down if n > 0 => selected = (selected + 1) % n,
                    KeyCode::Char('k') | KeyCode::Up if n > 0 => selected = (selected + n - 1) % n,
                    KeyCode::Char('m') => {
                        self.switch(Screen::Wallets);
                        return;
                    }
                    KeyCode::Enter => {
                        let here = self.meta.as_ref().map(|m| m.id.clone());
                        if let Some(w) = self.wallets.get(selected).cloned()
                            && Some(&w.id) != here.as_ref()
                        {
                            // Switching locks this wallet and opens the other at its lock screen.
                            self.switch_wallet(&w.id);
                        }
                        return;
                    }
                    KeyCode::Esc | KeyCode::Char('W') => return,
                    _ => {}
                }
                self.modal = Modal::Wallets { selected };
                return;
            }
            Modal::GoTo => {
                self.modal = Modal::None;
                match key.code {
                    KeyCode::Char('g') => self.selected = 0,
                    KeyCode::Char(c) => match super::super::keymap::route(c) {
                        Some(screen) => self.switch(screen),
                        None => self.info(format!("g {c} goes nowhere · g then ? lists where it goes")),
                    },
                    _ => {}
                }
                return;
            }
            // From the key overlay, g opens the glossary; anything else closes it.
            Modal::Help if key.code == KeyCode::Char('g') => {
                self.help_moved = false;
                Modal::Glossary { selected: 0 }
            }
            // Keys that carry on the Konami code keep Help open.
            Modal::Help if self.konami(key.code) => Modal::Help,
            // The overlay scrolls, so the end of it is reachable on a small terminal; anything that
            // isn't a scroll key closes it.
            Modal::Help
                if matches!(
                    key.code,
                    KeyCode::Down
                        | KeyCode::Char('j')
                        | KeyCode::Up
                        | KeyCode::Char('k')
                        | KeyCode::PageDown
                        | KeyCode::PageUp
                        | KeyCode::Char(' ')
                ) =>
            {
                let step: i64 = match key.code {
                    KeyCode::Down | KeyCode::Char('j') => 1,
                    KeyCode::Up | KeyCode::Char('k') => -1,
                    KeyCode::PageUp => -10,
                    _ => 10,
                };
                self.help_scroll = (i64::from(self.help_scroll) + step).max(0) as u16;
                Modal::Help
            }
            Modal::Help | Modal::Notifications => {
                self.help_moved = false;
                self.help_scroll = 0;
                Modal::None
            }
            Modal::Glossary { selected } => {
                let n = super::super::glossary::TERMS.len();
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => Modal::Glossary { selected: (selected + 1) % n },
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => Modal::Glossary { selected: (selected + n - 1) % n },
                    KeyCode::Char('g') => Modal::Glossary { selected: 0 },
                    KeyCode::Char('G') => Modal::Glossary { selected: n - 1 },
                    _ => Modal::None,
                }
            }
        };
    }

    pub(crate) fn form_key(&mut self, mut form: Form, key: KeyEvent) -> Modal {
        if form.pending {
            // Esc abandons the wait; the worker result then arrives as a toast.
            return if key.code == KeyCode::Esc { Modal::None } else { Modal::Form(form) };
        }
        let n = form.fields.len();
        let is_choice = matches!(form.fields[form.focus].kind, FieldKind::Choice(_));
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // The function this form is on before the key is handled. Only a key that actually changes
        // it may rebuild the argument fields — moving the cursor onto the picker must not, or a
        // Tab past it throws away everything typed into the arguments below.
        let chosen = |form: &Form| form.fields.iter().find(|f| f.label == "Function").map(|f| f.value.clone());
        let was_chosen = chosen(&form);
        // ^F on a destination that turned out to be a callable contract swaps this form for the
        // one that calls it, keeping the address.
        // ^F only means anything on a form that has a destination, and only once that destination
        // has come back a callable contract. Anywhere else — including inside the call form it
        // opens, where it would throw away typed arguments — it is left alone.
        if ctrl && key.code == KeyCode::Char('f') && Self::destination_field(&form.kind).is_some() {
            if let Some(found) = self.contract_found.clone().filter(|f| f.metadata.is_some())
                && self.open_contract_call(found)
            {
                return std::mem::replace(&mut self.modal, Modal::None);
            }
            // Nothing opened (no ABI, nothing callable in it): the form being typed into stays.
            return Modal::Form(form);
        }
        match key.code {
            KeyCode::Esc => return Modal::None,
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1) % n,
            KeyCode::BackTab | KeyCode::Up => form.focus = (form.focus + n - 1) % n,
            KeyCode::Left if is_choice => form.fields[form.focus].cycle(-1),
            KeyCode::Right | KeyCode::Char(' ') if is_choice => form.fields[form.focus].cycle(1),
            KeyCode::Enter => {
                if form.focus + 1 < n && form.fields[form.focus + 1..].iter().any(|f| f.value.trim().is_empty() && !f.optional) {
                    form.focus += 1;
                } else {
                    match validate(&form).and_then(|()| self.check_available(&form)) {
                        Err((i, msg)) => {
                            form.focus = i;
                            form.error_field = Some(i);
                            form.error = Some(msg);
                        }
                        Ok(()) if matches!(form.kind, FormKind::OrderCreate { .. }) => {
                            let FormKind::OrderCreate { from, to, input, slippage, .. } = &form.kind else { unreachable!() };
                            let account = form.fields.first().map(|f| f.value.trim().to_string()).filter(|s| !s.is_empty());
                            match super::super::order_ui::create_request(
                                account,
                                from.clone(),
                                to.clone(),
                                input.clone(),
                                *slippage,
                                &form.fields,
                            ) {
                                Ok(request) => {
                                    form.pending = true;
                                    self.send(Cmd::Order(request));
                                }
                                Err(e) => {
                                    form.error = Some(e.to_string());
                                    form.error_field = None;
                                }
                            }
                        }
                        Ok(()) if form.kind.is_local() => {
                            self.submit_form(&form);
                            return Modal::None;
                        }
                        Ok(()) if matches!(form.kind, FormKind::NftList { .. }) => {
                            let price = form.fields[0].value.trim().to_string();
                            if wallet_core::amount::parse_amount(&price, 18).is_err() || price.parse::<f64>().map_or(true, |p| p <= 0.0) {
                                form.focus = 0;
                                form.error_field = Some(0);
                                form.error = Some("prices are plain decimals above zero, like 250".into());
                            } else {
                                // A sequence (approvals, then the listing): the form closes and each step
                                // opens as its own review.
                                self.submit_form(&form);
                                return Modal::None;
                            }
                        }
                        Ok(()) => {
                            form.error = None;
                            form.error_field = None;
                            form.pending = true;
                            self.submit_form(&form);
                        }
                    }
                }
            }
            KeyCode::Backspace if !is_choice => {
                form.fields[form.focus].value.pop();
                form.error = None;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) && !is_choice => {
                form.fields[form.focus].value.zeroize();
            }
            KeyCode::Char(c) if !is_choice && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if form.fields[form.focus].value.chars().count() < 512 {
                    form.fields[form.focus].value.push(c);
                }
                if form.error_field == Some(form.focus) {
                    form.error = None;
                    form.error_field = None;
                }
            }
            _ => {}
        }
        // Picking another function changes which arguments are needed.
        if matches!(form.kind, FormKind::ContractCall { .. }) && chosen(&form) != was_chosen {
            self.rebuild_contract_fields(&mut form);
        }
        self.probe_destination(&form);
        form.contract_note = Self::contract_note(&form.kind, self.contract_found.as_ref());
        Modal::Form(form)
    }

    pub(crate) fn on_screen_key(&mut self, key: KeyEvent, size: (u16, u16)) {
        if self.jump_pending.take().is_some() {
            if let KeyCode::Char(c) = key.code
                && let Some(idx) = label_index(c)
            {
                let target = self.view_offset() + idx;
                if target < self.list_len() {
                    self.selected = target;
                }
            }
            return;
        }
        // The pinned chat, when it has the keyboard, takes every key; Tab reaches it after the
        // screen's last pane or card field.
        if self.dock_focus && !self.dock_shown {
            self.dock_focus = false;
        }
        if self.dock_focus {
            self.dock_key(key);
            return;
        }
        // Tab past the last pane or card field goes into the chat, whatever has the keyboard.
        if key.code == KeyCode::Tab && self.dock_shown && self.tab_reaches_dock() {
            self.dock_focus = true;
            return;
        }
        // Input layer: a focused field (a card's amount, a search, a filter) owns its keys, and a
        // printable key it has no use for is swallowed rather than read as a command. Space opens
        // the action sheet from a card, since no field here takes it.
        if self.detail.is_empty() && self.input_focused() {
            if key.code == KeyCode::Char(' ') && matches!(self.screen, Screen::Swap | Screen::Convert | Screen::Wrap) {
                self.open_sheet();
                return;
            }
            if self.view_key(key) {
                return;
            }
            if matches!(key.code, KeyCode::Char(_)) && !key.modifiers.contains(KeyModifiers::CONTROL) {
                return;
            }
        }
        // The collection detail's listings pane takes its own keys while it has the focus.
        if matches!(self.detail.last(), Some(Detail::Collection(_))) && self.eco.collection_listings_focused && self.detail_key(key) {
            return;
        }
        // A view's own letter — a sheet action under the very key its handler answers, like
        // Pools' h for harvest — beats the app's meaning of that key, which elsewhere is left.
        if let Some(how) = self.own_letter(&key, true) {
            self.run_do(how);
            return;
        }
        let section_keys: Vec<char> = Section::ALL.iter().map(|s| s.key()).collect();
        match super::super::keymap::resolve(&key, &section_keys, self.config.vim_keys) {
            Some(verb) => self.verb(verb, size),
            // A key the app has no use for still reaches a sheet action that answers it (Markets'
            // A for an alert), as it did before the actions moved into the sheet.
            None => {
                if let Some(how) = self.own_letter(&key, false) {
                    self.run_do(how);
                }
            }
        }
    }

    /// The sheet action on this view that the plain key `key` carries out directly: its own
    /// letter when `same_letter`, else any item whose handler answers that key.
    fn own_letter(&self, key: &KeyEvent, same_letter: bool) -> Option<super::verbs::Do> {
        use super::verbs::Do;
        let KeyCode::Char(c) = key.code else { return None };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        let trading = self.config.features.trading;
        self.keys_here()
            .sheet
            .iter()
            .filter(|i| trading || !i.trading)
            .filter(|i| !same_letter || i.key == c)
            .find(|i| matches!(i.how, Do::View(KeyCode::Char(k)) | Do::Detail(KeyCode::Char(k)) if k == c))
            .map(|i| i.how)
    }

    /// Whether an inline field on the screen has the keyboard.
    pub(crate) fn input_focused(&self) -> bool {
        match self.screen {
            Screen::Swap => self.eco.swap.field != 5,
            Screen::Convert => self.eco.convert.field < 4,
            Screen::Wrap => self.eco.wrap.field < 2,
            Screen::Explore => self.eco.search.is_some(),
            Screen::Board => self.eco.board.filter.is_some(),
            Screen::Pools => self.eco.pools_view.add.is_some(),
            _ => false,
        }
    }

    /// A modal closed by Esc (the sheet): nothing else to do.
    pub(crate) fn modal_closed(&mut self) {
        self.modal = Modal::None;
    }

    /// `[` / `]`: previous or next sub-tab (Activity: filter).
    pub fn change_tab(&mut self, delta: i32) {
        let section = self.screen.section();
        if section == Section::Activity {
            let i = ActivityFilter::ALL.iter().position(|f| *f == self.activity_filter).unwrap_or(0) as i32;
            self.activity_filter = ActivityFilter::ALL[(i + delta).rem_euclid(ActivityFilter::ALL.len() as i32) as usize];
            self.selected = 0;
            return;
        }
        let screens = section.screens(&self.config.features);
        if screens.len() < 2 {
            return;
        }
        let here = self.screen.tab_of(&self.config.features);
        let i = screens.iter().position(|s| *s == here).unwrap_or(0) as i32;
        self.open_tab(screens[(i + delta).rem_euclid(screens.len() as i32) as usize]);
    }

    pub(crate) fn open_link(&mut self) {
        let url = self.link_for_focus();
        match url {
            Some(u) => self.copy(super::super::clipboard::PublicText::link(u), "link"),
            None => self.toast("no link for this item", true),
        }
    }

    pub(crate) fn screen_enter(&mut self) {
        match self.screen {
            Screen::Wallets => {
                if let Some(w) = self.wallets.get(self.selected).cloned() {
                    self.switch_wallet(&w.id);
                }
            }
            Screen::Network => {
                if let Some((id, name)) = self.dash.networks.get(self.selected).cloned() {
                    if id == self.dash.network_id {
                        self.info(format!("already on {name}"));
                    } else {
                        let body = if id == "mainnet" {
                            format!("Switch to {name}? This uses real funds. It becomes your default network.")
                        } else {
                            format!("Switch to {name}? It becomes your default network.")
                        };
                        self.modal = Modal::Confirm { title: "Switch network".into(), body, action: ConfirmAction::SwitchNetwork(id) };
                    }
                }
            }
            Screen::Settings => self.settings_action(),
            Screen::DataSources => self.data_source_action(),
            Screen::Channels => {
                if let Some(o) = self.channel_offer() {
                    let who = wallet_core::session::short_code(&o.code);
                    let body = format!(
                        "{who} announced a payment channel, and {} Qi is waiting on it. Announcements are not authenticated: anyone can send one, and leave a little Qi to look real. Accept only a sender you expect; accepting adds the channel's Qi to this wallet and keeps scanning it.",
                        wallet_core::amount::qi(o.found)
                    );
                    self.modal =
                        Modal::Confirm { title: "Accept payment channel".into(), body, action: ConfirmAction::AcceptOffer(o.code.clone()) };
                } else if let Some(p) = self.channel_peer() {
                    let code = p.code.clone();
                    self.run_action("send_qi");
                    if let Modal::Form(f) = &mut self.modal {
                        f.fields[0].value = code;
                        f.focus = 1;
                    }
                }
            }
            Screen::Contacts => {
                if let Some(c) = self.dash.contacts.get(self.selected) {
                    let qi_address = c
                        .address
                        .as_deref()
                        .and_then(|a| wallet_core::registry::parse_any_address(a).ok())
                        .is_some_and(|a| a.ledger() == wallet_core::sdk::Ledger::Qi);
                    let form_kind = if c.payment_code.is_some() || qi_address { FormKind::SendQi } else { FormKind::SendQuai };
                    let name = c.name.clone();
                    self.open_form(form_kind.clone());
                    if let Modal::Form(f) = &mut self.modal {
                        // Contacts resolve by name, so the review shows the resolved destination.
                        let idx = if form_kind == FormKind::SendQi { 0 } else { 1 };
                        f.fields[idx].value = name;
                        f.focus = idx + 1;
                    }
                }
            }
            Screen::Activity => {
                if let Some(key) = self.activity_key(self.selected) {
                    self.push_detail(Detail::Activity(key));
                }
            }
            _ => self.enter_eco(),
        }
    }

    /// First visible row of the focused list (jump labels count from it).
    pub fn view_offset(&self) -> usize {
        self.lists.borrow().get(&self.main_list()).map_or(0, |s| s.offset)
    }

    pub(crate) fn receive_value(&self, asset_qi: bool, account: usize) -> Option<String> {
        if asset_qi {
            self.meta.as_ref().and_then(|m| m.payment_code.clone()).or_else(|| self.dash.qi_addresses.last().map(|(_, a, _)| a.clone()))
        } else {
            self.dash.accounts.get(account).map(|a| a.address.clone())
        }
    }

    /// The most useful string on the selected row (address, hash or code) for `y`.
    pub(crate) fn selected_value(&self) -> Option<String> {
        if let Some(v) = self.eco_selected_value() {
            return Some(v);
        }
        match self.screen {
            Screen::Accounts => self.dash.accounts.get(self.selected).map(|a| a.address.clone()),
            Screen::Activity => match self.activity_rows().get(self.selected) {
                Some((_, true, i)) => self.dash.ops[*i].tx_hash.clone(),
                Some((_, false, i)) => self.dash.activity[*i].tx_hash.clone(),
                None => None,
            },
            Screen::Qi => self.dash.qi.as_ref().and_then(|q| q.coins.get(self.selected)).map(|c| c.address.clone()),
            Screen::Channels => self.channel_offer().map(|o| o.code.clone()).or_else(|| self.channel_peer().map(|p| p.code.clone())),
            Screen::Contacts => match self.dash.contacts.get(self.selected) {
                Some(c) => c.payment_code.clone().or_else(|| c.address.clone()),
                None => self.meta.as_ref().and_then(|m| m.payment_code.clone()),
            },
            _ => self.receive_value(false, 0),
        }
    }

    /// Copy public text after the next frame; the toast comes when it is known how it went.
    pub(crate) fn copy(&mut self, text: super::super::clipboard::PublicText, what: &'static str) {
        self.clipboard = Some(super::super::clipboard::CopyRequest { text, what });
    }

    /// Report a finished copy.
    pub(crate) fn poll_copy(&mut self) {
        let Some(rx) = &self.copying else { return };
        match rx.try_recv() {
            Ok((req, outcome)) => {
                self.copying = None;
                let (mut text, error) = super::super::clipboard::describe(&req, &outcome);
                // Once a session, where it matters most: an address about to be pasted.
                if !error && req.what == "address" && !std::mem::replace(&mut self.paste_hint_shown, true) {
                    text.push_str(" · check its first and last characters where you paste");
                }
                self.toast(text, error);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.copying = None,
        }
    }
}
