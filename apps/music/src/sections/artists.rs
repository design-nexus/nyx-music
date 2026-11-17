//! Artists and Genres: a list; each opens into its albums.

use super::{library_stack, scan_banner};
use crate::library::store::{self, Bucket};
use crate::views::{self, AlbumGrid, BucketList};
use crate::widgets::{self, Page};
use crate::{fmt, window};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Artists,
    Genres,
}

impl Kind {
    fn buckets(self) -> Vec<Bucket> {
        match self {
            Kind::Artists => store::artists(),
            Kind::Genres => store::genres(),
        }
    }
}

struct Ui {
    inner: gtk::Stack,
    title: gtk::Label,
    meta: gtk::Label,
    grid: AlbumGrid,
    open: Rc<RefCell<String>>,
}

thread_local! {
    static ARTISTS: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
    static GENRES: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

fn ui(kind: Kind) -> Option<Rc<Ui>> {
    match kind {
        Kind::Artists => ARTISTS.with(|u| u.borrow().clone()),
        Kind::Genres => GENRES.with(|u| u.borrow().clone()),
    }
}

pub fn build_artists(page: &Page) {
    build(page, Kind::Artists);
}

pub fn build_genres(page: &Page) {
    build(page, Kind::Genres);
}

fn build(page: &Page, kind: Kind) {
    page.body.append(&super::music_chips(if kind == Kind::Artists { "artists" } else { "genres" }));
    page.body.append(&scan_banner());
    let inner = gtk::Stack::new();
    inner.set_transition_type(gtk::StackTransitionType::Crossfade);
    inner.set_transition_duration(if crate::prefs::get().reduce_motion { 0 } else { 160 });
    let icon = if kind == Kind::Artists { "avatar-default-symbolic" } else { "music-genre-symbolic" };
    let list = BucketList::new(icon, move |name| open(kind, &name));
    if kind == Kind::Artists {
        list.root.add_css_class("artists");
    }
    inner.add_named(&list.root, Some("list"));

    let detail = widgets::vbox(0);
    detail.set_vexpand(true);
    let back_label = if kind == Kind::Artists { "Artists" } else { "Genres" };
    detail.append(&views::back_button(back_label, move || {
        if let Some(u) = ui(kind) {
            u.inner.set_visible_child_name("list");
            u.open.borrow_mut().clear();
        }
    }));
    let meta = widgets::label("", "dim");
    meta.add_css_class("mono");
    meta.add_css_class("detail-meta");
    let open_name: Rc<RefCell<String>> = Rc::default();
    let n = open_name.clone();
    let actions = views::play_buttons(move || {
        let name = n.borrow().to_lowercase();
        kind.buckets()
            .into_iter()
            .find(|b| b.name.to_lowercase() == name)
            .map(|b| b.albums.iter().flat_map(|a| a.tracks.iter().map(|t| t.path.clone())).collect())
            .unwrap_or_default()
    });
    let (header, title) =
        views::detail_header(None, if kind == Kind::Artists { "Artist" } else { "Genre" }, "", meta.upcast_ref(), "", &actions);
    detail.append(&header);
    let grid = AlbumGrid::new(|a| super::albums::show(&a.key));
    detail.append(&grid.root);
    inner.add_named(&detail, Some("detail"));
    page.body.append(&library_stack(&inner));

    let ui = Rc::new(Ui { inner: inner.clone(), title, meta, grid, open: open_name });
    match kind {
        Kind::Artists => ARTISTS.with(|u| *u.borrow_mut() = Some(ui.clone())),
        Kind::Genres => GENRES.with(|u| *u.borrow_mut() = Some(ui.clone())),
    }
    list.set(kind.buckets());
    store::subscribe(&inner, move |c| {
        if c != store::Change::Library {
            return;
        }
        list.set(kind.buckets());
        let name = ui.open.borrow().clone();
        if !name.is_empty() {
            open(kind, &name);
        }
    });
}

fn open(kind: Kind, name: &str) {
    let Some(u) = ui(kind) else { return };
    let Some(b) = kind.buckets().into_iter().find(|b| b.name.eq_ignore_ascii_case(name)) else {
        u.inner.set_visible_child_name("list");
        u.open.borrow_mut().clear();
        return;
    };
    u.title.set_text(&b.name);
    let secs: f64 = b.albums.iter().map(|a| a.duration).sum();
    u.meta.set_text(&format!(
        "{} · {} · {}",
        fmt::count(b.albums.len(), "album", "albums"),
        fmt::count(b.tracks, "song", "songs"),
        fmt::total(secs)
    ));
    u.grid.set(&b.albums);
    *u.open.borrow_mut() = b.name.clone();
    u.inner.set_visible_child_name("detail");
}

/// Go to an artist from anywhere.
pub fn show(name: &str) {
    window::navigate("artists");
    open(Kind::Artists, name);
}
