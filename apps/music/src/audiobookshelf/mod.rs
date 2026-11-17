pub mod api;
pub mod downloads;
pub mod model;
pub mod playback;
pub mod settings;
pub mod storage;
pub mod ui;

use gtk::prelude::*;
use model::*;
use reqwest::Method;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ConnectionPhase {
    #[default]
    Disconnected,
    SigningIn,
    LoadingLibraries,
    Connected,
    Offline,
    NeedsSignIn,
    SignInFailed,
}
impl ConnectionPhase {
    pub fn busy(self) -> bool {
        matches!(self, Self::SigningIn | Self::LoadingLibraries)
    }
}
#[derive(Clone, Default)]
pub struct ConnectionUi {
    pub phase: ConnectionPhase,
    pub error: Option<String>,
}
pub fn connection() -> ConnectionUi {
    CONNECTION.with(|c| c.borrow().clone())
}
fn set_connection(phase: ConnectionPhase, error: Option<String>) {
    CONNECTION.with(|c| *c.borrow_mut() = ConnectionUi { phase, error });
    notify();
}
pub fn restore_saved_connection() {
    set_connection(if config().connected { ConnectionPhase::Connected } else { ConnectionPhase::Disconnected }, None);
}
pub fn dismiss_connection_error() {
    CONNECTION.with(|c| c.borrow_mut().error = None);
    notify();
}
pub fn authentication_required(message: String) {
    set_connection(ConnectionPhase::NeedsSignIn, Some(message));
}
fn connection_failed(error: &anyhow::Error) {
    let phase = if error.is::<api::AuthRequired>() { ConnectionPhase::NeedsSignIn } else { ConnectionPhase::Offline };
    set_connection(phase, Some(error.to_string()));
}
type CoverWaiters = std::collections::HashMap<String, Vec<Box<dyn FnOnce()>>>;
type Listener = (gtk::glib::WeakRef<gtk::Widget>, Rc<dyn Fn()>);
thread_local! {
    static CONNECTION:RefCell<ConnectionUi>=RefCell::new(ConnectionUi::default());
    static CONNECTION_EPOCH:Cell<u64>=const{Cell::new(0)};
    static CONFIG:RefCell<Config>=RefCell::new(storage::config());
    static STATUS:RefCell<String>=const{RefCell::new(String::new())};
    static COVER_WAITERS:RefCell<CoverWaiters>=RefCell::new(std::collections::HashMap::new());
    static LISTENERS:RefCell<Vec<Listener>>=const{RefCell::new(Vec::new())};
}
pub fn config() -> Config {
    CONFIG.with(|c| c.borrow().clone())
}
pub fn save_config(c: Config) {
    let _ = storage::put("config", "active", &c);
    CONFIG.with(|v| *v.borrow_mut() = c);
    notify();
}
pub fn status() -> String {
    STATUS.with(|s| s.borrow().clone())
}
pub fn set_status(s: impl Into<String>) {
    let message = s.into();
    let changed = STATUS.with(|v| {
        let mut previous = v.borrow_mut();
        if *previous == message {
            return false;
        }
        *previous = message.clone();
        true
    });
    if !changed {
        return;
    }
    if !matches!(
        message.as_str(),
        "" | "Ready"
            | "Connected"
            | "Disconnected"
            | "Signing in…"
            | "Connecting…"
            | "Opening audiobook…"
            | "Connected · progress synchronized"
    ) {
        crate::window::toast(&message);
    }
    notify();
}
pub fn subscribe(w: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push((w.upcast_ref::<gtk::Widget>().downgrade(), Rc::new(f))));
}
pub fn notify() {
    let fs = LISTENERS.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|(w, _)| w.upgrade().is_some());
        l.iter().map(|(_, f)| f.clone()).collect::<Vec<_>>()
    });
    for f in fs {
        f();
    }
}
pub fn init() {
    for mut d in storage::downloads(&config()) {
        if d.state == "downloading" {
            d.state = "paused".into();
            let _ = storage::put("download", &d.book.id.key(), &d);
        }
    }
    playback::init();
    if config().connected {
        refresh();
    }
}
pub fn libraries() -> Vec<Library> {
    storage::get("libraries", &config().connection)
}
pub fn refresh() {
    let c = config();
    if !c.connected || connection().phase.busy() {
        return;
    }
    set_connection(ConnectionPhase::LoadingLibraries, None);
    let expected = c.connection.clone();
    api::run(
        async move {
            let v = api::json(&c, Method::GET, "api/libraries", None).await?;
            let libs: Vec<Library> = serde_json::from_value(v["libraries"].clone())?;
            storage::put("libraries", &c.connection, &libs)?;
            Ok(libs)
        },
        move |r| {
            if config().connection != expected || !config().connected {
                return;
            }
            match r {
                Ok(libs) => {
                    let mut c = config();
                    if !c.library_selection_set {
                        c.library_selection_set = true;
                        c.libraries = libs.iter().filter(|l| l.media_type == "book").map(|l| l.id.clone()).collect();
                        save_config(c);
                    }
                    set_connection(ConnectionPhase::Connected, None);
                    let c = config();
                    api::run(async move { cache_home(&c, &libs).await }, |_| notify());
                    sync_pending();
                }
                Err(e) => connection_failed(&e),
            }
        },
    );
}
pub fn login(server: String, user: String, password: String) {
    if connection().phase.busy() {
        return;
    }
    playback::deactivate();
    let epoch = CONNECTION_EPOCH.with(|e| {
        let n = e.get() + 1;
        e.set(n);
        n
    });
    set_connection(ConnectionPhase::SigningIn, None);
    api::run(api::login(server, user, password), move |r| {
        if CONNECTION_EPOCH.with(|e| e.get()) != epoch {
            return;
        }
        match r {
            Ok(c) => {
                save_config(c);
                set_connection(ConnectionPhase::Connected, None);
                refresh();
            }
            Err(e) => set_connection(ConnectionPhase::SignInFailed, Some(e.to_string())),
        }
    });
}
pub fn disconnect() {
    downloads::pause_all();
    CONNECTION_EPOCH.with(|e| e.set(e.get() + 1));
    playback::deactivate();
    let mut c = config();
    let old = c.clone();
    c.connected = false;
    save_config(c);
    set_connection(ConnectionPhase::Disconnected, None);
    api::run(async move { api::logout(&old).await }, |r| {
        set_status(if r.is_ok() { "Disconnected" } else { "Disconnected locally; server logout could not be confirmed." });
    });
}
pub async fn sync_one(c: &Config, id: &MediaId, online_session: Option<String>, delta: f64) -> anyhow::Result<()> {
    if !c.sync || !c.connected {
        return Ok(());
    }
    anyhow::ensure!(
        id.connection == c.connection && id.user == c.user,
        "This item belongs to a different Audiobookshelf account."
    );
    let lock = api::shared_lock();
    let _guard = lock.lock().await;
    let remote = api::progress(c, id).await?;
    let local = storage::change_progress(id, |local| match reconcile(local, &remote) {
        Reconcile::Conflict => local.conflict = Some(remote.clone()),
        Reconcile::Server => {
            local.position = remote.current_time;
            local.finished = remote.is_finished;
            local.base = remote.clone();
            local.conflict = None;
        }
        Reconcile::Local => {}
    })?;
    if !local.dirty || local.conflict.is_some() {
        return Ok(());
    }
    let revision = local.revision;
    if let Some(session) = online_session {
        api::checked(
            api::request(
                c,
                Method::POST,
                &format!("api/session/{}/sync", api::encode(&session)),
                Some(&json!({"currentTime":local.position,"timeListened":delta})),
            )
            .await?,
        )?;
    } else if let Some(session) = &local.session {
        let v = api::json(c, Method::POST, "api/session/local-all", Some(&json!({"sessions":[session]}))).await?;
        anyhow::ensure!(v["results"][0]["success"] == true, "Offline session could not be synchronized.");
        if v["results"][0]["progressSynced"] != true {
            let remote = api::progress(c, id).await?;
            storage::change_progress(id, |current| {
                if (remote.current_time - local.position).abs() > 1.0 {
                    current.conflict = Some(remote);
                } else {
                    acknowledge(current, revision, remote);
                }
            })?;
            return Ok(());
        }
    } else {
        api::checked(
            api::request(
                c,
                Method::PATCH,
                &id.progress_path(),
                Some(&json!({"currentTime":local.position,"isFinished":local.finished})),
            )
            .await?,
        )?;
    }
    let remote = api::progress(c, id).await?;
    storage::change_progress(id, |current| {
        if (remote.current_time - local.position).abs() > 1.0 {
            current.conflict = Some(remote);
        } else {
            acknowledge(current, revision, remote);
        }
    })?;
    Ok(())
}
pub fn sync_all() {
    sync_impl(true);
}
pub fn sync_pending() {
    if has_pending() {
        sync_impl(false);
    }
}
fn sync_impl(all: bool) {
    let c = config();
    if !c.connected || !c.sync {
        return;
    }
    let active = playback::current_book().map(|b| b.id);
    playback::synchronize();
    api::run(
        async move {
            for b in storage::books(&c) {
                synchronize_bookmarks(&c, &b).await?;
                let mut items = vec![b.clone()];
                items.extend(b.episodes);
                for b in items {
                    if active.as_ref() != Some(&b.id) && (all || storage::progress(&b.id).dirty) {
                        sync_one(&c, &b.id, None, 0.0).await?;
                    }
                }
            }
            Ok(())
        },
        |r| match r {
            Ok(()) => {
                let mut c = config();
                c.last_sync = storage::now();
                save_config(c);
                set_status(if has_pending() {
                    "Some listening positions need a choice or are pending synchronization."
                } else {
                    "Connected · progress synchronized"
                });
            }
            Err(e) => set_status(e.to_string()),
        },
    );
}
pub fn resolve(id: MediaId, use_local: bool) {
    let mut p = storage::progress(&id);
    let Some(remote) = p.conflict.take() else {
        return;
    };
    p.base = remote.clone();
    if use_local {
        p.dirty = true;
        p.updated = storage::now();
        p.session = None;
    } else {
        p.position = remote.current_time;
        p.finished = remote.is_finished;
        p.dirty = false;
        p.session = None;
    }
    p.revision += 1;
    let _ = storage::put("progress", &id.key(), &p);
    playback::resolved(&id, p.position);
    notify();
    sync_all();
}
pub fn cover(book: &Book, done: impl FnOnce() + 'static) {
    if crate::library::art::file(&book.art, false).exists() {
        done();
        return;
    }
    let key = book.art.clone();
    let first = COVER_WAITERS.with(|w| {
        let mut w = w.borrow_mut();
        let first = !w.contains_key(&key);
        w.entry(key.clone()).or_default().push(Box::new(done));
        first
    });
    if !first {
        return;
    }
    let result_key = key.clone();
    let c = config();
    let id = book.id.item.clone();
    api::run(
        async move {
            let mut r = api::checked(
                api::request(&c, Method::GET, &format!("api/items/{}/cover?width=600", api::encode(&id)), None).await?,
            )?;
            let mut bytes = Vec::new();
            while let Some(chunk) = r.chunk().await? {
                anyhow::ensure!(bytes.len() + chunk.len() <= 12 * 1024 * 1024, "Artwork is too large.");
                bytes.extend_from_slice(&chunk);
            }
            crate::library::art::save(&key, &bytes);
            Ok(())
        },
        move |_| {
            let waiters = COVER_WAITERS.with(|w| w.borrow_mut().remove(&result_key).unwrap_or_default());
            for f in waiters {
                f();
            }
            if playback::current_book().is_some_and(|b| b.art == result_key) {
                crate::player::metadata_changed();
            }
        },
    );
}

pub fn has_pending() -> bool {
    storage::books(&config()).iter().any(|b| {
        storage::progress(&b.id).dirty
            || b.episodes.iter().any(|e| storage::progress(&e.id).dirty)
            || storage::get::<Vec<Bookmark>>("bookmarks", &b.id.key()).iter().any(|m| m.pending)
    })
}

pub async fn synchronize_bookmarks(c: &Config, b: &Book) -> anyhow::Result<()> {
    if b.podcast {
        return Ok(());
    }
    let key = b.id.key();
    let marks: Vec<Bookmark> = storage::get("bookmarks", &key);
    for mark in marks.into_iter().filter(|m| m.pending) {
        api::checked(
            api::request(
                c,
                Method::POST,
                &format!("api/me/item/{}/bookmark", api::encode(&b.id.item)),
                Some(&json!({"time":mark.position.floor() as i64,"title":mark.title})),
            )
            .await?,
        )?;
        let mut current: Vec<Bookmark> = storage::get("bookmarks", &key);
        for m in &mut current {
            if m.position == mark.position && m.title == mark.title {
                m.pending = false;
            }
        }
        storage::put("bookmarks", &key, &current)?;
    }
    Ok(())
}
pub fn bookmarks(book: &Book) -> Vec<Bookmark> {
    let mut marks: Vec<Bookmark> = storage::get("bookmarks", &book.id.key());
    let remote: Vec<serde_json::Value> = storage::get("server-bookmarks", &format!("{}/{}", book.id.connection, book.id.user));
    for v in remote.into_iter().filter(|v| v["libraryItemId"].as_str() == Some(&book.id.item)) {
        if let Some(position) = v["time"].as_f64()
            && !marks.iter().any(|m| (m.position - position).abs() < 1.0)
        {
            marks.push(Bookmark { position, title: v["title"].as_str().unwrap_or("Bookmark").into(), pending: false });
        }
    }
    marks.sort_by(|a, b| a.position.total_cmp(&b.position));
    marks
}

async fn cache_home(c: &Config, libs: &[Library]) -> anyhow::Result<()> {
    let selected: Vec<_> =
        libs.iter().filter(|l| l.media_type == "book" && (!c.library_selection_set || c.libraries.contains(&l.id))).collect();
    for lib in selected {
        for suffix in ["sort=addedAt&desc=1", "filter=progress.aW4tcHJvZ3Jlc3M%3D"] {
            if let Ok(items) = api::json(
                c,
                Method::GET,
                &format!("api/libraries/{}/items?limit=6&minified=1&include=progress&{suffix}", api::encode(&lib.id)),
                None,
            )
            .await
                && let Some(items) = items["results"].as_array()
            {
                for v in items {
                    let b = Book::parse(v, c);
                    storage::cache_book(&b)?;
                    if let Ok(remote) = serde_json::from_value::<ServerProgress>(v["userMediaProgress"].clone()) {
                        storage::change_progress(&b.id, |p| {
                            if !p.dirty {
                                p.position = remote.current_time;
                                p.finished = remote.is_finished;
                                p.base = remote;
                            }
                        })?;
                    }
                }
            }
        }
    }
    if let Ok(me) = api::json(c, Method::GET, "api/me", None).await {
        if let Some(marks) = me["bookmarks"].as_array() {
            storage::put("server-bookmarks", &format!("{}/{}", c.connection, c.user), marks)?;
        }
        if let Some(progress) = me["mediaProgress"].as_array() {
            for value in progress {
                if let (Some(item), Ok(remote)) =
                    (value["libraryItemId"].as_str(), serde_json::from_value::<ServerProgress>(value.clone()))
                {
                    let id = MediaId {
                        connection: c.connection.clone(),
                        user: c.user.clone(),
                        item: item.to_owned(),
                        episode: value["episodeId"].as_str().map(str::to_owned),
                    };
                    storage::change_progress(&id, |p| {
                        if !p.dirty {
                            p.position = remote.current_time;
                            p.finished = remote.is_finished;
                            p.updated = remote.last_update;
                            p.base = remote;
                        }
                    })?;
                }
            }
        }
    }
    Ok(())
}
