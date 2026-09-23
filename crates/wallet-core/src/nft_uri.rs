//! An NFT's own metadata, read from where its contract says it is, for when the explorer has none.
//!
//! explorer.qu.ai reads each token's metadata once, and sometimes that fails (`metadata_status:
//! FAILED`). The token then has no name, picture or traits in the wallet for as long as the
//! explorer does not retry, which may be never. The contract's `tokenURI` (ERC-721) or `uri`
//! (ERC-1155) still says where the metadata is, and for collections on Quai that is usually IPFS,
//! which the configured media gateway serves ([`crate::ipfs::Content::Media`]).
//!
//! Only `ipfs://` (with links to the public or the configured gateway) and inline `data:` JSON are
//! followed. As with pictures ([`crate::media::MEDIA_HOSTS`]), a URI a minter chose is never
//! fetched from an arbitrary host: that would let a spam airdrop learn when this wallet looked.

use crate::error::{CoreError, Result};
use crate::explorer::NftItem;
use crate::ipfs;
use serde_json::Value;

/// A metadata document is a few kilobytes; anything near this is not one.
pub const MAX_METADATA_BYTES: usize = 64 * 1024;

/// Whether the explorer gave nothing but the id: no picture, no description, no traits.
pub fn needs_metadata(item: &NftItem) -> bool {
    item.image.is_none() && item.description.is_empty() && item.traits.is_empty()
}

/// Where a token's metadata is to be read from.
#[derive(Debug, PartialEq, Eq)]
pub enum Where {
    /// A `data:` URI: the document itself.
    Inline(Vec<u8>),
    /// Through the media gateway, with the CID to check the bytes against where it pins them.
    Gateway(ipfs::Located),
}

/// Resolve a token URI. ERC-1155's `{id}` is the token id as 64 lowercase hex digits.
pub fn locate(uri: &str, token_id: &str) -> Result<Where> {
    let uri = expand_id(uri.trim(), token_id)?;
    if let Some(rest) = uri.strip_prefix("data:") {
        let (meta, payload) = rest.split_once(',').ok_or_else(|| CoreError::Invalid("malformed data URI".into()))?;
        if !meta.starts_with("application/json") {
            return Err(CoreError::Invalid("the token's data URI is not JSON".into()));
        }
        if payload.len() > MAX_METADATA_BYTES * 4 / 3 {
            return Err(CoreError::Invalid("the token's data URI is too large".into()));
        }
        let bytes = if meta.ends_with(";base64") {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(payload.trim())
                .map_err(|_| CoreError::Invalid("the token's data URI is not valid base64".into()))?
        } else {
            crate::media::percent_decode(payload)
        };
        return Ok(Where::Inline(bytes));
    }
    if let Some(path) = uri.strip_prefix("ipfs://") {
        return Ok(Where::Gateway(ipfs::locate(ipfs::Content::Media, path)?));
    }
    if let Some(path) = ipfs::from_public_url(&uri) {
        return Ok(Where::Gateway(ipfs::locate(ipfs::Content::Media, path)?));
    }
    if ipfs::gateway(ipfs::Content::Media).serves(&uri) {
        return Ok(Where::Gateway(ipfs::Located { url: uri, verify: None }));
    }
    let host = crate::http::host_of(&uri).unwrap_or_else(|_| "that address".into());
    Err(CoreError::Rejected(format!("NFT metadata is not fetched from {host}")))
}

fn expand_id(uri: &str, token_id: &str) -> Result<String> {
    if !uri.contains("{id}") {
        return Ok(uri.to_string());
    }
    let id = crate::sdk::U256::from_str_radix(token_id, 10).map_err(|_| CoreError::Invalid("token id must be a decimal integer".into()))?;
    Ok(uri.replace("{id}", &format!("{id:064x}")))
}

/// Read and parse a token's metadata document from its URI.
pub async fn fetch(uri: &str, contract: &str, token_id: &str) -> Result<NftItem> {
    let bytes = match locate(uri, token_id)? {
        Where::Inline(bytes) => bytes,
        Where::Gateway(located) => {
            let fetched = crate::http::get_with(&located.url, MAX_METADATA_BYTES, crate::http::Priority::Background).await?;
            // A gateway answers for content it did not write; where the CID pins the bytes, hold
            // it to them.
            if located.verify.as_ref().and_then(|cid| cid.verifies_content(&fetched.bytes)) == Some(false) {
                return Err(CoreError::Rejected("the gateway returned metadata that is not what the CID names".into()));
            }
            fetched.bytes
        }
    };
    parse(&bytes, contract, token_id)
}

/// Parse a metadata document. Separate from the fetch so it can be tested on a fixture.
pub fn parse(bytes: &[u8], contract: &str, token_id: &str) -> Result<NftItem> {
    let doc: Value = serde_json::from_slice(bytes).map_err(|_| CoreError::Invalid("the token's metadata is not JSON".into()))?;
    if !doc.is_object() {
        return Err(CoreError::Invalid("the token's metadata is not a JSON object".into()));
    }
    Ok(crate::explorer::item_from_metadata(&doc, contract, token_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Mojis #241 as its IPFS document has it; the explorer marked it FAILED.
    const BLACK_NIB: &str = r#"{
        "description": "The Mojis are a collection of 4292 unique digital collectibles on the Quai Network.",
        "external_url": "http://localhost:3000",
        "image": "ipfs://Qmc2qFt9Qx68F7ZgfpLRBdtsURgtCXbsAHD95AVgMP1gFY/241.png",
        "name": "BLACK NIB",
        "attributes": [{"trait_type": "Unicode Codepoint", "value": "2712"}]
    }"#;

    #[test]
    fn a_metadata_document_fills_in_the_item() {
        let item = parse(BLACK_NIB.as_bytes(), "0x0046E5085A830567F647FE52672926BEDC8D5C55", "241").unwrap();
        assert_eq!(item.name, "BLACK NIB");
        assert_eq!(item.contract, "0x0046e5085a830567f647fe52672926bedc8d5c55");
        assert_eq!(item.image.as_deref(), Some("ipfs://Qmc2qFt9Qx68F7ZgfpLRBdtsURgtCXbsAHD95AVgMP1gFY/241.png"));
        assert_eq!(item.traits, vec![("Unicode Codepoint".to_string(), "2712".to_string())]);
        assert!(!needs_metadata(&item));
        assert!(parse(b"not json", "0x00", "1").is_err());
        assert!(parse(b"[1,2]", "0x00", "1").is_err());
    }

    #[test]
    fn only_ipfs_and_inline_documents_are_followed() {
        let _gateway = crate::ipfs::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::ipfs::set_gateway(crate::ipfs::Content::Media, Some("http://10.0.0.13:8080")).unwrap();
        let cid = "QmZdegfWQ1pR4MEyQff7xnV1J47aLUDAhpR5GjsxrdWtFn";
        match locate(&format!("ipfs://{cid}/241.json"), "241").unwrap() {
            Where::Gateway(l) => assert_eq!(l.url, format!("http://10.0.0.13:8080/ipfs/{cid}/241.json")),
            other => panic!("{other:?}"),
        }
        // A hard-coded public gateway goes to the configured one instead.
        match locate(&format!("https://ipfs.io/ipfs/{cid}/241.json"), "241").unwrap() {
            Where::Gateway(l) => assert!(l.url.starts_with("http://10.0.0.13:8080/"), "{}", l.url),
            other => panic!("{other:?}"),
        }
        // Inline JSON, plain and base64.
        assert_eq!(locate(r#"data:application/json,{"name":"x"}"#, "1").unwrap(), Where::Inline(br#"{"name":"x"}"#.to_vec()));
        assert_eq!(locate("data:application/json;base64,eyJuYW1lIjoieCJ9", "1").unwrap(), Where::Inline(br#"{"name":"x"}"#.to_vec()));
        // A host the minter chose is never asked.
        assert!(matches!(locate("https://tracker.example/meta/1.json", "1"), Err(CoreError::Rejected(_))));
        assert!(locate("data:image/png;base64,AAAA", "1").is_err());
        crate::ipfs::set_gateway(crate::ipfs::Content::Media, None).unwrap();
    }

    #[test]
    fn an_erc1155_id_is_written_as_64_hex_digits() {
        let _gateway = crate::ipfs::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::ipfs::set_gateway(crate::ipfs::Content::Media, Some("http://127.0.0.1:8080")).unwrap();
        match locate("ipfs://QmZdegfWQ1pR4MEyQff7xnV1J47aLUDAhpR5GjsxrdWtFn/{id}.json", "255").unwrap() {
            Where::Gateway(l) => assert!(l.url.ends_with(&format!("/{}ff.json", "0".repeat(62))), "{}", l.url),
            other => panic!("{other:?}"),
        }
        crate::ipfs::set_gateway(crate::ipfs::Content::Media, None).unwrap();
    }
}
