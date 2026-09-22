//! Durable, public execution checkpoints. A saved plan is never signing authority.
use crate::appdb::{AppDb, OpStatus};
use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};

/// Versioned client intent and its exact operation dependencies, with optimistic concurrency.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TradePlan {
    pub version: u32,
    pub id: String,
    pub network: String,
    pub owner: String,
    pub label: String,
    pub revision: u64,
    pub state: PlanState,
    pub intent: serde_json::Value,
    pub operations: Vec<String>,
    pub reason: String,
    pub updated: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    Review,
    Waiting,
    Paused,
    Complete,
    Cancelled,
}

impl TradePlan {
    pub fn new(network: String, owner: String, label: String, intent: serde_json::Value) -> Result<Self> {
        Ok(Self {
            version: 1,
            id: crate::session::op_hex(crate::session::new_operation_id()?),
            network,
            owner,
            label,
            revision: 0,
            state: PlanState::Review,
            intent,
            operations: Vec::new(),
            reason: String::new(),
            updated: crate::registry::now(),
        })
    }

    /// Inspect authoritative operation state before offering another review. No signing or
    /// network calls occur. A missing/ambiguous result always prevents automatic continuation.
    pub fn readiness(&self, db: &AppDb) -> Result<PlanReadiness> {
        if self.version != 1 {
            return Err(CoreError::Invalid("unsupported trade plan version".into()));
        }
        if matches!(self.state, PlanState::Cancelled | PlanState::Complete) {
            return Ok(PlanReadiness::Stopped);
        }
        for id in &self.operations {
            let Some(op) = db.operation(id)? else { return Ok(PlanReadiness::Reconcile(id.clone())) };
            if op.network != self.network {
                return Err(CoreError::Rejected("plan operation belongs to another network".into()));
            }
            match op.status {
                OpStatus::Confirmed | OpStatus::Settled => {}
                OpStatus::Cancelled if op.tx_hash.is_none() => {}
                OpStatus::Failed | OpStatus::Refunded | OpStatus::Replaced => return Ok(PlanReadiness::Stopped),
                _ => return Ok(PlanReadiness::Reconcile(id.clone())),
            }
        }
        Ok(PlanReadiness::ReviewRequired)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanReadiness {
    ReviewRequired,
    Reconcile(String),
    Stopped,
}

/// Hold a kernel lock for the lifetime of a client coordinator. It survives no crash and
/// contains no secret; a second client cannot prepare another step for the same plan.
pub fn claim(directory: &std::path::Path, id: &str) -> Result<std::fs::File> {
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CoreError::Invalid("invalid trade plan identity".into()));
    }
    crate::paths::ensure_private_dir(directory)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(directory.join(id))?;
    lock.try_lock().map_err(|_| CoreError::Invalid("trade plan is active in another client".into()))?;
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoints_survive_reopen_and_reject_stale_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        let db = AppDb::open(&path).unwrap();
        let mut plan = TradePlan::new("test".into(), "owner".into(), "swap".into(), serde_json::json!({"amount": "1"})).unwrap();
        db.save_trade_plan(&mut plan).unwrap();
        let mut stale = plan.clone();
        plan.operations.push("missing".into());
        plan.state = PlanState::Waiting;
        db.save_trade_plan(&mut plan).unwrap();
        assert!(db.save_trade_plan(&mut stale).is_err());
        drop(db);
        let db = AppDb::open(&path).unwrap();
        let loaded = db.trade_plan(&plan.id).unwrap().unwrap();
        assert_eq!(loaded, plan);
        assert_eq!(loaded.readiness(&db).unwrap(), PlanReadiness::Reconcile("missing".into()));
        assert!(db.trade_plans("other").unwrap().is_empty());
    }
}
