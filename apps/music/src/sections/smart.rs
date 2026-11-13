//! Lists made from what the library already knows, on one page: albums recently
//! added, and songs recently or most played.

use super::{library_stack, scan_banner};
use crate::library::store;
use crate::library::Track;
use crate::tracklist::{Col, Options, TrackTable};
use crate::views::{self, AlbumGrid};
use crate::widgets::{self, Page};
use crate::{fmt, prefs};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

const ALBUMS: usize = 60;
const SONGS: usize = 200;

/// The page's lists; their ids are also the pages they once were (`--section most-played`).
pub const TABS: &[(&str, &str)] =
    &[("recently-added", "Recently added"), ("recently-played", "Recently played"), ("most-played", "Most played")];

thread_local! {
    static SWITCH: RefCell<Option<gtk::Box>> = const { RefCell::new(None) };
}

pub fn build(page: &Page) {
    page.body.append(&scan_banner());
    let head = widgets::hbox(12);
    head.add_css_class("list-toolbar");
    head.add_css_class("page-head");
    let tools = gtk::Stack::new();
    tools.set_hexpand(true);
    tools.set_halign(gtk::Align::End);
    let views = gtk::Stack::new();
    views.set_vexpand(true);
    views.set_transition_type(gtk::StackTransitionType::Crossfade);
    views.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 140 });
    for (id, _) in TABS {
        let (bar, content) = match *id {
            "recently-added" => build_added(),
            "recently-played" => build_recent(),
            _ => build_most(),
        };
        tools.add_named(&bar, Some(id));
        views.add_named(&content, Some(id));
    }
    let tab = prefs::get().recent_tab;
    let tab = if TABS.iter().any(|(t, _)| *t == tab) { tab } else { TABS[1].0.to_string() };
    let switch = {
        let (tools, views) = (tools.clone(), views.clone());
        widgets::segmented(&widgets::opts(TABS), &tab, move |id| {
            tools.set_visible_child_name(&id);
            views.set_visible_child_name(&id);
            prefs::update(|p| p.recent_tab = id);
        })
    };
    tools.set_visible_child_name(&tab);
    views.set_visible_child_name(&tab);
    head.append(&switch);
    head.append(&tools);
    page.body.append(&head);
    page.body.append(&library_stack(&views));
    SWITCH.with(|s| *s.borrow_mut() = Some(switch));
}

/// Show one of the page's lists (an id from [`TABS`]).
pub fn set_tab(id: &str) {
    let Some(i) = TABS.iter().position(|(t, _)| *t == id) else { return };
    SWITCH.with(|s| {
        let Some(switch) = s.borrow().clone() else { return };
        let mut child = switch.first_child();
        let mut n = 0;
        while let Some(c) = child {
            if n == i
                && let Some(b) = c.downcast_ref::<gtk::ToggleButton>()
            {
                b.set_active(true);
            }
            child = c.next_sibling();
            n += 1;
        }
    });
}

fn summary_label() -> gtk::Label {
    let summary = widgets::label("", "page-head-count");
    summary.set_hexpand(true);
    summary.set_xalign(1.0);
    summary
}

fn build_added() -> (gtk::Widget, gtk::Widget) {
    let bar = widgets::hbox(12);
    let summary = summary_label();
    bar.append(&summary);
    let grid = AlbumGrid::new(|a| super::albums::show(&a.key));
    let refresh = {
        let grid = grid.clone();
        move || {
            let albums: Vec<_> = store::sorted_albums("added").into_iter().take(ALBUMS).collect();
            summary.set_text(&fmt::count(albums.len(), "album", "albums"));
            grid.set(&albums);
        }
    };
    refresh();
    store::subscribe(&grid.root, move |c| {
        if c == store::Change::Library {
            refresh();
        }
    });
    (bar.upcast(), grid.root.upcast())
}

fn build_recent() -> (gtk::Widget, gtk::Widget) {
    songs("recent", &[Col::Indicator, Col::Title, Col::Artist, Col::Album, Col::Played, Col::Time], |tracks| {
        let mut played: Vec<_> = tracks.into_iter().filter(|t| t.last_played.get() > 0).collect();
        played.sort_by_key(|t| std::cmp::Reverse(t.last_played.get()));
        played
    })
}

fn build_most() -> (gtk::Widget, gtk::Widget) {
    songs("most-played", &[Col::Num, Col::Title, Col::Artist, Col::Album, Col::Plays, Col::Time], most_played)
}

/// Songs by how often they've been played, ties broken by the latest.
pub fn most_played(tracks: Vec<Rc<Track>>) -> Vec<Rc<Track>> {
    let mut played: Vec<_> = tracks.into_iter().filter(|t| t.plays.get() > 0).collect();
    played.sort_by_key(|t| std::cmp::Reverse((t.plays.get(), t.last_played.get())));
    played
}

/// A song list picked from the library, kept up to date as songs play.
fn songs(key: &'static str, cols: &'static [Col], pick: fn(Vec<Rc<Track>>) -> Vec<Rc<Track>>) -> (gtk::Widget, gtk::Widget) {
    let bar = widgets::hbox(12);
    let summary = summary_label();
    bar.append(&summary);
    let table =
        TrackTable::new(Options { cols, sortable: false, positions: cols.contains(&Col::Num), key: Some(key), ..Default::default() });
    let t = table.clone();
    let play = views::play_buttons(move || t.paths());
    bar.append(&play);
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_named(&table.root, Some("table"));
    stack.add_named(
        &widgets::empty_state(
            "document-open-recent-symbolic",
            "Nothing played yet",
            "A song counts as played once you've heard half of it, or four minutes.",
            None,
        ),
        Some("empty"),
    );

    let refresh = {
        let stack = stack.clone();
        move || {
            let tracks: Vec<_> = pick(store::tracks()).into_iter().take(SONGS).collect();
            let secs: f64 = tracks.iter().map(|t| t.duration).sum();
            summary.set_text(&format!("{} · {}", fmt::count(tracks.len(), "song", "songs"), fmt::total(secs)));
            play.set_sensitive(!tracks.is_empty());
            stack.set_visible_child_name(if tracks.is_empty() { "empty" } else { "table" });
            table.set(&tracks);
        }
    };
    refresh();
    store::subscribe(&stack, move |c| {
        if c == store::Change::Library || c == store::Change::Plays {
            refresh();
        }
    });
    (bar.upcast(), stack.upcast())
}
