//! MPRIS (org.mpris.MediaPlayer2): media keys, `playerctl` and status bars.
//! The D-Bus side runs on its own thread with zbus' blocking API. It reads a
//! shared snapshot the UI thread keeps current, sends commands back over a
//! channel, and emits change signals when the UI thread asks.

use crate::library::{Track, art, hash};
use crate::player::{self, Event, State, queue::Repeat};
use crate::{prefs, window};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const PATH: &str = "/org/mpris/MediaPlayer2";
pub const BUS_NAME: &str = "org.mpris.MediaPlayer2.music";

#[derive(Debug, Clone)]
enum Command {
    Raise,
    Quit,
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Relative, in microseconds.
    Seek(i64),
    /// Absolute, in microseconds, for the given track id.
    SetPosition(String, i64),
    OpenUri(String),
    Loop(String),
    Shuffle(bool),
    Volume(f64),
    Rate(f64),
}

#[derive(Debug, Clone, Default)]
struct Meta {
    track_id: String,
    length_us: i64,
    title: String,
    artist: String,
    album: String,
    album_artist: String,
    track_no: Option<u32>,
    art_url: String,
    url: String,
}

#[derive(Debug, Clone)]
struct Snapshot {
    status: &'static str,
    loop_status: &'static str,
    shuffle: bool,
    volume: f64,
    position_us: i64,
    rate: f64,
    at: Instant,
    can_next: bool,
    can_previous: bool,
    has_track: bool,
    can_seek: bool,
    meta: Option<Meta>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            status: "Stopped",
            loop_status: "None",
            shuffle: false,
            volume: 1.0,
            position_us: 0,
            rate: 1.0,
            at: Instant::now(),
            can_next: false,
            can_previous: false,
            has_track: false,
            can_seek: false,
            meta: None,
        }
    }
}

impl Snapshot {
    fn position(&self) -> i64 {
        if self.status == "Playing" {
            self.position_us + (self.at.elapsed().as_micros() as f64 * self.rate) as i64
        } else {
            self.position_us
        }
    }
}

type Shared = Arc<Mutex<Snapshot>>;

enum Signal {
    Changed(Vec<&'static str>),
    Seeked(i64),
}

fn track_id(t: &Track) -> String {
    let id = if t.id > 0 { t.id.to_string() } else { format!("f{}", hash(&t.path.to_string_lossy())) };
    format!("/io/github/design_nexus/nyx/Music/track/{id}")
}

fn value(v: Value<'_>) -> Option<OwnedValue> {
    v.try_to_owned().ok()
}

fn metadata(meta: &Option<Meta>) -> HashMap<String, OwnedValue> {
    let mut m = HashMap::new();
    let Some(t) = meta else {
        if let Ok(p) = ObjectPath::try_from("/org/mpris/MediaPlayer2/TrackList/NoTrack")
            && let Some(v) = value(Value::from(p))
        {
            m.insert("mpris:trackid".into(), v);
        }
        return m;
    };
    let mut put = |k: &str, v: Value<'_>| {
        if let Some(v) = value(v) {
            m.insert(k.to_string(), v);
        }
    };
    if let Ok(p) = ObjectPath::try_from(t.track_id.as_str()) {
        put("mpris:trackid", Value::from(p));
    }
    put("mpris:length", Value::from(t.length_us));
    put("xesam:title", Value::from(t.title.as_str()));
    put("xesam:artist", Value::from(vec![t.artist.as_str()]));
    put("xesam:album", Value::from(t.album.as_str()));
    if !t.album_artist.is_empty() {
        put("xesam:albumArtist", Value::from(vec![t.album_artist.as_str()]));
    }
    if let Some(n) = t.track_no {
        put("xesam:trackNumber", Value::from(n as i32));
    }
    if !t.art_url.is_empty() {
        put("mpris:artUrl", Value::from(t.art_url.as_str()));
    }
    put("xesam:url", Value::from(t.url.as_str()));
    m
}

struct Root {
    tx: async_channel::Sender<Command>,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        let _ = self.tx.send_blocking(Command::Raise);
    }

    fn quit(&self) {
        let _ = self.tx.send_blocking(Command::Quit);
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> String {
        "Music".into()
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        crate::APP_ID.into()
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec!["file".into(), "http".into(), "https".into()]
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        ["audio/mpeg", "audio/flac", "audio/ogg", "audio/opus", "audio/mp4", "audio/aac", "audio/x-wav", "audio/x-aiff"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
}

struct Player {
    tx: async_channel::Sender<Command>,
    state: Shared,
}

impl Player {
    fn snap(&self) -> Snapshot {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn send(&self, c: Command) {
        let _ = self.tx.send_blocking(c);
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        self.send(Command::Next);
    }

    fn previous(&self) {
        self.send(Command::Previous);
    }

    fn pause(&self) {
        self.send(Command::Pause);
    }

    fn play_pause(&self) {
        self.send(Command::PlayPause);
    }

    fn stop(&self) {
        self.send(Command::Stop);
    }

    fn play(&self) {
        self.send(Command::Play);
    }

    fn seek(&self, offset: i64) {
        self.send(Command::Seek(offset));
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        self.send(Command::SetPosition(track_id.to_string(), position));
    }

    fn open_uri(&self, uri: String) {
        self.send(Command::OpenUri(uri));
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.snap().status.into()
    }

    #[zbus(property)]
    fn loop_status(&self) -> String {
        self.snap().loop_status.into()
    }

    #[zbus(property)]
    fn set_loop_status(&mut self, value: String) {
        self.send(Command::Loop(value));
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        self.snap().rate
    }

    #[zbus(property)]
    fn set_rate(&mut self, value: f64) {
        self.send(Command::Rate(value));
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.snap().shuffle
    }

    #[zbus(property)]
    fn set_shuffle(&mut self, value: bool) {
        self.send(Command::Shuffle(value));
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        metadata(&self.snap().meta)
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.snap().volume
    }

    #[zbus(property)]
    fn set_volume(&mut self, value: f64) {
        self.send(Command::Volume(value));
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.snap().position()
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        0.5
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        3.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.snap().can_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.snap().can_previous
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.snap().has_track
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.snap().can_seek
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

fn serve(state: Shared, tx: async_channel::Sender<Command>, signals: mpsc::Receiver<Signal>) -> zbus::Result<()> {
    let conn = zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(PATH, Root { tx: tx.clone() })?
        .serve_at(PATH, Player { tx, state })?
        .build()?;
    let iface = conn.object_server().interface::<_, Player>(PATH)?;
    for sig in signals {
        let emitter = iface.signal_emitter();
        let guard = iface.get();
        let r = zbus::block_on(async {
            match sig {
                Signal::Seeked(pos) => Player::seeked(emitter, pos).await,
                Signal::Changed(props) => {
                    for p in props {
                        match p {
                            "PlaybackStatus" => guard.playback_status_changed(emitter).await?,
                            "LoopStatus" => guard.loop_status_changed(emitter).await?,
                            "Shuffle" => guard.shuffle_changed(emitter).await?,
                            "Metadata" => guard.metadata_changed(emitter).await?,
                            "Volume" => guard.volume_changed(emitter).await?,
                            "Rate" => guard.rate_changed(emitter).await?,
                            "CanGoNext" => guard.can_go_next_changed(emitter).await?,
                            "CanGoPrevious" => guard.can_go_previous_changed(emitter).await?,
                            "CanPause" => guard.can_pause_changed(emitter).await?,
                            "CanSeek" => guard.can_seek_changed(emitter).await?,
                            _ => {}
                        }
                    }
                    Ok(())
                }
            }
        });
        drop(guard);
        if let Err(e) = r {
            eprintln!("nyx-music: mpris: {e}");
        }
    }
    Ok(())
}

fn snapshot() -> Snapshot {
    let p = prefs::get();
    let current = player::current();
    let state = player::state();
    let meta = current.as_ref().map(|t| Meta {
        track_id: track_id(t),
        length_us: (player::duration().max(t.duration) * 1e6) as i64,
        title: t.title.clone(),
        artist: t.artist.clone(),
        album: t.album.clone(),
        album_artist: t.album_artist.clone(),
        track_no: t.track_no,
        art_url: if t.art.is_empty() || !art::file(&t.art, false).exists() {
            String::new()
        } else {
            glib::filename_to_uri(art::file(&t.art, false), None).map(|u| u.to_string()).unwrap_or_default()
        },
        url: t.file_uri(),
    });
    Snapshot {
        status: match state {
            State::Playing => "Playing",
            State::Paused => "Paused",
            State::Stopped => "Stopped",
        },
        loop_status: match player::repeat() {
            Repeat::Off => "None",
            Repeat::All => "Playlist",
            Repeat::One => "Track",
        },
        shuffle: player::shuffle(),
        volume: if p.muted { 0.0 } else { p.volume },
        position_us: (player::position() * 1e6) as i64,
        rate: crate::audiobookshelf::playback::rate(),
        at: Instant::now(),
        can_next: player::can_next(),
        can_previous: player::can_previous(),
        has_track: current.is_some(),
        can_seek: current.as_ref().is_some_and(|t| !t.is_live()),
        meta,
    }
}

fn handle(app: &gtk::Application, c: Command) {
    match c {
        Command::Rate(rate) => crate::audiobookshelf::playback::set_rate(rate),
        Command::Raise => window::present(app, None),
        Command::Quit => app.quit(),
        Command::Play => player::play(),
        Command::Pause => player::pause(),
        Command::PlayPause => player::toggle(),
        Command::Stop => player::stop(),
        Command::Next => player::next(),
        Command::Previous => player::previous(),
        Command::Seek(us) => player::seek_by(us as f64 / 1e6),
        Command::SetPosition(id, us) => {
            if player::current().is_some_and(|t| track_id(&t) == id) {
                player::seek(us as f64 / 1e6);
            }
        }
        Command::OpenUri(uri) => {
            if uri.starts_with("http://") || uri.starts_with("https://") {
                crate::online::open_urls(vec![uri], false);
            } else if let Some(path) = gio::File::for_uri(&uri).path() {
                player::play_tracks(vec![path], 0);
            }
        }
        Command::Loop(s) => player::set_repeat(match s.as_str() {
            "Playlist" => Repeat::All,
            "Track" => Repeat::One,
            _ => Repeat::Off,
        }),
        Command::Shuffle(on) => player::set_shuffle(on),
        Command::Volume(v) => {
            player::set_volume(v.clamp(0.0, 1.0));
            if prefs::get().muted && v > 0.0 {
                player::set_muted(false);
            }
        }
    }
}

/// Register on the session bus. Failures (no session bus) are reported once
/// and otherwise ignored; the app works without MPRIS.
pub fn start(app: &gtk::Application) {
    let shared: Shared = Arc::new(Mutex::new(snapshot()));
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<Command>();
    let (sig_tx, sig_rx) = mpsc::channel::<Signal>();
    {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("mpris".into())
            .spawn(move || {
                if let Err(e) = serve(shared, cmd_tx, sig_rx) {
                    eprintln!("nyx-music: MPRIS unavailable: {e}");
                }
            })
            .ok();
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        while let Ok(c) = cmd_rx.recv().await {
            handle(&app, c);
        }
    });

    let last: std::cell::RefCell<Snapshot> = std::cell::RefCell::new(snapshot());
    player::subscribe_global(move |e| {
        let now = snapshot();
        let mut changed = Vec::new();
        {
            let prev = last.borrow();
            if (prev.rate - now.rate).abs() > 1e-6 {
                changed.push("Rate");
            }
            if prev.status != now.status {
                changed.push("PlaybackStatus");
            }
            if prev.loop_status != now.loop_status {
                changed.push("LoopStatus");
            }
            if prev.shuffle != now.shuffle {
                changed.push("Shuffle");
            }
            if (prev.volume - now.volume).abs() > 1e-6 {
                changed.push("Volume");
            }
            if prev.can_next != now.can_next {
                changed.push("CanGoNext");
            }
            if prev.can_previous != now.can_previous {
                changed.push("CanGoPrevious");
            }
            if prev.has_track != now.has_track {
                changed.push("CanPause");
            }
            if prev.can_seek != now.can_seek {
                changed.push("CanSeek");
            }
            let same_meta = match (&prev.meta, &now.meta) {
                (Some(a), Some(b)) => {
                    a.track_id == b.track_id
                        && a.length_us == b.length_us
                        && a.art_url == b.art_url
                        && a.title == b.title
                        && a.artist == b.artist
                }
                (None, None) => true,
                _ => false,
            };
            if !same_meta {
                changed.push("Metadata");
            }
        }
        let seeked = (e == Event::Seeked).then_some(now.position_us);
        if let Ok(mut s) = shared.lock() {
            *s = now.clone();
        }
        *last.borrow_mut() = now;
        if !changed.is_empty() {
            let _ = sig_tx.send(Signal::Changed(changed));
        }
        if let Some(pos) = seeked {
            let _ = sig_tx.send(Signal::Seeked(pos));
        }
    });
}
