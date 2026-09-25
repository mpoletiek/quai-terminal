//! The picture-decoding helper, as the wallet runs it: this crate's own binary, re-executed.
//!
//! These run the real process, so they check what unit tests cannot: that the sandbox holds (a
//! forbidden call kills the helper), and that it does not break decoding (every picture the
//! in-process decoder accepts, the sandboxed helper returns pixel for pixel).

use wallet_core::media_helper::{self, Pixels};

fn helper() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_quai-terminal"))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn samples() -> Vec<(&'static str, Vec<u8>)> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(40, 24, |x, y| image::Rgba([x as u8 * 6, y as u8 * 10, 120, 255])));
    let encode = |img: &image::DynamicImage, f| {
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), f).unwrap();
        out
    };
    vec![
        ("png", wallet_core::media::fixture_png(64, 32, (200, 30, 40))),
        ("gif", encode(&img, image::ImageFormat::Gif)),
        ("jpeg", encode(&image::DynamicImage::ImageRgb8(img.to_rgb8()), image::ImageFormat::Jpeg)),
        ("webp", encode(&img, image::ImageFormat::WebP)),
        (
            "svg",
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="150"><rect width="300" height="150" fill="#1e90ff"/><path d="M10 10 L290 140" stroke="#fff" stroke-width="9"/><text x="20" y="80">hi</text></svg>"##.to_vec(),
        ),
    ]
}

fn isolated(source: &[u8], edges: &[u32]) -> Vec<Result<Pixels, String>> {
    wallet_core::media_helper::use_isolated_decoder(helper());
    runtime().block_on(media_helper::decode(source.to_vec(), edges)).unwrap()
}

#[test]
fn a_working_sandbox_kills_the_helper_on_a_forbidden_call() {
    let status = std::process::Command::new(helper()).arg(media_helper::SELFTEST_ARG).env_clear().status().unwrap();
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(31), "seccomp must kill the helper with SIGSYS; got {status:?}");
    }
    // Elsewhere there is no syscall filter: the limits apply, the open succeeds.
    #[cfg(not(target_os = "linux"))]
    assert!(status.code().is_some());
}

#[test]
fn every_format_decodes_in_the_sandbox_as_it_does_in_process() {
    for (name, bytes) in samples() {
        let got = isolated(&bytes, &[256, 32]);
        for (edge, r) in [256u32, 32].iter().zip(got) {
            let p = r.unwrap_or_else(|e| panic!("{name} at {edge}: {e}"));
            let local = wallet_core::media::trusted_rendition(&bytes, *edge).unwrap();
            assert_eq!((p.width, p.height), (local.width, local.height), "{name}");
            assert_eq!(p.rgba, local.rgba, "{name} at {edge}: pixels differ between the helper and in-process");
        }
    }
}

#[test]
fn hostile_inputs_fail_quietly() {
    // A PNG claiming 100000 × 100000 pixels, garbage, an SVG with an entity, an empty file.
    let mut bomb = wallet_core::media::fixture_png(1, 1, (0, 0, 0));
    bomb[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    bomb[20..24].copy_from_slice(&100_000u32.to_be_bytes());
    let entity = br#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY x "y">]><svg xmlns="http://www.w3.org/2000/svg"/>"#.to_vec();
    for bytes in [bomb, b"not an image".to_vec(), entity, Vec::new()] {
        for r in isolated(&bytes, &[32]) {
            assert!(r.is_err());
        }
    }
}

/// Differential fuzz through the real helper: mutated pictures of every format. Whatever the
/// in-process decoder makes of an input, the sandboxed helper must make the same — so a system
/// call missing from the allowlist (which would kill the helper) shows up as a mismatch.
#[test]
fn fuzz_the_sandboxed_helper_against_the_in_process_decoder() {
    let seeds = samples();
    let iterations: u64 = std::env::var("QW_FUZZ_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    for seed in 0..iterations {
        let mut rng = Rng::new(seed);
        let (name, mut bytes) = seeds[rng.below(seeds.len())].clone();
        for _ in 0..1 + rng.below(10) {
            let i = rng.below(bytes.len());
            match rng.below(3) {
                0 => bytes[i] = rng.next() as u8,
                1 => bytes[i] ^= 1 << rng.below(8),
                _ => bytes.truncate(i.max(12)),
            }
        }
        let edge = [16u32, 32, 256][rng.below(3)];
        let local = wallet_core::media::trusted_rendition(&bytes, edge);
        let remote = isolated(&bytes, &[edge]).remove(0);
        match (local, remote) {
            (Ok(l), Ok(r)) => assert_eq!((l.width, l.height, l.rgba), (r.width, r.height, r.rgba), "seed {seed} ({name})"),
            (Err(_), Err(_)) => {}
            (Ok(_), Err(e)) => panic!("seed {seed} ({name}): decodes in process but the helper failed: {e}"),
            (Err(e), Ok(_)) => panic!("seed {seed} ({name}): the helper decoded what the in-process decoder refused: {e}"),
        }
    }
}
