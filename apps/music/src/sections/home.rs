//! Home: albums and playlists to jump back into, new podcast episodes, your
//! favourite stations and the songs you play most.

use crate::library::art::Cover;
use crate::library::store::{self, Album};
use crate::library::{Track, db};
use crate::online::radio::Station;
use crate::online::{self, Change};
use crate::player::{self, Event, State};
use crate::widgets::{self, Page};
use crate::{fmt, window};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

const JUMP: usize = 6;
const EPISODES: usize = 3;
const STATIONS: usize = 4;
const TOP: usize = 6;

pub fn build(page: &Page) {
    let body = page.body.clone();
    body.add_css_class("home");
    body.append(&section_head("Jump back in", None));
    body.append(&jump_back_in(&body));

    let two = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    two.set_homogeneous(true);
    two.add_css_class("home-two");
    let eps = widgets::vbox(0);
    eps.append(&section_head("New episodes", Some(("All podcasts", "podcasts"))));
    eps.append(&new_episodes());
    two.append(&eps);
    let st = widgets::vbox(0);
    st.append(&section_head("Your stations", Some(("All radio", "radio"))));
    st.append(&stations());
    two.append(&st);
    body.append(&two);

    body.append(&section_head("Most played", Some(("All most played", "most-played"))));
    body.append(&most_played(&body));
}

/// A section's name, and a link to the page with all of it.
fn section_head(title: &str, more: Option<(&str, &'static str)>) -> gtk::Box {
    let row = widgets::hbox(8);
    row.add_css_class("home-head");
    let l = widgets::label(title, "home-title");
    l.set_hexpand(true);
    l.set_xalign(0.0);
    row.append(&l);
    if let Some((label, page)) = more {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.add_css_class("link-button");
        b.add_css_class("home-more");
        b.connect_clicked(move |_| window::navigate(page));
        row.append(&b);
    }
    row
}

/// A quiet line in place of a card's rows, with what to do about it.
fn card_note(text: &str, action: Option<(&str, &'static str)>) -> gtk::Box {
    let bx = widgets::vbox(8);
    bx.add_css_class("home-note");
    let l = widgets::label(text, "dim");
    l.set_wrap(true);
    l.set_xalign(0.0);
    bx.append(&l);
    if let Some((label, page)) = action {
        let b = gtk::Button::with_label(label);
        b.set_halign(gtk::Align::Start);
        b.connect_clicked(move |_| window::navigate(page));
        bx.append(&b);
    }
    bx
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

/// The round play button at a row's end: Resume or Play, Pause while it plays.
fn play_button(playing: bool, resume: bool) -> gtk::Button {
    let (icon, tip) = if playing {
        ("media-playback-pause-symbolic", "Pause")
    } else {
        ("media-playback-start-symbolic", if resume { "Resume" } else { "Play" })
    };
    let b = widgets::icon_button(icon, tip);
    b.add_css_class("home-play");
    b.set_valign(gtk::Align::Center);
    b.set_focus_on_click(false);
    b
}

fn is_current(path: &std::path::Path) -> bool {
    player::current().is_some_and(|t| t.path == path)
}

// ---------- Jump back in ----------

#[derive(Clone)]
enum Jump {
    Album(Rc<Album>),
    Playlist(db::Playlist, Vec<PathBuf>),
}

impl Jump {
    fn paths(&self) -> Vec<PathBuf> {
        match self {
            Jump::Album(a) => a.tracks.iter().map(|t| t.path.clone()).collect(),
            Jump::Playlist(_, items) => items.clone(),
        }
    }

    fn open(&self) {
        match self {
            Jump::Album(a) => super::albums::show(&a.key),
            Jump::Playlist(p, _) => window::navigate(&format!("playlist:{}", p.id)),
        }
    }
}

#[derive(Clone)]
struct JumpCard {
    root: gtk::Box,
    cover: Cover,
    title: gtk::Label,
    sub: gtk::Label,
    item: Rc<RefCell<Option<Jump>>>,
}

impl JumpCard {
    fn new() -> JumpCard {
        let root = widgets::vbox(2);
        root.add_css_class("album-card");
        root.add_css_class("jump-card");
        root.set_cursor_from_name(Some("pointer"));
        let cover = Cover::fluid(true);
        cover.root.set_margin_bottom(6);
        let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
        play.add_css_class("card-play");
        play.set_tooltip_text(Some("Play"));
        play.set_halign(gtk::Align::End);
        play.set_valign(gtk::Align::End);
        play.set_focus_on_click(false);
        cover.root.add_overlay(&play);
        root.append(&cover.root);
        let title = widgets::label("", "album-title");
        let sub = widgets::label("", "album-artist");
        for l in [&title, &sub] {
            l.set_xalign(0.0);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l.set_max_width_chars(1);
            l.set_hexpand(true);
            root.append(l);
        }
        let item: Rc<RefCell<Option<Jump>>> = Rc::default();
        let i = item.clone();
        play.connect_clicked(move |_| {
            if let Some(j) = i.borrow().clone() {
                player::set_shuffle(false);
                player::play_tracks(j.paths(), 0);
            }
        });
        let click = gtk::GestureClick::new();
        let i = item.clone();
        click.connect_released(move |g, _, _, _| {
            if g.widget().is_some_and(|w| w.is_sensitive())
                && let Some(j) = i.borrow().clone()
            {
                j.open();
            }
        });
        root.add_controller(click);
        let hover = gtk::EventControllerMotion::new();
        let r = root.clone();
        hover.connect_enter(move |_, _, _| r.add_css_class("hover"));
        let r = root.clone();
        hover.connect_leave(move |_| r.remove_css_class("hover"));
        root.add_controller(hover);
        JumpCard { root, cover, title, sub, item }
    }

    fn set(&self, j: Option<Jump>) {
        match &j {
            Some(Jump::Album(a)) => {
                self.cover.set_placeholder("media-optical-symbolic");
                self.cover.set_key(&a.art);
                self.title.set_text(&a.title);
                self.sub.set_text(&a.artist);
            }
            Some(Jump::Playlist(p, items)) => {
                let art = items.iter().map(|p| store::track_for(p).art.clone()).find(|a| !a.is_empty()).unwrap_or_default();
                self.cover.set_placeholder("music-playlist-symbolic");
                self.cover.set_key(&art);
                self.title.set_text(&p.name);
                self.sub.set_text(&format!("Playlist · {}", fmt::count(items.len(), "song", "songs")));
            }
            None => {}
        }
        self.title.set_tooltip_text(Some(&self.title.text()));
        *self.item.borrow_mut() = j;
    }
}

/// What you played last: albums, and playlists by the last song played from them.
/// Topped up with recently added albums while little has been played.
fn jump_items(lists: &[(db::Playlist, Vec<PathBuf>)]) -> Vec<Jump> {
    let mut played: Vec<(i64, Jump)> =
        store::albums().into_iter().filter(|a| a.last_played() > 0).map(|a| (a.last_played(), Jump::Album(a))).collect();
    for (p, items) in lists {
        let last = items.iter().map(|i| store::track_for(i).last_played.get()).max().unwrap_or(0);
        if last > 0 {
            played.push((last, Jump::Playlist(p.clone(), items.clone())));
        }
    }
    played.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    let mut out: Vec<Jump> = played.into_iter().map(|(_, j)| j).take(JUMP).collect();
    for a in store::sorted_albums("added") {
        if out.len() >= JUMP {
            break;
        }
        if !out.iter().any(|j| matches!(j, Jump::Album(b) if b.key == a.key)) {
            out.push(Jump::Album(a));
        }
    }
    out
}

fn jump_back_in(body: &gtk::Box) -> gtk::Widget {
    let stack = gtk::Stack::new();
    stack.set_vhomogeneous(false);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.set_homogeneous(true);
    row.add_css_class("jump-row");
    let cards: Rc<Vec<JumpCard>> = Rc::new((0..JUMP).map(|_| JumpCard::new()).collect());
    for c in cards.iter() {
        row.append(&c.root);
    }
    stack.add_named(&row, Some("cards"));
    stack.add_named(&card_note("Albums and playlists you play show up here.", Some(("Browse albums", "albums"))), Some("empty"));

    let count = Rc::new(Cell::new(0));
    // Narrow, four cards; the rest keep their place, hidden.
    let layout = {
        let (cards, count, body) = (cards.clone(), count.clone(), body.clone());
        move || {
            let cols = if body.has_css_class("narrow") { 4 } else { JUMP };
            for (i, c) in cards.iter().enumerate() {
                c.root.set_visible(i < cols);
                c.root.set_sensitive(i < count.get());
                c.root.set_opacity(if i < count.get() { 1.0 } else { 0.0 });
            }
        }
    };
    let refresh = {
        let (cards, stack, layout) = (cards.clone(), stack.clone(), layout.clone());
        move || {
            let (cards, stack, count, layout) = (cards.clone(), stack.clone(), count.clone(), layout.clone());
            super::playlist::load_items(move |lists| {
                let items = jump_items(&lists);
                count.set(items.len());
                for (i, c) in cards.iter().enumerate() {
                    c.set(items.get(i).cloned());
                }
                stack.set_visible_child_name(if items.is_empty() { "empty" } else { "cards" });
                layout();
            });
        }
    };
    refresh();
    store::subscribe(&stack, move |c| {
        if matches!(c, store::Change::Library | store::Change::Plays | store::Change::Playlists) {
            refresh();
        }
    });
    body.connect_css_classes_notify(move |_| layout());
    stack.upcast()
}

// ---------- New episodes ----------

fn episode_row(t: &Rc<Track>) -> gtk::Box {
    let row = widgets::hbox(12);
    row.add_css_class("home-row");
    let cover = Cover::new(44, true);
    cover.set_placeholder("music-podcast-symbolic");
    match online::podcast(&t.feed) {
        Some(p) => cover.set_url(&p.art_url),
        None => cover.set_key(&t.art),
    }
    cover.root.set_valign(gtk::Align::Center);
    row.append(&cover.root);

    let text = widgets::vbox(1);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let show = widgets::label(&t.album, "home-kicker");
    show.set_xalign(0.0);
    show.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&show);
    let title = widgets::label(&t.title, "home-row-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_tooltip_text(Some(&t.title));
    if is_current(&t.path) {
        title.add_css_class("accent-text");
    }
    text.append(&title);
    let meta = widgets::hbox(8);
    meta.add_css_class("home-meta");
    let pr = online::progress(&t.path);
    let started = pr.position > 0.0 && t.duration > 0.0;
    if started {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("episode-progress");
        bar.set_fraction((pr.position / t.duration).clamp(0.0, 1.0));
        bar.set_valign(gtk::Align::Center);
        bar.set_size_request(70, -1);
        meta.append(&bar);
        meta.append(&widgets::label(&format!("{} left", fmt::total((t.duration - pr.position).max(60.0))), "dim"));
    } else if t.duration > 0.0 {
        meta.append(&widgets::label(&fmt::total(t.duration), "dim"));
    }
    if let Some((done, total)) = online::download_state(&t.path) {
        let text = (done * 100).checked_div(total).map_or_else(|| "Downloading…".to_string(), |p| format!("Downloading {p}%"));
        meta.append(&widgets::label(&text, "dim"));
    } else if t.download.is_some() {
        meta.append(&downloaded_mark());
    }
    text.append(&meta);
    let open = gtk::GestureClick::new();
    let feed = t.feed.clone();
    open.connect_released(move |_, _, _, _| super::podcasts::show(&feed));
    text.add_controller(open);
    text.set_cursor_from_name(Some("pointer"));
    row.append(&text);

    let playing = is_current(&t.path) && player::state() == State::Playing;
    let play = play_button(playing, started);
    let track = t.clone();
    play.connect_clicked(move |_| {
        if is_current(&track.path) {
            player::toggle();
        } else {
            player::play_tracks(vec![track.path.clone()], 0);
        }
    });
    row.append(&play);
    row
}

/// "Downloaded", with its arrow, in the success colour.
pub fn downloaded_mark() -> gtk::Box {
    let bx = widgets::hbox(4);
    bx.add_css_class("downloaded");
    let i = gtk::Image::from_icon_name("folder-download-symbolic");
    i.set_pixel_size(12);
    bx.append(&i);
    bx.append(&widgets::label("Downloaded", ""));
    bx
}

fn new_episodes() -> gtk::Widget {
    let card = widgets::vbox(0);
    card.add_css_class("card");
    card.add_css_class("home-card");
    let refresh = {
        let card = card.clone();
        move || {
            clear(&card);
            if online::podcasts().is_empty() {
                card.append(&card_note(
                    "Subscribe to a show and its new episodes show up here.",
                    Some(("Find a podcast", "podcasts")),
                ));
                return;
            }
            let eps = online::fresh_episodes(EPISODES);
            if eps.is_empty() {
                card.append(&card_note("You're all caught up: nothing new from your shows.", None));
            }
            for t in &eps {
                card.append(&episode_row(t));
            }
        }
    };
    refresh();
    let r = refresh.clone();
    online::subscribe(&card, move |c| {
        if matches!(c, Change::Podcasts | Change::Episodes | Change::Downloads) {
            r();
        }
    });
    player::subscribe(&card, move |e| {
        if matches!(e, Event::Track | Event::State) {
            refresh();
        }
    });
    card.upcast()
}

// ---------- Your stations ----------

fn station_row(s: &Station) -> gtk::Box {
    let row = widgets::hbox(12);
    row.add_css_class("home-row");
    let cover = Cover::new(36, true);
    cover.root.add_css_class("round");
    cover.set_placeholder("music-radio-symbolic");
    cover.set_url(&s.favicon);
    cover.root.set_valign(gtk::Align::Center);
    row.append(&cover.root);

    let text = widgets::vbox(1);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let name = widgets::label(s.name.trim(), "home-row-title");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&name);
    let meta = widgets::hbox(6);
    meta.add_css_class("home-meta");
    let current = player::current().filter(|t| t.path.as_os_str() == s.stream());
    let on_air = current.as_ref().is_some_and(|_| player::state() == State::Playing);
    if on_air {
        name.add_css_class("accent-text");
        meta.append(&widgets::label("On air", "on-air"));
    }
    let what = match current.filter(|t| t.title != s.name.trim()) {
        Some(t) if on_air => {
            [t.artist.as_str(), t.title.as_str()].iter().filter(|x| !x.is_empty()).copied().collect::<Vec<_>>().join(" · ")
        }
        _ => [s.genre(), s.country.clone()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "),
    };
    let what = widgets::label(if what.is_empty() { "Live" } else { &what }, "dim");
    what.set_ellipsize(gtk::pango::EllipsizeMode::End);
    meta.append(&what);
    text.append(&meta);
    row.append(&text);

    let play = play_button(on_air, false);
    let station = s.clone();
    play.connect_clicked(move |_| {
        if player::current().is_some_and(|t| t.path.as_os_str() == station.stream()) {
            player::toggle();
        } else {
            online::play_station(&station);
        }
    });
    row.append(&play);
    row
}

fn stations() -> gtk::Widget {
    let card = widgets::vbox(0);
    card.add_css_class("card");
    card.add_css_class("home-card");
    let refresh = {
        let card = card.clone();
        move || {
            clear(&card);
            let list = online::favourites();
            if list.is_empty() {
                card.append(&card_note("Star a station in Radio and it waits for you here.", Some(("Browse radio", "radio"))));
            }
            for s in list.iter().take(STATIONS) {
                card.append(&station_row(s));
            }
        }
    };
    refresh();
    let r = refresh.clone();
    online::subscribe(&card, move |c| {
        if c == Change::Stations {
            r();
        }
    });
    player::subscribe(&card, move |e| {
        if matches!(e, Event::Track | Event::State) {
            refresh();
        }
    });
    card.upcast()
}

// ---------- Most played ----------

fn top_row(rank: usize, t: &Track, list: Rc<Vec<PathBuf>>) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("flat");
    b.add_css_class("top-row");
    b.set_tooltip_text(Some("Play"));
    let row = widgets::hbox(10);
    let r = widgets::label(&rank.to_string(), "top-rank");
    r.set_xalign(1.0);
    r.set_width_chars(2);
    row.append(&r);
    let cover = Cover::new(36, true);
    cover.set_key(&t.art);
    cover.root.set_valign(gtk::Align::Center);
    row.append(&cover.root);
    let text = widgets::vbox(0);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let title = widgets::label(&t.title, "home-row-title");
    let artist = widgets::label(&t.artist, "dim");
    artist.add_css_class("home-meta");
    for l in [&title, &artist] {
        l.set_xalign(0.0);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.set_max_width_chars(1);
        text.append(l);
    }
    if is_current(&t.path) {
        title.add_css_class("accent-text");
    }
    row.append(&text);
    let plays = widgets::label(&fmt::count(t.plays.get() as usize, "play", "plays"), "dim");
    plays.add_css_class("home-meta");
    row.append(&plays);
    b.set_child(Some(&row));
    b.connect_clicked(move |_| player::play_tracks(list.to_vec(), rank - 1));
    b
}

fn most_played(body: &gtk::Box) -> gtk::Widget {
    let stack = gtk::Stack::new();
    stack.set_vhomogeneous(false);
    let grid = gtk::FlowBox::new();
    grid.add_css_class("top-grid");
    grid.set_homogeneous(true);
    grid.set_selection_mode(gtk::SelectionMode::None);
    grid.set_min_children_per_line(2);
    grid.set_max_children_per_line(3);
    grid.set_column_spacing(18);
    grid.set_row_spacing(2);
    stack.add_named(&grid, Some("grid"));
    stack.add_named(&card_note("A song counts as played once you've heard half of it, or four minutes.", None), Some("empty"));
    let refresh = {
        let (grid, stack) = (grid.clone(), stack.clone());
        move || {
            grid.remove_all();
            let top: Vec<Rc<Track>> = super::smart::most_played(store::tracks()).into_iter().take(TOP).collect();
            let list: Rc<Vec<PathBuf>> = Rc::new(top.iter().map(|t| t.path.clone()).collect());
            for (i, t) in top.iter().enumerate() {
                grid.append(&top_row(i + 1, t, list.clone()));
            }
            stack.set_visible_child_name(if top.is_empty() { "empty" } else { "grid" });
        }
    };
    refresh();
    let r = refresh.clone();
    store::subscribe(&stack, move |c| {
        if matches!(c, store::Change::Library | store::Change::Plays) {
            r();
        }
    });
    player::subscribe(&stack, move |e| {
        if e == Event::Track {
            refresh();
        }
    });
    let g = grid.clone();
    body.connect_css_classes_notify(move |b| g.set_max_children_per_line(if b.has_css_class("narrow") { 2 } else { 3 }));
    stack.upcast()
}
