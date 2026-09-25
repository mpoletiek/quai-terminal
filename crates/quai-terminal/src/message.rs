//! `message`: private messages (v3), from the account in use (`account use`). Everything here
//! needs the wallet unlocked: an account's keys and local history are sealed under its own key.

use crate::args::MessageCmd;
use crate::commands::Ctx;
use serde_json::json;
use wallet_core::Result;
use wallet_core::messaging::service::{KeyNeed, SyncReport};
use wallet_core::registry::now;
use wallet_core::session::{Session, short_address};
use wallet_core::track::human_duration;

pub async fn run(ctx: &Ctx, cmd: MessageCmd) -> Result<()> {
    match cmd {
        MessageCmd::Status => {
            let s = ctx.unlocked().await?;
            let status = s.messaging_status().await?;
            if ctx.out.json() {
                ctx.out.emit("message status", &status);
                return Ok(());
            }
            let Some(account) = &status.account else {
                println!("this wallet has no Quai account to message from");
                return Ok(());
            };
            let balance = s
                .quai_balances()
                .await
                .ok()
                .and_then(|b| b.into_iter().find(|b| b.address.eq_ignore_ascii_case(account)))
                .map(|b| format!("{} QUAI", wallet_core::amount::quai(b.balance)));
            let label = s.meta.find_quai_account(account).map(|a| format!("{} · ", a.label)).unwrap_or_default();
            println!("account      {label}{account}{}", balance.map(|b| format!(" · {b}")).unwrap_or_default());
            println!("{}", ctx.out.dim("             messages go from the account in use: `account use` changes it"));
            if let Some(f) = &status.fingerprint {
                println!("fingerprint  {f}");
            }
            let need = match status.need {
                KeyNeed::NoKeys => "none on this computer yet: `message keys` publishes this week's".to_string(),
                KeyNeed::Publish => "this week's key is not published: `message keys`".to_string(),
                KeyNeed::Publishing => "this week's key is on its way".to_string(),
                KeyNeed::Ready => "ready".to_string(),
            };
            println!("keys         {need}");
            if let Some((seq, week, _)) = status.key {
                println!("             newest #{seq} (week {week}) · {} held", status.keys_held);
            }
            if let Some(b) = status.scanned_to {
                println!("read up to   block {b}");
            }
            Ok(())
        }
        MessageCmd::Keys { fee } => {
            let mut s = ctx.unlocked().await?;
            publish(ctx, &mut s, fee.max_fee.as_deref()).await
        }
        MessageCmd::Send { peer, text_file, fee } => {
            let text = crate::prompt::message(text_file.as_deref())?;
            let mut s = ctx.unlocked().await?;
            match s.messaging_status().await?.need {
                KeyNeed::Ready | KeyNeed::Publishing => {}
                KeyNeed::Publish | KeyNeed::NoKeys => {
                    // Weekly keys are published as they are used: this week's goes first (the
                    // first one makes the account's identity), and the message follows it (the
                    // account's nonce keeps them in that order).
                    if !ctx.out.json() {
                        println!("this week's messaging key goes first");
                    }
                    publish(ctx, &mut s, fee.max_fee.as_deref()).await?;
                }
            }
            let review = s.review_message(&peer, &text, fee.max_fee.as_deref()).await?;
            let submitted = ctx.authorize(&mut s, review).await?;
            ctx.print_submitted("message send", &submitted);
            Ok(())
        }
        MessageCmd::Sync => {
            let s = ctx.unlocked().await?;
            let report = s.messaging_sync().await?;
            if ctx.out.json() {
                ctx.out.emit("message sync", &report);
            } else {
                print_report(ctx, &s, &report);
            }
            Ok(())
        }
        MessageCmd::List | MessageCmd::Requests => {
            let requests = matches!(cmd, MessageCmd::Requests);
            let s = ctx.unlocked().await?;
            let report = sync_quietly(ctx, &s).await;
            let list = s.messaging_conversations(requests)?;
            if ctx.out.json() {
                ctx.out.emit(if requests { "message requests" } else { "message list" }, &json!({"conversations": list, "sync": report}));
                return Ok(());
            }
            if list.is_empty() {
                println!("{}", if requests { "no requests" } else { "no conversations yet · quai-terminal message send <address>" });
                return Ok(());
            }
            let rows: Vec<Vec<String>> = list
                .iter()
                .map(|c| {
                    let who = c.name.clone().unwrap_or_else(|| short_address(&c.peer));
                    let mut flags = Vec::new();
                    if c.identity_changed {
                        flags.push(ctx.out.red("identity changed"));
                    } else if c.verified {
                        flags.push("verified".into());
                    }
                    if c.unread > 0 {
                        flags.push(ctx.out.green(&format!("{} new", c.unread)));
                    }
                    vec![who, c.messages.to_string(), human_duration(now().saturating_sub(c.last_at)), flags.join(" · ")]
                })
                .collect();
            ctx.out.table(&["who", "messages", "last", ""], &rows);
            if requests {
                println!("{}", ctx.out.dim("`message read <address>` to see one, `message accept` or `message block` to decide"));
            }
            Ok(())
        }
        MessageCmd::Read { peer, keep_unread } => {
            let s = ctx.unlocked().await?;
            let report = sync_quietly(ctx, &s).await;
            let lines = s.messaging_read(&peer, !keep_unread)?;
            if ctx.out.json() {
                ctx.out.emit("message read", &json!({"peer": peer, "messages": lines, "sync": report}));
                return Ok(());
            }
            if lines.is_empty() {
                println!("no messages with {peer}");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = lines
                .iter()
                .map(|l| {
                    let who = if l.outgoing { "you".to_string() } else { peer.clone() };
                    let mut text = l.text.clone();
                    if l.unverified {
                        text = format!("{} {text}", ctx.out.red("[unverified identity]"));
                    }
                    let status = if l.outgoing && l.status != "sent" { l.status.clone() } else { String::new() };
                    vec![human_duration(now().saturating_sub(l.at)), who, text, status]
                })
                .collect();
            ctx.out.table(&["age", "from", "message", ""], &rows);
            Ok(())
        }
        MessageCmd::Accept { peer, name } => {
            let s = ctx.unlocked().await?;
            let address = s.messaging_accept(&peer, name.as_deref())?;
            println!("{} accepted {address}", ctx.out.green("✓"));
            Ok(())
        }
        MessageCmd::Block { peer } => {
            let s = ctx.unlocked().await?;
            let address = s.messaging_block(&peer, true)?;
            println!("{} blocked {address}: its messages are dropped unread from now on", ctx.out.green("✓"));
            Ok(())
        }
        MessageCmd::Unblock { peer } => {
            let s = ctx.unlocked().await?;
            let address = s.messaging_block(&peer, false)?;
            println!("{} unblocked {address}", ctx.out.green("✓"));
            Ok(())
        }
        MessageCmd::Verify { peer, confirm } => {
            let s = ctx.unlocked().await?;
            let f = s.messaging_verify(&peer, confirm).await?;
            if ctx.out.json() {
                ctx.out.emit("message verify", &f);
                return Ok(());
            }
            println!("theirs  {}", f.theirs.as_deref().unwrap_or("(no messaging key published)"));
            println!("yours   {}", f.ours);
            if f.verified {
                println!("{} verified", ctx.out.green("✓"));
            } else {
                println!("{}", ctx.out.dim("compare both in person or over another channel; if they match, run again with --confirm"));
            }
            Ok(())
        }
        MessageCmd::Trust { peer } => {
            let s = ctx.unlocked().await?;
            let fingerprint = s.messaging_trust(&peer)?;
            println!("{} accepted their new identity · fingerprint {fingerprint}", ctx.out.green("✓"));
            println!("{}", ctx.out.dim("compare it with them again: `message verify`"));
            Ok(())
        }
    }
}

async fn publish(ctx: &Ctx, s: &mut Session, max_fee: Option<&str>) -> Result<()> {
    let review = s.review_messaging_keys(max_fee).await?;
    let submitted = ctx.authorize(s, review).await?;
    ctx.print_submitted("message keys", &submitted);
    Ok(())
}

/// Sync before listing; a failure is said and what is stored is still shown.
async fn sync_quietly(ctx: &Ctx, s: &Session) -> Option<SyncReport> {
    match s.messaging_sync().await {
        Ok(r) => Some(r),
        Err(e) => {
            if !ctx.out.json() {
                eprintln!("{} could not read the chain for new messages: {e}", ctx.out.yellow("!"));
            }
            None
        }
    }
}

fn print_report(ctx: &Ctx, s: &Session, r: &SyncReport) {
    if r.arrived.is_empty() && r.requests == 0 {
        println!("nothing new · read up to block {}", r.scanned_to);
    }
    for (peer, n) in &r.arrived {
        let who =
            s.app.contacts().ok().and_then(|c| c.into_iter().find(|c| c.address.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(peer))));
        println!(
            "{} {} from {}",
            ctx.out.green("●"),
            wallet_core::amount::count(*n, "new message"),
            who.map_or_else(|| short_address(peer), |c| c.name)
        );
    }
    if r.requests > 0 {
        println!(
            "{} {} from people you have not accepted · `message requests`",
            ctx.out.green("●"),
            wallet_core::amount::count(r.requests, "message")
        );
    }
    if r.blocked > 0 {
        println!("{}", ctx.out.dim(&format!("{} from blocked addresses dropped", r.blocked)));
    }
    if r.rejected > 0 {
        println!("{}", ctx.out.yellow(&format!("{} dropped: the sender's key was never announced by their address", r.rejected)));
    }
    if r.keys_deleted > 0 {
        println!("{}", ctx.out.dim(&format!("{} old weekly key(s) deleted", r.keys_deleted)));
    }
}
