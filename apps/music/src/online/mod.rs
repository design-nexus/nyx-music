//! Internet radio and podcasts. Stations and episodes are `Track`s whose path
//! is their address (`https://…`), so the queue, playlists and resume handle
//! them like songs. Their details live in SQLite (`streams`) and in memory here,
//! on the UI thread, alongside favourite stations and podcast subscriptions.

pub mod http;
pub mod podcast;
pub mod radio;

use crate::library::db::{self, EpisodeInfo, Podcast, Progress};
use crate::library::{Kind, Track, art, hash};
use crate::{cmd, paths, prefs, window};
use gtk::glib;
use gtk::prelude::*;
use podcast::{Episode, Feed};
use radio::Station;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Subscriptions older than this are refreshed when Music opens.
const STALE_SECS: i64 = 6 * 3600;
/// Extensions that mark an address as a file rather than a live stream.
const FILE_EXTS: &[&str] = &["mp3", "m4a", "mp4", "aac", "ogg", "oga", "opus", "flac", "wav"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Favourite or custom stations.
    Stations,
    /// Subscriptions.
    Podcasts,
    /// Episodes were added, played or downloaded.
    Episodes,
    /// Download progress.
    Downloads,
}

#[derive(Default)]
struct Online {
    items: HashMap<PathBuf, Rc<Track>>,
    /// Items that have a row in the database.
    saved: HashSet<PathBuf>,
    info: HashMap<PathBuf, EpisodeInfo>,
    progress: HashMap<PathBuf, Progress>,
    stations: Vec<Station>,
    podcasts: Vec<Podcast>,
    /// (bytes done, total) for downloads in flight.
    downloads: HashMap<PathBuf, (u64, u64)>,
    refreshing: HashSet<String>,
}

type Listener = (glib::WeakRef<gtk::Widget>, Rc<dyn Fn(Change)>);

thread_local! {
    static ONLINE: RefCell<Online> = RefCell::new(Online::default());
    static LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
}

fn with<R>(f: impl FnOnce(&mut Online) -> R) -> R {
    ONLINE.with(|o| f(&mut o.borrow_mut()))
}

/// Call `f` on every change while `owner` is alive.
pub fn subscribe(owner: &impl IsA<gtk::Widget>, f: impl Fn(Change) + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push((owner.upcast_ref::<gtk::Widget>().downgrade(), Rc::new(f))));
}

fn notify(change: Change) {
    let fs: Vec<Rc<dyn Fn(Change)>> = LISTENERS.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|(w, _)| w.upgrade().is_some());
        l.iter().map(|(_, f)| f.clone()).collect()
    });
    for f in fs {
        f(change);
    }
}

pub fn is_remote(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    s.starts_with("http://") || s.starts_with("https://")
}

/// Read what's remembered. Runs before playback starts so a resumed queue
/// finds its stations and episodes.
pub fn init() {
    let loaded = db::open().and_then(|c| Ok((db::load_streams(&c)?, db::stations(&c)?, db::podcasts(&c)?)));
    match loaded {
        Ok((streams, stations, podcasts)) => with(|o| {
            for (t, p, i) in streams {
                o.saved.insert(t.path.clone());
                o.progress.insert(t.path.clone(), p);
                o.info.insert(t.path.clone(), i);
                o.items.insert(t.path.clone(), Rc::new(t));
            }
            o.stations = stations;
            o.podcasts = podcasts;
        }),
        Err(e) => eprintln!("nyx-music: couldn't read stations and podcasts: {e}"),
    }
    if prefs::get().podcast_refresh {
        glib::timeout_add_local_once(std::time::Duration::from_secs(3), || refresh_all(false));
    }
}

// ---------- Items ----------

pub fn find(path: &Path) -> Option<Rc<Track>> {
    with(|o| o.items.get(path).cloned())
}

/// The track for an address: remembered, or a bare one named after it.
pub fn track_for(path: &Path) -> Rc<Track> {
    find(path).unwrap_or_else(|| Rc::new(bare(&path.to_string_lossy())))
}

/// An address nothing is known about (from the command line or a playlist file).
pub fn bare(url: &str) -> Track {
    let host = http::host(url).to_string();
    let last = url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or("").to_string();
    let ext = last.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let file = FILE_EXTS.contains(&ext.as_str());
    Track {
        kind: if file { Kind::Episode } else { Kind::Station },
        path: PathBuf::from(url),
        title: if file && !last.is_empty() { last } else { host.clone() },
        artist: host,
        ..Default::default()
    }
}

/// Give a station known only by its address the name it broadcasts.
/// True when the name changed.
pub fn name_station(path: &Path, name: &str) -> bool {
    let Some(t) = find(path) else { return false };
    let host = http::host(&t.path.to_string_lossy()).to_string();
    if name.is_empty() || t.title != host || t.kind != Kind::Station {
        return false;
    }
    let renamed = Track { title: name.to_string(), artist: "Internet radio".into(), ..(*t).clone() };
    let saved = with(|o| o.saved.contains(path));
    remember(vec![(renamed, info(path))], saved);
    true
}

pub fn info(path: &Path) -> EpisodeInfo {
    with(|o| o.info.get(path).cloned()).unwrap_or_default()
}

pub fn progress(path: &Path) -> Progress {
    with(|o| o.progress.get(path).copied()).unwrap_or_default()
}

/// Keep tracks in memory; `save` also writes them to the database.
fn remember(items: Vec<(Track, EpisodeInfo)>, save: bool) {
    let mut to_save = Vec::new();
    with(|o| {
        for (mut t, i) in items {
            // A download outlives feed refreshes.
            if t.download.is_none() {
                t.download = o.items.get(&t.path).and_then(|old| old.download.clone());
            }
            let known = o.saved.contains(&t.path);
            if save || known {
                o.saved.insert(t.path.clone());
                to_save.push((t.clone(), i.clone()));
            }
            o.info.insert(t.path.clone(), i);
            o.items.insert(t.path.clone(), Rc::new(t));
        }
    });
    if !to_save.is_empty() {
        cmd::background(
            move || -> anyhow::Result<()> {
                let conn = db::open()?;
                let tx = conn.unchecked_transaction()?;
                for (t, i) in &to_save {
                    db::upsert_stream(&tx, t, i)?;
                }
                tx.commit()?;
                Ok(())
            },
            |r| {
                if let Err(e) = r {
                    eprintln!("nyx-music: couldn't save stations and episodes: {e}");
                }
            },
        );
    }
}

/// Make sure these items are in the database (before they're queued or saved
/// in a playlist), so they're still known after a restart.
pub fn keep(paths: &[PathBuf]) {
    let items: Vec<(Track, EpisodeInfo)> = with(|o| {
        paths
            .iter()
            .filter(|p| !o.saved.contains(*p))
            .filter_map(|p| Some(((*o.items.get(p)?.as_ref()).clone(), o.info.get(p).cloned().unwrap_or_default())))
            .collect()
    });
    remember(items, true);
}

/// Playback progress for an episode; `played` once it's heard to the end.
pub fn set_progress(path: &Path, position: f64, played: bool) {
    let p = Progress { position, played };
    let changed = with(|o| o.progress.insert(path.to_path_buf(), p) != Some(p));
    if !changed {
        return;
    }
    keep(&[path.to_path_buf()]);
    let path = path.to_path_buf();
    cmd::background(move || db::open().and_then(|c| db::set_progress(&c, &path, p)), |_| {});
    notify(Change::Episodes);
}

// ---------- Stations ----------

pub fn favourites() -> Vec<Station> {
    with(|o| o.stations.clone())
}

pub fn is_favourite(url: &str) -> bool {
    with(|o| o.stations.iter().any(|s| s.stream() == url))
}

pub fn station_track(s: &Station) -> (Track, EpisodeInfo) {
    let mut by = Vec::new();
    let genre = s.genre();
    if !genre.is_empty() {
        by.push(genre.clone());
    }
    if !s.country.is_empty() {
        by.push(s.country.clone());
    }
    (
        Track {
            kind: Kind::Station,
            path: PathBuf::from(s.stream()),
            title: s.name.trim().to_string(),
            artist: if by.is_empty() { "Internet radio".into() } else { by.join(" · ") },
            genre,
            art: art::key_for_url(&s.favicon),
            ..Default::default()
        },
        EpisodeInfo { guid: s.uuid.clone(), published: 0, description: s.homepage.clone() },
    )
}

/// Know these stations so they can be queued (they're saved once they are),
/// and fetch their logos.
pub fn prepare_stations(stations: &[Station]) -> Vec<PathBuf> {
    for s in stations {
        if !s.favicon.is_empty() {
            art::fetch(&s.favicon, |ok| {
                if ok {
                    crate::player::metadata_changed();
                }
            });
        }
    }
    let items: Vec<(Track, EpisodeInfo)> = stations.iter().map(station_track).collect();
    let paths = items.iter().map(|(t, _)| t.path.clone()).collect();
    remember(items, false);
    paths
}

pub fn play_station(s: &Station) {
    let paths = prepare_stations(std::slice::from_ref(s));
    crate::player::play_tracks(paths, 0);
}

pub fn set_favourite(s: &Station, on: bool) {
    let s = s.clone();
    let url = s.stream().to_string();
    with(|o| {
        o.stations.retain(|x| x.stream() != url);
        if on {
            o.stations.push(Station { url: url.clone(), url_resolved: String::new(), ..s.clone() });
            o.stations.sort_by_key(|x| x.name.to_lowercase());
        }
    });
    notify(Change::Stations);
    if on {
        prepare_stations(std::slice::from_ref(&s));
    }
    cmd::background(
        move || {
            let conn = db::open()?;
            if on { db::save_station(&conn, &s) } else { db::delete_station(&conn, &url) }
        },
        |r| {
            if let Err(e) = r {
                window::toast(&format!("Couldn't save the station: {e}"));
            }
        },
    );
}

/// Add or edit a station of your own. `old` is the address being edited.
pub fn save_custom(mut s: Station, old: Option<String>, done: impl FnOnce() + 'static) {
    let old2 = old.clone();
    cmd::background(
        move || -> anyhow::Result<Station> {
            let old = old2;
            s.url = radio::resolve(s.url.trim())?;
            s.url_resolved.clear();
            s.custom = true;
            let conn = db::open()?;
            if let Some(o) = &old
                && *o != s.url
            {
                db::delete_station(&conn, o)?;
            }
            db::save_station(&conn, &s)?;
            Ok(s)
        },
        move |r| match r {
            Ok(s) => {
                with(|o| {
                    o.stations.retain(|x| x.stream() != s.url && Some(x.stream()) != old.as_deref());
                    o.stations.push(s.clone());
                    o.stations.sort_by_key(|x| x.name.to_lowercase());
                });
                prepare_stations(std::slice::from_ref(&s));
                notify(Change::Stations);
                done();
            }
            Err(e) => window::toast(&format!("Couldn't add the station: {e}")),
        },
    );
}

/// Play or queue addresses from outside (command line, MPRIS): station
/// playlists are resolved first.
pub fn open_urls(urls: Vec<String>, enqueue: bool) {
    cmd::background(
        move || urls.iter().map(|u| radio::resolve(u).unwrap_or_else(|_| u.clone())).collect::<Vec<_>>(),
        move |resolved| {
            let paths: Vec<PathBuf> = resolved.iter().map(PathBuf::from).collect();
            remember(
                paths
                    .iter()
                    .filter(|p| find(p).is_none())
                    .map(|p| (bare(&p.to_string_lossy()), EpisodeInfo::default()))
                    .collect(),
                true,
            );
            if enqueue {
                crate::player::enqueue(paths);
            } else {
                crate::player::play_tracks(paths, 0);
            }
        },
    );
}

// ---------- Podcasts ----------

pub fn podcasts() -> Vec<Podcast> {
    with(|o| o.podcasts.clone())
}

pub fn podcast(feed: &str) -> Option<Podcast> {
    with(|o| o.podcasts.iter().find(|p| p.feed == feed).cloned())
}

pub fn is_subscribed(feed: &str) -> bool {
    podcast(feed).is_some()
}

pub fn refreshing(feed: &str) -> bool {
    with(|o| o.refreshing.contains(feed))
}

/// A podcast's episodes we know about, newest first.
pub fn episodes(feed: &str) -> Vec<Rc<Track>> {
    let mut list: Vec<(i64, Rc<Track>)> = with(|o| {
        o.items.values().filter(|t| t.feed == feed).map(|t| (o.info.get(&t.path).map_or(0, |i| i.published), t.clone())).collect()
    });
    list.sort_by_key(|(p, t)| (std::cmp::Reverse(*p), t.title.clone()));
    list.into_iter().map(|(_, t)| t).collect()
}

/// Episodes out since you subscribed that you haven't started.
pub fn new_count(p: &Podcast) -> usize {
    with(|o| {
        o.items
            .values()
            .filter(|t| t.feed == p.feed)
            .filter(|t| o.info.get(&t.path).is_some_and(|i| i.published > p.added))
            .filter(|t| o.progress.get(&t.path).is_none_or(|pr| !pr.played && pr.position <= 0.0))
            .count()
    })
}

/// Out since you subscribed to its show, and not started.
pub fn is_new(t: &Track) -> bool {
    with(|o| {
        let Some(p) = o.podcasts.iter().find(|p| p.feed == t.feed) else { return false };
        o.info.get(&t.path).is_some_and(|i| i.published > p.added)
            && o.progress.get(&t.path).is_none_or(|pr| !pr.played && pr.position <= 0.0)
    })
}

/// New episodes across every subscription.
pub fn new_total() -> usize {
    podcasts().iter().map(new_count).sum()
}

/// For Home: from each subscription, the episode you're part-way through or else
/// its newest new one, newest first.
pub fn fresh_episodes(n: usize) -> Vec<Rc<Track>> {
    let mut picks: Vec<(bool, i64, Rc<Track>)> = podcasts()
        .iter()
        .filter_map(|p| {
            let eps = episodes(&p.feed);
            let started = eps.iter().find(|t| progress(&t.path).position > 0.0 && !progress(&t.path).played);
            let pick = started.or_else(|| eps.iter().find(|t| is_new(t)))?;
            Some((started.is_some(), info(&pick.path).published, pick.clone()))
        })
        .collect();
    picks.sort_by_key(|(started, published, _)| std::cmp::Reverse((*started, *published)));
    picks.into_iter().take(n).map(|(_, _, t)| t).collect()
}

fn episode_track(feed_url: &str, feed: &Feed, e: &Episode) -> (Track, EpisodeInfo) {
    (
        Track {
            kind: Kind::Episode,
            path: PathBuf::from(&e.url),
            title: e.title.clone(),
            artist: if feed.author.is_empty() { feed.title.clone() } else { feed.author.clone() },
            album: feed.title.clone(),
            genre: "Podcast".into(),
            duration: e.duration,
            art: art::key_for_url(&feed.art_url),
            feed: feed_url.to_string(),
            ..Default::default()
        },
        EpisodeInfo { guid: e.guid.clone(), published: e.published, description: e.description.clone() },
    )
}

/// Fetch a feed and take in its episodes (saved when subscribed).
pub fn load_feed(feed_url: &str, done: impl FnOnce(anyhow::Result<Feed>) + 'static) {
    let url = feed_url.to_string();
    with(|o| o.refreshing.insert(url.clone()));
    notify(Change::Podcasts);
    cmd::background(
        {
            let url = url.clone();
            move || podcast::fetch(&url)
        },
        move |res: anyhow::Result<Feed>| {
            with(|o| o.refreshing.remove(&url));
            if let Ok(feed) = &res {
                art::fetch(&feed.art_url, |_| notify(Change::Podcasts));
                let subscribed = is_subscribed(&url);
                remember(feed.episodes.iter().map(|e| episode_track(&url, feed, e)).collect(), subscribed);
                if let Some(mut p) = podcast(&url) {
                    p.title = feed.title.clone();
                    p.author = feed.author.clone();
                    if !feed.art_url.is_empty() {
                        p.art_url = feed.art_url.clone();
                    }
                    p.description = feed.description.clone();
                    p.refreshed = db::now();
                    save_podcast(p);
                }
            }
            notify(Change::Podcasts);
            notify(Change::Episodes);
            done(res);
        },
    );
}

fn save_podcast(p: Podcast) {
    with(|o| {
        o.podcasts.retain(|x| x.feed != p.feed);
        o.podcasts.push(p.clone());
        o.podcasts.sort_by_key(|x| x.title.to_lowercase());
    });
    cmd::background(move || db::open().and_then(|c| db::save_podcast(&c, &p)), |_| {});
}

pub fn subscribe_feed(feed_url: &str, apple_id: &str, feed: &Feed) {
    let now = db::now();
    save_podcast(Podcast {
        feed: feed_url.to_string(),
        apple_id: apple_id.to_string(),
        title: feed.title.clone(),
        author: feed.author.clone(),
        art_url: feed.art_url.clone(),
        description: feed.description.clone(),
        // The latest episode counts as new.
        added: feed.episodes.first().map_or(now, |e| e.published - 1),
        refreshed: now,
    });
    remember(feed.episodes.iter().map(|e| episode_track(feed_url, feed, e)).collect(), true);
    notify(Change::Podcasts);
    window::toast(&format!("Subscribed to {}.", feed.title));
}

/// Subscribe to any feed address, after checking it's a podcast.
pub fn subscribe_url(url: &str, done: impl FnOnce(Option<String>) + 'static) {
    let url = url.trim().to_string();
    load_feed(&url.clone(), move |res| match res {
        Ok(feed) => {
            subscribe_feed(&url, "", &feed);
            done(Some(url));
        }
        Err(e) => {
            window::toast(&format!("Couldn't subscribe: {e:#}"));
            done(None);
        }
    });
}

pub fn unsubscribe(feed: &str) {
    let title = podcast(feed).map(|p| p.title).unwrap_or_default();
    with(|o| o.podcasts.retain(|p| p.feed != feed));
    notify(Change::Podcasts);
    let f = feed.to_string();
    cmd::background(move || db::open().and_then(|c| db::delete_podcast(&c, &f)), |_| {});
    window::toast(&format!("Unsubscribed from {title}."));
}

/// Look for new episodes (only stale subscriptions unless `force`).
pub fn refresh_all(force: bool) {
    let now = db::now();
    for p in podcasts() {
        if force || now - p.refreshed > STALE_SECS {
            load_feed(&p.feed, |_| {});
        }
    }
}

// ---------- Downloads ----------

pub fn download_state(path: &Path) -> Option<(u64, u64)> {
    with(|o| o.downloads.get(path).copied())
}

fn slug(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    let s: String = s.chars().take(60).collect();
    if s.is_empty() { "untitled".into() } else { s }
}

fn download_path(t: &Track) -> PathBuf {
    let url = t.path.to_string_lossy();
    let last = url.split(['?', '#']).next().unwrap_or(&url).rsplit('/').next().unwrap_or("");
    let ext = last.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).filter(|e| FILE_EXTS.contains(&e.as_str()));
    let name = format!("{}-{}.{}", slug(&t.title), &hash(&url)[..8], ext.as_deref().unwrap_or("mp3"));
    paths::podcasts_dir().join(slug(&t.album)).join(name)
}

pub fn download(path: &Path) {
    let Some(t) = find(path) else { return };
    if t.kind != Kind::Episode || download_state(path).is_some() {
        return;
    }
    keep(&[path.to_path_buf()]);
    let dest = download_path(&t);
    let url = path.to_string_lossy().into_owned();
    let key = path.to_path_buf();
    with(|o| o.downloads.insert(key.clone(), (0, 0)));
    notify(Change::Downloads);
    let (tx, rx) = async_channel::unbounded::<(u64, u64)>();
    let d = dest.clone();
    let handle = gtk::gio::spawn_blocking(move || {
        http::download(&url, &d, |done, total| {
            let _ = tx.send_blocking((done, total));
        })
    });
    let k = key.clone();
    glib::spawn_future_local(async move {
        while let Ok(p) = rx.recv().await {
            with(|o| o.downloads.insert(k.clone(), p));
            notify(Change::Downloads);
        }
    });
    glib::spawn_future_local(async move {
        let res = handle.await.unwrap_or_else(|_| Err(anyhow::anyhow!("the download stopped")));
        with(|o| o.downloads.remove(&key));
        match res {
            Ok(()) => set_download(&key, Some(dest)),
            Err(e) => window::toast(&format!("Couldn't download “{}”: {e:#}", t.title)),
        }
        notify(Change::Downloads);
    });
}

pub fn delete_download(path: &Path) {
    if let Some(f) = find(path).and_then(|t| t.download.clone()) {
        let _ = std::fs::remove_file(&f);
        if let Some(dir) = f.parent() {
            let _ = std::fs::remove_dir(dir); // only when empty
        }
    }
    set_download(path, None);
}

fn set_download(path: &Path, file: Option<PathBuf>) {
    with(|o| {
        if let Some(t) = o.items.get(path) {
            let t = Track { download: file.clone(), ..(**t).clone() };
            o.items.insert(path.to_path_buf(), Rc::new(t));
        }
    });
    let p = path.to_path_buf();
    cmd::background(move || db::open().and_then(|c| db::set_download(&c, &p, file.as_deref())), |_| {});
    notify(Change::Episodes);
    crate::player::library_changed();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_bare_addresses() {
        let t = bare("https://cdn.example.com/shows/ep12.mp3?x=1");
        assert_eq!((t.kind, t.title.as_str(), t.artist.as_str()), (Kind::Episode, "ep12.mp3", "cdn.example.com"));
        let t = bare("http://ice.example.com:8000/live");
        assert_eq!((t.kind, t.title.as_str()), (Kind::Station, "ice.example.com:8000"));
        assert!(is_remote(Path::new("https://a/b")));
        assert!(!is_remote(Path::new("/home/a.mp3")));
    }

    #[test]
    fn makes_tidy_file_names() {
        assert_eq!(slug("Episode 12: The “Big” One!"), "episode-12-the-big-one");
        assert_eq!(slug("???"), "untitled");
        let t =
            Track { title: "Ep 1".into(), album: "My Show".into(), path: "https://c/x/1.m4a?t=2".into(), ..Default::default() };
        let p = download_path(&t);
        assert!(p.ends_with(format!("my-show/ep-1-{}.m4a", &hash("https://c/x/1.m4a?t=2")[..8])));
    }
}
