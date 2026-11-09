//! earctl – controls Bluetooth headphones on Linux: noise cancelling, equalizer, battery and more.
//! Drivers: Sony WH-1000XM3/XM4 (sony.rs) and Jabra Elite 10 (jabra.rs).
use bluer::rfcomm::{Profile, Role};
use futures::StreamExt;
mod jabra;
mod panel;
mod sony;

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

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

/// Mode index: 0 = noise cancelling, 1 = ambient sound, 2 = off
fn mode_name(i: usize) -> &'static str {
    [
        "Noise Cancelling",
        tr("Umgebungsgeräusche", "Ambient Sound"),
        tr("Aus", "Off"),
    ][i]
}

/// Headline battery: the earbuds if the device reports them separately, otherwise the single level.
fn battery_text(s: &Status) -> String {
    let buds: Vec<String> = s
        .parts
        .iter()
        .filter(|p| p.0 != "Case")
        .map(|(n, l)| format!("{} {l}%", &n[..1]))
        .collect();
    let charging = if s.charging { " ⚡" } else { "" };
    if buds.is_empty() {
        format!("{}%{charging}", s.bat)
    } else {
        buds.join(" · ") + charging
    }
}

/// One equalizer band; gains go from -max to +max in `step`s.
#[derive(Clone, Debug, Default)]
struct Band {
    label: String,
    gain: f32,
    max: f32,
    step: f32,
}

/// Device state; `None`/empty fields are features the device doesn't have.
#[derive(Clone, Debug, Default)]
struct Status {
    name: String,
    bat: u8,
    charging: bool,
    /// extra batteries, e.g. left/right earbud and case
    parts: Vec<(&'static str, u8)>,
    mode: usize,
    voice: Option<bool>,
    /// ambient level 1..20
    level: Option<u8>,
    /// Sony equalizer preset
    eq: Option<u8>,
    bands: Vec<Band>,
    dsee: Option<bool>,
    stc: Option<bool>,
    power_off: bool,
}

/// Something the user wants to change, independent of the device protocol.
#[derive(Clone, Debug)]
enum Act {
    Mode(usize),
    Ambient {
        level: Option<u8>,
        voice: Option<bool>,
    },
    EqPreset(u8),
    EqBands(Vec<f32>),
    Dsee(bool),
    Stc(bool),
    AutoOff(u8, u8),
    PowerOff,
}

enum Link {
    Sony(sony::Mdr),
    Jabra(jabra::Gnp),
}

struct Dev {
    name: String,
    link: Link,
}

impl Dev {
    async fn status(&mut self) -> Res<Status> {
        let mut s = match &mut self.link {
            Link::Sony(m) => m.status().await?,
            Link::Jabra(g) => g.status().await?,
        };
        s.name = self.name.clone();
        Ok(s)
    }

    async fn apply(&mut self, act: &Act, cur: &Status, amb: u8) -> Res<()> {
        match &mut self.link {
            Link::Sony(m) => m.apply(act, cur, amb).await,
            Link::Jabra(g) => g.apply(act).await,
        }
    }

    /// Waits for an unsolicited message, e.g. after a button press on the headphones.
    async fn event(&mut self) -> Res<()> {
        match &mut self.link {
            Link::Sony(m) => drop(m.next_frame().await?),
            Link::Jabra(g) => drop(g.next().await?),
        }
        Ok(())
    }

    async fn raw(&mut self, p: &[u8]) -> Res<Vec<u8>> {
        match &mut self.link {
            Link::Sony(m) => m.raw(p).await,
            Link::Jabra(g) => g.raw(p).await,
        }
    }

    /// Disconnect cleanly, otherwise an immediate reconnect hangs.
    async fn close(&mut self) -> Res<()> {
        match &mut self.link {
            Link::Sony(m) => m.s.shutdown().await?,
            Link::Jabra(g) => g.s.shutdown().await?,
        }
        Ok(())
    }
}

/// Which driver handles a device, by its Bluetooth name.
fn driver(name: &str) -> Option<bluer::Uuid> {
    if name.contains("1000XM") {
        Some(sony::UUID)
    } else if name.starts_with("Jabra Elite") {
        Some(jabra::UUID)
    } else {
        None
    }
}

async fn connect() -> Res<(Dev, bluer::rfcomm::ProfileHandle)> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    // ponytail: first connected supported device; EARCTL_DEVICE picks one by name until the tray handles several
    let want = std::env::var("EARCTL_DEVICE").unwrap_or_default();
    let mut found = None;
    for addr in adapter.device_addresses().await? {
        let d = adapter.device(addr)?;
        let Some(name) = d.name().await? else {
            continue;
        };
        if let Some(uuid) = driver(&name)
            && name.contains(&want)
            && d.is_connected().await?
        {
            found = Some((d, name, uuid));
            break;
        }
    }
    let (dev, name, uuid) = found.ok_or(tr(
        "Keine unterstützten verbundenen Kopfhörer gefunden",
        "No supported connected headphones found",
    ))?;
    let mut handle = session
        .register_profile(Profile {
            uuid,
            role: Some(Role::Client),
            require_authentication: Some(false),
            require_authorization: Some(false),
            auto_connect: Some(false),
            ..Default::default()
        })
        .await?;
    let d = dev.clone();
    let conn = tokio::spawn(async move { d.connect_profile(&uuid).await });
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
    let link = if uuid == sony::UUID {
        Link::Sony(sony::Mdr::open(s).await?)
    } else {
        Link::Jabra(jabra::Gnp::new(s))
    };
    Ok((Dev { name, link }, handle))
}

const USAGE: &str = "earctl                         tray icon + panel (runs in background)
earctl status
earctl anc nc|off
earctl anc ambient [1-20] [voice]
earctl eq <preset>             (off rock pop jazz dance edm rnb acoustic bright excited
                                 mellow relaxed vocal treble bass speech custom user1-3)
earctl eq <preset> <clearbass> <400> <1k> <2.5k> <6.3k> <16k>   (Sony, -10..10 each)
earctl eq <gain> <gain> ...     (bands of the active preset; Jabra: dB, see status)
earctl dsee on|off
earctl stc on|off              (Speak-to-Chat)
earctl autooff 5|30|60|180|removed|off
earctl poweroff
earctl raw <hex bytes>         (Sony: MDR payload; Jabra: type cmd payload)

EARCTL_DEVICE=<name part> picks a device when several are connected.";

fn onoff(a: Option<&str>) -> Res<bool> {
    match a {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        _ => Err(USAGE.into()),
    }
}

fn print_status(s: &Status) {
    let onoff = |b: bool| if b { tr("an", "on") } else { tr("aus", "off") };
    let charging = if s.charging {
        tr(" (lädt)", " (charging)")
    } else {
        ""
    };
    println!("{:<14}{}", tr("Gerät:", "Device:"), s.name);
    if s.parts.is_empty() {
        println!("{:<14}{}%{charging}", tr("Akku:", "Battery:"), s.bat);
    }
    for (name, level) in &s.parts {
        println!("{:<14}{level}%{charging}", format!("{name}:"));
    }
    println!("{:<14}{}", "ANC:", mode_name(s.mode));
    if let Some(level) = s.level {
        let voice = if s.voice == Some(true) {
            tr(" +Stimme", " +voice")
        } else {
            ""
        };
        println!("{:<14}{level}{voice}", tr("Umgebung:", "Ambient:"));
    }
    if s.eq.is_some() || !s.bands.is_empty() {
        let preset =
            s.eq.map_or(String::new(), |e| format!("{} ", sony::eq_name(e)));
        let bands: Vec<String> = s
            .bands
            .iter()
            .map(|b| format!("{} {:+}", b.label, b.gain))
            .collect();
        println!("{:<14}{preset}{}", "EQ:", bands.join(" | "));
    }
    if let Some(on) = s.dsee {
        println!("{:<14}{}", "DSEE:", onoff(on));
    }
    if let Some(on) = s.stc {
        println!("{:<14}{}", "Speak-to-Chat:", onoff(on));
    }
}

async fn run(a: &[&str]) -> Res<()> {
    let (mut d, _profile) = connect().await?;
    let arg = a.get(1).copied();
    let act = match a[0] {
        "status" => {
            print_status(&d.status().await?);
            None
        }
        "anc" => Some(match arg {
            Some("nc") => Act::Mode(0),
            Some("off") => Act::Mode(2),
            Some("ambient") => Act::Ambient {
                level: a.get(2).and_then(|l| l.parse().ok()),
                voice: Some(a.contains(&"voice")),
            },
            _ => return Err(USAGE.into()),
        }),
        // numbers only: set the bands of the active preset (Jabra: dB per band)
        "eq" if arg.is_some_and(|v| v.parse::<f32>().is_ok()) => Some(Act::EqBands(
            a[1..].iter().map(|v| v.parse()).collect::<Result<_, _>>()?,
        )),
        "eq" => {
            let id = sony::EQ.iter().find(|p| Some(p.0) == arg).ok_or(USAGE)?.1;
            let cur = d.status().await?;
            d.apply(&Act::EqPreset(id), &cur, 0).await?;
            match a.len() {
                2 => None,
                8 => Some(Act::EqBands(
                    a[2..].iter().map(|v| v.parse()).collect::<Result<_, _>>()?,
                )),
                _ => return Err(USAGE.into()),
            }
        }
        "dsee" => Some(Act::Dsee(onoff(arg)?)),
        "stc" => Some(Act::Stc(onoff(arg)?)),
        // ponytail: value pairs taken from Gadgetbridge; the XM4 may only support off/removed
        "autooff" => Some(match arg {
            Some("5") => Act::AutoOff(0x00, 0x00),
            Some("30") => Act::AutoOff(0x01, 0x01),
            Some("60") => Act::AutoOff(0x02, 0x02),
            Some("180") => Act::AutoOff(0x03, 0x03),
            Some("removed") => Act::AutoOff(0x10, 0x00),
            Some("off") => Act::AutoOff(0x11, 0x00),
            _ => return Err(USAGE.into()),
        }),
        "poweroff" => Some(Act::PowerOff),
        "raw" => {
            let p = a[1..]
                .iter()
                .map(|h| u8::from_str_radix(h, 16))
                .collect::<Result<Vec<_>, _>>()?;
            println!("{:02x?}", d.raw(&p).await?);
            None
        }
        _ => return Err(USAGE.into()),
    };
    if let Some(act) = act {
        let cur = d.status().await?;
        let amb = cur.level.filter(|&l| l > 0).unwrap_or(20);
        d.apply(&act, &cur, amb).await?;
    }
    d.close().await
}

/// State shared by the Bluetooth thread, the tray and the panel.
#[derive(Default)]
struct Shared {
    st: Option<Status>,
    /// last ambient level, Sony reports 0 while in NC mode
    amb: u8,
    /// open panel, so it repaints on new data
    ctx: Option<eframe::egui::Context>,
}
type Sh = Arc<Mutex<Shared>>;
type Cmd = tokio::sync::mpsc::UnboundedSender<Act>;

fn lock(sh: &Sh) -> std::sync::MutexGuard<'_, Shared> {
    sh.lock().unwrap_or_else(|e| e.into_inner())
}

struct Tray {
    sh: Sh,
    tx: Cmd,
    open: std::sync::mpsc::Sender<()>,
}

impl Tray {
    fn title(&self) -> String {
        lock(&self.sh)
            .st
            .as_ref()
            .map_or_else(|| "earctl".into(), |s| s.name.clone())
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "earctl".into()
    }
    fn title(&self) -> String {
        Tray::title(self)
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
                "{} {} · {}",
                tr("Akku", "Battery"),
                battery_text(s),
                mode_name(s.mode)
            ),
            None => tr("Nicht verbunden", "Not connected").into(),
        };
        ksni::ToolTip {
            title: Tray::title(self),
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
        let mut items = vec![item(
            tr("Panel öffnen", "Open panel"),
            "audio-headphones",
            |t| {
                let _ = t.open.send(());
            },
        )];
        if let Some(s) = &lock(&self.sh).st {
            items.push(MenuItem::Separator);
            items.push(
                RadioGroup {
                    selected: s.mode,
                    select: Box::new(|t: &mut Self, i| {
                        let _ = t.tx.send(Act::Mode(i));
                    }),
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
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Act>,
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
            && let Some(l) = s.level.filter(|&l| l > 0)
        {
            g.amb = l;
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
        if let Ok((mut d, _profile)) = conn {
            let err: Res<()> = async {
                loop {
                    let cur = d.status().await?;
                    publish(Some(cur.clone()));
                    handle.update(|_| {}).await;
                    tokio::select! {
                        Some(act) = rx.recv() => {
                            let amb = lock(&sh).amb;
                            if let Err(e) = d.apply(&act, &cur, amb).await {
                                eprintln!("{e}");
                            }
                        }
                        // buttons on the headphones send events -> re-read the status
                        e = d.event() => e?,
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
