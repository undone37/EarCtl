//! Sony WH-1000XM3/XM4 via the MDR protocol
//! (reconstructed from Sony Sound Connect 13.3.1: com.sony.songpal.tandemfamily.message.mdr.v1.table1).
use crate::{Act, Band, Res, Status, tr};
use bluer::rfcomm::Stream;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const UUID: bluer::Uuid = bluer::Uuid::from_u128(0x96CC203E_5068_46AD_B32D_E316F5E069BA);
const DATA_MDR: u8 = 0x0c;
const ACK: u8 = 0x01;

/// (CLI name, ID, display name)
pub const EQ: &[(&str, u8, &str)] = &[
    ("off", 0x00, "Off"),
    ("rock", 0x01, "Rock"),
    ("pop", 0x02, "Pop"),
    ("jazz", 0x03, "Jazz"),
    ("dance", 0x04, "Dance"),
    ("edm", 0x05, "EDM"),
    ("rnb", 0x06, "R&B / Hip-Hop"),
    ("acoustic", 0x07, "Acoustic"),
    ("bright", 0x10, "Bright"),
    ("excited", 0x11, "Excited"),
    ("mellow", 0x12, "Mellow"),
    ("relaxed", 0x13, "Relaxed"),
    ("vocal", 0x14, "Vocal"),
    ("treble", 0x15, "Treble Boost"),
    ("bass", 0x16, "Bass Boost"),
    ("speech", 0x17, "Speech"),
    ("custom", 0xa0, "Manual"),
    ("user1", 0xa1, "Custom 1"),
    ("user2", 0xa2, "Custom 2"),
    ("user3", 0xa3, "Custom 3"),
];

const BANDS: [&str; 6] = ["Clear Bass", "400", "1k", "2.5k", "6.3k", "16k"];

pub fn eq_name(id: u8) -> &'static str {
    match EQ.iter().find(|p| p.1 == id) {
        Some((_, 0, _)) => tr("Aus", "Off"),
        Some(p) => p.2,
        None => "?",
    }
}

/// 0x3E | kind seq len(u32 BE) payload sum | 0x3C, with 0x3C..0x3E escaped as 0x3D,(b & 0xEF).
fn frame(kind: u8, seq: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![kind, seq];
    body.extend((payload.len() as u32).to_be_bytes());
    body.extend(payload);
    body.push(body.iter().fold(0u8, |a, b| a.wrapping_add(*b)));
    let mut out = vec![0x3e];
    for b in body {
        if (0x3c..=0x3e).contains(&b) {
            out.extend([0x3d, b & 0xef]);
        } else {
            out.push(b);
        }
    }
    out.push(0x3c);
    out
}

/// Frame content without start/end markers -> (kind, seq, payload).
fn parse(raw: &[u8]) -> Option<(u8, u8, Vec<u8>)> {
    let mut b = Vec::with_capacity(raw.len());
    let mut it = raw.iter();
    while let Some(&x) = it.next() {
        b.push(if x == 0x3d { it.next()? | 0x10 } else { x });
    }
    let (body, sum) = b.split_at(b.len().checked_sub(1)?);
    if body.len() < 6 || body.iter().fold(0u8, |a, x| a.wrapping_add(*x)) != sum[0] {
        return None;
    }
    let len = u32::from_be_bytes(body[2..6].try_into().ok()?) as usize;
    (body.len() == 6 + len).then(|| (body[0], body[1], body[6..].to_vec()))
}

/// NCASM_SET_PARAM: NC+ASM, effect, DUAL_SINGLE_OFF, nc, LEVEL_ADJUSTMENT, asmId, level
fn anc(mode: usize, voice: bool, level: u8) -> Vec<u8> {
    let (effect, nc) = match mode {
        0 => (0x11, 0x02),
        1 => (0x11, 0x00),
        _ => (0x00, 0x00),
    };
    // the XM4 reports level 0 in NC mode -> use full level instead of "silent" when switching to ambient
    let level = if mode == 1 && level == 0 {
        20
    } else {
        level.min(20)
    };
    vec![0x68, 0x02, effect, 0x02, nc, 0x01, voice as u8, level]
}

/// EQ_SET_PARAM with bands [ClearBass, 400, 1k, 2.5k, 6.3k, 16k] in -10..10.
/// Like the app: preset ID UNSPECIFIED (0xFF), the bands apply to the active preset.
/// With a concrete ID the XM4 silently drops the message.
fn eq_bands(bands: &[i8]) -> Vec<u8> {
    let mut p = vec![0x58, 0x01, 0xff, bands.len() as u8];
    p.extend(bands.iter().map(|b| (b.clamp(&-10, &10) + 10) as u8));
    p
}

pub struct Mdr {
    pub s: Stream,
    seq: u8,
    buf: Vec<u8>,
}

impl Mdr {
    pub async fn open(s: Stream) -> Res<Self> {
        let mut m = Mdr {
            s,
            seq: 0,
            buf: vec![],
        };
        // CONNECT_GET_PROTOCOL_INFO; right after a reconnect the XM4 sometimes ignores the first attempt
        if m.send(&[0x00, 0x00], Some(0x01)).await.is_err() {
            m.seq ^= 1;
            m.send(&[0x00, 0x00], Some(0x01)).await?;
        }
        Ok(m)
    }

    pub async fn next_frame(&mut self) -> Res<(u8, u8, Vec<u8>)> {
        loop {
            if let Some(end) = self.buf.iter().position(|&b| b == 0x3c) {
                let raw: Vec<u8> = self.buf.drain(..=end).collect();
                let Some(st) = raw.iter().rposition(|&b| b == 0x3e) else {
                    continue;
                };
                let Some(f) = parse(&raw[st + 1..raw.len() - 1]) else {
                    continue;
                };
                if f.0 != ACK {
                    self.s.write_all(&frame(ACK, 1 - (f.1 & 1), &[])).await?;
                }
                return Ok(f);
            }
            let mut tmp = [0u8; 1024];
            let n = self.s.read(&mut tmp).await?;
            if n == 0 {
                return Err(tr("Verbindung geschlossen", "Connection closed").into());
            }
            self.buf.extend(&tmp[..n]);
        }
    }

    /// Sends a command, waits for the ACK and (if `reply` is set) for the matching reply.
    pub async fn send(&mut self, payload: &[u8], reply: Option<u8>) -> Res<Vec<u8>> {
        self.s
            .write_all(&frame(DATA_MDR, self.seq, payload))
            .await?;
        let wait = async {
            let (mut acked, mut got) = (false, None);
            loop {
                let (kind, seq, p) = self.next_frame().await?;
                if kind == ACK {
                    acked = true;
                    self.seq = seq;
                } else if kind == DATA_MDR && reply.is_some() && p.first() == reply.as_ref() {
                    got = Some(p);
                }
                if acked && (reply.is_none() || got.is_some()) {
                    return Ok(got.unwrap_or_default());
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(3), wait)
            .await
            .map_err(|_| {
                format!(
                    "{} 0x{:02x}",
                    tr("Timeout bei Befehl", "Timeout on command"),
                    payload[0]
                )
            })?
    }

    pub async fn status(&mut self) -> Res<Status> {
        let b = self.send(&[0x10, 0x00], Some(0x11)).await?;
        let n = self.send(&[0x66, 0x02], Some(0x67)).await?;
        let e = self.send(&[0x56, 0x01], Some(0x57)).await?;
        let dsee = self
            .send(&[0xe6, 0x02], Some(0xe7))
            .await
            .ok()
            .map(|d| d[3] == 1);
        let stc = self
            .send(&[0xf6, 0x05], Some(0xf7))
            .await
            .ok()
            .map(|d| d[3] == 1);
        Ok(Status {
            bat: b[2],
            charging: b[3] == 1,
            mode: if n[2] == 0 {
                2
            } else if n[4] != 0 {
                0
            } else {
                1
            },
            voice: Some(n[6] == 1),
            level: Some(n[7]),
            eq: Some(e[2]),
            bands: e[4..]
                .iter()
                .zip(BANDS)
                .map(|(&v, label)| Band {
                    label: label.into(),
                    gain: v as f32 - 10.0,
                    max: 10.0,
                    step: 1.0,
                })
                .collect(),
            dsee,
            stc,
            power_off: true,
            ..Default::default()
        })
    }

    /// `amb` is the last known ambient level (the device reports 0 while in NC mode).
    pub async fn apply(&mut self, act: &Act, cur: &Status, amb: u8) -> Res<()> {
        let voice = cur.voice.unwrap_or(false);
        let p = match *act {
            Act::Mode(1) => anc(1, voice, amb),
            Act::Mode(m) => anc(m, voice, amb),
            Act::Ambient { level, voice: v } => anc(1, v.unwrap_or(voice), level.unwrap_or(amb)),
            Act::EqPreset(id) => vec![0x58, 0x01, id, 0x00],
            Act::EqBands(ref b) => {
                if cur.eq.is_some_and(|id| id < 0xa0) {
                    self.send(&[0x58, 0x01, 0xa0, 0x00], None).await?;
                }
                eq_bands(&b.iter().map(|g| g.round() as i8).collect::<Vec<_>>())
            }
            Act::Dsee(on) => vec![0xe8, 0x02, 0x00, on as u8],
            Act::Stc(on) => vec![0xf8, 0x05, 0x01, on as u8],
            Act::AutoOff(x, y) => vec![0xf8, 0x04, 0x01, x, y],
            Act::PowerOff => vec![0x22, 0x00, 0x01],
        };
        self.send(&p, None).await?;
        Ok(())
    }

    /// Raw MDR payload; waits for the reply code payload[0]+1.
    pub async fn raw(&mut self, p: &[u8]) -> Res<Vec<u8>> {
        self.send(p, Some(p.first().ok_or("empty")? + 1)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing() {
        // ACK frame from the APK (rm0.b): {62, 1, 0, 0,0,0,0, 1, 60}
        assert_eq!(frame(ACK, 0, &[]), [0x3e, 1, 0, 0, 0, 0, 0, 1, 0x3c]);
        // escaping + roundtrip
        let p = [0x68, 0x3c, 0x3d, 0x3e, 0x00];
        let f = frame(DATA_MDR, 1, &p);
        assert!(!f[1..f.len() - 1].iter().any(|b| *b == 0x3c || *b == 0x3e));
        assert_eq!(parse(&f[1..f.len() - 1]), Some((DATA_MDR, 1, p.to_vec())));
        // broken checksum
        let mut bad = f.clone();
        bad[2] ^= 1;
        assert_eq!(parse(&bad[1..bad.len() - 1]), None);
    }
}
