//! People: the message board, sealed conversations and the pinned chat.

use super::*;

/// What `p` says on an old payment-code conversation.
const LEGACY_READ_ONLY: &str = "old sealed conversations are read-only: write to their messaging address instead";

impl App {
    /// The board's left column: the channels this wallet follows, the people it can write to in
    /// private, then the channels seen on the board that it does not follow. A typed filter
    /// narrows all three by name.
    pub fn board_rows(&self) -> Vec<BoardRow> {
        let followed = &self.config.board_channels;
        let mut rows: Vec<BoardRow> = followed.iter().cloned().map(BoardRow::Channel).collect();
        // Private messages: the messaging account first, then conversations newest first, then
        // who is waiting.
        if let Some(Ok(view)) = &self.eco.board.msg {
            use wallet_core::messaging::service::KeyNeed;
            if self.can_sign() {
                rows.push(BoardRow::Messaging);
            }
            if !matches!(view.status.need, KeyNeed::NotSetUp | KeyNeed::NoKeys) {
                rows.extend(view.conversations.iter().map(|c| BoardRow::Chat(c.peer.clone(), c.name.clone())));
                rows.extend(view.requests.iter().map(|c| BoardRow::Request(c.peer.clone())));
            }
        }
        // Old sealed conversations, read-only: established payment channels, plus any contact
        // who has a payment code.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in &self.dash.peers {
            if seen.insert(p.code.clone()) {
                rows.push(BoardRow::Peer(p.code.clone(), p.contact.clone()));
            }
        }
        for c in &self.dash.contacts {
            if let Some(code) = c.payment_code.as_ref().filter(|code| seen.insert((*code).clone())) {
                rows.push(BoardRow::Peer(code.clone(), Some(c.name.clone())));
            }
        }
        rows.extend(
            self.eco
                .board
                .known
                .iter()
                .filter(|c| !followed.iter().any(|f| f == &c.name))
                .map(|c| BoardRow::Unfollowed(c.name.clone(), c.messages)),
        );
        let Some(filter) = self.eco.board.filter.as_ref().map(|f| f.trim().to_lowercase()).filter(|f| !f.is_empty()) else {
            return rows;
        };
        rows.retain(|r| match r {
            BoardRow::Channel(name) | BoardRow::Unfollowed(name, _) => name.to_lowercase().contains(&filter),
            // People match on the name you gave them, or on their code or address.
            BoardRow::Peer(id, contact) | BoardRow::Chat(id, contact) => {
                contact.as_ref().is_some_and(|c| c.to_lowercase().contains(&filter)) || id.to_lowercase().contains(&filter)
            }
            BoardRow::Request(address) => address.contains(&filter),
            BoardRow::Messaging => true,
        });
        rows
    }

    /// A contact's name for a Quai address, so the board reads as people rather than hex.
    ///
    /// Matches the contact's own address and any address the payment-channel scan has attributed
    /// to a peer, since a peer posting from a fresh payment address is still that peer.
    pub fn contact_name_for(&self, address: &str) -> Option<String> {
        let a = address.to_lowercase();
        if let Some(c) = self.dash.contacts.iter().find(|c| c.address.as_ref().is_some_and(|x| x.to_lowercase() == a)) {
            return Some(c.name.clone());
        }
        // Any other account the same person has written from.
        self.dash.contact_addresses.iter().find(|(addr, _)| *addr == a).map(|(_, name)| name.clone())
    }

    /// A board row as a chat target (`#channel` / `dm:<code>`) and how it reads.
    pub fn chat_target(row: &BoardRow) -> (String, String) {
        match row {
            BoardRow::Channel(c) | BoardRow::Unfollowed(c, _) => (wallet_core::chat::channel_target(c), format!("#{c}")),
            BoardRow::Peer(code, name) => {
                (wallet_core::chat::dm_target(code), name.clone().unwrap_or_else(|| wallet_core::session::short_code(code)))
            }
            BoardRow::Chat(address, name) => {
                (format!("msg:{address}"), name.clone().unwrap_or_else(|| wallet_core::session::short_address(address)))
            }
            BoardRow::Request(address) => (format!("msg:{address}"), wallet_core::session::short_address(address)),
            BoardRow::Messaging => (String::new(), "messaging account".into()),
        }
    }

    /// How a chat target reads: `#general`, or the contact's name for a conversation.
    pub fn chat_label(&self, target: &str) -> String {
        if let Some(address) = target.strip_prefix("msg:") {
            return self
                .private_conversation(address)
                .and_then(|c| c.name.clone())
                .or_else(|| self.contact_name_for(address))
                .unwrap_or_else(|| wallet_core::session::short_address(address));
        }
        match target.strip_prefix("dm:") {
            Some(code) => self
                .dash
                .contacts
                .iter()
                .find(|c| c.payment_code.as_deref() == Some(code))
                .map(|c| c.name.clone())
                .unwrap_or_else(|| wallet_core::session::short_code(code)),
            None => target.to_string(),
        }
    }

    /// Post what is in the pinned chat's box: the same form and review as `p` on the Board, with
    /// the text already in it.
    pub(crate) fn post_dock_draft(&mut self) {
        let text = self.dock_draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.write_pinned();
        let super::super::app::Modal::Form(mut form) = std::mem::replace(&mut self.modal, super::super::app::Modal::None) else { return };
        if let Some(field) = form.fields.iter_mut().find(|f| f.label.starts_with("Message")) {
            field.value = text;
        }
        form.focus = form.fields.len() - 1;
        self.modal = self.form_key(form, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Sent for review: the box empties, and focus stays in the chat for the next line.
        if matches!(&self.modal, super::super::app::Modal::Form(f) if f.pending) || matches!(self.modal, super::super::app::Modal::None) {
            self.dock_draft.clear();
        }
    }

    /// Write to the pinned chat, from any screen.
    pub fn write_pinned(&mut self) {
        let Some(pin) = self.eco.board.pin.clone() else {
            return self.toast("pin a chat first: Board (4 ]]), then P", true);
        };
        if !self.can_sign() {
            return self.toast("this wallet is watch-only", true);
        }
        if let Some(address) = pin.strip_prefix("msg:") {
            let name = self.contact_name_for(address);
            return self.write_private(address.to_string(), name);
        }
        match pin.strip_prefix("dm:") {
            Some(_) => self.toast(LEGACY_READ_ONLY, true),
            None => self.open_form(FormKind::BoardPost { channel: pin.trim_start_matches('#').to_string() }),
        }
    }

    /// The row under the cursor.
    pub fn board_row(&self) -> Option<BoardRow> {
        let rows = self.board_rows();
        let i = if self.pane == 1 { self.eco.board_channel_selected } else { self.selected };
        rows.get(i.min(rows.len().saturating_sub(1))).cloned()
    }

    /// The channel under the cursor, followed or merely seen.
    pub fn board_channel(&self) -> Option<String> {
        match self.board_row() {
            Some(BoardRow::Channel(c)) | Some(BoardRow::Unfollowed(c, _)) => Some(c),
            _ => None,
        }
    }

    /// Messages in the open channel, oldest first — a conversation reads downwards.
    pub fn board_posts(&self) -> Vec<&wallet_core::messages::Post> {
        let Some(channel) = self.board_channel() else { return Vec::new() };
        match self.eco.board.posts.get(&channel) {
            Some(Ok(posts)) => posts.iter().rev().collect(),
            _ => Vec::new(),
        }
    }

    /// The open conversation's messages, oldest first.
    pub fn board_dm_lines(&self) -> Vec<&wallet_core::ops::SealedLine> {
        let Some(BoardRow::Peer(code, _)) = self.board_row() else { return Vec::new() };
        match self.eco.board.dms.get(&code) {
            Some(Ok(lines)) => lines.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// The address that wrote the selected message, when the messages pane has the cursor.
    pub fn board_sender(&self) -> Option<String> {
        if self.pane != 1 {
            return None;
        }
        match self.board_row() {
            Some(BoardRow::Peer(..)) => self.board_dm_lines().get(self.selected).map(|l| l.from.clone()),
            Some(BoardRow::Chat(address, _) | BoardRow::Request(address)) => Some(address),
            Some(BoardRow::Messaging) => None,
            _ => self.board_posts().get(self.selected).map(|p| p.from.clone()),
        }
    }

    /// The open private conversation's messages, oldest first.
    pub fn board_private_lines(&self) -> Vec<&wallet_core::messaging::service::Line> {
        let (Some(BoardRow::Chat(address, _)) | Some(BoardRow::Request(address))) = self.board_row() else { return Vec::new() };
        match self.eco.board.msg_lines.get(&address) {
            Some(Ok(lines)) => lines.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Where private messages stand, when the wallet worker has said.
    pub fn messaging(&self) -> Option<&super::MessagingView> {
        match &self.eco.board.msg {
            Some(Ok(v)) => Some(v),
            _ => None,
        }
    }

    /// One conversation or request, by address.
    pub fn private_conversation(&self, address: &str) -> Option<&wallet_core::messaging::service::Conversation> {
        let v = self.messaging()?;
        v.conversations.iter().chain(v.requests.iter()).find(|c| c.peer == address)
    }

    /// Ask the wallet worker for where private messages stand, reading the chain first when
    /// `sync`, and for `open`'s messages.
    pub(crate) fn refresh_messaging(&mut self, open: Option<String>, sync: bool) {
        if self.locked || self.eco.board.msg_loading {
            return;
        }
        self.eco.board.msg_loading = true;
        self.eco.board.msg_at = Some(Instant::now());
        self.send(Cmd::Messaging { op: super::super::worker::MsgOp::Refresh { open, sync }, epoch: self.private_epoch });
    }

    /// A change to private messages, then the view again.
    pub(crate) fn messaging_op(&mut self, op: super::super::worker::MsgOp) {
        if self.locked {
            return;
        }
        self.eco.board.msg_loading = true;
        self.send(Cmd::Messaging { op, epoch: self.private_epoch });
    }

    /// Write to a private conversation. This week's key goes first when it is due: its review
    /// opens instead, and the message follows once it is sent.
    pub(crate) fn write_private(&mut self, address: String, name: Option<String>) {
        use wallet_core::messaging::service::KeyNeed;
        if !self.can_sign() {
            return self.toast("this wallet is watch-only", true);
        }
        match self.messaging().map(|v| v.status.need) {
            Some(KeyNeed::Publish) => self.publish_messaging_key(),
            Some(KeyNeed::NotSetUp | KeyNeed::NoKeys) => self.open_messaging_account(),
            _ => {
                if self.private_conversation(&address).is_some_and(|c| c.identity_changed) {
                    return self.toast("their identity key changed: compare fingerprints (v), then accept it (T)", true);
                }
                self.open_form(FormKind::Message { peer: address, name })
            }
        }
    }

    /// The accounts messaging can go from: every one but the main one, then a new one. Each is
    /// (address, `None` for a new account) and how it reads.
    pub fn messaging_choices(&self) -> Vec<(Option<String>, String)> {
        let mut out: Vec<(Option<String>, String)> = self
            .dash
            .accounts
            .iter()
            .skip(1)
            .map(|a| {
                let balance = wallet_core::amount::group_thousands(&wallet_core::amount::format_amount_short(a.balance, 18, 4));
                (Some(a.address.clone()), format!("{} · {} · {balance} QUAI", a.label, wallet_core::session::short_address(&a.address)))
            })
            .collect();
        out.push((None, "a new account, just for messaging".into()));
        out
    }

    /// Put the cursor on the messaging account's row, with the choice beside it.
    pub(crate) fn open_messaging_account(&mut self) {
        let Some(i) = self.board_rows().iter().position(|r| *r == BoardRow::Messaging) else {
            return self.toast("unlock to set up private messages", true);
        };
        self.pane = 0;
        self.selected = i;
        self.toast("choose the account messages go from: enter, then pick one", false);
    }

    /// Messages go from the `i`th choice. The first choice sets messaging up; a different one
    /// later moves it, which starts a new identity, so that asks first.
    pub(crate) fn choose_messaging_account(&mut self, i: usize) {
        use wallet_core::messaging::service::KeyNeed;
        let Some((account, label)) = self.messaging_choices().get(i).cloned() else { return };
        let status = self.messaging().map(|v| v.status.clone());
        let current = status.as_ref().and_then(|s| s.account.clone());
        let same = matches!((&account, &current), (Some(a), Some(c)) if a.eq_ignore_ascii_case(c));
        match status.map(|s| s.need) {
            None => {}
            Some(KeyNeed::NotSetUp) => self.messaging_op(super::super::worker::MsgOp::Setup { account, new_identity: false }),
            Some(KeyNeed::NoKeys) if same => self.messaging_op(super::super::worker::MsgOp::Setup { account, new_identity: true }),
            Some(_) if same => self.info("messages already go from this account"),
            Some(_) => {
                self.modal = super::super::app::Modal::Confirm {
                    title: "New messaging identity".into(),
                    body: format!(
                        "Move messaging to {label}? This starts a new identity: the keys and history on this computer are \
                         deleted, and people you talk to will see it change and should compare fingerprints with you again."
                    ),
                    action: super::super::app::ConfirmAction::MoveMessaging(account),
                };
            }
        }
    }

    /// Review publishing this week's messaging key.
    pub(crate) fn publish_messaging_key(&mut self) {
        self.toast("this week's messaging key goes first; write your message once it is sent", false);
        self.send(Cmd::Prepare(super::super::worker::Prepare::MessagingKeys));
    }

    /// Rows in whichever the cursor has open, for the message pane's own cursor.
    pub fn board_message_count(&self) -> usize {
        match self.board_row() {
            Some(BoardRow::Peer(..)) => self.board_dm_lines().len(),
            Some(BoardRow::Chat(..) | BoardRow::Request(..)) => self.board_private_lines().len(),
            Some(BoardRow::Messaging) => self.messaging_choices().len(),
            _ => self.board_posts().len(),
        }
    }

    /// Re-read whatever the board has open every 10 s. A channel is a plain log query and goes
    /// to the data worker; a conversation needs this wallet's payment key, so it goes to the
    /// wallet worker, which is the only one holding keys.
    pub(crate) fn tick_board(&mut self) {
        let blocks = wallet_core::messages::BOARD_BLOCKS;
        match self.board_row() {
            Some(BoardRow::Channel(channel)) | Some(BoardRow::Unfollowed(channel, _)) => {
                let board = &self.eco.board;
                let fresh = board.at.get(&channel).is_some_and(|t| t.elapsed() < Duration::from_secs(5));
                if board.loading.is_some() || fresh {
                    return;
                }
                self.eco.board.loading = Some(channel.clone());
                self.send_data(DataCmd::Board { channel, blocks });
            }
            Some(BoardRow::Peer(code, _)) => {
                if self.locked {
                    return;
                }
                let board = &self.eco.board;
                let fresh = board.dm_at.get(&code).is_some_and(|t| t.elapsed() < Duration::from_secs(5));
                if board.dm_loading.is_some() || fresh {
                    return;
                }
                self.eco.board.dm_loading = Some(code.clone());
                self.send(Cmd::ReadConversation { peer: code, blocks, epoch: self.private_epoch });
            }
            // Private messages: every 10 s while one is open, reading the chain each time.
            Some(BoardRow::Chat(address, _) | BoardRow::Request(address)) => {
                if self.eco.board.msg_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(10))
                    || !self.eco.board.msg_lines.contains_key(&address)
                {
                    self.refresh_messaging(Some(address), true);
                }
            }
            Some(BoardRow::Messaging) | None => {}
        }
        // The list itself: once on opening, then every half minute.
        if !self.locked && self.eco.board.msg_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(30)) {
            self.refresh_messaging(None, true);
        }
    }

    /// Scan the board for what is on it, wherever the user happens to be. One `quai_getLogs`
    /// covers every channel at once, so following a channel means something on any screen: often
    /// while the board is open, rarely otherwise.
    pub(crate) fn tick_board_watch(&mut self) {
        if self.locked || !self.config.features.messaging || self.config.board_channels.is_empty() {
            return;
        }
        if self.net().is_some_and(|n| n.ecosystem.messages.is_none()) {
            return;
        }
        let looking = self.screen == Screen::Board;
        let every = Duration::from_secs(if looking { 5 } else { 45 });
        let board = &self.eco.board;
        if board.known_loading || board.known_at.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        self.eco.board.known_loading = true;
        self.eco.board.known_at = Some(Instant::now());
        self.send_data(DataCmd::BoardChannels { blocks: wallet_core::messages::BOARD_BLOCKS });
    }

    /// Say what arrived in the followed channels. The first scan of a session only records
    /// where the board stands: opening the wallet is not news, everything after it is.
    pub(crate) fn announce_board(&mut self, seed: bool) {
        let followed = self.config.board_channels.clone();
        if seed {
            for channel in &followed {
                self.mark_board_seen(channel);
            }
            return;
        }
        // The channel on screen is being read, so it is caught up rather than announced.
        let open = (self.screen == Screen::Board).then(|| self.board_channel()).flatten();
        let mut arrived: Vec<(String, u32)> = Vec::new();
        for channel in followed {
            if open.as_deref() == Some(channel.as_str()) {
                self.mark_board_seen(&channel);
                continue;
            }
            match self.board_unread(&channel) {
                0 => {}
                n => arrived.push((channel, n)),
            }
        }
        // Only newly arrived messages are worth a toast; a count that has not moved is not news.
        let mut announce: Vec<String> = Vec::new();
        for (channel, n) in arrived {
            if self.eco.board.announced.get(&channel).copied().unwrap_or(0) < n {
                announce.push(if n == 1 { format!("1 new message in #{channel}") } else { format!("{n} new messages in #{channel}") });
            }
            self.eco.board.announced.insert(channel, n);
        }
        for text in announce {
            self.toast(text, false);
        }
    }

    /// Messages in a followed channel newer than the one last looked at.
    pub fn board_unread(&self, channel: &str) -> u32 {
        let Some(seen) = self.eco.board.seen.get(channel) else { return 0 };
        self.eco.board.known.iter().find(|c| c.name == channel).map_or(0, |c| c.recent_blocks.iter().filter(|b| *b > seen).count() as u32)
    }

    /// Everything on the board right now counts as looked at: what arrives afterwards is news,
    /// what was already there is not.
    pub(crate) fn mark_board_seen(&mut self, channel: &str) {
        let newest = self.eco.board.known.iter().find(|c| c.name == channel).map_or(0, |c| c.last_block);
        if newest > 0 {
            self.eco.board.seen.insert(channel.to_string(), newest);
        }
    }

    /// Follow a channel by name. Following is local: it only decides what this wallet lists,
    /// and a channel exists on the board whether anyone follows it or not.
    pub fn follow_channel(&mut self, name: &str) {
        let name = name.trim().to_string();
        if let Err(e) = wallet_core::messages::channel_tag(&name) {
            return self.toast(super::super::app::friendly_error(&e.to_string()), true);
        }
        if self.config.board_channels.iter().any(|c| c == &name) {
            return self.info(format!("already following #{name}"));
        }
        self.config.board_channels.push(name.clone());
        self.save_config();
        self.toast(format!("following #{name}"), false);
    }

    pub(crate) fn board_key(&mut self, key: KeyEvent) -> bool {
        // While the filter is open it takes the typing, so a name with p, a or x in it is not
        // read as a command. Esc closes it and shows everything again.
        if let Some(filter) = self.eco.board.filter.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.eco.board.filter = None;
                    self.selected = 0;
                    return true;
                }
                KeyCode::Enter | KeyCode::Down | KeyCode::Up | KeyCode::Tab | KeyCode::BackTab => {}
                KeyCode::Backspace => {
                    filter.pop();
                    self.selected = 0;
                    return true;
                }
                KeyCode::Char(c) => {
                    filter.push(c);
                    self.selected = 0;
                    return true;
                }
                _ => return false,
            }
        }
        match key.code {
            // The cursor belongs to one pane at a time, as on the markets screen.
            KeyCode::Tab | KeyCode::BackTab => {
                if self.pane == 0 {
                    self.eco.board_channel_selected = self.selected;
                } else {
                    self.eco.board_post_selected = self.selected;
                }
                self.pane = 1 - self.pane;
                self.selected = if self.pane == 0 {
                    self.eco.board_channel_selected
                } else {
                    self.eco.board_post_selected.min(self.board_message_count().saturating_sub(1))
                };
                true
            }
            KeyCode::Char('p') => {
                match self.board_row() {
                    Some(BoardRow::Messaging) if self.pane == 1 => self.choose_messaging_account(self.selected),
                    // The choice is the pane beside: go there, onto the account in use.
                    Some(BoardRow::Messaging) => {
                        self.eco.board_channel_selected = self.selected;
                        self.pane = 1;
                        let current = self.messaging().and_then(|v| v.status.account.clone());
                        self.selected = self
                            .messaging_choices()
                            .iter()
                            .position(|(a, _)| a.as_deref().is_some_and(|a| current.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(a))))
                            .unwrap_or(0);
                    }
                    Some(BoardRow::Chat(address, name)) => self.write_private(address, name),
                    Some(BoardRow::Request(_)) => self.toast("accept them first (a), or block them (B)", true),
                    Some(BoardRow::Channel(channel)) => self.open_form(FormKind::BoardPost { channel }),
                    Some(BoardRow::Unfollowed(channel, _)) => {
                        // Writing somewhere is reason enough to keep it in the list.
                        self.follow_channel(&channel);
                        self.open_form(FormKind::BoardPost { channel });
                    }
                    Some(BoardRow::Peer(..)) => self.toast(LEGACY_READ_ONLY, true),
                    None => self.toast("add a channel first (a)", true),
                }
                true
            }
            // On a channel the board knows but this wallet does not follow, `a` takes it; other-
            // wise it asks for a name, which is all it takes to start one.
            KeyCode::Char('a') => {
                match self.board_row() {
                    Some(BoardRow::Unfollowed(channel, _)) => self.follow_channel(&channel),
                    Some(BoardRow::Request(address)) => self.messaging_op(super::super::worker::MsgOp::Accept(address)),
                    _ => self.open_form(FormKind::FollowChannel),
                }
                true
            }
            KeyCode::Char('/') => {
                self.eco.board.filter = Some(String::new());
                self.selected = 0;
                true
            }
            // Unfollowing asks first, like every other removal; a person is here because they are a
            // payment peer, and only a channel is followed.
            KeyCode::Char('x') => {
                let i = if self.pane == 1 { self.eco.board_channel_selected } else { self.selected };
                if let Some(name) = self.config.board_channels.get(i).cloned() {
                    self.modal = super::super::app::Modal::Confirm {
                        title: "Unfollow channel".into(),
                        body: format!("Stop following #{name}? Its posts stay on chain; follow it again any time."),
                        action: super::super::app::ConfirmAction::Unfollow(name),
                    };
                }
                true
            }
            // Pin the chat beside every screen (again unpins it).
            KeyCode::Char('P') => {
                if let Some(row) = self.board_row() {
                    let (target, label) = Self::chat_target(&row);
                    let unpin = self.eco.board.pin.as_deref() == Some(target.as_str());
                    self.send(Cmd::Chat(super::super::worker::ChatOp::Pin { target: (!unpin).then_some(target), label }));
                }
                true
            }
            // Notify me when someone says something here (again stops).
            KeyCode::Char('n') if matches!(self.board_row(), Some(BoardRow::Chat(..) | BoardRow::Request(..) | BoardRow::Messaging)) => {
                self.info("private conversations always notify; requests never do until you accept them");
                true
            }
            KeyCode::Char('n') => {
                if let Some(row) = self.board_row() {
                    let (target, label) = Self::chat_target(&row);
                    self.send(Cmd::Chat(super::super::worker::ChatOp::Toggle { target, label }));
                }
                true
            }
            KeyCode::Char('R') => {
                self.eco.board.at.clear();
                self.eco.board.dm_at.clear();
                self.eco.board.known_at = None;
                self.eco.board.msg_at = None;
                self.tick_board();
                true
            }
            // Private messages.
            KeyCode::Char('m') => {
                if self.can_sign() {
                    self.open_form(FormKind::MessageNew);
                }
                true
            }
            KeyCode::Char('K') => {
                self.publish_messaging_key();
                true
            }
            KeyCode::Char('F') => {
                self.open_form(FormKind::MessagingFund);
                true
            }
            KeyCode::Char('B') => {
                if let Some(BoardRow::Chat(address, _) | BoardRow::Request(address)) = self.board_row() {
                    let who = self.chat_label(&format!("msg:{address}"));
                    self.modal = super::super::app::Modal::Confirm {
                        title: "Block".into(),
                        body: format!("Block {who}? Their messages are dropped unread from now on; the ones you have stay."),
                        action: super::super::app::ConfirmAction::BlockPeer(address),
                    };
                }
                true
            }
            KeyCode::Char('v') => {
                if let Some(BoardRow::Chat(address, _)) = self.board_row() {
                    let theirs = self.private_conversation(&address).and_then(|c| c.fingerprint.clone());
                    let ours = self.messaging().and_then(|v| v.status.fingerprint.clone()).unwrap_or_default();
                    let Some(theirs) = theirs else {
                        return {
                            self.toast("they have not published a messaging key yet", true);
                            true
                        };
                    };
                    self.modal = super::super::app::Modal::Confirm {
                        title: "Compare fingerprints".into(),
                        body: format!(
                            "Theirs:  {theirs}\nYours:   {ours}\n\nCompare both with them in person or over another channel. Confirm only if theirs matches exactly."
                        ),
                        action: super::super::app::ConfirmAction::VerifyPeer(address),
                    };
                }
                true
            }
            KeyCode::Char('T') => {
                if let Some(BoardRow::Chat(address, _)) = self.board_row()
                    && self.private_conversation(&address).is_some_and(|c| c.identity_changed)
                {
                    self.modal = super::super::app::Modal::Confirm {
                        title: "Accept a new identity".into(),
                        body: "Their identity key changed. That happens when someone restores a wallet or moves to a new computer, and \
                               also when someone else is pretending. Accept it only after you have asked them over another channel."
                            .into(),
                        action: super::super::app::ConfirmAction::TrustPeer(address),
                    };
                }
                true
            }
            // Whoever wrote the selected message, into the address book. In a sealed
            // conversation the payment code is known too — that is the identity, and the
            // address is merely the account this message came from.
            KeyCode::Char('c') if self.pane == 1 => {
                let sender = self.board_sender();
                match (self.board_row(), sender) {
                    (Some(BoardRow::Peer(code, _)), address) => {
                        let mine = self.owner_addresses();
                        let address = address.filter(|a| !mine.iter().any(|m| m.eq_ignore_ascii_case(a)));
                        self.open_form(FormKind::ContactFromPeer { code, address });
                    }
                    (_, Some(address)) => {
                        self.open_form(FormKind::Contact(None));
                        if let Modal::Form(f) = &mut self.modal {
                            f.fields[1].value = address;
                            f.focus = 0;
                        }
                    }
                    (_, None) => self.toast("select a message first (tab)", true),
                }
                true
            }
            _ => false,
        }
    }

    /// Keep the pinned chat current wherever the user is, and check subscriptions every half
    /// minute while no daemon does (two checkers would each notify).
    pub(crate) fn tick_chat(&mut self) {
        if self.locked || self.meta.is_none() || !self.config.features.messaging {
            return;
        }
        if !self.eco.board.chat_loaded {
            self.eco.board.chat_loaded = true;
            self.send(Cmd::Chat(super::super::worker::ChatOp::Load));
            return;
        }
        // Where private messages stand, once after each unlock: it decides whether this week's
        // key is offered and whether private conversations notify.
        if self.eco.board.msg.is_none()
            && self.eco.board.msg_at.is_none()
            && self.can_sign()
            && self.net().is_some_and(|n| n.ecosystem.messages.is_some())
        {
            self.refresh_messaging(None, false);
        }
        if let Some(pin) = self.eco.board.pin.clone()
            && self.screen != Screen::Board
        {
            let blocks = wallet_core::messages::BOARD_BLOCKS;
            match (pin.strip_prefix("msg:"), pin.strip_prefix("dm:")) {
                (Some(address), _) => {
                    if self.eco.board.msg_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(15)) {
                        self.refresh_messaging(Some(address.to_string()), true);
                    }
                }
                (None, Some(code)) => {
                    let b = &self.eco.board;
                    if b.dm_loading.is_none() && b.dm_at.get(code).is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
                        self.eco.board.dm_loading = Some(code.to_string());
                        self.send(Cmd::ReadConversation { peer: code.to_string(), blocks, epoch: self.private_epoch });
                    }
                }
                (None, None) => {
                    let channel = pin.trim_start_matches('#').to_string();
                    let b = &self.eco.board;
                    if b.loading.is_none() && b.at.get(&channel).is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
                        self.eco.board.loading = Some(channel.clone());
                        self.send_data(DataCmd::Board { channel, blocks });
                    }
                }
            }
        }
        // Private conversations always notify, subscribed or not.
        let private = self.messaging().is_some_and(|v| {
            !matches!(v.status.need, wallet_core::messaging::service::KeyNeed::NotSetUp | wallet_core::messaging::service::KeyNeed::NoKeys)
        });
        if (!self.eco.board.subs.is_empty() || private)
            && self.eco.board.news_checked.is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
        {
            self.eco.board.news_checked = Some(Instant::now());
            // A daemon reads the channels; it reads this wallet's sealed chats too only if it
            // holds the wallet unlocked. Whatever it cannot read, this window does.
            let id = self.meta.as_ref().map(|m| m.id.clone()).unwrap_or_default();
            match crate::daemon::state(&self.paths) {
                None => self.send(Cmd::ChatNews { dms_only: false, epoch: self.private_epoch }),
                Some(d) if !d.unlocked(&id) => self.send(Cmd::ChatNews { dms_only: true, epoch: self.private_epoch }),
                Some(_) => {}
            }
        }
    }

    /// Whether Tab, pressed now, would wrap back to the start of this screen: the pinned chat
    /// is the next stop instead.
    pub(crate) fn tab_reaches_dock(&self) -> bool {
        if let Some(d) = self.detail.last() {
            return !matches!(d, super::super::app::Detail::Collection(_));
        }
        match self.screen {
            // Exchange cards: from their last field (an unfocused card takes Tab to enter).
            Screen::Swap => self.eco.swap.field == 4,
            Screen::Convert => self.eco.convert.field == 3,
            Screen::Wrap => self.eco.wrap.field == 1,
            Screen::Pools if self.eco.pools_view.add.is_some() => self.eco.pools_view.add.as_ref().is_some_and(|a| a.field == 3),
            // A typed filter keeps its Tab.
            Screen::Board if self.eco.board.filter.is_some() => false,
            s => self.pane + 1 >= s.panes().max(1),
        }
    }

    /// Keys while the pinned chat has the keyboard: type, Enter to post (the review still
    /// decides), Tab or Esc to go back to the screen with the draft kept.
    pub(crate) fn dock_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::BackTab => self.dock_focus = false,
            KeyCode::Tab => {
                self.dock_focus = false;
                // On round to the screen's first stop, as its own Tab would.
                if !self.view_key(key) {
                    self.pane = 0;
                }
            }
            KeyCode::Enter => self.post_dock_draft(),
            KeyCode::Backspace => {
                self.dock_draft.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.dock_draft.clear(),
            // The chain takes 1024 bytes; the box stops where the post would be refused.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && self.dock_draft.len() + c.len_utf8() <= 1024 => {
                self.dock_draft.push(c);
            }
            _ => {}
        }
    }
}
