//! Untrusted pictures are decoded in a separate process.
//!
//! A token or NFT names its own image, and anyone can mint one, so the bytes behind it are hostile
//! until proven otherwise. Decoders (PNG, JPEG, GIF, WebP, and SVG through resvg) are large, and
//! `forbid(unsafe_code)` covers this workspace, not them. So they never run in a process that
//! can hold keys:
//!
//! - The wallet re-executes its own binary with [`HELPER_ARG`]. `exec` gives the helper a fresh
//!   address space: no vault, no unlocked keys, no open wallet.
//! - The helper reads one request from stdin, then shuts itself in before touching the bytes:
//!   resource limits (memory, CPU, no files written, no core dump), `no_new_privs`, and on Linux a
//!   seccomp allowlist (memory, reading stdin, writing stdout, exiting — nothing that opens,
//!   connects or executes). Anything else kills it.
//! - It answers with raw RGBA pixels only. The wallet checks every size against what it asked for
//!   and encodes the PNG itself, so nothing the helper produces is decoded again on this side.
//! - A crash, a kill, a timeout or a malformed answer means no picture: the monogram stays.
//!
//! The wire format is fixed-width and bounded:
//!
//! ```text
//! request   "QTMEDIA1" · u32 edge count (1..=4) · u32 edge × count · u32 length · bytes
//! answer    per edge: 0 · u32 width · u32 height · width×height×4 bytes
//!                  or 1 · u16 length · UTF-8 reason
//! ```

use quai_model::error::{CoreError, Result};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// The argument that turns the wallet's binary into the decoding helper.
pub const HELPER_ARG: &str = "--quai-media-decode-helper";
/// Applies the sandbox, then tries to open a file: a working sandbox kills the process.
pub const SELFTEST_ARG: &str = "--quai-media-decode-helper-selftest";

const MAGIC: &[u8; 8] = b"QTMEDIA1";
/// Most renditions one request may ask for.
const MAX_EDGES: usize = 4;
/// Largest edge a rendition may have.
pub const MAX_EDGE: u32 = 1024;
/// Largest source accepted (the HTTP cap already holds fetches below this).
const MAX_SOURCE: usize = crate::http::MAX_IMAGE_BYTES + 1024;
/// How long one decode may take, wall clock.
const TIMEOUT: Duration = Duration::from_secs(15);
/// Helpers running at once.
const CONCURRENCY: usize = 4;

/// Pixels the helper decoded, already checked against the edge they were asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    /// RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// How pictures are decoded in this process.
#[derive(Clone, Debug)]
pub enum Decoder {
    /// A fresh helper process per picture: the wallet's own binary with [`HELPER_ARG`].
    Isolated(PathBuf),
    /// In this process. Only for tests and tools that never hold keys.
    InProcess,
}

static DECODER: OnceLock<Decoder> = OnceLock::new();

/// Decode every untrusted picture in a helper started from `exe` (normally the running binary).
pub fn use_isolated_decoder(exe: PathBuf) {
    let _ = DECODER.set(Decoder::Isolated(exe));
}

/// Decode in this process. For tests and tools that never hold keys; a wallet must not call it.
pub fn use_in_process_decoder_for_tests() {
    let _ = DECODER.set(Decoder::InProcess);
}

fn decoder() -> Result<Decoder> {
    match DECODER.get() {
        Some(d) => Ok(d.clone()),
        None if cfg!(test) => Ok(Decoder::InProcess),
        None => Err(CoreError::Storage("image decoding is not set up in this process".into())),
    }
}

/// Decode `source` into one rendition per edge, each fitted inside `edge × edge`.
///
/// `Err` is a passing failure (no helper could be started, the decoder is not set up): the caller
/// may try again later. Each `Ok` entry is that rendition's own outcome.
pub async fn decode(source: Vec<u8>, edges: &[u32]) -> Result<Vec<std::result::Result<Pixels, String>>> {
    if edges.is_empty() || edges.len() > MAX_EDGES || edges.iter().any(|e| *e == 0 || *e > MAX_EDGE) {
        return Err(CoreError::Invalid("bad rendition sizes".into()));
    }
    if source.len() > MAX_SOURCE {
        return Ok(edges.iter().map(|_| Err("image too large".to_string())).collect());
    }
    match decoder()? {
        Decoder::InProcess => {
            let edges = edges.to_vec();
            tokio::task::spawn_blocking(move || edges.iter().map(|e| decode_one(&source, *e)).collect())
                .await
                .map_err(|e| CoreError::Storage(format!("image worker: {e}")))
        }
        Decoder::Isolated(exe) => isolated(&exe, &source, edges).await,
    }
}

fn gate() -> &'static tokio::sync::Semaphore {
    static GATE: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Semaphore::new(CONCURRENCY))
}

async fn isolated(exe: &std::path::Path, source: &[u8], edges: &[u32]) -> Result<Vec<std::result::Result<Pixels, String>>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _permit = gate().acquire().await.map_err(|_| CoreError::Storage("image decoder closed".into()))?;
    let request = encode_request(source, edges);
    let mut child = tokio::process::Command::new(exe)
        .arg(HELPER_ARG)
        .env_clear()
        .current_dir("/")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| CoreError::Storage(format!("image decoder could not start: {e}")))?;
    let mut stdin = child.stdin.take().ok_or_else(|| CoreError::Storage("image decoder stdin".into()))?;
    let mut stdout = child.stdout.take().ok_or_else(|| CoreError::Storage("image decoder stdout".into()))?;
    let limit = answer_limit(edges) as u64;
    let run = async {
        // Write and read side by side: a helper that dies early must not leave the write hanging.
        let write = async {
            let _ = stdin.write_all(&request).await;
            drop(stdin);
        };
        let mut answer = Vec::new();
        let mut bounded = (&mut stdout).take(limit + 1);
        let read = bounded.read_to_end(&mut answer);
        let ((), read) = tokio::join!(write, read);
        read.map_err(|e| e.to_string())?;
        let status = child.wait().await.map_err(|e| e.to_string())?;
        Ok::<_, String>((answer, status))
    };
    let outcome = tokio::time::timeout(TIMEOUT, run).await;
    let failed = |why: &str| Ok(edges.iter().map(|_| Err(why.to_string())).collect());
    match outcome {
        Err(_) => failed("the image took too long to decode"),
        Ok(Err(_)) => failed("the image decoder failed"),
        Ok(Ok((_, status))) if !status.success() => failed("the image decoder stopped on this picture"),
        Ok(Ok((answer, _))) => match parse_answer(&answer, edges) {
            Ok(renditions) => Ok(renditions),
            Err(_) => failed("the image decoder answered out of form"),
        },
    }
}

fn answer_limit(edges: &[u32]) -> usize {
    edges.iter().map(|e| 9 + (*e as usize) * (*e as usize) * 4).sum::<usize>().max(edges.len() * 3 + 512)
}

/// The request's bytes.
pub fn encode_request(source: &[u8], edges: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + edges.len() * 4 + source.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(edges.len() as u32).to_be_bytes());
    for e in edges {
        out.extend_from_slice(&e.to_be_bytes());
    }
    out.extend_from_slice(&(source.len() as u32).to_be_bytes());
    out.extend_from_slice(source);
    out
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> std::result::Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|end| *end <= self.bytes.len()).ok_or("truncated")?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn u32(&mut self) -> std::result::Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| "truncated")?))
    }
    fn u16(&mut self) -> std::result::Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| "truncated")?))
    }
    fn u8(&mut self) -> std::result::Result<u8, String> {
        Ok(self.take(1)?[0])
    }
}

/// Parse a request (the helper's side).
pub fn parse_request(bytes: &[u8]) -> std::result::Result<(Vec<u32>, &[u8]), String> {
    let mut c = Cursor { bytes, at: 0 };
    if c.take(MAGIC.len())? != MAGIC {
        return Err("not a decode request".into());
    }
    let count = c.u32()? as usize;
    if count == 0 || count > MAX_EDGES {
        return Err("bad rendition count".into());
    }
    let mut edges = Vec::with_capacity(count);
    for _ in 0..count {
        let e = c.u32()?;
        if e == 0 || e > MAX_EDGE {
            return Err("bad rendition size".into());
        }
        edges.push(e);
    }
    let len = c.u32()? as usize;
    if len > MAX_SOURCE {
        return Err("image too large".into());
    }
    let source = c.take(len)?;
    if c.at != bytes.len() {
        return Err("trailing bytes".into());
    }
    Ok((edges, source))
}

/// Parse the helper's answer (the wallet's side). Every size is checked against what was asked:
/// a rendition wider or taller than its edge, a pixel count that does not match, a reason that is
/// not text, or one byte too many or too few refuses the whole answer.
pub fn parse_answer(bytes: &[u8], edges: &[u32]) -> std::result::Result<Vec<std::result::Result<Pixels, String>>, String> {
    let mut c = Cursor { bytes, at: 0 };
    let mut out = Vec::with_capacity(edges.len());
    for edge in edges {
        match c.u8()? {
            0 => {
                let (w, h) = (c.u32()?, c.u32()?);
                if w == 0 || h == 0 || w > *edge || h > *edge {
                    return Err("rendition outside its edge".into());
                }
                let rgba = c.take(w as usize * h as usize * 4)?.to_vec();
                out.push(Ok(Pixels { width: w, height: h, rgba }));
            }
            1 => {
                let len = c.u16()? as usize;
                if len > 256 {
                    return Err("reason too long".into());
                }
                let text = std::str::from_utf8(c.take(len)?).map_err(|_| "reason not text")?;
                out.push(Err(crate::explorer::clean(text, 256)));
            }
            _ => return Err("bad status".into()),
        }
    }
    if c.at != bytes.len() {
        return Err("trailing bytes".into());
    }
    Ok(out)
}

fn encode_answer(renditions: &[std::result::Result<Pixels, String>]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in renditions {
        match r {
            Ok(p) => {
                out.push(0);
                out.extend_from_slice(&p.width.to_be_bytes());
                out.extend_from_slice(&p.height.to_be_bytes());
                out.extend_from_slice(&p.rgba);
            }
            Err(why) => {
                let mut text = why.as_bytes();
                if text.len() > 256 {
                    let mut end = 256;
                    while !why.is_char_boundary(end) {
                        end -= 1;
                    }
                    text = &text[..end];
                }
                out.push(1);
                out.extend_from_slice(&(text.len() as u16).to_be_bytes());
                out.extend_from_slice(text);
            }
        }
    }
    out
}

/// One rendition: decode (with the decoder's own limits) and fit inside `edge × edge`.
pub(crate) fn decode_one(source: &[u8], edge: u32) -> std::result::Result<Pixels, String> {
    let img = crate::media::decode_raw(source, edge).map_err(|e| e.to_string())?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    let scale = (edge as f64 / w.max(h) as f64).min(1.0);
    let (tw, th) = (((w as f64 * scale).round() as u32).clamp(1, edge), ((h as f64 * scale).round() as u32).clamp(1, edge));
    let fitted = if (tw, th) == (w, h) { img } else { image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle) };
    Ok(Pixels { width: tw, height: th, rgba: fitted.into_raw() })
}

/// The helper's `main`. Call first thing in the binary's `main`: when this process was started
/// as the helper it never returns.
pub fn helper_entry() {
    let mut args = std::env::args_os().skip(1);
    let Some(first) = args.next() else { return };
    if first == HELPER_ARG {
        std::process::exit(helper_main());
    }
    if first == SELFTEST_ARG {
        // Shut in exactly as for a picture, then try something the sandbox forbids.
        if sandbox::enter().is_err() {
            std::process::exit(3);
        }
        let opened = std::fs::File::open("/etc/hostname").is_ok();
        std::process::exit(if opened { 1 } else { 2 });
    }
}

fn helper_main() -> i32 {
    let mut request = Vec::new();
    if std::io::stdin().lock().take(MAX_SOURCE as u64 + 64).read_to_end(&mut request).is_err() {
        return 2;
    }
    let mut stdout = std::io::stdout().lock();
    // Nothing untrusted has been parsed yet. From here on the process can read what it already
    // holds, allocate, write its answer and exit — nothing else.
    if sandbox::enter().is_err() {
        return 3;
    }
    let Ok((edges, source)) = parse_request(&request) else { return 4 };
    let renditions: Vec<_> = edges.iter().map(|e| decode_one(source, *e)).collect();
    if stdout.write_all(&encode_answer(&renditions)).and_then(|_| stdout.flush()).is_err() {
        return 5;
    }
    0
}

mod sandbox {
    /// Limits, `no_new_privs` and (Linux) the seccomp allowlist. Fails closed: an error here
    /// means the helper exits without decoding.
    pub fn enter() -> std::result::Result<(), String> {
        limits()?;
        #[cfg(target_os = "linux")]
        seccomp()?;
        Ok(())
    }

    #[cfg(unix)]
    fn limits() -> std::result::Result<(), String> {
        use rustix::process::{Resource, Rlimit, setrlimit};
        let set = |r: Resource, v: u64| setrlimit(r, Rlimit { current: Some(v), maximum: Some(v) }).map_err(|e| format!("{r:?}: {e}"));
        // Decoders are held to 256 MiB of pixels; a GiB of address space leaves room for the
        // allocator and the binary while stopping a runaway.
        #[cfg(target_os = "linux")]
        set(Resource::As, 1 << 30)?;
        set(Resource::Cpu, 10)?;
        set(Resource::Fsize, 0)?;
        set(Resource::Core, 0)?;
        #[cfg(target_os = "linux")]
        rustix::thread::set_no_new_privs(true).map_err(|e| format!("no_new_privs: {e}"))?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn limits() -> std::result::Result<(), String> {
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn seccomp() -> std::result::Result<(), String> {
        use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, TargetArch};
        use std::collections::BTreeMap;
        // Hash maps seed themselves from the OS on first use; do it now, while that is allowed.
        let _ = std::collections::hash_map::RandomState::new();
        let allowed: &[i64] = &[
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_writev,
            libc::SYS_mmap,
            libc::SYS_munmap,
            libc::SYS_mremap,
            libc::SYS_madvise,
            libc::SYS_mprotect,
            libc::SYS_brk,
            libc::SYS_futex,
            libc::SYS_sigaltstack,
            libc::SYS_rt_sigreturn,
            libc::SYS_rt_sigprocmask,
            libc::SYS_rt_sigaction,
            libc::SYS_clock_gettime,
            libc::SYS_getrandom,
            libc::SYS_sched_yield,
            libc::SYS_close,
            libc::SYS_exit,
            libc::SYS_exit_group,
        ];
        let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = allowed.iter().map(|s| (*s, Vec::new())).collect();
        let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|e| format!("seccomp arch: {e}"))?;
        let filter =
            SeccompFilter::new(rules, SeccompAction::KillProcess, SeccompAction::Allow, arch).map_err(|e| format!("seccomp: {e}"))?;
        let program: BpfProgram = filter.try_into().map_err(|e: seccompiler::BackendError| format!("seccomp: {e}"))?;
        seccompiler::apply_filter(&program).map_err(|e| format!("seccomp: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use quai_model::testutil::Rng;

    #[test]
    fn requests_round_trip_and_refuse_anything_else() {
        let req = encode_request(b"abc", &[32, 256]);
        assert_eq!(parse_request(&req).unwrap(), (vec![32, 256], &b"abc"[..]));
        let mut trailing = req.clone();
        trailing.push(0);
        assert!(parse_request(&trailing).is_err());
        assert!(parse_request(&req[..req.len() - 1]).is_err());
        assert!(parse_request(&encode_request(b"x", &[])).is_err());
        assert!(parse_request(&encode_request(b"x", &[MAX_EDGE + 1])).is_err());
        assert!(parse_request(&encode_request(b"x", &[1, 2, 3, 4, 5])).is_err());
    }

    #[test]
    fn answers_are_held_to_the_edges_asked_for() {
        let ok = Pixels { width: 2, height: 1, rgba: vec![1; 8] };
        let bytes = encode_answer(&[Ok(ok.clone()), Err("bad".into())]);
        assert_eq!(parse_answer(&bytes, &[32, 32]).unwrap(), vec![Ok(ok.clone()), Err("bad".into())]);
        // Wider than asked, a missing rendition, a byte too many.
        assert!(parse_answer(&bytes, &[1, 32]).is_err());
        assert!(parse_answer(&bytes, &[32, 32, 32]).is_err());
        let mut extra = bytes.clone();
        extra.push(9);
        assert!(parse_answer(&extra, &[32, 32]).is_err());
        // A claimed size with too few pixels behind it.
        let mut short = encode_answer(&[Ok(Pixels { width: 4, height: 4, rgba: vec![0; 64] })]);
        short.truncate(short.len() - 1);
        assert!(parse_answer(&short, &[32]).is_err());
    }

    /// Fuzz: random and mutated answers never panic, and whatever is accepted is within bounds.
    #[test]
    fn fuzz_answer_parser() {
        let seed_answers = [
            encode_answer(&[Ok(Pixels { width: 3, height: 2, rgba: vec![7; 24] })]),
            encode_answer(&[Err("no".into()), Ok(Pixels { width: 1, height: 1, rgba: vec![0; 4] })]),
        ];
        for seed in 0..20_000u64 {
            let mut rng = Rng::new(seed);
            let mut bytes = if rng.below(3) == 0 {
                (0..rng.below(64)).map(|_| rng.next() as u8).collect::<Vec<u8>>()
            } else {
                seed_answers[rng.below(seed_answers.len())].clone()
            };
            for _ in 0..rng.below(6) {
                if bytes.is_empty() {
                    break;
                }
                let i = rng.below(bytes.len());
                match rng.below(3) {
                    0 => bytes[i] = rng.next() as u8,
                    1 => {
                        bytes.remove(i);
                    }
                    _ => bytes.insert(i, rng.next() as u8),
                }
            }
            let edges: Vec<u32> = (0..1 + rng.below(3)).map(|_| 1 + rng.below(8) as u32).collect();
            if let Ok(renditions) = parse_answer(&bytes, &edges) {
                assert_eq!(renditions.len(), edges.len());
                for (r, e) in renditions.iter().zip(&edges) {
                    if let Ok(p) = r {
                        assert!(p.width <= *e && p.height <= *e && p.rgba.len() == (p.width * p.height * 4) as usize);
                    }
                }
            }
        }
    }

    /// Fuzz: the request parser (what the helper reads) never panics on garbage.
    #[test]
    fn fuzz_request_parser() {
        let good = encode_request(&crate::media::fixture_png(3, 3, (1, 2, 3)), &[32, 256]);
        for seed in 0..20_000u64 {
            let mut rng = Rng::new(seed);
            let mut bytes = good.clone();
            for _ in 0..1 + rng.below(8) {
                let i = rng.below(bytes.len());
                bytes[i] = rng.next() as u8;
            }
            bytes.truncate(rng.below(bytes.len() + 1).max(1));
            let _ = parse_request(&bytes);
        }
    }

    /// Fuzz the decoders themselves, in process (the helper runs this same code): mutated real
    /// images of every accepted format either decode inside their edge or fail — never panic.
    #[test]
    fn fuzz_decoders_in_process() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#f00"/><circle cx="10" cy="10" r="5"/></svg>"##.to_vec();
        let png = crate::media::fixture_png(24, 16, (9, 90, 200));
        let mut gif = Vec::new();
        let mut jpeg = Vec::new();
        let mut webp = Vec::new();
        let img =
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(20, 12, |x, y| image::Rgba([x as u8 * 9, y as u8 * 17, 80, 255])));
        img.write_to(&mut std::io::Cursor::new(&mut gif), image::ImageFormat::Gif).unwrap();
        image::DynamicImage::ImageRgb8(img.to_rgb8()).write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        img.write_to(&mut std::io::Cursor::new(&mut webp), image::ImageFormat::WebP).unwrap();
        let seeds = [png, gif, jpeg, webp, svg];
        for s in &seeds {
            let p = decode_one(s, 32).unwrap();
            assert!(p.width <= 32 && p.height <= 32);
        }
        let iterations: u64 = std::env::var("QW_FUZZ_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(3_000);
        for seed in 0..iterations {
            let mut rng = Rng::new(seed);
            let mut bytes = seeds[rng.below(seeds.len())].clone();
            for _ in 0..1 + rng.below(12) {
                let i = rng.below(bytes.len());
                match rng.below(4) {
                    0 => bytes[i] = rng.next() as u8,
                    1 => bytes[i] ^= 1 << rng.below(8),
                    2 => bytes.truncate(i.max(8)),
                    _ => {
                        let v = [0u8, 0xff, 0x7f, 0x80][rng.below(4)];
                        let end = (i + 4).min(bytes.len());
                        bytes[i..end].fill(v);
                    }
                }
            }
            let edge = [1u32, 32, 256][rng.below(3)];
            if let Ok(p) = decode_one(&bytes, edge) {
                assert!(p.width <= edge && p.height <= edge && p.rgba.len() == (p.width * p.height * 4) as usize, "seed {seed}");
            }
        }
    }
}
