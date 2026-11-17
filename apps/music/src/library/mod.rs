//! The music library: what's on disk, read once into SQLite, held in memory
//! on the UI thread for the views.

pub mod art;
pub mod db;
pub mod lyrics;
pub mod m3u;
pub mod scan;
pub mod store;
pub mod tags;

use std::path::PathBuf;

/// Audio file extensions the scanner picks up.
pub const EXTENSIONS: &[&str] =
    &["mp3", "flac", "ogg", "oga", "opus", "m4a", "mp4", "aac", "wav", "aif", "aiff", "ape", "wv", "mpc", "wma", "alac", "dsf"];

pub fn is_audio(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Where a track comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    File,
    /// Provider-backed spoken media with a stable identity, not a stream URL.
    Audiobook,
    /// An internet radio station: live, no length, can't seek.
    Station,
    /// A podcast episode (or any other audio on the web).
    Episode,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    pub kind: Kind,
    /// Row id in the database; 0 for files played from outside the library.
    pub id: i64,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub year: Option<i32>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    /// Seconds.
    pub duration: f64,
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
    /// Cover cache key (see `art`), empty when the album has no picture.
    pub art: String,
    /// Bumped live when a song is played through; see `store::count_play`.
    pub plays: std::cell::Cell<u32>,
    /// Unix time; set live like `plays`.
    pub last_played: std::cell::Cell<i64>,
    pub mtime: i64,
    pub size: i64,
    /// kbps; 0 when unknown.
    pub bitrate: u32,
    /// Hz; 0 when unknown.
    pub sample_rate: u32,
    /// Bits per sample for lossless files; 0 for lossy or unknown.
    pub bit_depth: u8,
    /// A downloaded copy of an episode.
    pub download: Option<PathBuf>,
    /// The podcast feed an episode belongs to.
    pub feed: String,
}

impl Track {
    /// The artist an album is filed under.
    pub fn album_artist_or_artist(&self) -> &str {
        if self.album_artist.is_empty() { &self.artist } else { &self.album_artist }
    }

    /// Groups tracks into albums: same album title and album artist.
    pub fn album_key(&self) -> String {
        format!("{}\u{1f}{}", self.album_artist_or_artist().to_lowercase(), self.album.to_lowercase())
    }

    /// What GStreamer plays: the file, an episode's download, or the stream address.
    pub fn file_uri(&self) -> String {
        let file = match (&self.kind, &self.download) {
            (Kind::File, _) => &self.path,
            (_, Some(d)) if d.exists() => d,
            _ => return self.path.to_string_lossy().into_owned(),
        };
        gtk::glib::filename_to_uri(file, None).map(|u| u.to_string()).unwrap_or_default()
    }

    pub fn is_remote(&self) -> bool {
        self.kind != Kind::File
    }

    pub fn is_live(&self) -> bool {
        self.kind == Kind::Station
    }

    /// Folded text (see `store::fold`) the library search matches against.
    pub fn haystack(&self) -> String {
        store::fold(&format!("{} {} {} {} {}", self.title, self.artist, self.album, self.album_artist, self.genre))
    }
}

/// FNV-1a, for stable cache file names.
pub fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}
