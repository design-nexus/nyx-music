use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub connection: String,
    pub server: String,
    pub user: String,
    pub username: String,
    pub connected: bool,
    pub libraries: Vec<String>,
    pub library_selection_set: bool,
    pub podcasts: bool,
    pub sync: bool,
    pub speed: f64,
    pub backward: f64,
    pub forward: f64,
    pub sleep_minutes: u32,
    pub last_sync: i64,
}
impl Config {
    pub fn fresh() -> Self {
        Self { sync: true, speed: 1.0, backward: 15.0, forward: 30.0, ..Self::default() }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct MediaId {
    pub connection: String,
    pub user: String,
    pub item: String,
    pub episode: Option<String>,
}
impl MediaId {
    pub fn key(&self) -> String {
        format!("abs://{}/{}/{}/{}", self.connection, self.user, self.item, self.episode.as_deref().unwrap_or("book"))
    }
    pub fn progress_path(&self) -> String {
        format!(
            "api/me/progress/{}{}",
            super::api::encode(&self.item),
            self.episode.as_ref().map(|e| format!("/{}", super::api::encode(e))).unwrap_or_default()
        )
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Library {
    pub id: String,
    pub name: String,
    pub media_type: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Book {
    pub id: MediaId,
    pub library: String,
    pub title: String,
    pub author: String,
    pub narrator: String,
    pub series: String,
    pub genres: Vec<String>,
    pub description: String,
    pub details: Vec<(String, String)>,
    pub duration: f64,
    pub added: i64,
    pub podcast: bool,
    pub feed: String,
    pub art: String,
    pub chapters: Vec<Chapter>,
    pub files: Vec<AudioFile>,
    pub episodes: Vec<Book>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Chapter {
    pub title: String,
    pub start: f64,
    pub end: f64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioFile {
    pub ino: String,
    pub filename: String,
    pub offset: f64,
    pub duration: f64,
}
fn string(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_owned()
}
fn names(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_owned();
    }
    v.as_array()
        .map(|a| a.iter().filter_map(|n| n.as_str().or_else(|| n["name"].as_str())).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}
impl Book {
    pub fn parse(v: &Value, config: &Config) -> Self {
        let m = &v["media"];
        let meta = &m["metadata"];
        let id =
            MediaId { connection: config.connection.clone(), user: config.user.clone(), item: string(v, "id"), episode: None };
        let series = if let Some(a) = meta["series"].as_array() {
            a.iter()
                .map(|s| {
                    format!("{}{}", string(s, "name"), s["sequence"].as_str().map(|n| format!(" · {n}")).unwrap_or_default())
                })
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            names(&meta["series"])
        };
        let mut book = Self {
            art: format!("abs-{}", crate::library::hash(&id.key())),
            id,
            library: string(v, "libraryId"),
            title: string(meta, "title"),
            author: if meta["authors"].is_array() { names(&meta["authors"]) } else { string(meta, "authorName") },
            narrator: names(&meta["narrators"]),
            series,
            genres: meta["genres"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
            description: string(meta, "description"),
            duration: m["duration"].as_f64().unwrap_or_default(),
            added: v["addedAt"].as_i64().unwrap_or_default(),
            podcast: v["mediaType"] == "podcast",
            feed: string(meta, "feedUrl"),
            chapters: serde_json::from_value(m["chapters"].clone()).unwrap_or_default(),
            ..Self::default()
        };
        for (key, label) in [
            ("subtitle", "Subtitle"),
            ("publishedYear", "Year"),
            ("publisher", "Publisher"),
            ("language", "Language"),
            ("isbn", "ISBN"),
            ("asin", "ASIN"),
        ] {
            if let Some(value) = meta[key].as_str().filter(|s| !s.is_empty()) {
                book.details.push((label.into(), value.into()));
            }
        }
        if book.series.is_empty() {
            book.series = string(meta, "seriesName");
        }
        if book.narrator.is_empty() {
            book.narrator = string(meta, "narratorName");
        }
        if book.author.is_empty() {
            book.author = string(meta, "author");
        }
        let mut files = m["audioFiles"].as_array().cloned().unwrap_or_default();
        files.sort_by_key(|f| f["index"].as_i64().unwrap_or_default());
        let mut offset = 0.0;
        for f in files {
            let duration = f["duration"].as_f64().unwrap_or_default();
            book.files.push(AudioFile { ino: string(&f, "ino"), filename: string(&f["metadata"], "filename"), offset, duration });
            offset += duration;
        }
        if book.duration == 0.0 {
            book.duration = offset;
        }
        if let Some(eps) = m["episodes"].as_array() {
            book.episodes = eps
                .iter()
                .map(|ep| {
                    let mut e = book.clone();
                    e.episodes.clear();
                    e.id.episode = Some(string(ep, "id"));
                    e.series = book.title.clone();
                    e.title = string(ep, "title");
                    e.description = string(ep, "description");
                    e.duration = ep["audioFile"]["duration"].as_f64().or_else(|| ep["duration"].as_f64()).unwrap_or_default();
                    e.chapters.clear();
                    e.files = vec![AudioFile {
                        ino: string(&ep["audioFile"], "ino"),
                        filename: string(&ep["audioFile"]["metadata"], "filename"),
                        offset: 0.0,
                        duration: e.duration,
                    }];
                    e
                })
                .collect();
        }
        book
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerProgress {
    pub current_time: f64,
    pub is_finished: bool,
    pub last_update: i64,
    pub progress: f64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub position: f64,
    pub finished: bool,
    pub updated: i64,
    pub dirty: bool,
    pub revision: u64,
    pub base: ServerProgress,
    pub conflict: Option<ServerProgress>,
    pub session: Option<Value>,
}
pub fn acknowledge(current: &mut Progress, revision: u64, remote: ServerProgress) {
    current.base = remote.clone();
    if current.revision == revision {
        current.dirty = false;
        current.session = None;
        current.conflict = None;
        current.finished = remote.is_finished;
    }
}
#[derive(Debug, PartialEq)]
pub enum Reconcile {
    Local,
    Server,
    Conflict,
}
pub fn reconcile(local: &Progress, remote: &ServerProgress) -> Reconcile {
    if !local.dirty {
        return Reconcile::Server;
    }
    if remote == &local.base || (remote.current_time - local.position).abs() < 1.0 && remote.is_finished == local.finished {
        Reconcile::Local
    } else {
        Reconcile::Conflict
    }
}
#[cfg(test)]
pub fn part_at(files: &[AudioFile], position: f64) -> usize {
    files.iter().rposition(|f| position >= f.offset).unwrap_or(0)
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Download {
    pub book: Book,
    pub state: String,
    pub done: u64,
    pub total: u64,
    pub error: String,
    pub files: Vec<String>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Bookmark {
    pub position: f64,
    pub title: String,
    pub pending: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changes_on_both_devices_require_a_choice_even_for_rewind() {
        let base = ServerProgress { current_time: 200.0, last_update: 10, ..Default::default() };
        let local = Progress { base: base.clone(), position: 300.0, dirty: true, ..Default::default() };
        assert_eq!(reconcile(&local, &base), Reconcile::Local);
        assert_eq!(reconcile(&local, &ServerProgress { current_time: 20.0, last_update: 11, ..base }), Reconcile::Conflict);
    }
    #[test]
    fn multipart_and_episode_identity() {
        let c = Config { connection: "server".into(), user: "user".into(), ..Config::fresh() };
        let b = Book::parse(
            &serde_json::json!({"id":"book", "libraryId":"lib", "media":{"metadata":{"title":"Book", "authors":[{"name":"Author"}]}, "audioFiles":[{"index":2,"ino":"b","duration":50},{"index":1,"ino":"a","duration":100}]}}),
            &c,
        );
        assert_eq!(b.duration, 150.0);
        assert_eq!(b.author, "Author");
        assert_eq!(part_at(&b.files, 100.0), 1);
        assert_eq!(part_at(&b.files, 99.0), 0);
        let mut ep = b.id.clone();
        ep.episode = Some("episode".into());
        assert_ne!(ep.key(), b.id.key());
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    #[test]
    fn acknowledgement_preserves_newer_checkpoint() {
        let mut p = Progress {
            position: 300.0,
            revision: 2,
            dirty: true,
            session: Some(serde_json::json!({"currentTime":300})),
            ..Default::default()
        };
        acknowledge(&mut p, 1, ServerProgress { current_time: 290.0, last_update: 100, ..Default::default() });
        assert!(p.dirty);
        assert_eq!(p.position, 300.0);
        assert!(p.session.is_some());
        assert_eq!(p.base.current_time, 290.0);
        acknowledge(&mut p, 2, ServerProgress { current_time: 300.0, last_update: 110, ..Default::default() });
        assert!(!p.dirty);
        assert!(p.session.is_none());
    }
    #[test]
    fn clean_local_uses_server_and_identity_is_account_scoped() {
        assert_eq!(
            reconcile(&Progress::default(), &ServerProgress { current_time: 30.0, ..Default::default() }),
            Reconcile::Server
        );
        let a = MediaId { connection: "server".into(), user: "alice".into(), item: "book".into(), episode: None };
        let mut b = a.clone();
        b.user = "bob".into();
        assert_ne!(a.key(), b.key());
    }
    #[test]
    fn podcast_episode_fixture_preserves_parent_and_file() {
        let b = Book::parse(
            &serde_json::json!({"id":"show","mediaType":"podcast","media":{"metadata":{"title":"Show","feedUrl":"https://example.test/feed"},"episodes":[{"id":"ep","title":"Episode","audioFile":{"ino":"file","duration":120,"metadata":{"filename":"ep.mp3"}}}]}}),
            &Config::fresh(),
        );
        assert!(b.episodes[0].podcast);
        assert_eq!(b.episodes[0].id.item, "show");
        assert_eq!(b.episodes[0].id.episode.as_deref(), Some("ep"));
        assert_eq!(b.episodes[0].files[0].ino, "file");
        assert_eq!(b.episodes[0].duration, 120.0);
    }
}

/// Library, collection, and search endpoints use different result envelopes in ABS 2.37.
pub fn result_items(v: &Value) -> &[Value] {
    ["results", "libraryItems", "books", "book", "podcast", "podcasts"]
        .iter()
        .find_map(|key| v[*key].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
}
#[cfg(test)]
mod response_tests {
    use super::*;
    #[test]
    fn supports_search_and_collection_envelopes() {
        for key in ["results", "book", "podcast", "books"] {
            let v = serde_json::json!({key:[{"libraryItem":{"id":"a","media":{"metadata":{"title":"A"}}}}]});
            assert_eq!(result_items(&v).len(), 1);
        }
    }
}
