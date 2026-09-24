//! Chat subscriptions and the pinned chat.
//!
//! A subscription asks to be told when someone else says something in a board channel or a sealed
//! conversation. A channel's notification carries who said what, since the channel is public
//! anyway; a sealed conversation's says only who wrote and how many times, because notifications
//! are stored in plain text and reach the desktop. The pin is
//! the one chat the TUI docks beside every screen. Both belong to this wallet (a conversation is
//! with its payment code), so they live in its database, per network.
//!
//! Channels are public reads, so a locked daemon watches them too. A sealed conversation opens
//! only with this wallet's payment key, so it is checked only while unlocked.

use crate::error::Result;
use crate::session::Session;

/// A chat by name: `#channel` or `dm:<payment code>`.
pub fn channel_target(name: &str) -> String {
    format!("#{name}")
}

pub fn dm_target(code: &str) -> String {
    format!("dm:{code}")
}

fn subs_key(network: &str) -> String {
    format!("chat_subs:{network}")
}

fn pin_key(network: &str) -> String {
    format!("chat_pin:{network}")
}

fn seen_key(network: &str, target: &str) -> String {
    format!("chat_seen:{network}:{target}")
}

/// Most messages one chat puts in a single notification; the rest are counted.
const PER_NOTICE: usize = 3;

/// What replaced the text of sealed messages that older versions stored in notifications.
pub const REDACTED_NOTICE: &str = "(message text removed)";

/// A sealed conversation's notification: how many arrived, never what they said.
pub fn private_summary(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some("1 new message".into()),
        n => Some(format!("{n} new messages")),
    }
}

/// One message in a chat's news.
#[derive(Clone, Debug)]
pub struct ChatLine {
    /// Unix seconds.
    pub at: u64,
    /// Which message it is, the same for every wallet that reads it (`tx:index` for a channel
    /// post; empty for a sealed line, which only this wallet can read).
    pub id: String,
    /// Sender address, lowercase.
    pub from: String,
    /// The sender as this wallet names them.
    pub who: String,
    pub text: String,
}

/// What one subscribed chat said since this wallet last looked.
#[derive(Clone, Debug)]
pub struct ChatNews {
    /// `#channel` or `dm:<code>`.
    pub target: String,
    /// `#channel`, or `<name> · sealed`.
    pub title: String,
    pub body: String,
    /// The messages behind `body`, oldest first.
    pub fresh: Vec<ChatLine>,
    /// The notification it was stored as in this wallet.
    pub notice: Option<i64>,
}

impl ChatNews {
    /// A public channel, which reads the same from every wallet.
    pub fn is_channel(&self) -> bool {
        self.target.starts_with('#')
    }
}

impl Session {
    /// Subscribed chats; none until the user subscribes. A public channel is not subscribed on
    /// its own: anyone can post to it for the price of a transaction, and a subscription puts
    /// what they write on the desktop under this app's name.
    pub fn chat_subscriptions(&self) -> Vec<String> {
        self.app.kv(&subs_key(&self.network.id)).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Subscribe to a chat, or stop; returns whether it is subscribed now.
    pub fn toggle_chat_subscription(&self, target: &str) -> Result<bool> {
        let mut list = self.chat_subscriptions();
        let on = if let Some(i) = list.iter().position(|t| t == target) {
            list.remove(i);
            false
        } else {
            list.push(target.to_string());
            true
        };
        self.app.set_kv(&subs_key(&self.network.id), &serde_json::to_string(&list).unwrap_or_default())?;
        Ok(on)
    }

    /// The pinned chat, if any.
    pub fn chat_pin(&self) -> Option<String> {
        self.app.kv(&pin_key(&self.network.id)).ok().flatten().filter(|s| !s.is_empty())
    }

    /// Pin a chat (`None` unpins).
    pub fn set_chat_pin(&self, target: Option<&str>) -> Result<()> {
        self.app.set_kv(&pin_key(&self.network.id), target.unwrap_or(""))
    }

    /// Read every subscribed chat for messages from someone else since the last look, add a
    /// notification per chat that has any, and return them as (title, body). A chat's first look
    /// only marks where it stands. Sealed conversations are skipped while locked.
    pub async fn chat_news(&self) -> Result<Vec<ChatNews>> {
        self.chat_news_where(false).await
    }

    /// [`Session::chat_news`], sealed conversations only when `dms_only` — for a window whose
    /// daemon already reads the channels but cannot open this wallet's conversations.
    pub async fn chat_news_where(&self, dms_only: bool) -> Result<Vec<ChatNews>> {
        let subs: Vec<String> = self.chat_subscriptions().into_iter().filter(|t| !dms_only || t.starts_with("dm:")).collect();
        if subs.is_empty() || self.network.ecosystem.messages.is_none() {
            return Ok(Vec::new());
        }
        let ctx = self.data_ctx()?;
        let mine: Vec<String> = self.meta.quai_owner_addresses().iter().map(|a| a.to_lowercase()).collect();
        let contacts = self.app.contacts().unwrap_or_default();
        // Every account a contact is known by, to tell a public post from someone the user knows
        // apart from one by anybody at all.
        let known: Vec<String> = contacts
            .iter()
            .flat_map(|c| c.address.iter().cloned().chain(self.app.contact_addresses(c.id).unwrap_or_default()))
            .map(|a| a.to_lowercase())
            .collect();
        let who = |address: &str| {
            let a = address.to_lowercase();
            contacts
                .iter()
                .find(|c| c.address.as_ref().is_some_and(|x| x.to_lowercase() == a))
                .map(|c| c.name.clone())
                .unwrap_or_else(|| crate::session::short_address(address))
        };
        let mut news = Vec::new();
        for target in subs {
            // Everything read, oldest first.
            let (title, lines): (String, Vec<ChatLine>) = if let Some(name) = target.strip_prefix('#') {
                let Ok(tag) = crate::messages::channel_tag(name) else { continue };
                let Ok(mut posts) = crate::messages::channel(&ctx, &tag, crate::messages::BOARD_BLOCKS).await else { continue };
                posts.reverse();
                let lines = posts
                    .iter()
                    .filter(|p| !mine.contains(&p.from.to_lowercase()))
                    .map(|p| ChatLine {
                        at: p.at,
                        id: format!("{}:{}", p.tx, p.index),
                        from: p.from.to_lowercase(),
                        who: who(&p.from),
                        text: p.text().unwrap_or_else(|| "<sealed>".into()),
                    })
                    .collect();
                (target.clone(), lines)
            } else if let Some(code) = target.strip_prefix("dm:") {
                if !self.is_unlocked() {
                    continue;
                }
                let Ok(read) = self.read_conversation(code, crate::messages::BOARD_BLOCKS).await else { continue };
                let name = contacts
                    .iter()
                    .find(|c| c.payment_code.as_deref() == Some(code))
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| crate::session::short_code(code));
                // Counted, never quoted: the text stays in the conversation.
                let lines = read
                    .iter()
                    .filter(|l| !l.mine)
                    .map(|l| ChatLine { at: l.at, id: String::new(), from: l.from.to_lowercase(), who: who(&l.from), text: String::new() })
                    .collect();
                (format!("{name} · sealed"), lines)
            } else {
                continue;
            };
            let key = seen_key(&self.network.id, &target);
            let newest = lines.iter().map(|l| l.at).max().unwrap_or(0);
            let seen = self.app.kv(&key)?.and_then(|v| v.parse::<u64>().ok());
            if let Some(seen) = seen {
                let fresh: Vec<ChatLine> = lines.into_iter().filter(|l| l.at > seen).collect();
                // A public channel carries text from anyone: say so wherever it is shown, since a
                // notification under this app's name is exactly what a phishing post wants.
                let title = if target.starts_with('#') && fresh.iter().any(|l| !known.contains(&l.from)) {
                    format!("{title} · public, unverified")
                } else {
                    title
                };
                let body = if target.starts_with('#') { summarize(&fresh) } else { private_summary(fresh.len()) };
                if let Some(body) = body {
                    let notice = self.app.notify("chat", &title, &body).ok();
                    news.push(ChatNews { target: target.clone(), title, body, fresh, notice });
                }
            }
            if seen.is_none_or(|s| newest > s) {
                self.app.set_kv(&key, &newest.max(seen.unwrap_or(0)).to_string())?;
            }
        }
        Ok(news)
    }
}

/// The notification body for `fresh` (oldest first); None when it is empty. Newest last,
/// `who: text`, at most [`PER_NOTICE`] lines and a count of the rest.
pub fn summarize(fresh: &[ChatLine]) -> Option<String> {
    if fresh.is_empty() {
        return None;
    }
    let shown: Vec<String> = fresh.iter().rev().take(PER_NOTICE).rev().map(|l| format!("{}: {}", l.who, l.text)).collect();
    let more = fresh.len().saturating_sub(PER_NOTICE);
    Some(if more > 0 { format!("{} (+{more} more)", shown.join(" · ")) } else { shown.join(" · ") })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(at: u64, who: &str, text: &str) -> ChatLine {
        ChatLine { at, id: format!("0x{at}:0"), from: String::new(), who: who.into(), text: text.into() }
    }

    #[test]
    fn a_summary_keeps_the_newest_and_counts_the_rest() {
        assert_eq!(summarize(&[]), None, "nothing arrived");
        assert_eq!(summarize(&[line(20, "bob", "wen")]).as_deref(), Some("bob: wen"));
        let many: Vec<_> = (1..=5).map(|i| line(i, &format!("p{i}"), &format!("m{i}"))).collect();
        assert_eq!(summarize(&many).as_deref(), Some("p3: m3 · p4: m4 · p5: m5 (+2 more)"), "newest kept, rest counted");
    }

    /// A sealed conversation's notice counts; it never quotes.
    #[test]
    fn a_private_summary_counts_and_never_quotes() {
        assert_eq!(private_summary(0), None);
        assert_eq!(private_summary(1).as_deref(), Some("1 new message"));
        assert_eq!(private_summary(4).as_deref(), Some("4 new messages"));
    }

    /// A new wallet is subscribed to nothing: a public channel's posts reach the desktop only
    /// after the user asks for them.
    #[test]
    fn nothing_is_subscribed_until_the_user_asks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        let registry = crate::registry::Registry::fast(paths);
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let meta = registry.create_hd("main", phrase, "english", "", "password123", true).unwrap();
        let network = crate::network::NetworkProfile::builtins().into_iter().next().unwrap();
        let s = Session::open(registry, crate::config::AppConfig::default(), meta, network).unwrap();
        assert!(!s.config.board_channels.is_empty(), "#general is still followed on the Board");
        assert!(s.chat_subscriptions().is_empty());
        assert!(s.toggle_chat_subscription("#general").unwrap());
        assert_eq!(s.chat_subscriptions(), ["#general"]);
    }
}
