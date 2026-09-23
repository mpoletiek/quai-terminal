//! Forms: opening them, the fields they need, and submitting them.

use super::*;

impl App {
    pub(crate) fn account_choices(&self) -> Vec<(String, String)> {
        self.dash
            .accounts
            .iter()
            .map(|a| {
                (
                    a.address.clone(),
                    format!(
                        "{} · {} QUAI",
                        a.label,
                        wallet_core::amount::group_thousands(&wallet_core::amount::format_amount_short(a.balance, 18, 4))
                    ),
                )
            })
            .collect()
    }

    pub(crate) fn open_form(&mut self, kind: FormKind) {
        let accounts = self.account_choices();
        let preferred = self.dash.accounts.get(if self.screen == Screen::Accounts { self.selected } else { 0 }).map(|a| a.address.clone());
        let account = |label: &str| {
            let f = Field::new(label, "←/→ to choose").with(preferred.clone().unwrap_or_default());
            if accounts.is_empty() { Field::new(label, "label, address or #").optional() } else { f.choice(accounts.clone()) }
        };
        let directions = vec![("quai_to_qi".to_string(), "QUAI → Qi".to_string()), ("qi_to_quai".to_string(), "Qi → QUAI".to_string())];
        // A title only this arm can build (it names the contract); every other arm is a literal.
        let mut built_title: Option<String> = None;
        let (title, fields, note): (&str, Vec<Field>, Option<&str>) = match &kind {
            FormKind::BoundedSwap { input, .. } => (
                "Swap with explicit limits",
                vec![
                    account("Account"),
                    Field::new("Input amount", "pay token units").with(input.clone()),
                    Field::new("Minimum receive", "receive token units · optional").optional(),
                    Field::new("Maximum impact (bps)", "0 through 10000 · optional").optional(),
                    Field::new("Maximum fee", "QUAI · optional").optional(),
                ],
                Some("At least one explicit limit is required. Fresh route and allowance checks preserve it before each review."),
            ),
            FormKind::StakePosition { name, amount, stake, .. } => {
                built_title = Some(format!("{} {name} LP", if *stake { "Stake" } else { "Unstake" }));
                (
                    "LP position",
                    vec![account("Account"), Field::new("LP amount", "exact partial amount; choose signer first").with(amount.clone())],
                    Some("The selected account's live LP or staked balance bounds execution. Each transaction is reviewed."),
                )
            }
            FormKind::OrderCreate { .. } => (
                "Create limit order",
                super::super::order_ui::fields(account("Account")),
                Some("Fixed input from Swap. Creation signs nothing; each approval or swap requires a fresh review."),
            ),
            FormKind::ExactOutput { from, to } => (
                "Exact-output swap",
                vec![
                    account("Account"),
                    Field::new(&format!("Receive exactly ({to})"), "output amount"),
                    Field::new(&format!("Maximum input ({from})"), "strict spending limit"),
                    Field::new("Maximum fee", "QUAI · optional").optional(),
                ],
                Some("Every approval and swap is reviewed. The output is exact; unused input stays with you or is refunded."),
            ),
            FormKind::SendQuai => (
                "Send QUAI",
                vec![
                    account("From"),
                    Field::new("To", "address or contact name"),
                    Field::new("Amount", "").amount("QUAI"),
                    Field::new("Max fee", "QUAI · leave empty for the network default").optional(),
                ],
                None,
            ),
            FormKind::SendQi => (
                "Send Qi",
                vec![
                    Field::new("To", "payment code, contact or single-output Qi address"),
                    Field::new("Amount", "").amount("QI"),
                    Field::new("Max fee", "Qi · leave empty for the estimate").optional(),
                ],
                Some("Payment codes derive a fresh address for every output."),
            ),
            FormKind::SendToken => (
                "Send token",
                vec![
                    account("From"),
                    Field::new("Token", "symbol or contract"),
                    Field::new("To", "address or contact name"),
                    Field::new("Amount", "token units").amount("TOKEN"),
                ],
                None,
            ),
            FormKind::Approve => (
                "Approve spender",
                vec![
                    Field::new("Token", "symbol or contract"),
                    Field::new("Spender", "contract address"),
                    Field::new("Amount", "exact cap · type `unlimited` for no cap · 0 revokes"),
                ],
                Some("Approvals let the spender move your tokens up to the cap."),
            ),
            FormKind::ConvertQuaiToQi => (
                "Convert QUAI → Qi",
                vec![
                    account("From"),
                    Field::new("Amount", "minimum 10 QUAI").amount("QUAI"),
                    Field::new("Slippage", "empty: automatic from fresh quote; or manual basis points").optional(),
                ],
                Some("Conversions in one prime block share a discount; beyond your slippage it refunds (fee lost). Qi output time-locks."),
            ),
            FormKind::ConvertQiToQuai => (
                "Convert Qi → QUAI",
                vec![
                    account("To"),
                    Field::new("Amount", "").amount("QI"),
                    Field::new("Slippage", "empty: automatic from fresh quote; or manual basis points").optional(),
                ],
                Some("Converted QUAI is locked for the protocol lock period."),
            ),
            FormKind::RemoveLiquidity { name, .. } => (
                "Remove liquidity",
                vec![
                    account("Account"),
                    Field::new(&format!("Percent of your {name} position"), "1 to 100").with("100"),
                    Field::new("Slippage", "basis points (100 = 1%)").with(self.config.swap_slippage_bps.to_string()),
                ],
                Some("Staked LP must be unstaked first — the router can only burn LP held in the account."),
            ),
            FormKind::Incentivize { name, .. } => (
                "Fund pool rewards",
                vec![
                    account("Account"),
                    Field::new("Reward token", "WQUAI, WQI or USDT").with("WQUAI"),
                    Field::new(&format!("Amount to give to {name} stakers"), "").amount("WQUAI"),
                    Field::new("Streamed over (days)", "1 to 365").with("30"),
                ],
                Some(
                    "This gives the tokens away: they go to whoever stakes LP in this pool. The gauge has no recover function, so it cannot be undone.",
                ),
            ),
            FormKind::CurveBuy { symbol, .. } => (
                "Buy on the bonding curve",
                vec![
                    account("Account"),
                    Field::new(&format!("QUAI to spend on {symbol}"), "").amount("QUAI"),
                    Field::new("Slippage", "basis points (100 = 1%)").with(self.config.swap_slippage_bps.to_string()),
                ],
                Some("The curve quotes it before the review. A new token's price is set by its curve alone."),
            ),
            FormKind::CurveSell { symbol, held, .. } => (
                "Sell to the bonding curve",
                vec![
                    account("Account"),
                    Field::new(&format!("{symbol} to sell"), "whole tokens · prefilled with what you hold").with(held.clone()),
                    Field::new("Slippage", "basis points (100 = 1%)").with(self.config.swap_slippage_bps.to_string()),
                ],
                Some(
                    "Two steps: an exact approval for the curve, then the sale. Quainance credits QUAI for a later claim; Hartii pays QUAI directly.",
                ),
            ),
            FormKind::WrapQi => (
                "Wrap Qi → WQI (step 1 of 2)",
                vec![account("Beneficiary"), Field::new("Amount", "").amount("QI")],
                Some("After settlement, claim WQI (m)."),
            ),
            FormKind::UnwrapWqi => ("Unwrap WQI → Qi", vec![account("Account"), Field::new("Amount", "whole Qi").amount("WQI")], None),
            FormKind::WrapQuai => ("Wrap QUAI → WQUAI", vec![account("Account"), Field::new("Amount", "").amount("QUAI")], None),
            FormKind::UnwrapQuai => ("Unwrap WQUAI → QUAI", vec![account("Account"), Field::new("Amount", "").amount("WQUAI")], None),
            FormKind::Notify => (
                "Notify payment peer",
                vec![account("Gas from"), Field::new("Peer", "payment code or contact")],
                Some("Public: links your payment code to the peer on-chain."),
            ),
            FormKind::BoardPost { channel } => (
                "Post a message",
                vec![account("Post from"), Field::new(&format!("Message to #{channel}"), "up to 1024 bytes")],
                Some("Public and permanent: anyone can read it, it cannot be taken back, and it is signed by this account."),
            ),
            FormKind::BoardDm { peer, name } => (
                "Send a sealed message",
                vec![
                    account("Send from"),
                    Field::new(
                        &format!("Message to {}", name.clone().unwrap_or_else(|| wallet_core::session::short_code(peer))),
                        "only you two can read it",
                    ),
                ],
                Some("Encrypted, but not hidden: your address, the time and the size are public, and it cannot be taken back."),
            ),
            FormKind::FollowChannel => (
                "Follow a channel",
                vec![Field::new("Channel", "a name, up to 32 bytes")],
                Some("A channel is just a name: following one only decides what this wallet shows."),
            ),
            FormKind::RenameWallet(id) => {
                let current = self.wallets.iter().find(|w| &w.id == id).map(|w| w.name.clone()).unwrap_or_default();
                ("Rename wallet", vec![Field::new("Name", "letters, digits, - and _").with(&current)], None)
            }
            FormKind::AddAccount => ("Add Quai account", vec![Field::new("Label", "e.g. Savings").optional()], None),
            FormKind::RenameAccount(_) => ("Rename account", vec![Field::new("Label", "")], None),
            FormKind::NewQiAddress => ("New Qi / mining address", vec![Field::new("Label", "e.g. mining rig").with("mining")], None),
            FormKind::Contact(original) => {
                let existing = original.as_ref().and_then(|n| self.dash.contacts.iter().find(|c| c.name == *n)).cloned();
                (
                    if original.is_some() { "Edit contact" } else { "Add contact" },
                    vec![
                        Field::new("Name", "how you'll pick them when sending")
                            .with(existing.as_ref().map(|c| c.name.clone()).unwrap_or_default()),
                        Field::new("Address", "Quai or Qi address (0x…)")
                            .with(existing.as_ref().and_then(|c| c.address.clone()).unwrap_or_default())
                            .optional(),
                        Field::new("Payment code", "PM8T… for private Qi payments")
                            .with(existing.as_ref().and_then(|c| c.payment_code.clone()).unwrap_or_default())
                            .optional(),
                        Field::new("Note", "").with(existing.map(|c| c.note).unwrap_or_default()).optional(),
                    ],
                    Some("An address, a payment code, or both. A payment code also lets the wallet find their payments automatically."),
                )
            }
            FormKind::ContactFromPeer { code, address } => {
                let existing = self.dash.contacts.iter().find(|c| c.payment_code.as_deref() == Some(code.as_str())).cloned();
                (
                    if existing.is_some() { "Update contact" } else { "Name this contact" },
                    vec![
                        Field::new("Name", "how you'll pick them when sending")
                            .with(existing.as_ref().map(|c| c.name.clone()).unwrap_or_default()),
                        Field::new("Address", "the account this message came from")
                            .with(address.clone().or_else(|| existing.as_ref().and_then(|c| c.address.clone())).unwrap_or_default())
                            .optional(),
                        Field::new("Payment code", "").with(code.clone()),
                        Field::new("Note", "").with(existing.map(|c| c.note).unwrap_or_default()).optional(),
                    ],
                    Some(
                        "The payment code is who they are; the address is one account they write from. Both are kept, and later accounts are added as they appear.",
                    ),
                )
            }
            FormKind::ImportToken => ("Import token", vec![Field::new("Contract address", "0x…")], None),
            FormKind::DeepScan => {
                ("Deep scan Qi", vec![Field::new("Scan to raw index", "").with("200000")], Some("Deep scans can take a while."))
            }
            FormKind::ExportPhrase => (
                "Reveal recovery phrase",
                vec![Field::new("Password", "re-enter your wallet password").secret()],
                Some("Make sure nobody can see your screen."),
            ),
            FormKind::Backup => (
                "Encrypted backup",
                vec![
                    Field::new("File", "").with(default_backup_path()),
                    Field::new("Backup password", "separate from the wallet password").new_secret(),
                ],
                None,
            ),
            FormKind::NftTransfer { contract, token_id, multi } => (
                "Transfer NFT",
                {
                    let mut f = vec![account("From"), Field::new("To", "Quai address or contact name")];
                    if *multi {
                        f.push(Field::new("Quantity", "whole number").with("1"));
                    }
                    let _ = (contract, token_id);
                    f
                },
                Some("Ownership is re-checked on-chain; NFT transfers cannot be undone."),
            ),
            FormKind::Monitor { network } => (
                "Monitoring endpoint",
                {
                    let current = self.config.monitor_endpoints.get(network).cloned();
                    let pathing = vec![
                        ("exact".to_string(), "exact Cyprus-1 URL".to_string()),
                        ("gateway".to_string(), "gateway base (derives /cyprus1)".to_string()),
                    ];
                    let mut mode = Field::new("URL type", "←/→");
                    if current.as_ref().is_some_and(|c| c.use_pathing) {
                        mode = mode.with("gateway");
                    }
                    vec![
                        Field::new("URL", "e.g. http://10.0.0.12:9200 · empty clears")
                            .with(current.map(|c| c.rpc_url).unwrap_or_default())
                            .optional(),
                        mode.choice(pathing),
                    ]
                },
                Some(
                    "Market data, charts and balance re-reads use it; reviews, signing and broadcasting always use the network's main RPC. The endpoint must report this network's chain id and genesis.",
                ),
            ),
            FormKind::DaemonUnlock => (
                "Unlock in the daemon",
                vec![Field::new("Password", "this wallet's password").secret()],
                Some(
                    "The daemon then holds this wallet unlocked after the terminal closes: it runs its interval conversions and reads its sealed chats. The password goes over the daemon's private socket, only after checking the other end is your daemon, and is not stored. quai-terminal daemon lock takes it back.",
                ),
            ),
            FormKind::Alert { name, .. } => (
                "Set an alert",
                vec![
                    Field::new("When", "←/→").choice(vec![
                        ("above".into(), format!("{name} rises to")),
                        ("below".into(), format!("{name} falls to")),
                        ("moves".into(), format!("{name} moves in 24h by (%)")),
                    ]),
                    Field::new("Value", "a price in the quote token, or a percentage"),
                ],
                Some(
                    "It fires once when the line is crossed, and again only after it has crossed back. The daemon checks while it runs; otherwise this window checks every minute while unlocked.",
                ),
            ),
            FormKind::ContractCall { address, name, functions } => {
                let choices: Vec<(String, String)> = functions.iter().map(|c| (c.signature.clone(), c.label())).collect();
                let first = functions.first().cloned();
                let mut fields = vec![account("From"), Field::new("Function", "←/→ to choose").choice(choices)];
                if let Some(c) = &first {
                    if c.payable {
                        fields.push(Field::new("QUAI to send", "this function accepts QUAI").optional().amount("QUAI"));
                    }
                    for (arg, ty) in &c.inputs {
                        let label = if arg.is_empty() { ty.clone() } else { format!("{arg} ({ty})") };
                        fields.push(Field::new(&label, &argument_hint(ty)));
                    }
                }
                built_title = Some(format!("Call {name} · {}", wallet_core::session::short_code(address)));
                (
                    "",
                    fields,
                    Some(
                        "The arguments are typed against the ABI this contract publishes about itself. That proves what was published, not what the deployed code does — the review shows the exact call data that gets signed.",
                    ),
                )
            }
            FormKind::IpfsGateway(content) => {
                use wallet_core::ipfs::Content;
                let current = match content {
                    Content::Abi => self.config.abi_ipfs_gateway.clone(),
                    Content::Media => self.config.ipfs_gateway.clone(),
                };
                let hint = format!("http://127.0.0.1:8080 · https://{{cid}}.ipfs.dweb.link · empty = {}", content.default_gateway());
                (
                    match content {
                        Content::Abi => "IPFS gateway for contract ABIs",
                        Content::Media => "IPFS gateway for images and NFT metadata",
                    },
                    vec![Field::new("URL", &hint).with(current.unwrap_or_default()).optional()],
                    Some(match content {
                        Content::Abi => {
                            "Where a contract's own metadata — the ABI its bytecode names by CID — is fetched from. ipfs.qu.ai is the authority for these: it is what Quai's deploy tooling pins to and what Quaiscan verifies against, so leaving this alone is right unless you run a node that pins Quai contract metadata itself. It is tested before it is saved."
                        }
                        Content::Media => {
                            "Where NFT images and metadata on IPFS are fetched from. This is the one worth pointing at your own node (Kubo's gateway, usually port 8080): it is the bulk of the fetching and the most revealing. A node on this machine or your network may be plain http and is reached directly, not through the proxy; a public gateway must be https. It is tested before it is saved: a file is fetched through it and checked against its CID."
                        }
                    }),
                )
            }
            FormKind::NftList { name, current, .. } => (
                if current.is_some() { "Change listing price" } else { "List for sale" },
                {
                    let currencies = ["QUAI", "WQI", "WQUAI", "USDT"].iter().map(|c| (c.to_string(), c.to_string())).collect();
                    let (price, currency) = current.clone().unwrap_or_default();
                    let mut currency_field = Field::new("Currency", "←/→");
                    if !currency.is_empty() {
                        currency_field = currency_field.with(currency);
                    }
                    let _ = name;
                    vec![Field::new("Price", "e.g. 250").with(price), currency_field.choice(currencies)]
                },
                Some(
                    "Bazarr shows it within a minute. Anyone can buy at this price until you cancel (X). Approvals, if needed, come first as their own reviews.",
                ),
            ),
            FormKind::Quote => (
                "Conversion quote",
                vec![Field::new("Direction", "←/→").choice(directions), Field::new("Amount", "source asset")],
                Some("Shows the node quote and batch-discount scenarios; nothing is signed."),
            ),
        };
        let focus = fields.iter().position(|f| f.value.is_empty() && !matches!(f.kind, FieldKind::Choice(_))).unwrap_or(0);
        self.contract_probe = None;
        self.contract_found = None;
        self.contract_asked.clear();
        self.modal = Modal::Form(Form {
            kind,
            title: built_title.unwrap_or_else(|| title.into()),
            fields,
            focus,
            note: note.map(str::to_string),
            contract_note: None,
            pending: false,
            error: None,
            error_field: None,
        });
        self.dirty = true;
    }

    pub(crate) fn submit_form(&mut self, form: &Form) {
        let v = |i: usize| form.fields.get(i).map(|f| f.value.trim().to_string()).unwrap_or_default();
        let opt = |i: usize| Some(v(i)).filter(|s| !s.is_empty());
        let bps = |i: usize| v(i).parse::<u16>().unwrap_or(self.config.swap_slippage_bps);
        if matches!(form.kind, FormKind::ConvertQuaiToQi | FormKind::ConvertQiToQuai) {
            let direction = if form.kind == FormKind::ConvertQiToQuai {
                wallet_core::qi_market::Direction::QiToQuai
            } else {
                wallet_core::qi_market::Direction::QuaiToQi
            };
            self.start_protocol_conversion(direction, v(1), opt(2).and_then(|v| v.parse().ok()), opt(0));
            return;
        }
        if let FormKind::Monitor { network } = &form.kind {
            self.set_monitor(network, &v(0), v(1) == "gateway");
            return;
        }
        if let FormKind::IpfsGateway(content) = &form.kind {
            self.set_ipfs_gateway(*content, &v(0));
            return;
        }
        if let FormKind::DaemonUnlock = &form.kind {
            if let Some(id) = self.meta.as_ref().map(|m| m.id.clone()) {
                // The form's own buffer is wiped when the form drops; this copy, when the task ends.
                let password = Zeroizing::new(form.fields[0].value.clone());
                self.hand_to_daemon(id, password);
            }
            return;
        }
        if let FormKind::Alert { pool, name, inverted } = &form.kind {
            let Ok(value) = v(1).parse::<f64>() else {
                self.toast("the value is a number, like 125 or 10", true);
                return;
            };
            if !(value.is_finite() && value > 0.0) {
                self.toast("the value must be above zero", true);
                return;
            }
            use wallet_core::alerts::{Alert, Rule};
            let rule = match v(0).as_str() {
                "below" => Rule::Below { price: value },
                "moves" => Rule::Moves { pct: value },
                _ => Rule::Above { price: value },
            };
            let alert = Alert { id: 0, pool: pool.clone(), name: name.clone(), inverted: *inverted, rule, active: false, fired: 0 };
            self.send_data(super::super::data::DataCmd::Alerts(super::super::data::AlertOp::Add(Box::new(alert))));
            return;
        }
        if let FormKind::NftList { contract, token_id, owner, name, current } = &form.kind {
            let price = v(0);
            let currency = v(1);
            let label = if current.is_some() {
                format!("re-price {name} to {price} {currency}")
            } else {
                format!("list {name} for {price} {currency}")
            };
            self.start_flow(super::super::eco::FlowKind::NftList {
                account: Some(owner.clone()),
                contract: contract.clone(),
                token_id: token_id.clone(),
                price: Some(price),
                currency,
                label,
            });
            return;
        }
        if let FormKind::RenameWallet(id) = &form.kind {
            let name = v(0);
            let Some(mut meta) = self.wallets.iter().find(|w| &w.id == id).cloned() else {
                self.toast("that wallet is gone", true);
                return;
            };
            match self.registry.rename(&mut meta, &name) {
                Ok(()) => {
                    // The open wallet keeps its new name in the header and as the default.
                    if self.meta.as_ref().is_some_and(|m| m.id == meta.id) {
                        self.config.default_wallet = Some(meta.name.clone());
                        self.save_config();
                        self.meta = Some(meta.clone());
                        self.dash.meta = Some(meta.clone());
                    }
                    self.load_wallets();
                    self.toast(format!("renamed to `{}`", meta.name), false);
                }
                Err(e) => self.toast(friendly_error(&e.to_string()), true),
            }
            return;
        }
        if let FormKind::FollowChannel = &form.kind {
            self.follow_channel(&v(0));
            return;
        }
        // Sequences: the worker answers each of these with the next review it needs — an exact
        // approval while one is outstanding, then the operation. Sent as a bare command they
        // would stop after the approval, so they go through the flow driver.
        let steps = match &form.kind {
            FormKind::BoundedSwap { from, to, .. } => Some((
                Prepare::Trading {
                    intent: wallet_core::execution::TradingIntent {
                        account: v(0),
                        max_fee: opt(4),
                        action: wallet_core::execution::TradingAction::BoundedSwap {
                            from: from.clone(),
                            to: to.clone(),
                            amount: v(1),
                            slippage: self.config.swap_slippage_bps,
                            deadline: self.config.swap_deadline_minutes,
                            bounds: wallet_core::swap::SwapBounds {
                                minimum_output: opt(2),
                                maximum_impact_bps: opt(3).and_then(|v| v.parse().ok()),
                            },
                        },
                    },
                },
                "bounded swap".into(),
            )),
            FormKind::StakePosition { pair, gauge, name, stake, .. } => Some((
                if *stake {
                    Prepare::StakeNext { account: opt(0), pair: pair.clone(), gauge: gauge.clone(), amount: v(1) }
                } else {
                    Prepare::Unstake { account: opt(0), pair: pair.clone(), gauge: gauge.clone(), amount: v(1) }
                },
                format!("{} {name} LP", if *stake { "stake" } else { "unstake" }),
            )),
            FormKind::ExactOutput { from, to } => Some((
                Prepare::Trading {
                    intent: wallet_core::execution::TradingIntent {
                        account: v(0),
                        max_fee: opt(3),
                        action: wallet_core::execution::TradingAction::ExactOutput {
                            from: from.clone(),
                            to: to.clone(),
                            output: v(1),
                            max_input: v(2),
                            deadline: self.config.swap_deadline_minutes,
                        },
                    },
                },
                format!("exact output {from} → {to}"),
            )),
            FormKind::RemoveLiquidity { pair, name } => Some((
                Prepare::RemoveLiquidityNext {
                    account: opt(0),
                    pair: pair.clone(),
                    percent: v(1).parse().unwrap_or(100),
                    slippage: bps(2),
                    deadline: self.config.swap_deadline_minutes,
                },
                format!("remove liquidity from {name}"),
            )),
            FormKind::CurveSell { token, symbol, curve, .. } => Some((
                Prepare::CurveSellNext {
                    account: opt(0),
                    token: token.clone(),
                    symbol: symbol.clone(),
                    curve: curve.clone(),
                    amount: v(1),
                    slippage: bps(2),
                    deadline: Some(self.config.swap_deadline_minutes),
                },
                format!("sell {symbol} to its curve"),
            )),
            FormKind::Incentivize { pair, name } => Some((
                Prepare::IncentivizeNext {
                    account: opt(0),
                    pair: pair.clone(),
                    token: v(1),
                    amount: v(2),
                    days: v(3).parse().unwrap_or(30),
                },
                format!("fund {name} rewards"),
            )),
            _ => None,
        };
        if let Some((prepare, label)) = steps {
            self.start_flow(super::super::eco::FlowKind::Steps { prepare: Box::new(prepare), label });
            return;
        }
        let cmd = match &form.kind {
            FormKind::SendQuai => Cmd::Prepare(Prepare::SendQuai { from: opt(0), to: v(1), amount: v(2), max_fee: opt(3) }),
            FormKind::SendQi => Cmd::Prepare(Prepare::SendQi { to: v(0), amount: v(1), max_fee: opt(2) }),
            FormKind::SendToken => Cmd::Prepare(Prepare::SendToken { from: opt(0), token: v(1), to: v(2), amount: v(3) }),
            FormKind::Approve => Cmd::Prepare(Prepare::Approve {
                token: v(0),
                spender: v(1),
                amount: Some(v(2)).filter(|a| !a.eq_ignore_ascii_case("unlimited")),
            }),
            FormKind::ConvertQuaiToQi | FormKind::ConvertQiToQuai => unreachable!("handled above as core plans"),
            FormKind::WrapQi => Cmd::Prepare(Prepare::WrapQi { account: opt(0), amount: v(1) }),
            FormKind::UnwrapWqi => Cmd::Prepare(Prepare::UnwrapWqi { account: opt(0), amount: v(1) }),
            FormKind::WrapQuai => Cmd::Prepare(Prepare::WrapQuai { account: opt(0), amount: v(1) }),
            FormKind::UnwrapQuai => Cmd::Prepare(Prepare::UnwrapQuai { account: opt(0), amount: v(1) }),
            FormKind::Notify => Cmd::Prepare(Prepare::Notify { from: opt(0), peer: v(1) }),
            FormKind::BoardPost { channel } => Cmd::Prepare(Prepare::BoardPost { from: opt(0), channel: channel.clone(), text: v(1) }),
            FormKind::BoardDm { peer, .. } => Cmd::Prepare(Prepare::BoardDm { from: opt(0), peer: peer.clone(), text: v(1) }),
            FormKind::OrderCreate { .. } | FormKind::FollowChannel | FormKind::RenameWallet(_) => unreachable!("handled above"),
            FormKind::AddAccount => Cmd::AddAccount(opt(0)),
            FormKind::RenameAccount(a) => Cmd::RenameAccount { account: a.clone(), label: v(0) },
            FormKind::NewQiAddress => Cmd::NewQiAddress(opt(0)),
            FormKind::Contact(original) => {
                Cmd::SaveContact { original: original.clone(), name: v(0), address: opt(1), code: opt(2), note: v(3) }
            }
            FormKind::ContactFromPeer { code, .. } => {
                // Editing the person already behind this code, when there is one: the code is
                // the identity, so this must not create a second contact holding it.
                let original = self.dash.contacts.iter().find(|c| c.payment_code.as_deref() == Some(code.as_str())).map(|c| c.name.clone());
                Cmd::SaveContact { original, name: v(0), address: opt(1), code: opt(2), note: v(3) }
            }
            FormKind::ImportToken => Cmd::ImportToken(v(0)),
            FormKind::DeepScan => Cmd::ScanQi { deep: v(0).parse().ok() },
            FormKind::ExportPhrase => Cmd::ExportPhrase(Zeroizing::new(v(0))),
            FormKind::Backup => Cmd::Backup { path: v(0), password: Zeroizing::new(v(1)) },
            FormKind::Quote => Cmd::Quote { direction: v(0), amount: v(1) },
            FormKind::ContractCall { address, functions, .. } => {
                let signature = v(1);
                let Some(callable) = functions.iter().find(|c| c.signature == signature) else { return };
                let mut rest = form.fields.iter().skip(2);
                let value = callable.payable.then(|| rest.next().map(|f| f.value.trim().to_string())).flatten();
                let args: Vec<String> = rest.map(|f| f.value.trim().to_string()).collect();
                Cmd::Prepare(Prepare::ContractCall {
                    account: opt(0),
                    address: address.clone(),
                    signature,
                    args,
                    value: value.filter(|v| !v.is_empty()),
                })
            }
            FormKind::NftList { .. }
            | FormKind::Monitor { .. }
            | FormKind::IpfsGateway(_)
            | FormKind::Alert { .. }
            | FormKind::DaemonUnlock => {
                return;
            }
            FormKind::BoundedSwap { .. }
            | FormKind::StakePosition { .. }
            | FormKind::ExactOutput { .. }
            | FormKind::RemoveLiquidity { .. }
            | FormKind::Incentivize { .. }
            | FormKind::CurveSell { .. } => {
                unreachable!("handled above as sequences")
            }
            FormKind::CurveBuy { token, symbol, curve } => Cmd::Prepare(Prepare::CurveBuy {
                account: opt(0),
                token: token.clone(),
                symbol: symbol.clone(),
                curve: curve.clone(),
                amount: v(1),
                slippage: bps(2),
                deadline: Some(self.config.swap_deadline_minutes),
            }),
            FormKind::NftTransfer { contract, token_id, multi } => Cmd::Prepare(Prepare::NftTransfer {
                account: opt(0),
                contract: contract.clone(),
                token_id: token_id.clone(),
                to: v(1),
                quantity: if *multi { opt(2) } else { None },
            }),
        };
        self.send(cmd);
    }

    /// What a probed destination turned out to be, as the line the form carries above its fields.
    /// `None` when there is nothing worth saying: a form with no destination in it, no answer yet,
    /// or an answer that came back a plain account.
    ///
    /// Takes the answer rather than reading `self.modal`, so both the key path (which has the form
    /// in hand) and the event path (which has it in `self.modal`) can use the one builder.
    pub(crate) fn contract_note(kind: &FormKind, found: Option<&wallet_core::contracts::Discovered>) -> Option<String> {
        Self::destination_field(kind)?;
        let found = found.filter(|f| f.is_contract())?;
        Some(match &found.metadata {
            Some(m) => format!("{} · a contract, not a wallet — ^F to call one of its functions", m.name),
            None => "a contract, not a wallet · it publishes no ABI, so it cannot be called from here".to_string(),
        })
    }

    /// The channel offer under the cursor on Channels (offers are listed first).
    pub(crate) fn channel_offer(&self) -> Option<&wallet_core::ops::ChannelOffer> {
        self.dash.offers.get(self.selected)
    }

    /// The registered channel under the cursor on Channels, below the offers.
    pub(crate) fn channel_peer(&self) -> Option<&wallet_core::ops::PeerView> {
        self.selected.checked_sub(self.dash.offers.len()).and_then(|i| self.dash.peers.get(i))
    }

    /// Save a payment channel's sender as a contact, or edit the contact it already belongs to.
    pub(crate) fn save_channel_contact(&mut self) {
        let Some(p) = self.channel_peer() else { return };
        let code = p.code.clone();
        match self.dash.contacts.iter().find(|c| c.payment_code.as_deref() == Some(code.as_str())).map(|c| c.name.clone()) {
            Some(name) => self.open_form(FormKind::Contact(Some(name))),
            None => {
                self.open_form(FormKind::Contact(None));
                if let Modal::Form(f) = &mut self.modal {
                    f.fields[2].value = code;
                    f.focus = 0;
                }
            }
        }
    }

    /// Catch amounts above the known spendable balance before asking the node.
    pub(crate) fn check_available(&self, form: &Form) -> Result<(), (usize, String)> {
        let account =
            form.fields.iter().find(|f| matches!(f.kind, FieldKind::Choice(_)) && f.value.starts_with("0x")).map(|f| f.value.clone());
        for (i, f) in form.fields.iter().enumerate() {
            let FieldKind::Amount(asset) = f.kind else { continue };
            let (have, decimals, unit) = match asset {
                "QUAI" => {
                    let a =
                        account.as_ref().and_then(|v| self.dash.accounts.iter().find(|a| a.address == *v)).or(self.dash.accounts.first());
                    match a {
                        Some(a) => (a.balance, 18, "QUAI"),
                        None => continue,
                    }
                }
                "QI" => match &self.dash.qi {
                    Some(q) => (q.balance.spendable, 3, "Qi"),
                    None => continue,
                },
                _ => continue,
            };
            let Ok(want) = wallet_core::amount::parse_amount(f.value.trim(), decimals) else { continue };
            if want > have {
                let shown = wallet_core::amount::group_thousands(&wallet_core::amount::format_amount_short(have, decimals, 4));
                return Err((i, format!("more than the {shown} {unit} available")));
            }
        }
        Ok(())
    }

    /// Test an IPFS gateway on its own thread, then save it (`url` empty: back to ipfs.io).
    ///
    /// Open the call form for a contract the send form found, on its first writable function.
    /// `false` when there was nothing to open, so the caller can leave the form it was on alone
    /// rather than closing it over a toast.
    ///
    /// Takes the answer by value: it can carry the contract's whole literal source.
    pub(crate) fn open_contract_call(&mut self, found: wallet_core::contracts::Discovered) -> bool {
        let Some(metadata) = &found.metadata else { return false };
        let Ok(interface) = metadata.interface() else {
            self.toast("this contract's ABI could not be read", true);
            return false;
        };
        // Reads are answered, not signed; this form is for the ones that cost something.
        let functions: Vec<wallet_core::contracts::Callable> =
            wallet_core::contracts::callables(&interface).into_iter().filter(|c| !c.read_only).collect();
        if functions.is_empty() {
            self.toast(format!("{} declares nothing that can be called", metadata.name), true);
            return false;
        }
        self.open_form(FormKind::ContractCall { address: found.address.clone(), name: metadata.name.clone(), functions });
        // `open_form` clears the probe; this form is about that contract, so it keeps it.
        self.contract_found = Some(found);
        true
    }

    /// Rebuild the argument fields under the chosen function, keeping the account and any value
    /// already typed. Each function needs its own arguments, so the form changes shape with it.
    pub(crate) fn rebuild_contract_fields(&mut self, form: &mut Form) {
        let FormKind::ContractCall { functions, .. } = &form.kind else { return };
        let Some(chosen) = form.fields.iter().find(|f| f.label == "Function").map(|f| f.value.clone()) else { return };
        let Some(callable) = functions.iter().find(|c| c.signature == chosen).cloned() else { return };
        let keep = |label: &str| form.fields.iter().find(|f| f.label == label).map(|f| f.value.clone()).unwrap_or_default();
        let (account, value) = (keep("From"), keep("QUAI to send"));
        let mut fields: Vec<Field> = form.fields.iter().take(2).cloned().collect();
        if callable.payable {
            fields.push(Field::new("QUAI to send", "this function accepts QUAI").with(value).optional().amount("QUAI"));
        }
        for (name, ty) in &callable.inputs {
            let label = if name.is_empty() { ty.clone() } else { format!("{name} ({ty})") };
            fields.push(Field::new(&label, &argument_hint(ty)));
        }
        if let Some(f) = fields.first_mut() {
            f.value = account;
        }
        form.focus = form.focus.min(fields.len().saturating_sub(1));
        form.fields = fields;
        form.error = None;
        form.error_field = None;
    }

    /// Which form field holds a destination worth asking the chain about, if any.
    pub(crate) fn destination_field(kind: &FormKind) -> Option<&'static str> {
        match kind {
            FormKind::SendQuai | FormKind::SendToken => Some("To"),
            FormKind::Approve => Some("Spender"),
            _ => None,
        }
    }

    /// Ask what a send destination is, once it looks like a finished address. Sending QUAI to a
    /// contract with no payable fallback burns the fee for nothing, and a contract someone means
    /// to *use* needs a different form than a transfer — so the form finds out while they type
    /// rather than after they have signed.
    pub(crate) fn probe_destination(&mut self, form: &Form) {
        let Some(label) = Self::destination_field(&form.kind) else { return };
        let Some(text) = form.fields.iter().find(|f| f.label == label).map(|f| f.value.trim().to_string()) else { return };
        // Only a complete address; a contact name resolves to one the worker looks up itself.
        let looks_done = text.len() >= 42 && text.starts_with("0x");
        if !looks_done {
            self.contract_found = None;
            self.contract_probe = None;
            return;
        }
        // Asked once per destination per form, whatever the answer was. A failure is an answer.
        if self.contract_probe.as_deref() == Some(text.as_str()) || !self.contract_asked.insert(text.to_lowercase()) {
            return;
        }
        self.contract_found = None;
        self.contract_probe = Some(text.clone());
        self.send(Cmd::InspectContract { address: text });
    }

    /// Put what the probe found under the open form, without disturbing what is typed in it.
    pub(crate) fn refresh_form_note(&mut self) {
        let Modal::Form(mut form) = std::mem::replace(&mut self.modal, Modal::None) else { return };
        form.contract_note = Self::contract_note(&form.kind, self.contract_found.as_ref());
        self.modal = Modal::Form(form);
        self.dirty = true;
    }
}
