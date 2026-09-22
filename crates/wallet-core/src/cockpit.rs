//! Every wallet on this computer at a glance, without unlocking any of them.
//!
//! Each wallet leaves a small summary in its own directory whenever its portfolio is priced: the
//! total, its QUAI and Qi, its biggest holdings and when. A wallet's QUAI balance can also be read
//! live from its public addresses, which needs no keys. Qi needs a scan, so it stays last known.

use crate::data::DataCtx;
use crate::paths::Paths;
use crate::portfolio::{AssetKey, Portfolio};
use crate::sdk::U256;
use serde::{Deserialize, Serialize};

/// What a wallet looked like the last time it was priced.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct WalletSummary {
    pub total_usd: f64,
    /// QUAI in wei.
    pub quai: String,
    /// Qi in qits.
    pub qi: String,
    /// The largest holdings by value, symbols only.
    pub top: Vec<String>,
    /// When (unix seconds).
    pub at: u64,
}

fn summary_path(paths: &Paths, wallet: &str, network: &str) -> std::path::PathBuf {
    paths.wallet_dir(wallet).join(format!("summary-{network}.json"))
}

/// Keep a wallet's summary from a freshly priced portfolio. A stale or partial answer is not kept.
pub fn save_summary(paths: &Paths, wallet: &str, portfolio: &Portfolio) {
    if portfolio.stale {
        return;
    }
    let base = |key: &AssetKey| portfolio.rows.iter().find(|r| r.key == *key).map(|r| r.balance.clone()).unwrap_or_else(|| "0".into());
    let mut ranked: Vec<(&str, f64)> =
        portfolio.rows.iter().filter_map(|r| r.value_usd.filter(|v| *v > 0.0).map(|v| (r.symbol.as_str(), v))).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let summary = WalletSummary {
        total_usd: portfolio.total_usd,
        quai: base(&AssetKey::Quai),
        qi: base(&AssetKey::Qi),
        top: ranked.into_iter().take(3).map(|(s, _)| s.to_string()).collect(),
        at: portfolio.observed_at,
    };
    if let Ok(text) = serde_json::to_string(&summary) {
        let _ = std::fs::write(summary_path(paths, wallet, &portfolio.network), text);
    }
}

/// A wallet's last summary on a network, if it has been priced there.
pub fn load_summary(paths: &Paths, wallet: &str, network: &str) -> Option<WalletSummary> {
    serde_json::from_str(&std::fs::read_to_string(summary_path(paths, wallet, network)).ok()?).ok()
}

/// Live QUAI per wallet: the sum over its public Quai addresses. Reads run together; a wallet
/// whose reads fail is left out rather than shown as zero.
pub async fn quai_totals(ctx: &DataCtx, wallets: &[(String, Vec<String>)]) -> Vec<(String, U256)> {
    use quai_sdk::{BlockTag, QuaiAddress};
    if ctx.online().is_err() {
        return vec![];
    }
    let provider = &ctx.node.provider;
    let reads = wallets.iter().map(|(id, addresses)| async move {
        let each = addresses.iter().filter_map(|a| a.parse::<QuaiAddress>().ok()).map(|a| provider.balance(a, BlockTag::Latest));
        let balances = futures::future::join_all(each).await;
        let mut total = U256::ZERO;
        for b in balances {
            total = total.saturating_add(b.ok()?);
        }
        Some((id.clone(), total))
    });
    futures::future::join_all(reads).await.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::{AssetRow, PriceKind, Trust};

    fn row(key: AssetKey, symbol: &str, balance: &str, usd: f64) -> AssetRow {
        AssetRow {
            key,
            symbol: symbol.into(),
            name: symbol.into(),
            balance: balance.into(),
            decimals: 18,
            exact: true,
            price_usd: Some(usd),
            price_kind: PriceKind::Market,
            price_source: String::new(),
            price_at: 0,
            value_usd: Some(usd * 2.0),
            allocation: 0.0,
            change_24h: None,
            icon_url: None,
            trust: Trust::Verified,
            holders: None,
        }
    }

    #[test]
    fn a_summary_is_kept_per_wallet_and_network() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
        std::fs::create_dir_all(paths.wallet_dir("w1")).unwrap();
        let p = Portfolio {
            network: "mainnet".into(),
            rows: vec![
                row(AssetKey::Quai, "QUAI", "2000000000000000000", 0.5),
                row(AssetKey::Token("0x00aa".into()), "WQI", "3000000000000000000", 1.0),
            ],
            total_usd: 4.0,
            observed_at: 7,
            ..Default::default()
        };
        save_summary(&paths, "w1", &p);
        let s = load_summary(&paths, "w1", "mainnet").unwrap();
        assert_eq!((s.total_usd, s.quai.as_str(), s.qi.as_str(), s.at), (4.0, "2000000000000000000", "0", 7));
        assert_eq!(s.top, ["WQI", "QUAI"], "biggest first");
        assert!(load_summary(&paths, "w1", "orchard").is_none(), "per network");
        // A stale answer does not replace a good one.
        save_summary(&paths, "w1", &Portfolio { stale: true, total_usd: 0.0, ..p });
        assert_eq!(load_summary(&paths, "w1", "mainnet").unwrap().total_usd, 4.0);
    }
}
