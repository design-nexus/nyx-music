//! Shared library views: the album grid, artist/genre lists and the header
//! of a detail view.

use crate::library::art::Cover;
use crate::library::store::{Album, Bucket};
use crate::{fmt, player, widgets};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::PathBuf;
use std::rc::Rc;

fn boxed<T: 'static>(obj: &glib::Object) -> Option<std::cell::Ref<'_, T>> {
    obj.downcast_ref::<glib::BoxedAnyObject>().map(|b| b.borrow::<T>())
}

/// A virtualized grid of album cards.
#[derive(Clone)]
pub struct AlbumGrid {
    pub root: gtk::ScrolledWindow,
    store: gio::ListStore,
}

impl AlbumGrid {
    pub fn new(on_open: impl Fn(Rc<Album>) + 'static) -> AlbumGrid {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let card = widgets::vbox(6);
            card.add_css_class("album-card");
            let cover = Cover::new(168, true);
            card.append(&cover.root);
            // Play straight from the grid; shows on hover.
            let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
            play.add_css_class("card-play");
            play.set_tooltip_text(Some("Play"));
            play.set_halign(gtk::Align::End);
            play.set_valign(gtk::Align::End);
            play.set_focus_on_click(false);
            cover.root.add_overlay(&play);
            let w = item.downgrade();
            play.connect_clicked(move |_| {
                if let Some(a) = w.upgrade().and_then(|i| i.item()).and_then(|o| boxed::<Rc<Album>>(&o).map(|a| a.clone())) {
                    player::set_shuffle(false);
                    player::play_tracks(album_paths(&a), 0);
                }
            });
            let w = item.downgrade();
            on_context(&card, move |anchor, x, y| {
                if let Some(a) = w.upgrade().and_then(|i| i.item()).and_then(|o| boxed::<Rc<Album>>(&o).map(|a| a.clone())) {
                    album_menu(anchor, x, y, &a);
                }
            });
            let title = widgets::label("", "album-title");
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.set_max_width_chars(18);
            let artist = widgets::label("", "album-artist");
            artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
            artist.set_max_width_chars(18);
            card.append(&title);
            card.append(&artist);
            item.set_child(Some(&card));
            // Keep the parts with the item so bind can reach them.
            unsafe { item.set_data("parts", (cover, title, artist)) };
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let Some(obj) = item.item() else { return };
            let Some(album) = boxed::<Rc<Album>>(&obj) else { return };
            if let Some(parts) = unsafe { item.data::<(Cover, gtk::Label, gtk::Label)>("parts") } {
                let (cover, title, artist) = unsafe { parts.as_ref() };
                cover.set_key(&album.art);
                title.set_text(&album.title);
                title.set_tooltip_text(Some(&album.title));
                artist.set_text(&match album.year {
                    Some(y) => format!("{} · {y}", album.artist),
                    None => album.artist.clone(),
                });
            }
        });
        let model = gtk::NoSelection::new(Some(store.clone()));
        let grid = gtk::GridView::new(Some(model), Some(factory));
        grid.add_css_class("album-grid");
        grid.set_min_columns(2);
        grid.set_max_columns(10);
        grid.set_single_click_activate(true);
        let s = store.clone();
        grid.connect_activate(move |_, pos| {
            if let Some(obj) = s.item(pos)
                && let Some(a) = boxed::<Rc<Album>>(&obj)
            {
                let a = a.clone();
                on_open(a);
            }
        });
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&grid)
            .vexpand(true)
            .build();
        AlbumGrid { root, store }
    }

    pub fn set(&self, albums: &[Rc<Album>]) {
        let objs: Vec<glib::BoxedAnyObject> = albums.iter().map(|a| glib::BoxedAnyObject::new(a.clone())).collect();
        self.store.splice(0, self.store.n_items(), &objs);
    }
}

fn album_paths(a: &Album) -> Vec<PathBuf> {
    a.tracks.iter().map(|t| t.path.clone()).collect()
}

/// Right-click on an album: play it, queue it, or go to its artist or folder.
fn album_menu(anchor: &gtk::Widget, x: f64, y: f64, a: &Rc<Album>) {
    let paths = album_paths(a);
    let (p1, p2, p3) = (paths.clone(), paths.clone(), paths.clone());
    let artist = a.artist.clone();
    let dir = a.tracks.first().and_then(|t| t.path.parent()).map(|d| d.to_path_buf());
    let mut items: Vec<(String, Box<dyn Fn()>)> = vec![
        (
            "Play".into(),
            Box::new(move || {
                player::set_shuffle(false);
                player::play_tracks(p1.clone(), 0);
            }),
        ),
        (
            "Play next".into(),
            Box::new(move || {
                player::play_next(p2.clone());
                crate::window::toast(&crate::tracklist::added_text(p2.len(), "to play next"));
            }),
        ),
        (
            "Add to queue".into(),
            Box::new(move || {
                player::enqueue(p3.clone());
                crate::window::toast(&crate::tracklist::added_text(p3.len(), "to the queue"));
            }),
        ),
        ("-".into(), Box::new(|| {})),
        ("Show artist".into(), Box::new(move || crate::sections::artists::show(&artist))),
    ];
    if let Some(d) = dir {
        items.push((
            "Open folder".into(),
            Box::new(move || crate::cmd::spawn(&["xdg-open", &d.to_string_lossy()])),
        ));
    }
    popup_menu(anchor, x, y, items, Some(paths));
}

/// A list of artists or genres: cover, name, counts.
#[derive(Clone)]
pub struct BucketList {
    pub root: gtk::Box,
    store: gio::ListStore,
}

impl BucketList {
    /// `placeholder` is the icon shown where there's no picture.
    pub fn new(placeholder: &'static str, on_open: impl Fn(String) + 'static) -> BucketList {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let row = widgets::hbox(12);
            row.add_css_class("bucket-row");
            let cover = Cover::new(40, true);
            cover.set_placeholder(placeholder);
            row.append(&cover.root);
            let text = widgets::vbox(1);
            text.set_valign(gtk::Align::Center);
            text.set_hexpand(true);
            let name = widgets::label("", "bucket-name");
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let meta = widgets::label("", "dim");
            meta.add_css_class("bucket-meta");
            text.append(&name);
            text.append(&meta);
            row.append(&text);
            item.set_child(Some(&row));
            unsafe { item.set_data("parts", (cover, name, meta)) };
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let Some(obj) = item.item() else { return };
            let Some(b) = boxed::<Rc<Bucket>>(&obj) else { return };
            if let Some(parts) = unsafe { item.data::<(Cover, gtk::Label, gtk::Label)>("parts") } {
                let (cover, name, meta) = unsafe { parts.as_ref() };
                cover.set_key(&b.art);
                name.set_text(&b.name);
                meta.set_text(&format!(
                    "{} · {}",
                    fmt::count(b.albums.len(), "album", "albums"),
                    fmt::count(b.tracks, "song", "songs")
                ));
            }
        });
        let model = gtk::SingleSelection::new(Some(store.clone()));
        model.set_autoselect(false);
        model.set_can_unselect(true);
        let list = gtk::ListView::new(Some(model), Some(factory));
        list.add_css_class("bucket-list");
        list.set_single_click_activate(true);
        let s = store.clone();
        list.connect_activate(move |_, pos| {
            if let Some(obj) = s.item(pos)
                && let Some(b) = boxed::<Rc<Bucket>>(&obj)
            {
                let name = b.name.clone();
                on_open(name);
            }
        });
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&list)
            .vexpand(true)
            .build();
        let root = widgets::vbox(0);
        root.add_css_class("table-card");
        root.set_overflow(gtk::Overflow::Hidden);
        root.set_vexpand(true);
        root.append(&scroll);
        BucketList { root, store }
    }

    pub fn set(&self, buckets: Vec<Bucket>) {
        let objs: Vec<glib::BoxedAnyObject> = buckets.into_iter().map(|b| glib::BoxedAnyObject::new(Rc::new(b))).collect();
        self.store.splice(0, self.store.n_items(), &objs);
    }
}

/// A back button for detail views.
pub fn back_button(label: &str, on_back: impl Fn() + 'static) -> gtk::Button {
    let b = widgets::labeled_button("go-previous-symbolic", label);
    b.add_css_class("flat");
    b.add_css_class("back-button");
    b.set_halign(gtk::Align::Start);
    b.connect_clicked(move |_| on_back());
    b
}

/// Play · Shuffle · Add to queue, for a list of songs.
pub fn play_buttons(paths: impl Fn() -> Vec<PathBuf> + 'static) -> gtk::Box {
    let row = widgets::hbox(8);
    let paths = Rc::new(paths);
    let play = widgets::labeled_button("media-playback-start-symbolic", "Play");
    play.add_css_class("suggested-action");
    let p = paths.clone();
    play.connect_clicked(move |_| {
        player::set_shuffle(false);
        player::play_tracks(p(), 0);
    });
    let shuffle = widgets::labeled_button("media-playlist-shuffle-symbolic", "Shuffle");
    let p = paths.clone();
    shuffle.connect_clicked(move |_| player::shuffle_tracks(p()));
    let queue = widgets::labeled_button("list-add-symbolic", "Add to queue");
    let p = paths.clone();
    queue.connect_clicked(move |_| {
        let paths = p();
        let n = paths.len();
        player::enqueue(paths);
        crate::window::toast(&crate::tracklist::added_text(n, "to the queue"));
    });
    row.append(&play);
    row.append(&shuffle);
    row.append(&queue);
    row
}

/// A small search field that filters a song table as you type.
pub fn filter_entry(table: &crate::tracklist::TrackTable, placeholder: &str) -> gtk::SearchEntry {
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.add_css_class("settings-search");
    entry.add_css_class("page-search");
    // 22 characters when there's room; a narrow window can squeeze it.
    entry.set_width_chars(10);
    entry.set_max_width_chars(22);
    entry.set_valign(gtk::Align::Center);
    let t = table.clone();
    entry.connect_search_changed(move |e| t.set_filter(&e.text()));
    entry.connect_stop_search(|e| e.set_text(""));
    entry
}

/// The header of an album, artist, folder or playlist view.
/// Returns the header and its title label (to update in place).
pub fn detail_header(
    cover: Option<&Cover>,
    kicker: &str,
    title: &str,
    subtitle: &gtk::Widget,
    meta: &str,
    actions: &gtk::Box,
) -> (gtk::Box, gtk::Label) {
    let header = widgets::hbox(22);
    header.add_css_class("detail-header");
    if let Some(c) = cover {
        header.append(&c.root);
    }
    let text = widgets::vbox(4);
    text.set_valign(gtk::Align::End);
    text.set_hexpand(true);
    if !kicker.is_empty() {
        text.append(&widgets::label(&kicker.to_uppercase(), "group-title"));
    }
    let t = widgets::label(title, "detail-title");
    t.set_wrap(true);
    t.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    t.set_lines(2);
    t.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&t);
    text.append(subtitle);
    if !meta.is_empty() {
        let m = widgets::label(meta, "dim");
        m.add_css_class("mono");
        m.add_css_class("detail-meta");
        text.append(&m);
    }
    actions.set_margin_top(10);
    text.append(actions);
    header.append(&text);
    (header, t)
}

/// A clickable artist name.
pub fn artist_link(name: &str) -> gtk::Widget {
    let b = gtk::Button::with_label(name);
    b.add_css_class("flat");
    b.add_css_class("link-button");
    b.set_halign(gtk::Align::Start);
    let n = name.to_string();
    b.connect_clicked(move |_| crate::sections::artists::show(&n));
    b.upcast()
}

// ---------- Radio and podcasts ----------

/// A card in a [`CoverGrid`]: a picture from the web, a title and a line under it.
pub struct CoverItem {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub art_url: String,
    /// A highlighted note in place of the subtitle ("3 new").
    pub badge: String,
}

/// A virtualized grid of podcast covers.
#[derive(Clone)]
pub struct CoverGrid {
    pub root: gtk::ScrolledWindow,
    store: gio::ListStore,
}

impl CoverGrid {
    pub fn new(on_open: impl Fn(String) + 'static) -> CoverGrid {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let card = widgets::vbox(6);
            card.add_css_class("album-card");
            let cover = Cover::new(168, true);
            cover.set_placeholder("music-podcast-symbolic");
            card.append(&cover.root);
            let title = widgets::label("", "album-title");
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.set_max_width_chars(18);
            let sub = widgets::label("", "album-artist");
            sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
            sub.set_max_width_chars(18);
            card.append(&title);
            card.append(&sub);
            item.set_child(Some(&card));
            unsafe { item.set_data("parts", (cover, title, sub)) };
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let Some(obj) = item.item() else { return };
            let Some(c) = boxed::<CoverItem>(&obj) else { return };
            if let Some(parts) = unsafe { item.data::<(Cover, gtk::Label, gtk::Label)>("parts") } {
                let (cover, title, sub) = unsafe { parts.as_ref() };
                cover.set_url(&c.art_url);
                title.set_text(&c.title);
                title.set_tooltip_text(Some(&c.title));
                if c.badge.is_empty() {
                    sub.set_text(&c.subtitle);
                    sub.remove_css_class("accent-text");
                } else {
                    sub.set_text(&c.badge);
                    sub.add_css_class("accent-text");
                }
            }
        });
        let model = gtk::NoSelection::new(Some(store.clone()));
        let grid = gtk::GridView::new(Some(model), Some(factory));
        grid.add_css_class("album-grid");
        grid.set_min_columns(2);
        grid.set_max_columns(10);
        grid.set_single_click_activate(true);
        let s = store.clone();
        grid.connect_activate(move |_, pos| {
            if let Some(obj) = s.item(pos) {
                let id = boxed::<CoverItem>(&obj).map(|c| c.id.clone());
                if let Some(id) = id {
                    on_open(id);
                }
            }
        });
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&grid)
            .vexpand(true)
            .build();
        CoverGrid { root, store }
    }

    pub fn set(&self, items: Vec<CoverItem>) {
        let objs: Vec<glib::BoxedAnyObject> = items.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objs);
    }
}

/// A plain list of names with a count under each (genres, countries, categories).
#[derive(Clone)]
pub struct NameList {
    pub root: gtk::Box,
    store: gio::ListStore,
}

/// (id, name, detail)
pub type Name = (String, String, String);

impl NameList {
    pub fn new(icon: &'static str, on_open: impl Fn(String, String) + 'static) -> NameList {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let row = widgets::hbox(12);
            row.add_css_class("bucket-row");
            row.add_css_class("name-row");
            let i = gtk::Image::from_icon_name(icon);
            i.add_css_class("dim");
            row.append(&i);
            let name = widgets::label("", "bucket-name");
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.set_hexpand(true);
            let meta = widgets::label("", "dim");
            meta.add_css_class("mono");
            meta.add_css_class("bucket-meta");
            row.append(&name);
            row.append(&meta);
            item.set_child(Some(&row));
            unsafe { item.set_data("parts", (name, meta)) };
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let Some(obj) = item.item() else { return };
            let Some(n) = boxed::<Name>(&obj) else { return };
            if let Some(parts) = unsafe { item.data::<(gtk::Label, gtk::Label)>("parts") } {
                let (name, meta) = unsafe { parts.as_ref() };
                name.set_text(&n.1);
                meta.set_text(&n.2);
            }
        });
        let model = gtk::SingleSelection::new(Some(store.clone()));
        model.set_autoselect(false);
        model.set_can_unselect(true);
        let list = gtk::ListView::new(Some(model), Some(factory));
        list.add_css_class("bucket-list");
        list.set_single_click_activate(true);
        let s = store.clone();
        list.connect_activate(move |_, pos| {
            if let Some(obj) = s.item(pos) {
                let n = boxed::<Name>(&obj).map(|n| (n.0.clone(), n.1.clone()));
                if let Some((id, name)) = n {
                    on_open(id, name);
                }
            }
        });
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&list)
            .vexpand(true)
            .build();
        let root = widgets::vbox(0);
        root.add_css_class("table-card");
        root.set_overflow(gtk::Overflow::Hidden);
        root.set_vexpand(true);
        root.append(&scroll);
        NameList { root, store }
    }

    pub fn set(&self, names: Vec<Name>) {
        let objs: Vec<glib::BoxedAnyObject> = names.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objs);
    }
}

type Retry = Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>>;

/// Content fetched from the web: a spinner while it loads, a message (and
/// Try again) when it fails, and an empty state when there's nothing.
#[derive(Clone)]
pub struct Loader {
    pub root: gtk::Stack,
    error: gtk::Label,
    empty: gtk::Box,
    retry: Retry,
}

impl Loader {
    pub fn new(content: &impl IsA<gtk::Widget>) -> Loader {
        let root = gtk::Stack::new();
        root.set_vexpand(true);
        root.add_named(content, Some("content"));
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        spinner.set_size_request(32, 32);
        root.add_named(&spinner, Some("loading"));
        let retry: Retry = Rc::default();
        let r = retry.clone();
        let failed = widgets::empty_state(
            "network-offline-symbolic",
            "Couldn't load this",
            "",
            Some((
                "Try again",
                Box::new(move || {
                    let f = r.borrow().clone();
                    if let Some(f) = f {
                        f();
                    }
                }),
            )),
        );
        let error = widgets::label("", "dim");
        error.set_wrap(true);
        error.set_justify(gtk::Justification::Center);
        error.set_max_width_chars(60);
        error.set_halign(gtk::Align::Center);
        failed.insert_child_after(&error, failed.first_child().and_then(|c| c.next_sibling()).as_ref());
        root.add_named(&failed, Some("error"));
        let empty = widgets::vbox(0);
        empty.set_vexpand(true);
        root.add_named(&empty, Some("empty"));
        Loader { root, error, empty, retry }
    }

    pub fn loading(&self) {
        self.root.set_visible_child_name("loading");
    }

    pub fn failed(&self, err: &anyhow::Error, retry: impl Fn() + 'static) {
        self.error.set_text(&format!("{err:#}"));
        *self.retry.borrow_mut() = Some(Rc::new(retry));
        self.root.set_visible_child_name("error");
    }

    pub fn show(&self) {
        self.root.set_visible_child_name("content");
    }

    pub fn show_empty(&self, state: &gtk::Box) {
        while let Some(c) = self.empty.first_child() {
            self.empty.remove(&c);
        }
        self.empty.append(state);
        self.root.set_visible_child_name("empty");
    }
}

/// A right-click menu: labelled actions ("-" for a separator), and an
/// "Add to playlist" page when `paths` is given.
pub fn popup_menu(anchor: &gtk::Widget, x: f64, y: f64, items: Vec<(String, Box<dyn Fn()>)>, paths: Option<Vec<PathBuf>>) {
    let pop = gtk::Popover::new();
    pop.set_has_arrow(false);
    pop.set_parent(anchor);
    pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    pop.add_css_class("menu-popover");
    let pages = gtk::Stack::new();
    pages.set_vhomogeneous(false);
    pages.set_hhomogeneous(false);
    let main = widgets::vbox(1);
    let item = |label: &str, f: Box<dyn Fn()>| {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.add_css_class("menu-item");
        if let Some(l) = b.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }
        let pop = pop.clone();
        b.connect_clicked(move |_| {
            pop.popdown();
            f();
        });
        b
    };
    for (index, (label, f)) in items.into_iter().enumerate() {
        if label == "-" {
            main.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        } else {
            main.append(&item(&label, f));
        }
        // "Add to playlist" goes after Play, Play next and Add to queue.
        if index == 2
            && let Some(p) = &paths
        {
            let to_playlist = gtk::Button::new();
            to_playlist.add_css_class("flat");
            to_playlist.add_css_class("menu-item");
            let row = widgets::hbox(8);
            let l = widgets::label("Add to playlist", "");
            l.set_hexpand(true);
            row.append(&l);
            row.append(&gtk::Image::from_icon_name("pan-end-symbolic"));
            to_playlist.set_child(Some(&row));
            let pg = pages.clone();
            to_playlist.connect_clicked(move |_| pg.set_visible_child_name("playlists"));
            main.append(&to_playlist);
            let lists = widgets::vbox(1);
            let back = gtk::Button::new();
            back.add_css_class("flat");
            back.add_css_class("menu-item");
            let row = widgets::hbox(8);
            row.append(&gtk::Image::from_icon_name("pan-start-symbolic"));
            row.append(&widgets::label("Add to playlist", "menu-heading-inline"));
            back.set_child(Some(&row));
            let pg = pages.clone();
            back.connect_clicked(move |_| pg.set_visible_child_name("main"));
            lists.append(&back);
            lists.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let pp = p.clone();
            lists.append(&item("New playlist…", Box::new(move || crate::sections::playlist::new_dialog(pp.clone()))));
            let scroll_box = widgets::vbox(1);
            for pl in crate::library::store::playlists() {
                let pp = p.clone();
                scroll_box.append(&item(&pl.name, Box::new(move || crate::sections::playlist::append(pl.id, pp.clone()))));
            }
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::External)
                .propagate_natural_height(true)
                .max_content_height(320)
                .child(&scroll_box)
                .build();
            lists.append(&scroll);
            pages.add_named(&lists, Some("playlists"));
        }
    }
    pages.add_named(&main, Some("main"));
    pages.set_visible_child_name("main");
    pop.set_child(Some(&pages));
    let anchor_weak = anchor.downgrade();
    pop.connect_closed(move |p| {
        let p = p.clone();
        let a = anchor_weak.clone();
        glib::idle_add_local_once(move || {
            if a.upgrade().is_some() {
                p.unparent();
            }
        });
    });
    pop.popup();
}

/// Right-click (or long-press) on `w` calls `f` with the click position.
pub fn on_context(w: &impl IsA<gtk::Widget>, f: impl Fn(&gtk::Widget, f64, f64) + 'static) {
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    let weak = w.upcast_ref::<gtk::Widget>().downgrade();
    click.connect_pressed(move |_, _, x, y| {
        if let Some(w) = weak.upgrade() {
            f(&w, x, y);
        }
    });
    w.add_controller(click);
}
