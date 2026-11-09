//! Jabra Elite 10 (and likely other recent Jabra earbuds) via the GN Protocol (GNP)
//! over the SPP RFCOMM service, reconstructed from Jabra Moments 5.21 (libjabralib.so).
//!
//! Packet: [dst] [src] [seq] [type<<6 | total length] [command] [payload..], max. 63 bytes,
//! no checksum. The app talks from address 0x09 to 0x04. Types: 0 event, 1 read, 2 write,
//! 3 reply. Writes are confirmed with command 0xFF (ACK) or rejected with 0xFE (NACK).
use crate::{Act, Band, Res, Status, tr};
use bluer::rfcomm::Stream;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const UUID: bluer::Uuid = bluer::Uuid::from_u128(0x00001101_0000_1000_8000_00805F9B34FB);
const DST: u8 = 0x04;
const SRC: u8 = 0x09;
const READ: u8 = 1;
const WRITE: u8 = 2;
const REPLY: u8 = 3;
const ACK: u8 = 0xff;
const NACK: u8 = 0xfe;

const CMD_BATTERY: u8 = 0x12;
/// Settings read/write; the ambience mode is setting 0xBE (followed by count 1).
const CMD_SETTING: u8 = 0x13;
const AMBIENCE: [u8; 2] = [0xbe, 0x01];
/// Mode index (0 = ANC, 1 = HearThrough, 2 = off) -> wire value (AmbienceMode::SerializeMode).
const MODE_WIRE: [u8; 3] = [0x04, 0x02, 0x01];
/// Music equalizer, setting 0x7D. Reply: [7d, count, ?, ?, 7-byte band records..];
/// record: [max gain*4, freq*3 (u16 BE), gain*60 (i16 BE), 2 more bytes, probably Q].
/// Scales from the JabraDevice constructor (4.0, 3.0, 60.0).
const EQ: u8 = 0x7d;

fn eq_bands(d: &[u8]) -> Vec<Band> {
    let n = d.get(1).copied().unwrap_or(0) as usize;
    d.get(4..)
        .unwrap_or(&[])
        .as_chunks::<7>()
        .0
        .iter()
        .take(n)
        .map(|r| {
            let hz = u16::from_be_bytes([r[1], r[2]]) as f32 / 3.0;
            Band {
                label: if hz < 1000.0 {
                    format!("{hz:.0}")
                } else {
                    format!("{}k", hz / 1000.0)
                },
                gain: i16::from_be_bytes([r[3], r[4]]) as f32 / 60.0,
                max: r[0] as f32 / 4.0,
                step: 0.5,
            }
        })
        .collect()
}

/// Write payload: [7d, count, 0, d[2], d[3]] + per band [freq, gain, last 2 bytes], from the last read.
fn eq_write(d: &[u8], gains: &[f32]) -> Vec<u8> {
    let mut p = vec![EQ, d[1], 0, d[2], d[3]];
    for (r, g) in d[4..].as_chunks::<7>().0.iter().zip(gains) {
        let max = r[0] as f32 / 4.0;
        let g = (g.clamp(-max, max) * 60.0).round() as i16;
        p.extend([r[1], r[2]]);
        p.extend(g.to_be_bytes());
        p.extend([r[5], r[6]]);
    }
    p
}

fn packet(typ: u8, cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![DST, SRC, 0, (typ << 6) | (5 + payload.len() as u8), cmd];
    p.extend(payload);
    p
}

/// Takes one complete packet off the front of the buffer; several can arrive in one read.
fn take(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    loop {
        if buf.len() < 5 {
            return None;
        }
        let len = (buf[3] & 0x3f) as usize;
        if len < 5 {
            buf.remove(0); // not a packet start, resync
            continue;
        }
        return (buf.len() >= len).then(|| buf.drain(..len).collect());
    }
}

/// Battery reply payload: [0x02, flags, level, ?, ?, count, (unit id, ?, level) * count].
/// Flags: bit 0 charging, bit 7 extra units follow.
fn battery(d: &[u8]) -> (u8, bool, Vec<(&'static str, u8)>) {
    let (flags, level) = (
        d.get(1).copied().unwrap_or(0),
        d.get(2).copied().unwrap_or(0),
    );
    let mut parts = vec![];
    if flags & 0x80 != 0 {
        let n = d.get(5).copied().unwrap_or(0) as usize;
        for u in d.get(6..).unwrap_or(&[]).as_chunks::<3>().0.iter().take(n) {
            let name = match u[0] {
                0x04 => tr("Rechts", "Right"),
                0x05 => tr("Links", "Left"),
                0x15 => "Case",
                _ => continue,
            };
            parts.push((name, u[2]));
        }
    }
    // The main level isn't labelled; it belongs to the earbud that isn't listed separately.
    let (l, r) = (tr("Links", "Left"), tr("Rechts", "Right"));
    let has = |n: &str| parts.iter().any(|p: &(&str, u8)| p.0 == n);
    match (has(l), has(r)) {
        (true, false) => parts.push((r, level)),
        (false, true) => parts.push((l, level)),
        _ => {}
    }
    parts.sort_by_key(|p| [l, r, "Case"].iter().position(|n| *n == p.0));
    (level, flags & 1 != 0, parts)
}

pub struct Gnp {
    pub s: Stream,
    buf: Vec<u8>,
    /// last equalizer reply; writes reuse its band frequencies and Q values
    eq: Vec<u8>,
}

impl Gnp {
    pub fn new(s: Stream) -> Self {
        Gnp {
            s,
            buf: vec![],
            eq: vec![],
        }
    }

    pub async fn next(&mut self) -> Res<Vec<u8>> {
        loop {
            if let Some(p) = take(&mut self.buf) {
                return Ok(p);
            }
            let mut tmp = [0u8; 256];
            let n = self.s.read(&mut tmp).await?;
            if n == 0 {
                return Err(tr("Verbindung geschlossen", "Connection closed").into());
            }
            self.buf.extend(&tmp[..n]);
        }
    }

    /// Sends a read or write and returns the reply payload (empty for an ACKed write).
    async fn req(&mut self, typ: u8, cmd: u8, payload: &[u8]) -> Res<Vec<u8>> {
        self.s.write_all(&packet(typ, cmd, payload)).await?;
        let wait = async {
            loop {
                let p = self.next().await?;
                match (p[3] >> 6, p[4]) {
                    (_, NACK) => return Err(format!("NACK 0x{cmd:02x}").into()),
                    (_, ACK) if typ == WRITE => return Ok(vec![]),
                    (REPLY, c) if c == cmd && typ == READ => return Ok(p[5..].to_vec()),
                    _ => {} // events and unrelated replies
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(3), wait)
            .await
            .map_err(|_| {
                format!(
                    "{} 0x{cmd:02x}",
                    tr("Timeout bei Befehl", "Timeout on command")
                )
            })?
    }

    pub async fn status(&mut self) -> Res<Status> {
        let (bat, charging, parts) = battery(&self.req(READ, CMD_BATTERY, &[0x02]).await?);
        let m = self.req(READ, CMD_SETTING, &AMBIENCE).await?;
        let mode = MODE_WIRE
            .iter()
            .position(|&w| m.get(1) == Some(&w))
            .unwrap_or(2);
        // ponytail: devices without the music equalizer just show no EQ
        self.eq = self
            .req(READ, CMD_SETTING, &[EQ, 0])
            .await
            .unwrap_or_default();
        Ok(Status {
            bat,
            charging,
            parts,
            mode,
            bands: eq_bands(&self.eq),
            ..Default::default()
        })
    }

    pub async fn apply(&mut self, act: &Act) -> Res<()> {
        let mode = match act {
            Act::Mode(m) => *m,
            Act::Ambient { .. } => 1,
            Act::EqBands(g) if self.eq.len() > 4 => {
                let p = eq_write(&self.eq, g);
                self.req(WRITE, CMD_SETTING, &p).await?;
                return Ok(());
            }
            _ => {
                return Err(tr(
                    "Wird von diesem Gerät nicht unterstützt",
                    "Not supported by this device",
                )
                .into());
            }
        };
        let [a, b] = AMBIENCE;
        self.req(WRITE, CMD_SETTING, &[a, b, MODE_WIRE[mode.min(2)]])
            .await?;
        Ok(())
    }

    /// Raw packet: first byte type (1 read, 2 write), second command, then payload.
    pub async fn raw(&mut self, p: &[u8]) -> Res<Vec<u8>> {
        let [typ, cmd, rest @ ..] = p else {
            return Err("raw <type> <cmd> [payload]".into());
        };
        self.req(*typ, *cmd, rest).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // bytes captured from a Jabra Elite 10
    #[test]
    fn packets() {
        assert_eq!(
            packet(READ, CMD_BATTERY, &[0x02]),
            [4, 9, 0, 0x46, 0x12, 0x02]
        );
        assert_eq!(
            packet(WRITE, CMD_SETTING, &[0xbe, 0x01, 0x04]),
            [4, 9, 0, 0x88, 0x13, 0xbe, 0x01, 0x04]
        );
        // ACK + ambience event in one read, followed by half a packet
        let mut buf = vec![
            9, 4, 0, 0xca, 0xff, 4, 9, 0, 0x88, 0x13, 9, 4, 0, 0x09, 0x0d, 0x4c, 9, 1, 1, 9, 4, 0,
            0xc7,
        ];
        assert_eq!(take(&mut buf).unwrap()[4], ACK);
        assert_eq!(take(&mut buf).unwrap(), [9, 4, 0, 9, 0x0d, 0x4c, 9, 1, 1]);
        assert_eq!(take(&mut buf), None);
        assert_eq!(buf.len(), 4);
    }

    #[test]
    fn equalizer() {
        let d = [
            0x7d, 5, 0, 0, 0x1e, 0x00, 0xb4, 0, 0, 0x06, 0xc2, 0x18, 0x02, 0xee, 0x00, 0xb4, 0x06,
            0x76, 0x18, 0x0b, 0xb8, 0, 0, 0x06, 0x29, 0x18, 0x2e, 0xe0, 0, 0, 0x06, 0x3d, 0x28,
            0x59, 0x10, 0, 0, 0x08, 0x28,
        ];
        let b = eq_bands(&d);
        let labels: Vec<&str> = b.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["60", "250", "1k", "4k", "7.6k"]);
        assert_eq!((b[0].max, b[1].gain, b[4].max), (7.5, 3.0, 10.0));
        // writing the same gains back reproduces the payload the device accepted
        let gains: Vec<f32> = b.iter().map(|b| b.gain).collect();
        let p = eq_write(&d, &gains);
        assert_eq!(p[..5], [0x7d, 5, 0, 0, 0]);
        assert_eq!(p[11..17], [0x02, 0xee, 0x00, 0xb4, 0x06, 0x76]);
        assert_eq!(p.len(), 35);
    }

    #[test]
    fn battery_units() {
        let d = [2, 0x80, 0x1e, 0, 0, 2, 0x15, 0, 0x5a, 0x05, 0, 0x14];
        let (level, charging, parts) = battery(&d);
        assert_eq!((level, charging), (30, false));
        // left 20, right (main level) 30, case 90
        let levels: Vec<u8> = parts.iter().map(|p| p.1).collect();
        assert_eq!(levels, [20, 30, 90]);
        assert_eq!(parts[2].0, "Case");
    }
}
