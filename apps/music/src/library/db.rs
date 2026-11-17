//! SQLite storage. Every call opens its own connection, so it can run on any
//! thread; WAL mode keeps readers and the scanner out of each other's way.

use super::{Kind, Track};
use crate::online::radio::Station;
use crate::paths;
use anyhow::Result;
use rusqlite::{Connection, Transaction, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS abs_records (
    kind TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
    PRIMARY KEY(kind, key)
);
CREATE TABLE IF NOT EXISTS tracks (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    artist TEXT NOT NULL,
    album TEXT NOT NULL,
    album_artist TEXT NOT NULL,
    genre TEXT NOT NULL,
    year INTEGER,
    track_no INTEGER,
    disc_no INTEGER,
    duration REAL NOT NULL,
    rg_track_gain REAL,
    rg_track_peak REAL,
    rg_album_gain REAL,
    rg_album_peak REAL,
    art TEXT NOT NULL DEFAULT '',
    plays INTEGER NOT NULL DEFAULT 0,
    last_played INTEGER NOT NULL DEFAULT 0,
    mtime INTEGER NOT NULL,
    size INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS playlists (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS playlist_items (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    pos INTEGER NOT NULL,
    path TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS playlist_items_by_list ON playlist_items(playlist_id, pos);
CREATE TABLE IF NOT EXISTS streams (
    url TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    artist TEXT NOT NULL,
    album TEXT NOT NULL,
    genre TEXT NOT NULL DEFAULT '',
    art TEXT NOT NULL DEFAULT '',
    feed TEXT NOT NULL DEFAULT '',
    guid TEXT NOT NULL DEFAULT '',
    duration REAL NOT NULL DEFAULT 0,
    published INTEGER NOT NULL DEFAULT 0,
    description TEXT NOT NULL DEFAULT '',
    position REAL NOT NULL DEFAULT 0,
    played INTEGER NOT NULL DEFAULT 0,
    file TEXT NOT NULL DEFAULT '',
    added INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS streams_by_feed ON streams(feed);
CREATE TABLE IF NOT EXISTS stations (
    url TEXT PRIMARY KEY,
    uuid TEXT NOT NULL,
    name TEXT NOT NULL,
    homepage TEXT NOT NULL,
    favicon TEXT NOT NULL,
    tags TEXT NOT NULL,
    country TEXT NOT NULL,
    countrycode TEXT NOT NULL,
    codec TEXT NOT NULL,
    bitrate INTEGER NOT NULL,
    custom INTEGER NOT NULL,
    added INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS podcasts (
    feed TEXT PRIMARY KEY,
    apple_id TEXT NOT NULL,
    title TEXT NOT NULL,
    author TEXT NOT NULL,
    art_url TEXT NOT NULL,
    description TEXT NOT NULL,
    added INTEGER NOT NULL,
    refreshed INTEGER NOT NULL
);
";

pub fn open() -> Result<Connection> {
    open_at(&paths::library_db())
}

pub fn open_at(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Columns added after the first release. A library from before them has
/// every file read again once (mtime 0 no longer matches) to fill them in.
fn migrate(conn: &Connection) -> Result<()> {
    let has = |col: &str| -> Result<bool> {
        let mut stmt = conn.prepare("SELECT 1 FROM pragma_table_info('tracks') WHERE name = ?1")?;
        Ok(stmt.exists([col])?)
    };
    if !has("bitrate")? {
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE tracks ADD COLUMN bitrate INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE tracks ADD COLUMN sample_rate INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE tracks ADD COLUMN bit_depth INTEGER NOT NULL DEFAULT 0;
             UPDATE tracks SET mtime = 0;
             COMMIT;",
        )?;
    }
    Ok(())
}

fn path_text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub fn load_all(conn: &Connection) -> Result<Vec<Track>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, title, artist, album, album_artist, genre, year, track_no, disc_no, duration,
                rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, art, plays, last_played, mtime, size,
                bitrate, sample_rate, bit_depth
         FROM tracks",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Track {
            id: r.get(0)?,
            path: PathBuf::from(r.get::<_, String>(1)?),
            title: r.get(2)?,
            artist: r.get(3)?,
            album: r.get(4)?,
            album_artist: r.get(5)?,
            genre: r.get(6)?,
            year: r.get(7)?,
            track_no: r.get(8)?,
            disc_no: r.get(9)?,
            duration: r.get(10)?,
            rg_track_gain: r.get(11)?,
            rg_track_peak: r.get(12)?,
            rg_album_gain: r.get(13)?,
            rg_album_peak: r.get(14)?,
            art: r.get(15)?,
            plays: std::cell::Cell::new(r.get(16)?),
            last_played: std::cell::Cell::new(r.get(17)?),
            mtime: r.get(18)?,
            size: r.get(19)?,
            bitrate: r.get(20)?,
            sample_rate: r.get(21)?,
            bit_depth: r.get(22)?,
            ..Default::default()
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Path → (mtime, size) for every known file: what the scanner diffs against.
pub fn stamps(conn: &Connection) -> Result<HashMap<PathBuf, (i64, i64)>> {
    let mut stmt = conn.prepare("SELECT path, mtime, size FROM tracks")?;
    let rows = stmt.query_map([], |r| Ok((PathBuf::from(r.get::<_, String>(0)?), (r.get(1)?, r.get(2)?))))?;
    Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

/// Insert or refresh a file's tags, keeping its play history.
pub fn upsert(tx: &Transaction, t: &Track) -> Result<()> {
    tx.execute(
        "INSERT INTO tracks (path, title, artist, album, album_artist, genre, year, track_no, disc_no, duration,
                             rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, art, mtime, size,
                             bitrate, sample_rate, bit_depth)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT(path) DO UPDATE SET
            title = excluded.title, artist = excluded.artist, album = excluded.album,
            album_artist = excluded.album_artist, genre = excluded.genre, year = excluded.year,
            track_no = excluded.track_no, disc_no = excluded.disc_no, duration = excluded.duration,
            rg_track_gain = excluded.rg_track_gain, rg_track_peak = excluded.rg_track_peak,
            rg_album_gain = excluded.rg_album_gain, rg_album_peak = excluded.rg_album_peak,
            art = excluded.art, mtime = excluded.mtime, size = excluded.size,
            bitrate = excluded.bitrate, sample_rate = excluded.sample_rate, bit_depth = excluded.bit_depth",
        params![
            path_text(&t.path),
            t.title,
            t.artist,
            t.album,
            t.album_artist,
            t.genre,
            t.year,
            t.track_no,
            t.disc_no,
            t.duration,
            t.rg_track_gain,
            t.rg_track_peak,
            t.rg_album_gain,
            t.rg_album_peak,
            t.art,
            t.mtime,
            t.size,
            t.bitrate,
            t.sample_rate,
            t.bit_depth
        ],
    )?;
    Ok(())
}

pub fn delete(tx: &Transaction, path: &Path) -> Result<()> {
    tx.execute("DELETE FROM tracks WHERE path = ?1", [path_text(path)])?;
    Ok(())
}

pub fn count_play(conn: &Connection, path: &Path, when: i64) -> Result<()> {
    conn.execute("UPDATE tracks SET plays = plays + 1, last_played = ?2 WHERE path = ?1", params![path_text(path), when])?;
    Ok(())
}

// ---------- Playlists ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
}

pub fn playlists(conn: &Connection) -> Result<Vec<Playlist>> {
    let mut stmt = conn.prepare("SELECT id, name FROM playlists ORDER BY name COLLATE NOCASE, id")?;
    let rows = stmt.query_map([], |r| Ok(Playlist { id: r.get(0)?, name: r.get(1)? }))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn playlist_paths(conn: &Connection, id: i64) -> Result<Vec<PathBuf>> {
    let mut stmt = conn.prepare("SELECT path FROM playlist_items WHERE playlist_id = ?1 ORDER BY pos")?;
    let rows = stmt.query_map([id], |r| Ok(PathBuf::from(r.get::<_, String>(0)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn create_playlist(conn: &Connection, name: &str, items: &[PathBuf]) -> Result<i64> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    conn.execute("INSERT INTO playlists (name, created) VALUES (?1, ?2)", params![name, now])?;
    let id = conn.last_insert_rowid();
    set_playlist(conn, id, items)?;
    Ok(id)
}

pub fn rename_playlist(conn: &Connection, id: i64, name: &str) -> Result<()> {
    conn.execute("UPDATE playlists SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

pub fn delete_playlist(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
    Ok(())
}

/// Replace a playlist's contents.
pub fn set_playlist(conn: &Connection, id: i64, items: &[PathBuf]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
    {
        let mut stmt = tx.prepare("INSERT INTO playlist_items (playlist_id, pos, path) VALUES (?1, ?2, ?3)")?;
        for (i, p) in items.iter().enumerate() {
            stmt.execute(params![id, i as i64, path_text(p)])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn append_playlist(conn: &Connection, id: i64, items: &[PathBuf]) -> Result<()> {
    let mut all = playlist_paths(conn, id)?;
    all.extend(items.iter().cloned());
    set_playlist(conn, id, &all)
}

// ---------- Radio and podcasts ----------

/// What's kept for a station or episode beyond its `Track`: listening progress.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    pub position: f64,
    pub played: bool,
}

/// An episode's extra details (for the podcast view).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EpisodeInfo {
    pub guid: String,
    pub published: i64,
    pub description: String,
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn kind_id(k: Kind) -> &'static str {
    match k {
        Kind::Station => "station",
        _ => "episode",
    }
}

/// Every remembered station and episode, with progress and episode details.
pub fn load_streams(conn: &Connection) -> Result<Vec<(Track, Progress, EpisodeInfo)>> {
    let mut stmt = conn.prepare(
        "SELECT url, kind, title, artist, album, genre, art, feed, duration, position, played, file, guid, published, description
         FROM streams",
    )?;
    let rows = stmt.query_map([], |r| {
        let file: String = r.get(11)?;
        Ok((
            Track {
                kind: if r.get::<_, String>(1)? == "station" { Kind::Station } else { Kind::Episode },
                path: PathBuf::from(r.get::<_, String>(0)?),
                title: r.get(2)?,
                artist: r.get(3)?,
                album: r.get(4)?,
                genre: r.get(5)?,
                art: r.get(6)?,
                feed: r.get(7)?,
                duration: r.get(8)?,
                download: (!file.is_empty()).then(|| PathBuf::from(file)),
                ..Default::default()
            },
            Progress { position: r.get(9)?, played: r.get::<_, i64>(10)? != 0 },
            EpisodeInfo { guid: r.get(12)?, published: r.get(13)?, description: r.get(14)? },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Remember a station or episode's details, keeping its progress and download.
pub fn upsert_stream(conn: &Connection, t: &Track, info: &EpisodeInfo) -> Result<()> {
    conn.execute(
        "INSERT INTO streams (url, kind, title, artist, album, genre, art, feed, guid, duration, published, description, added)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(url) DO UPDATE SET
            kind = excluded.kind, title = excluded.title, artist = excluded.artist, album = excluded.album,
            genre = excluded.genre, art = excluded.art, feed = excluded.feed, guid = excluded.guid,
            duration = CASE WHEN excluded.duration > 0 THEN excluded.duration ELSE streams.duration END,
            published = excluded.published, description = excluded.description",
        params![
            path_text(&t.path),
            kind_id(t.kind),
            t.title,
            t.artist,
            t.album,
            t.genre,
            t.art,
            t.feed,
            info.guid,
            t.duration,
            info.published,
            info.description,
            now()
        ],
    )?;
    Ok(())
}

pub fn set_progress(conn: &Connection, url: &Path, p: Progress) -> Result<()> {
    conn.execute(
        "UPDATE streams SET position = ?2, played = ?3 WHERE url = ?1",
        params![path_text(url), p.position, p.played as i64],
    )?;
    Ok(())
}

pub fn set_download(conn: &Connection, url: &Path, file: Option<&Path>) -> Result<()> {
    conn.execute(
        "UPDATE streams SET file = ?2 WHERE url = ?1",
        params![path_text(url), file.map(path_text).unwrap_or_default()],
    )?;
    Ok(())
}

/// Forget episodes of a podcast that nothing refers to any more.
pub fn prune_feed(conn: &Connection, feed: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM streams WHERE feed = ?1 AND file = '' AND position = 0 AND played = 0
           AND url NOT IN (SELECT path FROM playlist_items)",
        [feed],
    )?;
    Ok(())
}

pub fn stations(conn: &Connection) -> Result<Vec<Station>> {
    let mut stmt = conn.prepare(
        "SELECT url, uuid, name, homepage, favicon, tags, country, countrycode, codec, bitrate, custom
         FROM stations ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Station {
            url: r.get(0)?,
            uuid: r.get(1)?,
            name: r.get(2)?,
            homepage: r.get(3)?,
            favicon: r.get(4)?,
            tags: r.get(5)?,
            country: r.get(6)?,
            countrycode: r.get(7)?,
            codec: r.get(8)?,
            bitrate: r.get(9)?,
            custom: r.get::<_, i64>(10)? != 0,
            url_resolved: String::new(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Save a favourite or custom station (keyed by its stream).
pub fn save_station(conn: &Connection, s: &Station) -> Result<()> {
    conn.execute(
        "INSERT INTO stations (url, uuid, name, homepage, favicon, tags, country, countrycode, codec, bitrate, custom, added)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(url) DO UPDATE SET
            uuid = excluded.uuid, name = excluded.name, homepage = excluded.homepage, favicon = excluded.favicon,
            tags = excluded.tags, country = excluded.country, countrycode = excluded.countrycode,
            codec = excluded.codec, bitrate = excluded.bitrate, custom = excluded.custom",
        params![
            s.stream(),
            s.uuid,
            s.name,
            s.homepage,
            s.favicon,
            s.tags,
            s.country,
            s.countrycode,
            s.codec,
            s.bitrate,
            s.custom as i64,
            now()
        ],
    )?;
    Ok(())
}

pub fn delete_station(conn: &Connection, url: &str) -> Result<()> {
    conn.execute("DELETE FROM stations WHERE url = ?1", [url])?;
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Podcast {
    pub feed: String,
    pub apple_id: String,
    pub title: String,
    pub author: String,
    pub art_url: String,
    pub description: String,
    pub added: i64,
    pub refreshed: i64,
}

pub fn podcasts(conn: &Connection) -> Result<Vec<Podcast>> {
    let mut stmt = conn.prepare(
        "SELECT feed, apple_id, title, author, art_url, description, added, refreshed FROM podcasts ORDER BY title COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Podcast {
            feed: r.get(0)?,
            apple_id: r.get(1)?,
            title: r.get(2)?,
            author: r.get(3)?,
            art_url: r.get(4)?,
            description: r.get(5)?,
            added: r.get(6)?,
            refreshed: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn save_podcast(conn: &Connection, p: &Podcast) -> Result<()> {
    conn.execute(
        "INSERT INTO podcasts (feed, apple_id, title, author, art_url, description, added, refreshed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(feed) DO UPDATE SET
            apple_id = CASE WHEN excluded.apple_id = '' THEN podcasts.apple_id ELSE excluded.apple_id END,
            title = excluded.title, author = excluded.author, art_url = excluded.art_url,
            description = excluded.description, refreshed = excluded.refreshed",
        params![p.feed, p.apple_id, p.title, p.author, p.art_url, p.description, p.added, p.refreshed],
    )?;
    Ok(())
}

pub fn delete_podcast(conn: &Connection, feed: &str) -> Result<()> {
    conn.execute("DELETE FROM podcasts WHERE feed = ?1", [feed])?;
    prune_feed(conn, feed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (Connection, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nyx-music-test-{}-{}", std::process::id(), rand_suffix()));
        let path = dir.join("t.db");
        (open_at(&path).unwrap(), dir)
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn upsert_keeps_play_counts() {
        let (mut conn, dir) = temp_db();
        let t = Track { path: "/m/a.flac".into(), title: "A".into(), mtime: 1, size: 2, ..Default::default() };
        let tx = conn.transaction().unwrap();
        upsert(&tx, &t).unwrap();
        tx.commit().unwrap();
        count_play(&conn, &t.path, 99).unwrap();
        let tx = conn.transaction().unwrap();
        upsert(&tx, &Track { title: "A2".into(), mtime: 5, ..t.clone() }).unwrap();
        tx.commit().unwrap();
        let all = load_all(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "A2");
        assert_eq!(all[0].plays.get(), 1);
        assert_eq!(all[0].last_played.get(), 99);
        assert_eq!(stamps(&conn).unwrap()[&t.path], (5, 2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn playlists_round_trip() {
        let (conn, dir) = temp_db();
        let id = create_playlist(&conn, "Road trip", &["/a.mp3".into(), "/b.mp3".into()]).unwrap();
        append_playlist(&conn, id, &["/c.mp3".into()]).unwrap();
        assert_eq!(playlist_paths(&conn, id).unwrap(), vec![PathBuf::from("/a.mp3"), "/b.mp3".into(), "/c.mp3".into()]);
        rename_playlist(&conn, id, "Drive").unwrap();
        assert_eq!(playlists(&conn).unwrap(), vec![Playlist { id, name: "Drive".into() }]);
        delete_playlist(&conn, id).unwrap();
        assert!(playlists(&conn).unwrap().is_empty());
        assert!(playlist_paths(&conn, id).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn streams_keep_progress_and_downloads() {
        let (conn, dir) = temp_db();
        let ep = Track {
            kind: Kind::Episode,
            path: "https://cdn.example.com/1.mp3".into(),
            title: "One".into(),
            album: "Show".into(),
            feed: "https://feed".into(),
            duration: 60.0,
            ..Default::default()
        };
        let info = EpisodeInfo { guid: "g1".into(), published: 5, description: "Notes".into() };
        upsert_stream(&conn, &ep, &info).unwrap();
        set_progress(&conn, &ep.path, Progress { position: 12.5, played: false }).unwrap();
        set_download(&conn, &ep.path, Some(Path::new("/d/1.mp3"))).unwrap();
        // A feed refresh without a duration keeps the old one, and the progress.
        upsert_stream(&conn, &Track { title: "One!".into(), duration: 0.0, ..ep.clone() }, &info).unwrap();
        let all = load_streams(&conn).unwrap();
        assert_eq!(all.len(), 1);
        let (t, p, i) = &all[0];
        assert_eq!((t.kind, t.title.as_str(), t.duration), (Kind::Episode, "One!", 60.0));
        assert_eq!(t.download.as_deref(), Some(Path::new("/d/1.mp3")));
        assert_eq!(*p, Progress { position: 12.5, played: false });
        assert_eq!(i, &info);
        // Pruning keeps episodes with progress or downloads.
        prune_feed(&conn, "https://feed").unwrap();
        assert_eq!(load_streams(&conn).unwrap().len(), 1);
        set_download(&conn, &ep.path, None).unwrap();
        set_progress(&conn, &ep.path, Progress::default()).unwrap();
        prune_feed(&conn, "https://feed").unwrap();
        assert!(load_streams(&conn).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stations_and_podcasts_round_trip() {
        let (conn, dir) = temp_db();
        let s = Station {
            name: "Jazz FM".into(),
            url: "https://j/live".into(),
            tags: "jazz".into(),
            custom: true,
            ..Default::default()
        };
        save_station(&conn, &s).unwrap();
        assert_eq!(stations(&conn).unwrap(), vec![s.clone()]);
        delete_station(&conn, "https://j/live").unwrap();
        assert!(stations(&conn).unwrap().is_empty());
        let p = Podcast { feed: "https://f".into(), apple_id: "1".into(), title: "Show".into(), added: 3, ..Default::default() };
        save_podcast(&conn, &p).unwrap();
        save_podcast(&conn, &Podcast { apple_id: String::new(), refreshed: 9, ..p.clone() }).unwrap();
        assert_eq!(podcasts(&conn).unwrap(), vec![Podcast { refreshed: 9, ..p }]);
        delete_podcast(&conn, "https://f").unwrap();
        assert!(podcasts(&conn).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
