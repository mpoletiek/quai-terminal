//! Image pipeline for token icons and NFT thumbnails.
//!
//! fetch (capped) → sniff the real type → decode with limits → resize → two cached renditions
//! keyed by content hash: a 256 px thumbnail (NFTs) and a 32 px icon (tokens), each stored as PNG
//! (kitty graphics) with a dominant color; raw RGBA (half-block previews) is decoded on load. Remote SVG is
//! rendered with every external reference disabled (no files, no URLs, no nested images, no
//! scripts). Anything that fails falls back to a monogram badge, so an image is never the only
//! carrier of meaning.

use crate::appdb::AppDb;
use crate::error::{CoreError, Result};
use crate::http;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

const QUAI_SVG: &str = include_str!("../assets/quai.svg");
const QI_SVG: &str = include_str!("../assets/qi.svg");

/// Data URL of the bundled logo for a native coin (`quai` or `qi`). Rendered locally and never
/// fetched, so it needs no data-source permission.
pub fn native_icon(asset: &str) -> Option<&'static str> {
    static QUAI: OnceLock<String> = OnceLock::new();
    static QI: OnceLock<String> = OnceLock::new();
    let (cell, svg) = match asset.to_ascii_lowercase().as_str() {
        "quai" => (&QUAI, QUAI_SVG),
        "qi" => (&QI, QI_SVG),
        _ => return None,
    };
    Some(cell.get_or_init(|| {
        use base64::Engine;
        format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg))
    }))
}

/// Whether `url` is one of the bundled native-coin logos.
pub fn is_native_icon(url: &str) -> bool {
    ["quai", "qi"].iter().any(|a| native_icon(a) == Some(url))
}

/// NFT thumbnail edge in pixels.
pub const THUMB: u32 = 256;
/// Token icon edge in pixels.
pub const ICON: u32 = 32;
/// Request size for a token icon drawn larger than a couple of cells: served from the thumbnail
/// rendition, but still governed by the token-icon setting rather than the NFT-image one.
pub const ICON_LARGE: u32 = 128;
/// Failed fetches are retried after this many seconds.
const RETRY_AFTER: u64 = 3600;
/// Largest decoded image dimension accepted.
const MAX_DIMENSION: u32 = 8192;
/// Largest SVG document accepted.
const MAX_SVG_BYTES: usize = 1024 * 1024;

/// A decoded, resized rendition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendition {
    /// Content hash of the source bytes.
    pub hash: String,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// PNG encoding.
    pub png: Vec<u8>,
    /// RGBA8 pixels, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// Dominant opaque color.
    pub dominant: (u8, u8, u8),
}

impl Rendition {
    /// Pixel at (x, y) as RGBA.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y.min(self.height.saturating_sub(1)) * self.width + x.min(self.width.saturating_sub(1))) * 4) as usize;
        self.rgba.get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]]).unwrap_or([0, 0, 0, 0])
    }
}

/// Detected source format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF (first frame only).
    Gif,
    /// WebP.
    WebP,
    /// SVG.
    Svg,
}

/// Identify an image from its bytes; the declared content type is not trusted.
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Format::Png);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Format::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(Format::Gif);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(Format::WebP);
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_lowercase();
    let head = head.trim_start_matches('\u{feff}').trim_start();
    if head.starts_with("<svg")
        || (head.starts_with("<?xml") && head.contains("<svg"))
        || (head.starts_with("<!--") && head.contains("<svg"))
    {
        return Some(Format::Svg);
    }
    None
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Decode source bytes into RGBA at most `MAX_DIMENSION` on a side.
fn decode(bytes: &[u8], render_edge: u32) -> Result<image::RgbaImage> {
    let format = sniff(bytes).ok_or_else(|| CoreError::Invalid("not a supported image (PNG, JPEG, GIF, WebP, SVG)".into()))?;
    match format {
        Format::Svg => render_svg(bytes, render_edge),
        raster => {
            let fmt = match raster {
                Format::Png => image::ImageFormat::Png,
                Format::Jpeg => image::ImageFormat::Jpeg,
                Format::Gif => image::ImageFormat::Gif,
                _ => image::ImageFormat::WebP,
            };
            let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), fmt);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_DIMENSION);
            limits.max_image_height = Some(MAX_DIMENSION);
            limits.max_alloc = Some(256 * 1024 * 1024);
            reader.limits(limits);
            // GIF decoding through `decode` yields the first frame only.
            let img = reader.decode().map_err(|e| CoreError::Invalid(format!("image decode: {e}")))?;
            Ok(img.to_rgba8())
        }
    }
}

/// Render an SVG with every external resource refused.
fn render_svg(bytes: &[u8], edge: u32) -> Result<image::RgbaImage> {
    use resvg::{tiny_skia, usvg};
    if bytes.len() > MAX_SVG_BYTES {
        return Err(CoreError::Invalid("SVG too large".into()));
    }
    let lowered = String::from_utf8_lossy(bytes).to_lowercase();
    if lowered.contains("<!entity") {
        return Err(CoreError::Invalid("SVG with entity declarations refused".into()));
    }
    let options = usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver { resolve_data: Box::new(|_, _, _| None), resolve_string: Box::new(|_, _| None) },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).map_err(|e| CoreError::Invalid(format!("SVG: {e}")))?;
    let size = tree.size();
    let (w, h) = (size.width().max(1.0), size.height().max(1.0));
    let scale = edge as f32 / w.max(h);
    let pw = ((w * scale).round() as u32).clamp(1, edge);
    let ph = ((h * scale).round() as u32).clamp(1, edge);
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or_else(|| CoreError::Invalid("SVG canvas".into()))?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia stores premultiplied alpha; convert to straight alpha.
    let mut data = pixmap.take();
    for px in data.chunks_exact_mut(4) {
        let a = px[3];
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8;
            }
        }
    }
    image::RgbaImage::from_raw(pw, ph, data).ok_or_else(|| CoreError::Invalid("SVG pixels".into()))
}

/// Most common opaque color, bucketed to 4 bits per channel; mid gray when fully transparent.
pub fn dominant_color(img: &image::RgbaImage) -> (u8, u8, u8) {
    // bucket -> (weight, pixels, sum r, sum g, sum b)
    let mut buckets = std::collections::HashMap::<(u8, u8, u8), (u64, u64, u64, u64, u64)>::new();
    for px in img.pixels() {
        let [r, g, b, a] = px.0;
        if a < 128 {
            continue;
        }
        // Near-white and near-black backgrounds rarely identify a logo, so they count less.
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let weight = if (max > 240 && min > 240) || max < 16 { 1 } else { 4 };
        let e = buckets.entry((r >> 4, g >> 4, b >> 4)).or_insert((0, 0, 0, 0, 0));
        e.0 += weight;
        e.1 += 1;
        e.2 += u64::from(r);
        e.3 += u64::from(g);
        e.4 += u64::from(b);
    }
    buckets.values().max_by_key(|e| e.0).map(|&(_, n, r, g, b)| ((r / n) as u8, (g / n) as u8, (b / n) as u8)).unwrap_or((128, 128, 128))
}

/// Fit an image inside `edge × edge` and produce a rendition.
pub fn make_rendition(bytes: &[u8], edge: u32) -> Result<Rendition> {
    let hash = hash_bytes(bytes);
    let img = decode(bytes, edge)?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(CoreError::Invalid("empty image".into()));
    }
    let scale = (edge as f64 / w.max(h) as f64).min(1.0);
    let (tw, th) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
    let resized = if (tw, th) == (w, h) { img } else { image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle) };
    let dominant = dominant_color(&resized);
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(resized.clone())
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| CoreError::Invalid(format!("png encode: {e}")))?;
    Ok(Rendition { hash, width: tw, height: th, png, rgba: resized.into_raw(), dominant })
}

/// Resolve a media reference to something fetchable. `data:` URIs decode locally; `ipfs://`
/// goes to the configured IPFS gateway ([`crate::ipfs`]); only http(s) is fetched. Everything else
/// is refused.
pub enum Source {
    /// Inline bytes from a data URI.
    Inline(Vec<u8>),
    /// An http(s) URL.
    Remote(String),
    /// IPFS content through the configured gateway, and — when its CID pins the bytes — what the
    /// bytes must hash to. A gateway is a third party answering for content it did not create.
    Ipfs(crate::ipfs::Located),
}

/// Classify a media URL.
pub fn resolve(url: &str) -> Result<Source> {
    let u = url.trim();
    if let Some(rest) = u.strip_prefix("data:") {
        let (meta, payload) = rest.split_once(',').ok_or_else(|| CoreError::Invalid("malformed data URI".into()))?;
        if payload.len() > http::MAX_IMAGE_BYTES * 4 / 3 {
            return Err(CoreError::Invalid("data URI too large".into()));
        }
        let bytes = if meta.ends_with(";base64") {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(payload.trim()).map_err(|_| CoreError::Invalid("bad base64 image".into()))?
        } else {
            percent_decode(payload)
        };
        return Ok(Source::Inline(bytes));
    }
    if let Some(path) = u.strip_prefix("ipfs://") {
        return Ok(Source::Ipfs(crate::ipfs::locate(crate::ipfs::Content::Media, path)?));
    }
    // Metadata that hard-codes the public gateway still goes to the one configured.
    if let Some(path) = crate::ipfs::from_public_url(u) {
        return Ok(Source::Ipfs(crate::ipfs::locate(crate::ipfs::Content::Media, path)?));
    }
    // A link straight to the configured gateway (http is allowed only when that gateway is local,
    // which `Gateway::parse` already enforced).
    // Only as the immutable content it names, rebuilt so a whole object is checked against its
    // CID: an `/ipns/` name or the node's API on that host is a stranger's choice to resolve.
    let gateway = crate::ipfs::gateway(crate::ipfs::Content::Media);
    if gateway.serves(u) {
        let path = gateway.cid_path(u).ok_or_else(|| CoreError::Rejected("only /ipfs/ content is read from the gateway".into()))?;
        return Ok(Source::Ipfs(crate::ipfs::locate(crate::ipfs::Content::Media, &path)?));
    }
    if u.starts_with("https://") {
        let host = http::host_of(u)?;
        // Quainance's proxy fetches what it is asked for, so only the one shape the wallet builds
        // itself is taken from a token: a CID, no query.
        if host == "www.quainance.com" && !quainance_media(u) {
            return Err(CoreError::Rejected("only a CID is fetched through Quainance's media proxy".into()));
        }
        if MEDIA_HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{h}"))) {
            return Ok(Source::Remote(u.to_string()));
        }
        return Err(CoreError::Rejected(format!("images are not fetched from {host}")));
    }
    Err(CoreError::Invalid("unsupported media URL".into()))
}

/// `https://www.quainance.com/api/media/<cid>`, exactly: what [`crate::launches`] builds.
fn quainance_media(url: &str) -> bool {
    url.strip_prefix(&format!("{}/", crate::launches::MEDIA_PROXY))
        .is_some_and(|cid| !cid.is_empty() && cid.len() <= 128 && cid.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Hosts pictures may be fetched from. A token or NFT names its own image URL, and anyone can
/// mint one: fetching wherever it points would let a spam airdrop learn this wallet's IP and when
/// it was looked at, or reach into the local network. So pictures come only from the explorer's
/// media proxy and the public IPFS gateway, over HTTPS; anything else shows its monogram.
///
/// `www.quainance.com` is Quainance's media proxy for launch logos. The wallet reaches it only with
/// URLs it builds itself from a verified CID (`launches::logo`), never with one a token supplied.
pub const MEDIA_HOSTS: [&str; 4] = ["explorer.qu.ai", "quaiscan.io", "ipfs.io", "www.quainance.com"];

pub(crate) fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Load a rendition for `url` at `edge` px, fetching and caching as needed.
/// Returns `Ok(None)` for a failure worth remembering (not found, too large, undecodable, or a
/// recent such attempt) and `Err` for a passing one (timeout, busy request budget, rate limit)
/// that callers can retry soon.
pub async fn load(app: &AppDb, url: &str, edge: u32) -> Result<Option<Rendition>> {
    if let Some((hash, error, fetched)) = app.media_get(url)? {
        match hash {
            Some(h) => {
                if let Some((w, hh, png, rgba, dom)) = app.rendition_get(&h, if edge <= ICON { ICON } else { THUMB })? {
                    // Pixels are kept as PNG only (rows from older versions also carry raw RGBA).
                    let rgba = if rgba.len() == (w * hh * 4) as usize {
                        rgba
                    } else {
                        let bytes = png.clone();
                        tokio::task::spawn_blocking(move || {
                            image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                                .map(|i| i.to_rgba8().into_raw())
                                .unwrap_or_default()
                        })
                        .await
                        .unwrap_or_default()
                    };
                    return Ok(Some(Rendition {
                        hash: h,
                        width: w,
                        height: hh,
                        png,
                        rgba,
                        dominant: ((dom >> 16) as u8, (dom >> 8) as u8, dom as u8),
                    }));
                }
            }
            None if !error.is_empty() && crate::registry::now().saturating_sub(fetched) < RETRY_AFTER => return Ok(None),
            None => {}
        }
    }
    let (source, verify) = match resolve(url)? {
        Source::Inline(b) => (Err(b), None),
        Source::Remote(u) => (Ok(u), None),
        Source::Ipfs(located) => (Ok(located.url), located.verify),
    };
    let bytes = match source {
        Err(b) => b,
        Ok(u) => match http::get_with(&u, http::MAX_IMAGE_BYTES, http::Priority::Background).await {
            Ok(f) => f.bytes,
            Err(e) => {
                // Only permanent failures are remembered; a timeout, a busy request budget or a
                // rate limit is retried on the next request.
                let text = e.to_string();
                if matches!(e, CoreError::NotFound(_)) || text.contains("larger than") {
                    app.media_put(url, None, &text)?;
                    return Ok(None);
                }
                return Err(e);
            }
        },
    };
    // Content that is not what its CID names is refused, and remembered: a gateway that answers
    // with the wrong bytes will answer with them again.
    if let Some(cid) = &verify
        && cid.verifies_content(&bytes) == Some(false)
    {
        app.media_put(url, None, "the IPFS gateway returned content that does not match its CID")?;
        return Ok(None);
    }
    let bytes_for_decode = bytes.clone();
    let decoded = tokio::task::spawn_blocking(move || {
        let thumb = make_rendition(&bytes_for_decode, THUMB);
        let icon = make_rendition(&bytes_for_decode, ICON);
        (thumb, icon)
    })
    .await
    .map_err(|e| CoreError::Storage(format!("image worker: {e}")))?;
    match decoded {
        (Ok(thumb), Ok(icon)) => {
            for r in [&thumb, &icon] {
                let dom = (u32::from(r.dominant.0) << 16) | (u32::from(r.dominant.1) << 8) | u32::from(r.dominant.2);
                let size = if r.width.max(r.height) > ICON { THUMB } else { ICON };
                app.rendition_put(&r.hash, size, r.width, r.height, &r.png, &[], dom)?;
            }
            app.media_put(url, Some(&thumb.hash), "")?;
            Ok(Some(if edge <= ICON { icon } else { thumb }))
        }
        (Err(e), _) | (_, Err(e)) => {
            app.media_put(url, None, &e.to_string())?;
            Ok(None)
        }
    }
}

/// Monogram badge text (up to two letters) and a stable color derived from the contract.
pub fn monogram(symbol: &str, contract: &str) -> (String, (u8, u8, u8)) {
    let letters: String = symbol.chars().filter(|c| c.is_alphanumeric()).take(2).collect::<String>().to_uppercase();
    let letters = if letters.is_empty() { "?".to_string() } else { letters };
    let digest = Sha256::digest(contract.to_lowercase().as_bytes());
    // HSL with fixed saturation/lightness keeps badges readable on light and dark themes.
    let hue = f64::from(u16::from_be_bytes([digest[0], digest[1]])) / 65535.0 * 360.0;
    (letters, hsl(hue, 0.55, 0.52))
}

fn hsl(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to(r), to(g), to(b))
}

/// Generate a test PNG (solid color with a diagonal stripe) for fixtures and render tests.
pub fn fixture_png(width: u32, height: u32, color: (u8, u8, u8)) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(width, height, |x, y| {
        if x == y { image::Rgba([255, 255, 255, 255]) } else { image::Rgba([color.0, color.1, color.2, 255]) }
    });
    let mut png = Vec::new();
    let _ = image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quainance's proxy fetches what it is asked for, so a token's image there is taken only in
    /// the one shape the wallet builds: a CID, nothing after it.
    #[test]
    fn quainance_media_is_a_cid_or_nothing() {
        assert!(matches!(resolve("https://www.quainance.com/api/media/QmAbc123"), Ok(Source::Remote(_))));
        for url in [
            "https://www.quainance.com/api/media/QmAbc?url=https://tracker.example/x.png",
            "https://www.quainance.com/api/media/proxy/https://tracker.example/x.png",
            "https://www.quainance.com/launch/0x00aa",
            "https://www.quainance.com/api/media/",
        ] {
            assert!(matches!(resolve(url), Err(CoreError::Rejected(_))), "{url}");
        }
    }

    #[test]
    fn sniffing_ignores_declared_types() {
        assert_eq!(sniff(&fixture_png(2, 2, (1, 2, 3))), Some(Format::Png));
        assert_eq!(sniff(b"GIF89a...."), Some(Format::Gif));
        assert_eq!(sniff(b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>"), Some(Format::Svg));
        assert_eq!(sniff(b"<html><script>"), None);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::WebP));
    }

    #[test]
    fn raster_renditions_fit_and_find_color() {
        let png = fixture_png(640, 320, (200, 30, 40));
        let r = make_rendition(&png, THUMB).unwrap();
        assert_eq!((r.width, r.height), (256, 128));
        assert_eq!(r.rgba.len(), (256 * 128 * 4) as usize);
        let (red, green, _) = r.dominant;
        assert!(red > 150 && green < 80, "{:?}", r.dominant);
        let icon = make_rendition(&png, ICON).unwrap();
        assert_eq!(icon.width, 32);
        assert_eq!(&icon.png[1..4], b"PNG");
        // Small images are not upscaled.
        assert_eq!(make_rendition(&fixture_png(10, 10, (0, 0, 255)), THUMB).unwrap().width, 10);
    }

    #[test]
    fn svg_renders_without_external_references() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="512" height="512" viewBox="0 0 512 512">
            <circle cx="256" cy="256" r="240" fill="#FF2D00"/>
            <image href="/etc/passwd" width="10" height="10"/>
            <image xlink:href="https://evil.example/track.png" width="10" height="10"/>
            <script>alert(1)</script></svg>"##;
        let r = make_rendition(svg, ICON).unwrap();
        assert_eq!((r.width, r.height), (32, 32));
        assert!(r.dominant.0 > 200 && r.dominant.1 < 90, "{:?}", r.dominant);
        let entity = br#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY x "y">]><svg xmlns="http://www.w3.org/2000/svg"/>"#;
        assert!(make_rendition(entity, ICON).is_err());
    }

    #[test]
    fn decode_bombs_and_garbage_are_refused() {
        assert!(make_rendition(b"not an image", ICON).is_err());
        // A PNG header claiming 100000×100000 pixels.
        let mut png = fixture_png(1, 1, (0, 0, 0));
        png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(make_rendition(&png, ICON).is_err());
    }

    /// With a gateway configured, every way metadata names IPFS content goes to it — `ipfs://`,
    /// a hard-coded public gateway link, or a link to the gateway itself — and a local node may be
    /// plain http while nothing else on the local network becomes reachable.
    #[test]
    fn ipfs_goes_to_the_configured_gateway() {
        let _gateway = crate::ipfs::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::ipfs::set_gateway(crate::ipfs::Content::Media, Some("http://127.0.0.1:8080")).unwrap();
        let url = |s: &str| match resolve(s) {
            Ok(Source::Ipfs(l)) => l.url,
            _ => String::from("refused"),
        };
        assert_eq!(url("ipfs://QmAbc/1.png"), "http://127.0.0.1:8080/ipfs/QmAbc/1.png");
        assert_eq!(url("https://ipfs.io/ipfs/QmAbc/1.png"), "http://127.0.0.1:8080/ipfs/QmAbc/1.png");
        assert_eq!(url("http://127.0.0.1:8080/ipfs/QmAbc"), "http://127.0.0.1:8080/ipfs/QmAbc");
        // Other local addresses are still not reachable from a minted URL.
        assert!(resolve("http://127.0.0.1:9200/").is_err() && resolve("http://10.0.0.12:8080/ipfs/QmAbc").is_err());
        // On the gateway itself, only content: not a name to resolve, not the node's API.
        assert_eq!(url("http://127.0.0.1:8080/ipns/tracker.example/1.png"), "refused");
        assert_eq!(url("http://127.0.0.1:8080/api/v0/id"), "refused");
        assert_eq!(url("http://127.0.0.1:8080/ipfs/QmAbc/1.png?seen=me"), "http://127.0.0.1:8080/ipfs/QmAbc/1.png", "no query rides along");
        // A subdomain gateway gets a CID label it can carry.
        crate::ipfs::set_gateway(crate::ipfs::Content::Media, Some("https://{cid}.ipfs.dweb.link")).unwrap();
        assert_eq!(
            url("ipfs://QmdfTbBqBPQ7VNxZEYEj14VmRuZBkqFbiwReogJgS1zR1n"),
            "https://bafybeihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku.ipfs.dweb.link/"
        );
        crate::ipfs::set_gateway(crate::ipfs::Content::Media, None).unwrap();
        assert_eq!(url("ipfs://QmAbc"), "https://ipfs.qu.ai/ipfs/QmAbc", "cleared, it is the default again");
    }

    /// A gateway that answers a raw CID with other bytes is not believed, and is not asked again.
    // The lock only serialises tests over the process-wide gateway; this test's runtime is
    // single-threaded, and the other holders are plain synchronous tests.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn content_that_does_not_match_its_cid_is_refused() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let png = fixture_png(4, 4, (200, 10, 10));
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let head = format!("HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ncontent-length: {}\r\n\r\n", png.len());
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&png).await;
            }
        });
        let app = AppDb::memory().unwrap();
        // "hello world" is what this CID names; the gateway sends a PNG instead.
        let lying = "ipfs://bafkreifzjut3te2nhyekklss27nh3k72ysco7y32koao5eei66wof36n5e";
        let honest_path = "ipfs://QmdfTbBqBPQ7VNxZEYEj14VmRuZBkqFbiwReogJgS1zR1n/pic.png";
        let (first, second) = {
            let _gateway = crate::ipfs::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            crate::ipfs::set_gateway(crate::ipfs::Content::Media, Some(&format!("http://127.0.0.1:{port}"))).unwrap();
            let first = load(&app, lying, ICON).await;
            // Nothing to check a file inside a DAG against: it is shown.
            let second = load(&app, honest_path, ICON).await;
            crate::ipfs::set_gateway(crate::ipfs::Content::Media, None).unwrap();
            (first, second)
        };
        assert!(matches!(first, Ok(None)), "refused: {first:?}");
        assert!(app.media_get(lying).unwrap().is_some_and(|(_, e, _)| e.contains("does not match")));
        assert!(matches!(second, Ok(Some(_))), "{second:?}");
    }

    #[test]
    fn sources_and_monograms() {
        let _gateway = crate::ipfs::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(matches!(resolve("ipfs://QmAbc/1.png").unwrap(), Source::Ipfs(l) if l.url == "https://ipfs.qu.ai/ipfs/QmAbc/1.png"));
        // Only the explorer and the IPFS gateway, only over HTTPS: a minted URL cannot make the
        // wallet call a tracker or a machine on the local network.
        assert!(matches!(resolve("https://explorer.qu.ai/api/nft-media/ab").unwrap(), Source::Remote(_)));
        assert!(matches!(resolve("https://orchard.quaiscan.io/x.png").unwrap(), Source::Remote(_)));
        for refused in [
            "http://explorer.qu.ai/a.png",
            "https://tracker.example/pixel.png",
            "https://192.168.1.1/a.png",
            "https://explorer.qu.ai.evil.io/a.png",
            "file:///etc/passwd",
        ] {
            assert!(resolve(refused).is_err(), "{refused}");
        }
        assert!(resolve("file:///etc/passwd").is_err());
        assert!(resolve("javascript:alert(1)").is_err());
        let inline = resolve("data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciLz4=").unwrap();
        assert!(matches!(inline, Source::Inline(b) if b.starts_with(b"<svg")));
        let (letters, color) = monogram("boss", "0xAB");
        assert_eq!(letters, "BO");
        assert_eq!(monogram("boss", "0xab").1, color);
        assert_ne!(monogram("boss", "0xac").1, color);
        assert_eq!(monogram("", "0x1").0, "?");
    }

    #[tokio::test]
    async fn load_caches_inline_images() {
        use base64::Engine;
        let app = AppDb::memory().unwrap();
        let url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(fixture_png(64, 64, (10, 200, 10))));
        let first = load(&app, &url, ICON).await.unwrap().unwrap();
        assert_eq!(first.width, 32);
        let again = load(&app, &url, THUMB).await.unwrap().unwrap();
        assert_eq!(again.width, 64);
        assert_eq!(again.hash, first.hash);
        assert!(load(&app, "data:image/png;base64,AAAA", ICON).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn native_logos_render_in_brand_colors() {
        let app = AppDb::memory().unwrap();
        assert!(native_icon("QUAI").is_some() && native_icon("wqi").is_none());
        for asset in ["quai", "qi"] {
            let url = native_icon(asset).unwrap();
            assert!(is_native_icon(url));
            let icon = load(&app, url, ICON).await.unwrap().unwrap();
            let large = load(&app, url, ICON_LARGE).await.unwrap().unwrap();
            assert_eq!((icon.width, large.width), (ICON, THUMB), "{asset}");
            // Both marks are drawn in Quai red-orange (Qi on a black disc).
            let red = large.rgba.chunks_exact(4).filter(|p| p[0] > 200 && p[1] < 80 && p[2] < 40 && p[3] > 200).count();
            assert!(red > 1000, "{asset}: {red} red pixels");
        }
    }
}
