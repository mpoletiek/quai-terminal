//! Quai Terminal's untrusted inputs: everything read from someone else's server. The HTTP client
//! (one budget per host, the proxy, size limits), the explorer, IPFS gateways, NFT metadata and
//! pictures, decoded in a sandboxed helper process. Nothing here holds a key, opens the vault or
//! signs; what it returns is data for the engine to check, never an instruction.

pub mod explorer;
pub mod http;
pub mod ipfs;
pub mod media;
pub mod media_helper;
pub mod nft_uri;

pub use quai_model::{CoreError, Result, sdk};
