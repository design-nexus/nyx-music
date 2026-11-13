//! Podcasts: your shows across the top, the chosen one's episodes to stream,
//! download or queue below them, and Find a podcast for Apple's charts,
//! categories and search.

use crate::library::Track;
use crate::library::art::Cover;
use crate::online::podcast::{self, Feed, Show};
use crate::online::{self, Change};
use crate::views::{self, CoverGrid, CoverItem, Loader, NameList};
use crate::widgets::{self, Page};
use crate::{cmd, fmt, player, prefs, window};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

/// Episodes shown at first, and how many more each "Show more" adds.
const PAGE: usize = 100;

/// Find a podcast's tabs.
const TABS: &[(&str, &str)] = &[("top", "Top shows"), ("categories", "Categories"), ("search", "Search")];

struct Ui {
    /// "home" (your shows), "find" or "detail" (a show you don't follow).
    outer: gtk::Stack,
    /// The chosen show of yours, under the row of shows.
    chosen_box: gtk::Box,
    detail: gtk::Box,
    /// The feed the open view shows.
    open: String,
    /// The show chosen in the row.
    chosen: String,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn country() -> String {
    super::radio::country()
}

// ---------- Show grids ----------

/// A grid of shows from the directory; opening one shows its episodes.
fn show_grid() -> (CoverGrid, Rc<RefCell<Vec<Show>>>) {
    let shows: Rc<RefCell<Vec<Show>>> = Rc::default();
    let s = shows.clone();
    let grid = CoverGrid::new(move |id| {
        let show = id.parse::<usize>().ok().and_then(|i| s.borrow().get(i).cloned());
        if let Some(show) = show {
            open_show(show);
        }
    });
    (grid, shows)
}

fn set_shows(grid: &CoverGrid, store: &Rc<RefCell<Vec<Show>>>, shows: Vec<Show>) {
    grid.set(
        shows
            .iter()
            .enumerate()
            .map(|(i, s)| CoverItem {
                id: i.to_string(),
                title: s.title.clone(),
                subtitle: s.author.clone(),
                art_url: s.art_url.clone(),
                badge: if online::is_subscribed(&s.feed) && !s.feed.is_empty() { "Subscribed".into() } else { String::new() },
            })
            .collect(),
    );
    *store.borrow_mut() = shows;
}

fn load_shows(
    loader: &Loader,
    grid: &CoverGrid,
    store: &Rc<RefCell<Vec<Show>>>,
    work: std::sync::Arc<dyn Fn() -> anyhow::Result<Vec<Show>> + Send + Sync>,
) {
    loader.loading();
    let (l, g, s) = (loader.clone(), grid.clone(), store.clone());
    let w = work.clone();
    cmd::background(
        move || w(),
        move |res| match res {
            Ok(shows) if shows.is_empty() => {
                l.show_empty(&widgets::empty_state("music-podcast-symbolic", "No shows", "Nothing was found.", None))
            }
            Ok(shows) => {
                set_shows(&g, &s, shows);
                l.show();
            }
            Err(e) => {
                let (l2, g2, s2) = (l.clone(), g.clone(), s.clone());
                l.failed(&e, move || load_shows(&l2, &g2, &s2, work.clone()));
            }
        },
    );
}

fn categories_view() -> gtk::Widget {
    let inner = gtk::Stack::new();
    inner.set_transition_type(gtk::StackTransitionType::Crossfade);
    inner.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 160 });
    let (grid, shows) = show_grid();
    let loader = Loader::new(&grid.root);
    let detail = widgets::vbox(10);
    let title = widgets::label("", "detail-title");
    title.set_xalign(0.0);
    let i = inner.clone();
    detail.append(&views::back_button("Categories", move || i.set_visible_child_name("names")));
    detail.append(&title);
    detail.append(&loader.root);
    let (t, i) = (title.clone(), inner.clone());
    let names = NameList::new("music-podcast-symbolic", move |id, name| {
        t.set_text(&name);
        i.set_visible_child_name("detail");
        let genre = id.parse::<u32>().ok();
        let cc = country();
        load_shows(&loader, &grid, &shows, std::sync::Arc::new(move || podcast::top(&cc, genre)));
    });
    names.set(podcast::CATEGORIES.iter().map(|(id, name)| (id.to_string(), name.to_string(), String::new())).collect());
    inner.add_named(&names.root, Some("names"));
    inner.add_named(&detail, Some("detail"));
    inner.upcast()
}

fn search_view() -> (gtk::Widget, gtk::SearchEntry) {
    let b = widgets::vbox(10);
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Search podcasts"));
    entry.add_css_class("settings-search");
    entry.add_css_class("page-search");
    b.append(&entry);
    let (grid, shows) = show_grid();
    let loader = Loader::new(&grid.root);
    loader.show_empty(&widgets::empty_state("system-search-symbolic", "Find a podcast", "Search by its name or its host.", None));
    b.append(&loader.root);
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    entry.connect_search_changed(move |e| {
        if let Some(id) = pending.borrow_mut().take() {
            id.remove();
        }
        let q = e.text().trim().to_string();
        if q.chars().count() < 2 {
            return;
        }
        let (l, g, s, p) = (loader.clone(), grid.clone(), shows.clone(), pending.clone());
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            p.borrow_mut().take();
            let cc = country();
            load_shows(&l, &g, &s, std::sync::Arc::new(move || podcast::search(&cc, &q)));
        });
        *pending.borrow_mut() = Some(id);
    });
    (b.upcast(), entry)
}

// ---------- Episodes ----------

/// How much there is to hear: "Played", "31 min left" or its length.
fn episode_time(t: &Track) -> String {
    let pr = online::progress(&t.path);
    if pr.played {
        "Played".into()
    } else if pr.position > 0.0 && t.duration > 0.0 {
        format!("{} left", fmt::total((t.duration - pr.position).max(60.0)))
    } else if t.duration > 0.0 {
        fmt::total(t.duration)
    } else {
        String::new()
    }
}

#[derive(Clone)]
struct EpisodeRow {
    root: gtk::Box,
    date: gtk::Label,
    new: gtk::Label,
    title: gtk::Label,
    bar: gtk::ProgressBar,
    time: gtk::Label,
    downloaded: gtk::Box,
    downloading: gtk::Label,
    download: gtk::Button,
    play: gtk::Button,
    track: Rc<RefCell<Option<Rc<Track>>>>,
}

impl EpisodeRow {
    fn new() -> EpisodeRow {
        let root = widgets::hbox(6);
        root.add_css_class("episode-row");
        let text = widgets::vbox(2);
        text.set_valign(gtk::Align::Center);
        text.set_hexpand(true);
        let top = widgets::hbox(8);
        let date = widgets::label("", "episode-date");
        let new = widgets::label("New", "episode-new");
        top.append(&date);
        top.append(&new);
        text.append(&top);
        let title = widgets::label("", "episode-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&title);
        let meta = widgets::hbox(8);
        meta.add_css_class("episode-meta");
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("episode-progress");
        bar.set_valign(gtk::Align::Center);
        bar.set_size_request(80, -1);
        let time = widgets::label("", "");
        let downloaded = super::home::downloaded_mark();
        let downloading = widgets::label("", "");
        meta.append(&bar);
        meta.append(&time);
        meta.append(&downloaded);
        meta.append(&downloading);
        text.append(&meta);
        root.append(&text);
        let download = widgets::icon_button("folder-download-symbolic", "Download to play offline");
        download.set_valign(gtk::Align::Center);
        download.set_focus_on_click(false);
        root.append(&download);
        let more = widgets::icon_button("view-more-horizontal-symbolic", "Play next, add to queue, mark as played");
        more.set_valign(gtk::Align::Center);
        more.set_focus_on_click(false);
        root.append(&more);
        let play = widgets::icon_button("media-playback-start-symbolic", "Play");
        play.add_css_class("episode-play");
        play.set_valign(gtk::Align::Center);
        play.set_focus_on_click(false);
        root.append(&play);
        let track: Rc<RefCell<Option<Rc<Track>>>> = Rc::default();
        let t = track.clone();
        play.connect_clicked(move |_| {
            if let Some(t) = t.borrow().clone() {
                if player::current().is_some_and(|c| c.path == t.path) {
                    player::toggle();
                } else {
                    play_episode(&t);
                }
            }
        });
        let t = track.clone();
        download.connect_clicked(move |_| {
            let Some(t) = t.borrow().clone() else { return };
            if online::download_state(&t.path).is_none() && t.download.is_none() {
                online::download(&t.path);
            }
        });
        let t = track.clone();
        more.connect_clicked(move |b| {
            if let Some(t) = t.borrow().clone() {
                episode_menu(b.upcast_ref(), b.width() as f64 / 2.0, b.height() as f64, &t);
            }
        });
        let t = track.clone();
        views::on_context(&root, move |w, x, y| {
            if let Some(t) = t.borrow().clone() {
                episode_menu(w, x, y, &t);
            }
        });
        EpisodeRow { root, date, new, title, bar, time, downloaded, downloading, download, play, track }
    }

    fn bind(&self, t: &Rc<Track>) {
        *self.track.borrow_mut() = Some(t.clone());
        self.title.set_text(&t.title);
        let current = player::current().is_some_and(|c| c.path == t.path);
        if current {
            self.title.add_css_class("accent-text");
        } else {
            self.title.remove_css_class("accent-text");
        }
        let published = online::info(&t.path).published;
        self.date.set_text(&if published > 0 { fmt::date(published) } else { String::new() });
        self.new.set_visible(online::is_new(t));
        let pr = online::progress(&t.path);
        if pr.played {
            self.root.add_css_class("played");
        } else {
            self.root.remove_css_class("played");
        }
        let fraction = if !pr.played && pr.position > 0.0 && t.duration > 0.0 { pr.position / t.duration } else { 0.0 };
        self.bar.set_visible(fraction > 0.0);
        self.bar.set_fraction(fraction.clamp(0.0, 1.0));
        self.time.set_text(&episode_time(t));
        let state = online::download_state(&t.path);
        self.downloaded.set_visible(state.is_none() && t.download.is_some());
        self.downloading.set_visible(state.is_some());
        if let Some((done, total)) = state {
            self.downloading
                .set_text(&(done * 100).checked_div(total).map_or_else(|| "Downloading…".to_string(), |p| format!("Downloading {p}%")));
        }
        self.download.set_visible(t.download.is_none() && !pr.played);
        self.download.set_sensitive(state.is_none());
        self.download.set_icon_name(if state.is_some() { "content-loading-symbolic" } else { "folder-download-symbolic" });
        let playing = current && player::state() == player::State::Playing;
        self.play.set_icon_name(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
        let tip = if playing {
            "Pause"
        } else if fraction > 0.0 {
            "Resume"
        } else {
            "Play"
        };
        self.play.set_tooltip_text(Some(tip));
        self.play.update_property(&[gtk::accessible::Property::Label(tip)]);
        let notes = online::info(&t.path).description;
        let notes: String = notes.chars().take(600).collect();
        self.root.set_tooltip_text((!notes.is_empty()).then_some(notes.as_str()));
    }
}

fn play_episode(t: &Track) {
    player::play_tracks(vec![t.path.clone()], 0);
}

fn episode_menu(anchor: &gtk::Widget, x: f64, y: f64, t: &Rc<Track>) {
    let paths = vec![t.path.clone()];
    let mut items: Vec<(String, Box<dyn Fn()>)> = Vec::new();
    let tt = t.clone();
    items.push(("Play".into(), Box::new(move || play_episode(&tt))));
    let p = paths.clone();
    items.push((
        "Play next".into(),
        Box::new(move || {
            player::play_next(p.clone());
            window::toast("The episode plays next.");
        }),
    ));
    let p = paths.clone();
    items.push((
        "Add to queue".into(),
        Box::new(move || {
            player::enqueue(p.clone());
            window::toast("Added the episode to the queue.");
        }),
    ));
    items.push(("-".into(), Box::new(|| {})));
    let played = online::progress(&t.path).played;
    let path = t.path.clone();
    items.push((
        if played { "Mark as not played" } else { "Mark as played" }.into(),
        Box::new(move || online::set_progress(&path, 0.0, !played)),
    ));
    let path = t.path.clone();
    if online::download_state(&t.path).is_none() {
        if t.download.is_some() {
            items.push(("Delete download".into(), Box::new(move || online::delete_download(&path))));
        } else {
            items.push(("Download".into(), Box::new(move || online::download(&path))));
        }
    }
    let url = t.path.to_string_lossy().into_owned();
    let a = anchor.clone();
    items.push((
        "Copy episode address".into(),
        Box::new(move || {
            a.clipboard().set_text(&url);
            window::toast("Copied the episode address.");
        }),
    ));
    views::popup_menu(anchor, x, y, items, Some(paths));
}

#[derive(Clone)]
struct EpisodeList {
    root: gtk::Box,
    store: gio::ListStore,
    all: Rc<RefCell<Vec<Rc<Track>>>>,
    shown: Rc<std::cell::Cell<usize>>,
    more: gtk::Button,
}

impl EpisodeList {
    fn new() -> EpisodeList {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let row = EpisodeRow::new();
            item.set_child(Some(&row.root));
            unsafe { item.set_data("row", row) };
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let Some(obj) = item.item().and_downcast::<glib::BoxedAnyObject>() else { return };
            if let Some(row) = unsafe { item.data::<EpisodeRow>("row") } {
                unsafe { row.as_ref() }.bind(&obj.borrow::<Rc<Track>>());
            }
        });
        let model = gtk::SingleSelection::new(Some(store.clone()));
        model.set_autoselect(false);
        model.set_can_unselect(true);
        let list = gtk::ListView::new(Some(model), Some(factory));
        list.add_css_class("episode-list");
        let s = store.clone();
        list.connect_activate(move |_, pos| {
            if let Some(obj) = s.item(pos).and_downcast::<glib::BoxedAnyObject>() {
                let t = obj.borrow::<Rc<Track>>().clone();
                play_episode(&t);
            }
        });
        // The show scrolls as a whole, so the list is paged instead.
        list.set_vexpand(false);
        let root = widgets::vbox(10);
        root.append(&list);
        let more = gtk::Button::with_label("");
        more.set_halign(gtk::Align::Center);
        root.append(&more);
        let l = EpisodeList { root, store, all: Rc::default(), shown: Rc::new(std::cell::Cell::new(PAGE)), more };
        let me = l.clone();
        l.more.connect_clicked(move |_| {
            me.shown.set(me.shown.get() + PAGE);
            me.render();
        });
        l
    }

    fn set(&self, tracks: Vec<Rc<Track>>) {
        *self.all.borrow_mut() = tracks;
        self.render();
    }

    fn render(&self) {
        let all = self.all.borrow();
        let n = all.len().min(self.shown.get());
        let objs: Vec<glib::BoxedAnyObject> = all[..n].iter().cloned().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objs);
        let rest = all.len() - n;
        self.more.set_visible(rest > 0);
        self.more.set_label(&format!("Show {} more", rest.min(PAGE)));
    }

    fn rebind(&self) {
        let n = self.store.n_items();
        self.store.items_changed(0, n, n);
    }
}

// ---------- A show ----------

/// What the header shows, from whichever source we have.
#[derive(Default, Clone)]
struct Header {
    title: String,
    author: String,
    art_url: String,
    description: String,
    apple_id: String,
}

fn header_for(feed: &str, show: Option<&Show>, loaded: Option<&Feed>) -> Header {
    if let Some(f) = loaded {
        return Header {
            title: f.title.clone(),
            author: f.author.clone(),
            art_url: f.art_url.clone(),
            description: f.description.clone(),
            apple_id: show.map(|s| s.apple_id.clone()).unwrap_or_default(),
        };
    }
    if let Some(p) = online::podcast(feed) {
        return Header { title: p.title, author: p.author, art_url: p.art_url, description: p.description, apple_id: p.apple_id };
    }
    show.map(|s| Header {
        title: s.title.clone(),
        author: s.author.clone(),
        art_url: s.art_url.clone(),
        description: s.summary.clone(),
        apple_id: s.apple_id.clone(),
    })
    .unwrap_or_default()
}

/// A chart entry has no feed address yet; look it up first.
fn open_show(show: Show) {
    if !show.feed.is_empty() {
        open_feed(&show.feed.clone(), Some(show));
        return;
    }
    let Some(detail) = begin_detail("", false) else { return };
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_size_request(32, 32);
    spinner.set_vexpand(true);
    detail.append(&spinner);
    let id = show.apple_id.clone();
    cmd::background(
        move || podcast::lookup(&id),
        move |res| match res {
            Ok(feed) => open_feed(&feed, Some(Show { feed: feed.clone(), ..show })),
            Err(e) => {
                window::toast(&format!("Couldn't open {}: {e:#}", show.title));
                back();
            }
        },
    );
}

/// Show a podcast's page (from anywhere, e.g. Now playing).
pub fn show(feed: &str) {
    if feed.is_empty() {
        return;
    }
    window::navigate("podcasts");
    open_feed(feed, None);
}

/// Switch to a show's view, emptied: under the row of shows for one of yours
/// (`mine`), else on its own page with a way back to Find a podcast. Returns its box.
fn begin_detail(feed: &str, mine: bool) -> Option<gtk::Box> {
    UI.with(|u| {
        let mut u = u.borrow_mut();
        let u = u.as_mut()?;
        u.open = feed.to_string();
        let target = if mine { u.chosen_box.clone() } else { u.detail.clone() };
        while let Some(c) = target.first_child() {
            target.remove(&c);
        }
        u.outer.set_visible_child_name(if mine { "home" } else { "detail" });
        if !mine {
            target.append(&views::back_button("Find a podcast", back));
        }
        Some(target)
    })
}

/// Back: from a show to Find a podcast, from there to your shows.
fn back() {
    let to_find = UI.with(|u| u.borrow().as_ref().is_some_and(|u| u.outer.visible_child_name().as_deref() == Some("detail")));
    if to_find {
        open_find();
    } else {
        go_home();
    }
}

fn open_find() {
    UI.with(|u| {
        if let Some(u) = u.borrow_mut().as_mut() {
            u.outer.set_visible_child_name("find");
            u.open.clear();
        }
    });
}

fn go_home() {
    let chosen = UI.with(|u| {
        let mut u = u.borrow_mut();
        let u = u.as_mut()?;
        u.outer.set_visible_child_name("home");
        Some((u.chosen.clone(), u.open.clone()))
    });
    if let Some((chosen, open)) = chosen
        && !chosen.is_empty()
        && chosen != open
    {
        open_feed(&chosen, None);
    }
}

fn open_feed(feed: &str, show: Option<Show>) {
    let mine = online::is_subscribed(feed);
    if mine {
        UI.with(|u| {
            if let Some(u) = u.borrow_mut().as_mut() {
                u.chosen = feed.to_string();
            }
        });
    }
    let Some(detail) = begin_detail(feed, mine) else { return };
    if mine {
        refresh_strip();
    }
    let feed = feed.to_string();
    let loaded: Rc<RefCell<Option<Feed>>> = Rc::default();
    let h = header_for(&feed, show.as_ref(), None);

    let header = widgets::hbox(18);
    header.add_css_class("show-head");
    let cover = Cover::new(88, false);
    cover.set_placeholder("music-podcast-symbolic");
    cover.set_url(&h.art_url);
    header.append(&cover.root);
    let text = widgets::vbox(3);
    text.set_hexpand(true);
    let title = widgets::label(&h.title, "show-title");
    title.set_xalign(0.0);
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    text.append(&title);
    let author = widgets::label(&h.author, "show-author");
    author.set_xalign(0.0);
    author.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&author);
    let about = widgets::label(&h.description, "dim");
    about.add_css_class("podcast-about");
    about.set_wrap(true);
    about.set_lines(3);
    about.set_ellipsize(gtk::pango::EllipsizeMode::End);
    about.set_xalign(0.0);
    about.set_visible(!h.description.is_empty());
    text.append(&about);
    let actions = widgets::hbox(8);
    actions.add_css_class("show-actions");
    let play_latest = widgets::labeled_button("media-playback-start-symbolic", "Play latest");
    play_latest.add_css_class("suggested-action");
    let subscribe = gtk::Button::new();
    subscribe.add_css_class("outlined");
    let sub_icon = gtk::Image::from_icon_name("object-select-symbolic");
    let sub_label = widgets::label("", "");
    let sub_row = widgets::hbox(6);
    sub_row.append(&sub_icon);
    sub_row.append(&sub_label);
    subscribe.set_child(Some(&sub_row));
    let more = widgets::icon_button("view-more-horizontal-symbolic", "Check for new episodes, copy feed address, unsubscribe");
    actions.append(&play_latest);
    actions.append(&subscribe);
    actions.append(&more);
    text.append(&actions);
    header.append(&text);
    detail.append(&header);
    let list = EpisodeList::new();
    let loader = Loader::new(&list.root);
    loader.root.set_vexpand(false);
    loader.root.add_css_class("show-episodes");
    detail.append(&loader.root);

    let sync = {
        let (feed, subscribe, list, loader, play_latest) =
            (feed.clone(), subscribe.clone(), list.clone(), loader.clone(), play_latest.clone());
        let loaded = loaded.clone();
        move || {
            let subscribed = online::is_subscribed(&feed);
            sub_icon.set_icon_name(Some(if subscribed { "object-select-symbolic" } else { "list-add-symbolic" }));
            sub_label.set_text(if subscribed { "Subscribed" } else { "Subscribe" });
            subscribe.set_tooltip_text(Some(if subscribed { "Unsubscribe" } else { "Keep this show and see its new episodes" }));
            subscribe.set_sensitive(subscribed || loaded.borrow().is_some());
            let episodes = online::episodes(&feed);
            play_latest.set_sensitive(!episodes.is_empty());
            if !episodes.is_empty() {
                list.set(episodes);
                loader.show();
            }
        }
    };
    sync();

    let ctx = Rc::new(Fetch {
        feed: feed.clone(),
        show: show.clone(),
        loaded: loaded.clone(),
        loader: loader.clone(),
        sync: Rc::new(sync.clone()),
        title: title.clone(),
        author: author.clone(),
        about: about.clone(),
        cover: cover.clone(),
    });
    let fetch = Rc::new({
        let ctx = ctx.clone();
        move || fetch_feed(ctx.clone())
    });
    fetch();

    let toggle_subscription: Rc<dyn Fn()> = {
        let (fd, ld, sh) = (feed.clone(), loaded.clone(), show.clone());
        Rc::new(move || {
            if online::is_subscribed(&fd) {
                online::unsubscribe(&fd);
            } else if let Some(f) = ld.borrow().as_ref() {
                online::subscribe_feed(&fd, &sh.as_ref().map(|s| s.apple_id.clone()).unwrap_or(h.apple_id.clone()), f);
            }
        })
    };
    let t = toggle_subscription.clone();
    subscribe.connect_clicked(move |_| t());
    let fd = feed.clone();
    more.connect_clicked(move |b| {
        let mut items: Vec<(String, Box<dyn Fn()>)> = Vec::new();
        let subscribed = online::is_subscribed(&fd);
        if subscribed && !online::refreshing(&fd) {
            let f = fetch.clone();
            items.push(("Check for new episodes".into(), Box::new(move || f())));
        }
        let (url, a) = (fd.clone(), b.clone());
        items.push((
            "Copy feed address".into(),
            Box::new(move || {
                a.clipboard().set_text(&url);
                window::toast("Copied the feed address.");
            }),
        ));
        let t = toggle_subscription.clone();
        items.push((if subscribed { "Unsubscribe" } else { "Subscribe" }.into(), Box::new(move || t())));
        views::popup_menu(b.upcast_ref(), b.width() as f64 / 2.0, b.height() as f64, items, None);
    });
    let fd = feed.clone();
    play_latest.connect_clicked(move |_| {
        if let Some(t) = online::episodes(&fd).first() {
            play_episode(t);
        }
    });

    let (s, l) = (sync.clone(), list.clone());
    online::subscribe(&detail_marker(&detail), move |c| match c {
        Change::Downloads => l.rebind(),
        _ => s(),
    });
    let l = list.clone();
    player::subscribe(&list.root, move |e| {
        if matches!(e, player::Event::Track | player::Event::State) {
            l.rebind();
        }
    });
}

/// What a feed fetch updates on the detail view.
struct Fetch {
    feed: String,
    show: Option<Show>,
    loaded: Rc<RefCell<Option<Feed>>>,
    loader: Loader,
    sync: Rc<dyn Fn()>,
    title: gtk::Label,
    author: gtk::Label,
    about: gtk::Label,
    cover: Cover,
}

fn fetch_feed(ctx: Rc<Fetch>) {
    if online::episodes(&ctx.feed).is_empty() {
        ctx.loader.loading();
    }
    let feed = ctx.feed.clone();
    online::load_feed(&feed, move |res| {
        // The view may have moved on to another show.
        if UI.with(|u| u.borrow().as_ref().map(|u| u.open.clone())) != Some(ctx.feed.clone()) {
            return;
        }
        match res {
            Ok(f) => {
                let h = header_for(&ctx.feed, ctx.show.as_ref(), Some(&f));
                ctx.title.set_text(&h.title);
                ctx.author.set_text(&h.author);
                ctx.about.set_text(&h.description);
                ctx.about.set_visible(!h.description.is_empty());
                ctx.cover.set_url(&h.art_url);
                let empty = f.episodes.is_empty();
                *ctx.loaded.borrow_mut() = Some(f);
                (ctx.sync)();
                if empty {
                    ctx.loader.show_empty(&widgets::empty_state(
                        "music-podcast-symbolic",
                        "No episodes",
                        "This show has no episodes to play yet.",
                        None,
                    ));
                }
            }
            Err(e) if online::episodes(&ctx.feed).is_empty() => {
                let c = ctx.clone();
                ctx.loader.failed(&e, move || fetch_feed(c.clone()));
            }
            Err(e) => window::toast(&format!("Couldn't check for new episodes: {e:#}")),
        }
    });
}

/// A widget that lives exactly as long as this detail view (its subscriptions
/// end when the view is rebuilt for another show).
fn detail_marker(detail: &gtk::Box) -> gtk::Widget {
    let m = widgets::hbox(0);
    detail.append(&m);
    m.upcast()
}

// ---------- Your shows ----------

thread_local! {
    static STRIP: RefCell<Option<(gtk::Box, gtk::Stack)>> = const { RefCell::new(None) };
}

fn show_tile(p: &crate::library::db::Podcast, chosen: bool) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("flat");
    b.add_css_class("show-tile");
    if chosen {
        b.add_css_class("selected");
    }
    b.set_tooltip_text(Some(&p.title));
    let bx = widgets::vbox(6);
    let art = gtk::Overlay::new();
    let cover = Cover::new(72, true);
    cover.set_placeholder("music-podcast-symbolic");
    cover.set_url(&p.art_url);
    art.set_child(Some(&cover.root));
    art.set_halign(gtk::Align::Center);
    if online::new_count(p) > 0 {
        let badge = widgets::label("New", "show-new");
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::Start);
        art.add_overlay(&badge);
    }
    bx.append(&art);
    let name = widgets::label(&two_lines(&p.title, 13), "show-name");
    name.set_justify(gtk::Justification::Center);
    bx.append(&name);
    b.set_child(Some(&bx));
    let feed = p.feed.clone();
    b.connect_clicked(move |_| open_feed(&feed, None));
    b
}

/// A show's name in at most two lines of about `width` characters, broken between
/// words, the rest cut with an ellipsis. (A wrapping label asks for its whole text's
/// width in a row that scrolls sideways, so the tile would never wrap it.)
fn two_lines(name: &str, width: usize) -> String {
    let mut lines: Vec<String> = vec![String::new()];
    for word in name.split_whitespace() {
        let line = lines.last_mut().expect("a line");
        if line.is_empty() || line.chars().count() + 1 + word.chars().count() <= width {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        } else {
            lines.push(word.to_string());
        }
    }
    let cut = lines.len() > 2;
    lines.truncate(2);
    for line in lines.iter_mut() {
        if line.chars().count() > width {
            let keep: String = line.chars().take(width - 1).collect();
            *line = format!("{}…", keep.trim_end());
        }
    }
    if cut
        && let Some(last) = lines.last_mut()
        && !last.ends_with('…')
    {
        last.push('…');
    }
    lines.join("\n")
}


/// Rebuild the row of shows, keeping (or making) a choice.
fn refresh_strip() {
    let Some((strip, home)) = STRIP.with(|s| s.borrow().clone()) else { return };
    let list = online::podcasts();
    let fallback = list.first().map(|p| p.feed.clone()).unwrap_or_default();
    let (chosen, open, showing_home) = UI.with(|u| {
        let mut u = u.borrow_mut();
        let Some(u) = u.as_mut() else { return (String::new(), String::new(), false) };
        if !list.iter().any(|p| p.feed == u.chosen) {
            u.chosen = fallback.clone();
        }
        (u.chosen.clone(), u.open.clone(), u.outer.visible_child_name().as_deref() == Some("home"))
    });
    while let Some(c) = strip.first_child() {
        strip.remove(&c);
    }
    for p in &list {
        strip.append(&show_tile(p, p.feed == chosen));
    }
    home.set_visible_child_name(if list.is_empty() { "empty" } else { "shows" });
    if showing_home && !chosen.is_empty() && chosen != open {
        glib::idle_add_local_once(move || open_feed(&chosen, None));
    }
}

// ---------- The page ----------

fn add_by_url() {
    widgets::ask_text(
        "Add a podcast",
        "The address of the show's RSS feed. You'll be subscribed to it.",
        "",
        "Subscribe",
        |url| {
            online::subscribe_url(&url, |feed| {
                if let Some(f) = feed {
                    open_feed(&f, None);
                }
            })
        },
    );
}

/// "4 shows · 3 new episodes"
fn summary() -> String {
    let shows = online::podcasts().len();
    let new = online::new_total();
    let mut s = fmt::count(shows, "show", "shows");
    if new > 0 {
        s.push_str(&format!(" · {}", fmt::count(new, "new episode", "new episodes")));
    }
    s
}

/// Your shows: a head with the counts, Check for new and Find a podcast; the row
/// of shows; the chosen one.
fn home_view(chosen_box: &gtk::Box) -> (gtk::Widget, Rc<dyn Fn()>) {
    let root = widgets::vbox(0);
    let head = widgets::hbox(12);
    head.add_css_class("list-toolbar");
    head.add_css_class("page-head");
    head.append(&widgets::label("Podcasts", "page-head-title"));
    let count = widgets::label("", "page-head-count");
    count.set_hexpand(true);
    count.set_xalign(0.0);
    head.append(&count);
    let check = widgets::labeled_button("view-refresh-symbolic", "Check for new");
    check.add_css_class("flat");
    check.set_tooltip_text(Some("Check every show for new episodes"));
    check.connect_clicked(|_| online::refresh_all(true));
    head.append(&check);
    let find = widgets::labeled_button("system-search-symbolic", "Find a podcast");
    find.add_css_class("outlined");
    find.set_tooltip_text(Some("Top shows, categories and search"));
    find.connect_clicked(|_| open_find());
    head.append(&find);
    root.append(&head);

    let home = gtk::Stack::new();
    home.set_vexpand(true);
    let shows = widgets::vbox(0);
    let strip = widgets::hbox(12);
    strip.add_css_class("show-strip");
    let strip_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&strip)
        .build();
    strip_scroll.add_css_class("show-strip-scroll");
    shows.append(&strip_scroll);
    chosen_box.add_css_class("podcast-detail");
    let clamp = nyx_ui::clamp::Clamp::new(chosen_box, 820);
    clamp.set_start(true);
    let detail_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&clamp)
        .vexpand(true)
        .build();
    shows.append(&detail_scroll);
    home.add_named(&shows, Some("shows"));
    let empty = widgets::empty_state(
        "music-podcast-symbolic",
        "No podcasts yet",
        "Subscribe to a show to keep it here and see its new episodes.",
        Some(("Find a podcast", Box::new(open_find))),
    );
    home.add_named(&empty, Some("empty"));
    root.append(&home);
    STRIP.with(|s| *s.borrow_mut() = Some((strip, home)));

    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        count.set_text(&summary());
        check.set_sensitive(!online::podcasts().iter().any(|p| online::refreshing(&p.feed)));
        refresh_strip();
    });
    let r = refresh.clone();
    online::subscribe(&root, move |c| {
        if matches!(c, Change::Podcasts | Change::Episodes) {
            r();
        }
    });
    (root.upcast(), refresh)
}

/// Find a podcast: Apple's charts, categories and search, and adding a feed by its address.
fn find_view() -> gtk::Widget {
    let p = prefs::get();
    let tab = if TABS.iter().any(|(t, _)| *t == p.podcast_tab) { p.podcast_tab.clone() } else { "top".into() };
    let browse = widgets::vbox(0);
    let toolbar = widgets::hbox(12);
    toolbar.add_css_class("list-toolbar");
    let back_button = views::back_button("Podcasts", go_home);
    back_button.set_margin_bottom(0);
    toolbar.append(&back_button);
    let tabs = gtk::Stack::new();
    tabs.set_vexpand(true);
    tabs.set_transition_type(gtk::StackTransitionType::Crossfade);
    tabs.set_transition_duration(if p.reduce_motion { 0 } else { 140 });
    let (top, top_shows) = show_grid();
    let top_loader = Loader::new(&top.root);
    tabs.add_named(&top_loader.root, Some("top"));
    tabs.add_named(&categories_view(), Some("categories"));
    let (search, entry) = search_view();
    tabs.add_named(&search, Some("search"));

    let loaded: Rc<RefCell<HashSet<String>>> = Rc::default();
    let show_tab = {
        let (tabs, entry) = (tabs.clone(), entry.clone());
        move |id: &str| {
            tabs.set_visible_child_name(id);
            if id == "search" {
                entry.grab_focus();
            }
            if id == "top" && loaded.borrow_mut().insert(id.to_string()) {
                let cc = country();
                load_shows(&top_loader, &top, &top_shows, std::sync::Arc::new(move || podcast::top(&cc, None)));
            }
        }
    };
    let s = show_tab.clone();
    let opts: Vec<(String, String)> = TABS.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let seg = widgets::segmented(&opts, &tab, move |id| {
        s(&id);
        prefs::update(|p| p.podcast_tab = id);
    });
    toolbar.append(&seg);
    let spacer = widgets::hbox(0);
    spacer.set_hexpand(true);
    toolbar.append(&spacer);
    let add = widgets::labeled_button("list-add-symbolic", "Add by address");
    add.set_tooltip_text(Some("Subscribe to a show by its RSS feed"));
    add.connect_clicked(|_| add_by_url());
    toolbar.append(&add);
    browse.append(&toolbar);
    browse.append(&tabs);
    // The chart loads when Find a podcast first opens, not with the page.
    let opened = std::cell::Cell::new(false);
    browse.connect_map(move |_| {
        if !opened.replace(true) {
            show_tab(&tab);
        }
    });
    browse.upcast()
}

pub fn build(page: &Page) {
    let p = prefs::get();
    let outer = gtk::Stack::new();
    outer.set_vexpand(true);
    outer.set_transition_type(gtk::StackTransitionType::Crossfade);
    outer.set_transition_duration(if p.reduce_motion { 0 } else { 160 });

    let chosen_box = widgets::vbox(0);
    let (home, refresh_home) = home_view(&chosen_box);
    outer.add_named(&home, Some("home"));
    outer.add_named(&find_view(), Some("find"));

    let detail = widgets::vbox(14);
    detail.add_css_class("podcast-detail");
    let detail_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&detail)
        .vexpand(true)
        .build();
    outer.add_named(&detail_scroll, Some("detail"));
    page.body.append(&outer);
    outer.set_visible_child_name("home");
    UI.with(|u| *u.borrow_mut() = Some(Ui { outer, chosen_box, detail, open: String::new(), chosen: String::new() }));
    refresh_home();
}

#[cfg(test)]
mod tests {
    use super::two_lines;

    #[test]
    fn breaks_show_names_in_two_lines() {
        assert_eq!(two_lines("Hard Fork", 13), "Hard Fork");
        assert_eq!(two_lines("Power On with Mark Gurman", 13), "Power On with\nMark Gurman");
        assert_eq!(two_lines("Software Engineering Daily", 13), "Software\nEngineering…");
        assert_eq!(two_lines("Supercalifragilistic", 13), "Supercalifra…");
    }
}
