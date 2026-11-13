//! Album covers. The scanner extracts each album's picture once (embedded art,
//! else a cover image in the folder), crops it square and saves two sizes in the
//! cache. The UI loads those small JPEGs off the main thread.

use super::{Track, hash, tags};
use crate::paths;
use gtk::gdk_pixbuf::{InterpType, Pixbuf, PixbufLoader};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SMALL: i32 = 320;
const LARGE: i32 = 720;
const FOLDER_NAMES: &[&str] = &["cover", "folder", "front", "album", "albumart", "albumartsmall"];
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// The cache key for a track's album.
pub fn key_for(t: &Track) -> String {
    hash(&t.album_key())
}

pub fn file(key: &str, small: bool) -> PathBuf {
    paths::covers_dir().join(if small { format!("{key}-s.jpg") } else { format!("{key}.jpg") })
}

/// Make sure the album of `t` has a cached cover. Returns the key, or "" when
/// there's no picture to be found. Runs on the scanner thread.
pub fn ensure(t: &Track) -> String {
    let key = key_for(t);
    if file(&key, true).exists() {
        return key;
    }
    let bytes = tags::embedded_picture(&t.path).or_else(|| t.path.parent().and_then(folder_image));
    match bytes.and_then(|b| save(&key, &b)) {
        Some(()) => key,
        None => String::new(),
    }
}

/// A conventionally named image beside the music, or the only image there is.
fn folder_image(dir: &Path) -> Option<Vec<u8>> {
    let images: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str())))
        .collect();
    let named = images.iter().find(|p| {
        p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| FOLDER_NAMES.contains(&s.to_ascii_lowercase().as_str()))
    });
    let pick = named.or(if images.len() == 1 { images.first() } else { None })?;
    std::fs::read(pick).ok()
}

fn decode(bytes: &[u8]) -> Option<Pixbuf> {
    let loader = PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    loader.pixbuf()
}

/// Centre-crop to a square and scale.
fn square(src: &Pixbuf, size: i32) -> Option<Pixbuf> {
    let (w, h) = (src.width(), src.height());
    let side = w.min(h);
    if side <= 0 {
        return None;
    }
    let cropped = src.new_subpixbuf((w - side) / 2, (h - side) / 2, side, side);
    let size = size.min(side);
    cropped.scale_simple(size, size, InterpType::Bilinear)
}

/// Decode, square and cache a picture under `key` (any thread).
pub fn save(key: &str, bytes: &[u8]) -> Option<()> {
    let src = decode(bytes)?;
    std::fs::create_dir_all(paths::covers_dir()).ok()?;
    for (small, size) in [(false, LARGE), (true, SMALL)] {
        let img = square(&src, size)?;
        // Flatten any transparency; JPEG has none.
        let img = if img.has_alpha() {
            img.composite_color_simple(img.width(), img.height(), InterpType::Nearest, 255, 8, 0x202020, 0x202020)?
        } else {
            img
        };
        let out = file(key, small);
        let tmp = out.with_extension("tmp");
        img.savev(&tmp, "jpeg", &[("quality", "90")]).ok()?;
        std::fs::rename(&tmp, &out).ok()?;
    }
    Some(())
}

// ---------- UI side ----------

/// Who's waiting for a picture to download.
type Waiters = Vec<Box<dyn FnOnce(bool)>>;

thread_local! {
    static TEXTURES: RefCell<HashMap<(String, bool), gdk::Texture>> = RefCell::new(HashMap::new());
    /// Pictures being downloaded (or that failed this run), so each is tried once.
    static FETCHING: RefCell<HashMap<String, Waiters>> = RefCell::new(HashMap::new());
    static FAILED: RefCell<std::collections::HashSet<String>> = RefCell::new(Default::default());
}

/// The picture's most colourful tone, as 0..1 RGB, from its small copy:
/// pixels count by how saturated and bright they are, so a cover's grey
/// background doesn't wash its colour out.
pub fn tone(key: &str) -> Option<(f64, f64, f64)> {
    let pb = Pixbuf::from_file_at_scale(file(key, true), 24, 24, false).ok()?;
    let (n, stride) = (pb.n_channels() as usize, pb.rowstride() as usize);
    let bytes = pb.read_pixel_bytes();
    let (mut r, mut g, mut b, mut total) = (0.0, 0.0, 0.0, 0.0);
    for y in 0..pb.height() as usize {
        for x in 0..pb.width() as usize {
            let i = y * stride + x * n;
            let (pr, pg, pbl) = (bytes[i] as f64 / 255.0, bytes[i + 1] as f64 / 255.0, bytes[i + 2] as f64 / 255.0);
            let (max, min) = (pr.max(pg).max(pbl), pr.min(pg).min(pbl));
            let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
            let w = sat * sat * max;
            r += pr * w;
            g += pg * w;
            b += pbl * w;
            total += w;
        }
    }
    if total < 2.0 {
        return None;
    }
    let (r, g, b) = (r / total, g / total, b / total);
    // Bright enough to read on a dark page.
    let lift = 0.75 / r.max(g).max(b).max(0.01);
    Some(((r * lift).min(1.0), (g * lift).min(1.0), (b * lift).min(1.0)))
}

/// The cache key for a picture on the web (station logos, podcast covers).
pub fn key_for_url(url: &str) -> String {
    if url.is_empty() { String::new() } else { hash(url) }
}

/// Download a picture into the cover cache once; `done(true)` when it's there.
pub fn fetch(url: &str, done: impl FnOnce(bool) + 'static) {
    let key = key_for_url(url);
    if key.is_empty() || FAILED.with(|f| f.borrow().contains(url)) {
        done(false);
        return;
    }
    if file(&key, true).exists() {
        done(true);
        return;
    }
    let first = FETCHING.with(|f| {
        let mut f = f.borrow_mut();
        let waiting = f.entry(url.to_string()).or_default();
        waiting.push(Box::new(done));
        waiting.len() == 1
    });
    if !first {
        return;
    }
    let u = url.to_string();
    crate::cmd::background(
        move || {
            let bytes = crate::online::http::get_bytes(&u, 8 * 1024 * 1024).ok()?;
            save(&key, &bytes)
        },
        {
            let url = url.to_string();
            move |ok: Option<()>| {
                let ok = ok.is_some();
                if !ok {
                    FAILED.with(|f| f.borrow_mut().insert(url.clone()));
                }
                let waiting = FETCHING.with(|f| f.borrow_mut().remove(&url)).unwrap_or_default();
                for w in waiting {
                    w(ok);
                }
            }
        },
    );
}

/// Load a cover texture off the main thread and hand it to `done`
/// (immediately when it's cached). `done` gets `None` when there's no cover.
pub fn load(key: &str, small: bool, done: impl FnOnce(Option<gdk::Texture>) + 'static) {
    if key.is_empty() {
        done(None);
        return;
    }
    let id = (key.to_string(), small);
    if let Some(t) = TEXTURES.with(|c| c.borrow().get(&id).cloned()) {
        done(Some(t));
        return;
    }
    let path = file(key, small);
    let handle = gio::spawn_blocking(move || std::fs::read(path).ok());
    glib::spawn_future_local(async move {
        let bytes = handle.await.ok().flatten();
        let tex = bytes.and_then(|b| gdk::Texture::from_bytes(&glib::Bytes::from_owned(b)).ok());
        if let Some(t) = &tex {
            TEXTURES.with(|c| {
                let mut c = c.borrow_mut();
                // Keep memory bounded on huge libraries; covers reload quickly.
                if c.len() > 600 {
                    c.clear();
                }
                c.insert(id, t.clone());
            });
        }
        done(tex);
    });
}

/// Forget cached textures (after a rescan replaced pictures).
pub fn forget() {
    TEXTURES.with(|c| c.borrow_mut().clear());
}

/// A square cover with a placeholder glyph when there's no picture.
#[derive(Clone)]
pub struct Cover {
    pub root: gtk::Overlay,
    picture: gtk::Picture,
    placeholder: gtk::Image,
    key: std::rc::Rc<RefCell<String>>,
    small: bool,
}

impl Cover {
    pub fn new(size: i32, small: bool) -> Cover {
        let root = gtk::Overlay::new();
        root.add_css_class("cover");
        root.set_overflow(gtk::Overflow::Hidden);
        root.set_size_request(size, size);
        root.set_halign(gtk::Align::Start);
        root.set_valign(gtk::Align::Start);
        let placeholder = gtk::Image::from_icon_name("media-optical-symbolic");
        placeholder.add_css_class("cover-placeholder");
        placeholder.set_pixel_size((size / 3).clamp(16, 96));
        placeholder.set_size_request(size, size);
        root.set_child(Some(&placeholder));
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_can_shrink(true);
        picture.set_size_request(size, size);
        picture.set_visible(false);
        root.add_overlay(&picture);
        Cover { root, picture, placeholder, key: Default::default(), small }
    }

    /// A cover as wide as its column and as tall as it is wide; unlike [`Cover::new`],
    /// it can shrink with the window.
    pub fn fluid(small: bool) -> Cover {
        let root = gtk::Overlay::new();
        root.add_css_class("cover");
        root.set_overflow(gtk::Overflow::Hidden);
        let square = gtk::DrawingArea::new();
        square.set_hexpand(true);
        // A resize asked for while GTK is allocating is lost: ask after it.
        square.connect_resize(|area, w, _| {
            if area.content_height() != w {
                let area = area.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(area) = area.upgrade() {
                        area.set_content_height(area.width().max(1));
                    }
                });
            }
        });
        root.set_child(Some(&square));
        let placeholder = gtk::Image::from_icon_name("media-optical-symbolic");
        placeholder.add_css_class("cover-placeholder");
        placeholder.set_pixel_size(32);
        root.add_overlay(&placeholder);
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_can_shrink(true);
        picture.set_visible(false);
        root.add_overlay(&picture);
        Cover { root, picture, placeholder, key: Default::default(), small }
    }

    /// Resize (Now playing scales its cover with the window).
    pub fn set_size(&self, size: i32) {
        self.root.set_size_request(size, size);
        self.picture.set_size_request(size, size);
        self.placeholder.set_size_request(size, size);
        self.placeholder.set_pixel_size((size / 3).clamp(16, 96));
    }

    /// The glyph shown when there's no picture.
    pub fn set_placeholder(&self, icon: &str) {
        self.placeholder.set_icon_name(Some(icon));
    }

    /// Show a picture from the web, downloading it into the cache first.
    pub fn set_url(&self, url: &str) {
        let key = key_for_url(url);
        if key.is_empty() || file(&key, true).exists() {
            self.set_key(&key);
            return;
        }
        self.set_key(&key);
        let me = self.clone();
        fetch(url, move |ok| {
            if ok && *me.key.borrow() == key {
                me.set_key(&key);
            }
        });
    }

    pub fn set_key(&self, key: &str) {
        if *self.key.borrow() == key && (self.picture.is_visible() || key.is_empty()) {
            return;
        }
        *self.key.borrow_mut() = key.to_string();
        self.picture.set_visible(false);
        self.placeholder.set_visible(true);
        let me = self.clone();
        let want = key.to_string();
        load(key, self.small, move |tex| {
            // The widget may have been re-bound to another album meanwhile.
            if *me.key.borrow() != want {
                return;
            }
            if let Some(t) = tex {
                me.picture.set_paintable(Some(&t));
                me.picture.set_visible(true);
                me.placeholder.set_visible(false);
            }
        });
    }
}
