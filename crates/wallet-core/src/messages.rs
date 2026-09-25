//! On-chain messages: what the wallet reads and writes on the board. The format, sealing and
//! decoding live in [`quai_messaging::board`]; this is where they meet the node and the cache.

pub use quai_messaging::board::*;

use crate::data::DataCtx;
use crate::error::{CoreError, Result};
use crate::registry::now;

/// Posts under one tag, newest first, over the last `blocks` blocks. One `quai_getLogs`
/// against the node; block times come from the headers of the blocks that carried a post.
pub async fn channel(ctx: &DataCtx, tag: &[u8; 32], blocks: u64) -> Result<Vec<Post>> {
    posts(ctx, &hex::encode(tag), blocks, |_, _| vec![*tag]).await
}

/// A sealed conversation's posts over the last `blocks` blocks, under every tag the range can
/// hold (see [`Conversation::tags_between`]), newest first.
pub async fn conversation_posts(ctx: &DataCtx, c: &Conversation, blocks: u64) -> Result<Vec<Post>> {
    posts(ctx, &hex::encode(c.id()), blocks, |from, to| c.tags_between(from, to)).await
}

/// Posts under the tags `tags(from, to)` names for the block range being read, cached under
/// `cache_key` so a later read only asks for newer blocks.
async fn posts(ctx: &DataCtx, cache_key: &str, blocks: u64, tags: impl Fn(u64, u64) -> Vec<[u8; 32]>) -> Result<Vec<Post>> {
    use quai_sdk::provider::{LogFilter, LogRange, TopicMatch};
    let contract = ctx
        .network
        .ecosystem
        .messages
        .as_ref()
        .ok_or_else(|| CoreError::NotFound(format!("no messages contract on {}", ctx.network.name)))?;
    let key = format!("{}:board:{cache_key}", ctx.network.id);
    let mut posts: Vec<Post> = ctx.app.cache_get(&key)?.and_then(|(t, _)| serde_json::from_str(&t).ok()).unwrap_or_default();
    if ctx.cache_only {
        return Ok(posts);
    }
    let contract = crate::data::verify_pinned(&ctx.app, &ctx.node, &ctx.network, contract, "messages contract", ctx.trust).await?;
    let head = ctx.node.provider.latest_header(crate::network::ZONE).await?.ok_or_else(|| CoreError::Network("no head".into()))?;
    let head_time = crate::network::header_time(&head).unwrap_or_else(now);
    let to = head.number;
    let floor = to.saturating_sub(blocks.max(1));
    let from = posts.iter().map(|p| p.block).max().map_or(floor, |b| b.saturating_add(1)).clamp(floor, to);
    let topics = vec![
        TopicMatch::AnyOf([MESSAGE_TOPIC].iter().filter_map(|t| t.parse().ok()).collect()),
        TopicMatch::Any,
        TopicMatch::AnyOf(tags(from, to).iter().filter_map(|t| tag_topic(t).parse().ok()).collect()),
    ];
    let filter =
        LogFilter::new(crate::network::ZONE, LogRange::Inclusive { from, to }).with_addresses(vec![contract.address()]).with_topics(topics);
    let logs = ctx.node.provider.logs(&filter).await?;
    let mut times: std::collections::HashMap<u64, u64> = posts.iter().filter(|p| p.timed).map(|p| (p.block, p.at)).collect();
    times.insert(to, head_time);
    let mut fresh = Vec::new();
    for log in logs.iter().filter(|l| !l.removed) {
        let block = log.inclusion.block_number;
        let at = match times.get(&block) {
            Some(t) => *t,
            None => {
                let header = ctx.node.provider.header_at(crate::network::ZONE, block).await.ok().flatten();
                let t = header.as_ref().and_then(crate::network::header_time).unwrap_or(0);
                times.insert(block, t);
                t
            }
        };
        let topics: Vec<String> = log.topics.iter().map(|t| t.to_string()).collect();
        if let Some(p) = decode_message(&topics, &log.data.to_hex(), at, block, &log.transaction_hash.to_string(), log.log_index) {
            fresh.push(p);
        }
    }
    let known: std::collections::HashSet<(String, u64)> = posts.iter().map(|p| (p.tx.clone(), p.index)).collect();
    posts.extend(fresh.into_iter().filter(|p| !known.contains(&(p.tx.clone(), p.index))));
    posts.sort_by(|a, b| b.position().cmp(&a.position()));
    posts.truncate(BOARD_KEEP);
    if let Ok(text) = serde_json::to_string(&posts) {
        let _ = ctx.app.cache_put(&key, &text);
    }
    Ok(posts)
}

/// Every public channel with a message in the last `blocks` blocks, busiest first. One
/// `quai_getLogs` for text messages across all tags: sealed conversations are filed under tags
/// that are not names, so they are neither counted nor shown.
pub async fn channels(ctx: &DataCtx, blocks: u64) -> Result<Vec<ChannelSummary>> {
    use quai_sdk::provider::{LogFilter, LogRange, TopicMatch};
    let contract = ctx
        .network
        .ecosystem
        .messages
        .as_ref()
        .ok_or_else(|| CoreError::NotFound(format!("no messages contract on {}", ctx.network.name)))?;
    let contract = crate::data::verify_pinned(&ctx.app, &ctx.node, &ctx.network, contract, "messages contract", ctx.trust).await?;
    let head = ctx.node.provider.latest_header(crate::network::ZONE).await?.ok_or_else(|| CoreError::Network("no head".into()))?;
    let head_time = crate::network::header_time(&head).unwrap_or_else(now);
    let to = head.number;
    let from = to.saturating_sub(blocks.max(1));
    let kind_text = format!("0x{:0>64}", format!("{KIND_TEXT:x}"));
    let filter =
        LogFilter::new(crate::network::ZONE, LogRange::Inclusive { from, to }).with_addresses(vec![contract.address()]).with_topics(vec![
            TopicMatch::AnyOf([MESSAGE_TOPIC].iter().filter_map(|t| t.parse().ok()).collect()),
            TopicMatch::Any,
            TopicMatch::Any,
            TopicMatch::AnyOf([kind_text].iter().filter_map(|t| t.parse().ok()).collect()),
        ]);
    let logs = ctx.node.provider.logs(&filter).await?;
    let mut seen: std::collections::HashMap<String, (u32, u64, Vec<u64>)> = std::collections::HashMap::new();
    for log in logs.iter().filter(|l| !l.removed) {
        let Some(tag) = log.topics.get(2).map(|t| t.to_string()) else { continue };
        // A tag that is not a readable name is a sealed conversation, not a channel.
        let Some(name) = tag_name(&tag) else { continue };
        let block = log.inclusion.block_number;
        // Heights order them; the newest is dated from the head rather than reading every block.
        let at = head_time.saturating_sub(to.saturating_sub(block) * 5);
        let e = seen.entry(name).or_insert((0, 0, Vec::new()));
        e.0 += 1;
        e.1 = e.1.max(at);
        e.2.push(block);
    }
    let mut out: Vec<ChannelSummary> = seen
        .into_iter()
        .map(|(name, (messages, last_at, mut blocks))| {
            blocks.sort_unstable_by(|a, b| b.cmp(a));
            blocks.truncate(BOARD_KEEP);
            ChannelSummary { name, messages, last_at, last_block: blocks.first().copied().unwrap_or(0), recent_blocks: blocks }
        })
        .collect();
    out.sort_by(|a, b| b.messages.cmp(&a.messages).then(a.name.cmp(&b.name)));
    Ok(out)
}

impl crate::session::Session {
    /// Messages that arrived in the followed public channels since this wallet last looked,
    /// recording where the board stands as it goes. The first look at a channel announces
    /// nothing: starting the daemon is not news, what comes after it is.
    ///
    /// Only public channels. Reading a sealed conversation needs this wallet's payment key, and
    /// this path deliberately holds none, so the daemon can run without ever unlocking.
    pub async fn track_board(&self, follows: &[String]) -> Result<Vec<(String, u32)>> {
        if follows.is_empty() || self.network.ecosystem.messages.is_none() {
            return Ok(Vec::new());
        }
        let ctx = self.data_ctx()?;
        let found = channels(&ctx, BOARD_BLOCKS).await?;
        let mut news = Vec::new();
        for name in follows {
            let Some(channel) = found.iter().find(|c| &c.name == name) else { continue };
            let key = format!("board_seen:{}:{name}", self.network.id);
            match self.app.kv(&key)?.and_then(|v| v.parse::<u64>().ok()) {
                // Heights only rise, so this is the mark to compare against next time.
                None => {
                    self.app.set_kv(&key, &channel.last_block.to_string())?;
                }
                Some(seen) => {
                    let arrived = channel.recent_blocks.iter().filter(|b| **b > seen).count() as u32;
                    if arrived > 0 {
                        self.app.set_kv(&key, &channel.last_block.to_string())?;
                        news.push((name.clone(), arrived));
                    }
                }
            }
        }
        Ok(news)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_is_news_only_above_the_mark_last_recorded() {
        let app = crate::appdb::AppDb::memory().unwrap();
        let key = "board_seen:local:general";
        let summary = |blocks: Vec<u64>| ChannelSummary {
            name: "general".into(),
            messages: blocks.len() as u32,
            last_at: 0,
            last_block: blocks.first().copied().unwrap_or(0),
            recent_blocks: blocks,
        };
        // The same arithmetic `track_board` runs, against the store it keeps it in.
        let arrived = |app: &crate::appdb::AppDb, c: &ChannelSummary| -> u32 {
            match app.kv(key).unwrap().and_then(|v| v.parse::<u64>().ok()) {
                None => {
                    app.set_kv(key, &c.last_block.to_string()).unwrap();
                    0
                }
                Some(seen) => {
                    let n = c.recent_blocks.iter().filter(|b| **b > seen).count() as u32;
                    if n > 0 {
                        app.set_kv(key, &c.last_block.to_string()).unwrap();
                    }
                    n
                }
            }
        };
        assert_eq!(arrived(&app, &summary(vec![100, 99])), 0, "the first look is not news");
        assert_eq!(app.kv(key).unwrap().as_deref(), Some("100"));
        assert_eq!(arrived(&app, &summary(vec![102, 101, 100, 99])), 2, "two rose above the mark");
        assert_eq!(arrived(&app, &summary(vec![102, 101, 100, 99])), 0, "the same board again is not news");
        // Old messages ageing out of the window lowers the count without anything arriving.
        assert_eq!(arrived(&app, &summary(vec![102, 101])), 0, "a shrinking window is not news");
    }
}
