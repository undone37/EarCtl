//! sonyctl – controls Sony WH-1000XM3/XM4 on Linux via the MDR protocol
//! (reconstructed from Sony Sound Connect 13.3.1: com.sony.songpal.tandemfamily.message.mdr.v1.table1).
use bluer::rfcomm::{Profile, Role, Stream};
use futures::StreamExt;
mod panel;

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const UUID: bluer::Uuid = bluer::Uuid::from_u128(0x96CC203E_5068_46AD_B32D_E316F5E069BA);
const DATA_MDR: u8 = 0x0c;
const ACK: u8 = 0x01;

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

struct Mdr {
    s: Stream,
    seq: u8,
    buf: Vec<u8>,
}

impl Mdr {
    async fn next_frame(&mut self) -> Res<(u8, u8, Vec<u8>)> {
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
    async fn send(&mut self, payload: &[u8], reply: Option<u8>) -> Res<Vec<u8>> {
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
}

async fn connect() -> Res<(Mdr, bluer::rfcomm::ProfileHandle)> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    let mut dev = None;
    for addr in adapter.device_addresses().await? {
        let d = adapter.device(addr)?;
        // ponytail: first connected WH-1000XM* device; add an address option once someone has several
        if d.is_connected().await? && d.name().await?.is_some_and(|n| n.contains("1000XM")) {
            dev = Some(d);
            break;
        }
    }
    let dev = dev.ok_or(tr(
        "Kein verbundener WH-1000XM* gefunden",
        "No connected WH-1000XM* found",
    ))?;
    let mut handle = session
        .register_profile(Profile {
            uuid: UUID,
            role: Some(Role::Client),
            require_authentication: Some(false),
            require_authorization: Some(false),
            auto_connect: Some(false),
            ..Default::default()
        })
        .await?;
    let d = dev.clone();
    let conn = tokio::spawn(async move { d.connect_profile(&UUID).await });
    let req = tokio::time::timeout(Duration::from_secs(10), handle.next())
        .await
        .map_err(|_| {
            tr(
                "Timeout beim Verbinden (Kopfhörer an und verbunden?)",
                "Timeout while connecting (headphones on and connected?)",
            )
        })?
        .ok_or(tr("Profil geschlossen", "Profile closed"))?;
    let s = req.accept()?;
    conn.await??;
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
    Ok((m, handle))
}

/// German if the locale is "de", English otherwise.
static GERMAN: LazyLock<bool> = LazyLock::new(|| {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| v.starts_with("de"))
});

fn tr(de: &'static str, en: &'static str) -> &'static str {
    if *GERMAN { de } else { en }
}

/// (CLI name, ID, display name)
const EQ: &[(&str, u8, &str)] = &[
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

fn eq_name(id: u8) -> &'static str {
    match EQ.iter().find(|p| p.1 == id) {
        Some((_, 0, _)) => tr("Aus", "Off"),
        Some(p) => p.2,
        None => "?",
    }
}

/// Mode index: 0 = noise cancelling, 1 = ambient sound, 2 = off
fn mode_name(i: usize) -> &'static str {
    [
        "Noise Cancelling",
        tr("Umgebungsgeräusche", "Ambient Sound"),
        tr("Aus", "Off"),
    ][i]
}

#[derive(Clone, Debug)]
struct Status {
    bat: u8,
    charging: bool,
    mode: usize,
    voice: bool,
    level: u8,
    eq: u8,
    bands: Vec<i8>,
    dsee: Option<bool>,
    stc: Option<bool>,
}

async fn status(m: &mut Mdr) -> Res<Status> {
    let b = m.send(&[0x10, 0x00], Some(0x11)).await?;
    let n = m.send(&[0x66, 0x02], Some(0x67)).await?;
    let e = m.send(&[0x56, 0x01], Some(0x57)).await?;
    let dsee = m
        .send(&[0xe6, 0x02], Some(0xe7))
        .await
        .ok()
        .map(|d| d[3] == 1);
    let stc = m
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
        voice: n[6] == 1,
        level: n[7],
        eq: e[2],
        bands: e[4..].iter().map(|&v| v as i8 - 10).collect(),
        dsee,
        stc,
    })
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

const USAGE: &str = "sonyctl                         tray icon + panel (runs in background)
sonyctl status
sonyctl anc nc|off
sonyctl anc ambient [1-20] [voice]
sonyctl eq <preset>             (off rock pop jazz dance edm rnb acoustic bright excited
                                 mellow relaxed vocal treble bass speech custom user1-3)
sonyctl eq custom <clearbass> <400> <1k> <2.5k> <6.3k> <16k>   (-10..10 each)
sonyctl dsee on|off
sonyctl stc on|off              (Speak-to-Chat)
sonyctl autooff 5|30|60|180|removed|off
sonyctl poweroff
sonyctl raw <hex bytes>         (send payload, wait for reply code+1)";

fn onoff(a: Option<&str>) -> Res<u8> {
    match a {
        Some("on") => Ok(1),
        Some("off") => Ok(0),
        _ => Err(USAGE.into()),
    }
}

async fn run(a: &[&str]) -> Res<()> {
    let (mut m, _profile) = connect().await?;
    let arg = a.get(1).copied();
    match a[0] {
        "status" => {
            let s = status(&mut m).await?;
            let opt = |o: Option<bool>| {
                o.map_or("?", |b| if b { tr("an", "on") } else { tr("aus", "off") })
            };
            let charging = if s.charging {
                tr(" (lädt)", " (charging)")
            } else {
                ""
            };
            println!("{:<14}{}%{charging}", tr("Akku:", "Battery:"), s.bat);
            println!("{:<14}{}", "ANC:", mode_name(s.mode));
            println!(
                "{:<14}{}{}",
                tr("Umgebung:", "Ambient:"),
                s.level,
                if s.voice {
                    tr(" +Stimme", " +voice")
                } else {
                    ""
                }
            );
            println!("{:<14}{} {:?}", "EQ:", eq_name(s.eq), s.bands);
            println!("{:<14}{}", "DSEE:", opt(s.dsee));
            println!("{:<14}{}", "Speak-to-Chat:", opt(s.stc));
        }
        "anc" => {
            let s = status(&mut m).await?;
            let p = match arg {
                Some("nc") => anc(0, s.voice, s.level),
                Some("off") => anc(2, s.voice, s.level),
                Some("ambient") => {
                    let level = a.get(2).and_then(|l| l.parse().ok()).unwrap_or(s.level);
                    anc(1, a.contains(&"voice"), level)
                }
                _ => return Err(USAGE.into()),
            };
            m.send(&p, None).await?;
        }
        "eq" => {
            let id = EQ.iter().find(|p| Some(p.0) == arg).ok_or(USAGE)?.1;
            m.send(&[0x58, 0x01, id, 0x00], None).await?;
            if a.len() > 2 {
                if a.len() != 8 {
                    return Err(USAGE.into());
                }
                let bands = a[2..]
                    .iter()
                    .map(|v| v.parse())
                    .collect::<Result<Vec<i8>, _>>()?;
                m.send(&eq_bands(&bands), None).await?;
            }
        }
        "dsee" => drop(m.send(&[0xe8, 0x02, 0x00, onoff(arg)?], None).await?),
        "stc" => drop(m.send(&[0xf8, 0x05, 0x01, onoff(arg)?], None).await?),
        "autooff" => {
            // ponytail: value pairs taken from Gadgetbridge; the XM4 may only support off/removed
            let (x, y) = match arg {
                Some("5") => (0x00, 0x00),
                Some("30") => (0x01, 0x01),
                Some("60") => (0x02, 0x02),
                Some("180") => (0x03, 0x03),
                Some("removed") => (0x10, 0x00),
                Some("off") => (0x11, 0x00),
                _ => return Err(USAGE.into()),
            };
            m.send(&[0xf8, 0x04, 0x01, x, y], None).await?;
        }
        "poweroff" => drop(m.send(&[0x22, 0x00, 0x01], None).await?),
        "raw" => {
            let p = a[1..]
                .iter()
                .map(|h| u8::from_str_radix(h, 16))
                .collect::<Result<Vec<_>, _>>()?;
            let r = m.send(&p, Some(p.first().ok_or(USAGE)? + 1)).await?;
            println!("{r:02x?}");
        }
        _ => return Err(USAGE.into()),
    }
    m.s.shutdown().await?; // disconnect cleanly, otherwise an immediate reconnect hangs
    Ok(())
}

/// State shared by the Bluetooth thread, the tray and the panel.
#[derive(Default)]
struct Shared {
    st: Option<Status>,
    /// last ambient level, the device reports 0 in NC mode
    amb: u8,
    /// open panel, so it repaints on new data
    ctx: Option<eframe::egui::Context>,
}
type Sh = Arc<Mutex<Shared>>;
type Cmd = tokio::sync::mpsc::UnboundedSender<Vec<u8>>;

fn lock(sh: &Sh) -> std::sync::MutexGuard<'_, Shared> {
    sh.lock().unwrap_or_else(|e| e.into_inner())
}

struct Tray {
    sh: Sh,
    tx: Cmd,
    open: std::sync::mpsc::Sender<()>,
}

impl Tray {
    fn cmd(&self, p: Vec<u8>) {
        let _ = self.tx.send(p);
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "sonyctl".into()
    }
    fn title(&self) -> String {
        "WH-1000XM4".into()
    }
    fn icon_name(&self) -> String {
        "audio-headphones".into()
    }
    /// Left click opens the panel, right click the menu.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.open.send(());
    }
    fn status(&self) -> ksni::Status {
        // disconnected: the icon moves to KDE's hidden icons
        if lock(&self.sh).st.is_some() {
            ksni::Status::Active
        } else {
            ksni::Status::Passive
        }
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        let description = match &lock(&self.sh).st {
            Some(s) => format!(
                "{} {}% · {}",
                tr("Akku", "Battery"),
                s.bat,
                mode_name(s.mode)
            ),
            None => tr("Nicht verbunden", "Not connected").into(),
        };
        ksni::ToolTip {
            title: "WH-1000XM4".into(),
            description,
            ..Default::default()
        }
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let item = |label: &str, icon: &str, f: fn(&mut Self)| {
            StandardItem {
                label: label.into(),
                icon_name: icon.into(),
                activate: Box::new(f),
                ..Default::default()
            }
            .into()
        };
        let (st, amb) = {
            let g = lock(&self.sh);
            (g.st.clone(), g.amb)
        };
        let mut items = vec![item(
            tr("Panel öffnen", "Open panel"),
            "audio-headphones",
            |t| {
                let _ = t.open.send(());
            },
        )];
        if let Some(s) = st {
            let voice = s.voice;
            items.push(MenuItem::Separator);
            items.push(
                RadioGroup {
                    selected: s.mode,
                    select: Box::new(move |t: &mut Self, i| t.cmd(anc(i, voice, amb))),
                    options: (0..3)
                        .map(|i| RadioItem {
                            label: mode_name(i).into(),
                            ..Default::default()
                        })
                        .collect(),
                }
                .into(),
            );
        }
        items.extend([
            MenuItem::Separator,
            item(tr("Beenden", "Quit"), "application-exit", |_| {
                std::process::exit(0)
            }),
        ]);
        items
    }
}

/// Keeps the connection, reads the status and runs commands from the tray/panel.
async fn bt_loop(
    sh: Sh,
    tx: Cmd,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    open: std::sync::mpsc::Sender<()>,
) -> Res<()> {
    use ksni::TrayMethods;
    let handle = Tray {
        sh: sh.clone(),
        tx,
        open,
    }
    .spawn()
    .await?;
    let publish = |st: Option<Status>| {
        let mut g = lock(&sh);
        if let Some(s) = &st
            && s.mode == 1
            && s.level > 0
        {
            g.amb = s.level;
        }
        g.st = st;
        if let Some(c) = &g.ctx {
            c.request_repaint();
        }
    };
    loop {
        let conn = connect().await;
        if let Err(e) = &conn {
            eprintln!("{e}");
        }
        if let Ok((mut m, _profile)) = conn {
            let err: Res<()> = async {
                loop {
                    publish(Some(status(&mut m).await?));
                    handle.update(|_| {}).await;
                    tokio::select! {
                        Some(p) = rx.recv() => drop(m.send(&p, None).await?),
                        // buttons on the headphones send NTFY frames -> re-read the status
                        f = m.next_frame() => drop(f?),
                        _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                    }
                }
            }
            .await;
            eprintln!(
                "{}: {}",
                tr("Verbindung verloren", "Connection lost"),
                err.unwrap_err()
            );
        }
        publish(None);
        handle.update(|_| {}).await;
        while rx.try_recv().is_ok() {} // drop clicks made while disconnected
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

/// Tray on a background thread, panel window (egui) on the main thread, which winit requires.
fn gui() -> Res<()> {
    let sh = Sh::default();
    lock(&sh).amb = 20;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (open_tx, open_rx) = std::sync::mpsc::channel();
    let (sh2, tx2) = (sh.clone(), tx.clone());
    std::thread::spawn(move || {
        if let Err(e) = runtime().block_on(bt_loop(sh2, tx2, rx, open_tx)) {
            eprintln!("{e}");
            std::process::exit(1);
        }
    });
    while open_rx.recv().is_ok() {
        while open_rx.try_recv().is_ok() {} // collapse double clicks
        panel::show(sh.clone(), tx.clone())?;
        lock(&sh).ctx = None;
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let res = match a.first() {
        None => gui(),
        Some(&("-h" | "--help")) => {
            println!("{USAGE}");
            Ok(())
        }
        Some(_) => runtime().block_on(run(&a)),
    };
    if let Err(e) = res {
        eprintln!("{e}");
        std::process::exit(1);
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
