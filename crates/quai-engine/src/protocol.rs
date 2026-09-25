//! The engine socket's protocol: what a client (the TUI) and the host (the daemon, or the same
//! process with `--standalone`) say to each other.
//!
//! A frame is a 4-byte big-endian length and that many bytes of CBOR. The first frame each way
//! is the handshake ([`ClientMsg::Hello`], [`HostMsg::Welcome`]); a host refuses a client that
//! speaks another [`PROTOCOL`] and says why. Then the client attaches to one wallet on one
//! network, and from there it is the engine's own [`Cmd`] and [`Ev`], plus the unlock, which the
//! host answers itself: the password is checked where the keys will live, and the keys never
//! come back.
//!
//! Frames are bounded both ways ([`CLIENT_FRAME_LIMIT`], [`HOST_FRAME_LIMIT`]) and anything that
//! does not parse closes the connection. Every buffer a frame passes through is wiped when it
//! goes: an unlock carries a password, and an export carries the recovery phrase.

use crate::worker::{Cmd, Ev};
use zeroize::Zeroizing;

/// This build's protocol. Both ends must speak the same one; anything that changes a message's
/// shape changes it.
pub const PROTOCOL: u32 = 1;

/// The largest frame a host reads from a client. A client sends commands, never data.
pub const CLIENT_FRAME_LIMIT: usize = 1 << 20;

/// The largest frame a client reads from the host: a dashboard carries the wallet's recent
/// activity and a journal read carries its operations.
pub const HOST_FRAME_LIMIT: usize = 64 << 20;

/// From a client.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum ClientMsg {
    /// The first frame: the protocol and the program the client runs ([`build`] of it).
    Hello { protocol: u32, build: String },
    /// Start an engine for this wallet on this network. Its keys start locked for this client,
    /// whatever any other client or the daemon holds.
    Attach { wallet: String, network: String },
    /// Check this password and hold the wallet's keys for this client. Answered with
    /// [`Ev::Unlocked`] (from the worker, once the keys are in) or [`Ev::UnlockFailed`].
    Unlock { wallet: String, password: Zeroizing<String> },
    /// A command for the engine.
    Cmd(Cmd),
    /// Someone is at the keyboard: the engine's auto-lock starts over.
    Activity,
}

/// From the host.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum HostMsg {
    /// The handshake's answer: the host's protocol and program.
    Welcome { protocol: u32, build: String },
    /// Not served, and why. The connection closes after it.
    Refused(String),
    /// An engine event.
    Ev(Ev),
}

/// A frame that could not be read or written.
#[derive(Debug)]
pub enum FrameError {
    /// The peer went away (cleanly, between frames, or mid-frame).
    Closed,
    /// A frame larger than the limit was announced.
    TooLarge(usize),
    /// The bytes were not a message of this protocol.
    Malformed(String),
    Io(std::io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Closed => f.write_str("the other end closed the connection"),
            FrameError::TooLarge(n) => write!(f, "a frame of {n} bytes is over the limit"),
            FrameError::Malformed(why) => write!(f, "malformed frame: {why}"),
            FrameError::Io(e) => write!(f, "engine socket: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// A message as one frame: the length, then the body. Wiped when dropped.
pub fn encode<T: serde::Serialize>(msg: &T) -> Result<Zeroizing<Vec<u8>>, FrameError> {
    let mut out = Zeroizing::new(vec![0u8; 4]);
    ciborium::into_writer(msg, &mut *out).map_err(|e| FrameError::Malformed(e.to_string()))?;
    let len = u32::try_from(out.len() - 4).map_err(|_| FrameError::TooLarge(out.len()))?;
    out[..4].copy_from_slice(&len.to_be_bytes());
    Ok(out)
}

/// A frame's body as a message.
pub fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    ciborium::from_reader(body).map_err(|e| FrameError::Malformed(e.to_string()))
}

/// The first whole frame at the start of `buf`: its body and how many bytes it took. `None`
/// while more bytes are needed. A length over `limit` is an error at once, before any of the
/// body is waited for, so an announced giant costs nothing.
pub fn split(buf: &[u8], limit: usize) -> Result<Option<(&[u8], usize)>, FrameError> {
    let Some(head) = buf.get(..4) else { return Ok(None) };
    let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if len > limit {
        return Err(FrameError::TooLarge(len));
    }
    Ok(buf.get(4..4 + len).map(|body| (body, 4 + len)))
}

/// Read one frame's body, at most `limit` bytes of it. Wiped when dropped.
pub async fn read_frame(read: &mut (impl tokio::io::AsyncRead + Unpin), limit: usize) -> Result<Zeroizing<Vec<u8>>, FrameError> {
    use tokio::io::AsyncReadExt;
    let mut head = [0u8; 4];
    match read.read_exact(&mut head).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(FrameError::Io(e)),
    }
    let len = u32::from_be_bytes(head) as usize;
    if len > limit {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = Zeroizing::new(vec![0u8; len]);
    match read.read_exact(&mut body).await {
        Ok(_) => Ok(body),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(FrameError::Closed),
        Err(e) => Err(FrameError::Io(e)),
    }
}

/// Write one message as a frame.
pub async fn write_msg<T: serde::Serialize>(write: &mut (impl tokio::io::AsyncWrite + Unpin), msg: &T) -> Result<(), FrameError> {
    use tokio::io::AsyncWriteExt;
    let frame = encode(msg)?;
    write.write_all(&frame).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset => FrameError::Closed,
        _ => FrameError::Io(e),
    })
}

/// Read one message.
pub async fn read_msg<T: serde::de::DeserializeOwned>(
    read: &mut (impl tokio::io::AsyncRead + Unpin),
    limit: usize,
) -> Result<T, FrameError> {
    let body = read_frame(read, limit).await?;
    decode(&body)
}

/// Which program this is: version, path, size and modification time. A rebuild or an upgrade
/// changes it, which is how a client knows the host runs other code.
pub fn build() -> String {
    let exe = std::env::current_exe().ok();
    let meta = exe.as_ref().and_then(|e| std::fs::metadata(e).ok());
    let modified = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    format!(
        "{} {} {} {}",
        env!("CARGO_PKG_VERSION"),
        exe.map(|e| e.display().to_string()).unwrap_or_default(),
        meta.map_or(0, |m| m.len()),
        modified.map_or(0, |d| d.as_secs())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::{ChatOp, MsgOp};

    /// Xorshift: the same bytes on every run, so a failure reproduces.
    struct Rng(u64);
    impl Rng {
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

    fn iterations(default: usize) -> usize {
        std::env::var("QW_FUZZ_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    fn client_samples() -> Vec<ClientMsg> {
        vec![
            ClientMsg::Hello { protocol: PROTOCOL, build: "0.1.0 /bin/qt 123 456".into() },
            ClientMsg::Attach { wallet: "w-1".into(), network: "mainnet".into() },
            ClientMsg::Unlock { wallet: "w-1".into(), password: Zeroizing::new("correct horse ✓ \u{0}".into()) },
            ClientMsg::Activity,
            ClientMsg::Cmd(Cmd::Refresh { full: true }),
            ClientMsg::Cmd(Cmd::Lock),
            ClientMsg::Cmd(Cmd::CommitConfirmed { op_id: "op".into(), words: "pay 5b32".into() }),
            ClientMsg::Cmd(Cmd::SplitQuote {
                key: u64::MAX,
                account: None,
                from: "QUAI".into(),
                to: "0x00".into(),
                amount: "1.5".into(),
                slippage: 50,
            }),
            ClientMsg::Cmd(Cmd::Messaging { op: MsgOp::Refresh { open: Some("peer".into()), sync: true }, epoch: 7 }),
            ClientMsg::Cmd(Cmd::Chat(ChatOp::Pin { target: None, label: "x".into() })),
            ClientMsg::Cmd(Cmd::ImportKey { label: None, key: Zeroizing::new("00".repeat(32)), password: Zeroizing::new("pw".into()) }),
            ClientMsg::Cmd(Cmd::ExportPhrase(Zeroizing::new("pw".into()))),
        ]
    }

    fn host_samples() -> Vec<HostMsg> {
        vec![
            HostMsg::Welcome { protocol: PROTOCOL, build: "b".into() },
            HostMsg::Refused("protocol 2 is not this host's".into()),
            HostMsg::Ev(Ev::Head(u64::MAX)),
            HostMsg::Ev(Ev::Unlocked),
            HostMsg::Ev(Ev::UnlockFailed("wrong password".into())),
            HostMsg::Ev(Ev::Dashboard(Box::default())),
            HostMsg::Ev(Ev::CommitError { op_id: "op".into(), message: "m".into(), ambiguous: true }),
            HostMsg::Ev(Ev::Secret(Zeroizing::new("abandon ".repeat(12)))),
            HostMsg::Ev(Ev::ChatNews { epoch: 3, news: vec![("t".into(), "b".into())] }),
            HostMsg::Ev(Ev::Pnl(Err("no trades".into()))),
        ]
    }

    /// What a message says, as CBOR re-encoded: equal bytes mean an equal message.
    fn same<T: serde::Serialize>(a: &T, b: &T) -> bool {
        *encode(a).unwrap() == *encode(b).unwrap()
    }

    #[test]
    fn every_sample_survives_the_trip_both_ways() {
        for msg in client_samples() {
            let frame = encode(&msg).unwrap();
            let (body, used) = split(&frame, CLIENT_FRAME_LIMIT).unwrap().unwrap();
            assert_eq!(used, frame.len());
            let back: ClientMsg = decode(body).unwrap();
            assert!(same(&msg, &back));
        }
        for msg in host_samples() {
            let frame = encode(&msg).unwrap();
            let (body, _) = split(&frame, HOST_FRAME_LIMIT).unwrap().unwrap();
            let back: HostMsg = decode(body).unwrap();
            assert!(same(&msg, &back));
        }
    }

    #[test]
    fn a_dashboard_with_prices_that_are_not_numbers_still_arrives() {
        // JSON would write NaN as null and then fail to read it back: the whole dashboard lost.
        let dash = crate::worker::Dashboard {
            price: Some(wallet_core::extras::Price { usd_per_quai: f64::NAN, fetched_at: 1, source: "s".into() }),
            ..Default::default()
        };
        let frame = encode(&HostMsg::Ev(Ev::Dashboard(Box::new(dash)))).unwrap();
        let back: HostMsg = decode(&frame[4..]).unwrap();
        let HostMsg::Ev(Ev::Dashboard(d)) = back else { panic!("not a dashboard") };
        assert!(d.price.is_some_and(|p| p.usd_per_quai.is_nan()));
    }

    #[test]
    fn keys_and_host_only_commands_never_cross() {
        // The keys go from the host to its worker in-process; no frame can carry them, and a
        // client cannot pretend to be the signing lane.
        for name in ["UseKeys", "QiSynced", "Committed", "Journal"] {
            let mut bytes = Vec::new();
            ciborium::into_writer(&ciborium::Value::Map(vec![(name.into(), ciborium::Value::Null)]), &mut bytes).unwrap();
            let mut wrapped = Vec::new();
            ciborium::into_writer(&ciborium::Value::Map(vec![("Cmd".into(), ciborium::from_reader(&bytes[..]).unwrap())]), &mut wrapped)
                .unwrap();
            assert!(decode::<ClientMsg>(&wrapped).is_err(), "{name} must not decode from a client");
            let text: ciborium::Value = ciborium::Value::Map(vec![("Cmd".into(), ciborium::Value::Text(name.into()))]);
            let mut unit = Vec::new();
            ciborium::into_writer(&text, &mut unit).unwrap();
            assert!(decode::<ClientMsg>(&unit).is_err(), "{name} must not decode from a client");
        }
    }

    #[test]
    fn a_frame_over_the_limit_is_refused_before_its_body_arrives() {
        let mut buf = ((CLIENT_FRAME_LIMIT + 1) as u32).to_be_bytes().to_vec();
        assert!(matches!(split(&buf, CLIENT_FRAME_LIMIT), Err(FrameError::TooLarge(_))));
        buf.truncate(3);
        assert!(split(&buf, CLIENT_FRAME_LIMIT).unwrap().is_none());
        assert!(matches!(split(&u32::MAX.to_be_bytes(), HOST_FRAME_LIMIT), Err(FrameError::TooLarge(_))));
    }

    #[tokio::test]
    async fn the_async_reader_takes_frames_split_anywhere_and_stops_cleanly() {
        let mut stream = Vec::new();
        for msg in client_samples() {
            stream.extend_from_slice(&encode(&msg).unwrap());
        }
        let (mut a, mut b) = tokio::io::duplex(7);
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            for chunk in stream.chunks(5) {
                a.write_all(chunk).await.unwrap();
            }
        });
        let mut n = 0;
        loop {
            match read_msg::<ClientMsg>(&mut b, CLIENT_FRAME_LIMIT).await {
                Ok(_) => n += 1,
                Err(FrameError::Closed) => break,
                Err(e) => panic!("{e}"),
            }
        }
        writer.await.unwrap();
        assert_eq!(n, client_samples().len());
    }

    /// Fuzz: random bytes, and valid frames with bytes flipped, cut or grown, never panic the
    /// codec; whatever decodes re-encodes to a frame that decodes to the same message.
    #[test]
    fn fuzz_the_codec_with_random_and_mutated_frames() {
        let mut rng = Rng(0x5eed_f00d_cafe_d00d);
        let seeds: Vec<Vec<u8>> = client_samples().iter().map(|m| encode(m).unwrap().to_vec()).collect();
        let host_seeds: Vec<Vec<u8>> = host_samples().iter().map(|m| encode(m).unwrap().to_vec()).collect();
        let mut decoded = 0usize;
        for i in 0..iterations(20_000) {
            let mut bytes = match i % 3 {
                0 => (0..rng.below(64)).map(|_| rng.next() as u8).collect::<Vec<u8>>(),
                1 => seeds[rng.below(seeds.len())].clone(),
                _ => host_seeds[rng.below(host_seeds.len())].clone(),
            };
            for _ in 0..=rng.below(4) {
                if bytes.is_empty() {
                    break;
                }
                let at = rng.below(bytes.len());
                match rng.below(4) {
                    0 => bytes[at] ^= 1 << rng.below(8),
                    1 => bytes[at] = rng.next() as u8,
                    2 => bytes.truncate(at),
                    _ => bytes.insert(at, rng.next() as u8),
                }
            }
            // Half the time the length is made right again, so the mutation reaches the decoder
            // rather than stopping at a length that no longer matches.
            if i % 2 == 0 && bytes.len() >= 4 {
                let len = (bytes.len() - 4) as u32;
                bytes[..4].copy_from_slice(&len.to_be_bytes());
            }
            let Ok(Some((body, _))) = split(&bytes, CLIENT_FRAME_LIMIT) else { continue };
            if let Ok(msg) = decode::<ClientMsg>(body) {
                decoded += 1;
                let again = encode(&msg).unwrap();
                let back: ClientMsg = decode(&again[4..]).unwrap();
                assert!(same(&msg, &back));
            }
            if let Ok(msg) = decode::<HostMsg>(body) {
                decoded += 1;
                let again = encode(&msg).unwrap();
                let back: HostMsg = decode(&again[4..]).unwrap();
                assert!(same(&msg, &back));
            }
        }
        assert!(decoded > 0, "mutation never produced a decodable frame: the fuzz is not reaching the decoder");
    }
}
