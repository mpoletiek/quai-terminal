//! Grind one throwaway Qi-ledger key for the disposable trading fixture.
//!
//! An imported key's ledger and zone are not chosen: both are read off the address the key derives
//! (`identity::imported_key_record`), so the only way to obtain a Qi-ledger Cyprus-1 key is to keep
//! generating until one lands. That takes a few dozen tries, which is why this is a binary rather
//! than a line of shell.
//!
//! The key is printed on stdout. It is fixture material for a loopback chain with no value on it —
//! never point this at a wallet holding real funds.
use std::io::Read;
use wallet_vault::KeyLedger;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut random = std::fs::File::open("/dev/urandom")?;
    let mut seed = [0u8; 32];
    for attempt in 1..=100_000u32 {
        random.read_exact(&mut seed)?;
        let Ok(key) = quai_sdk::crypto::SecretKey::from_bytes(&seed) else {
            continue;
        };
        // Rejects a wrong zone as well as classifying the ledger, so Ok already means usable here.
        let Ok(record) = wallet_core::identity::imported_key_record(&key) else {
            continue;
        };
        if record.ledger != KeyLedger::Qi {
            continue;
        }
        println!("{}", serde_json::json!({"address": record.address, "secret_hex": record.secret_hex, "attempts": attempt}));
        return Ok(());
    }
    Err("no Qi-ledger Cyprus-1 key found within the attempt bound".into())
}
