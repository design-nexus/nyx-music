//! Albums: a cover grid; an album opens into its own view with its songs.

use super::{library_stack, scan_banner};
use crate::library::art::Cover;
use crate::library::store::{self, Album};
use crate::tracklist::{Col, Options, TrackTable};
use crate::views::{self, AlbumGrid};
use crate::widgets::{self, Page};
use crate::{fmt, window};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

struct Ui {
    inner: gtk::Stack,
    detail: gtk::Box,
    open_key: String,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

pub fn build(page: &Page) {
    page.body.append(&super::music_chips("albums"));
    page.body.append(&scan_banner());
    let inner = gtk::Stack::new();
    inner.set_transition_type(gtk::StackTransitionType::Crossfade);
    inner.set_transition_duration(if crate::prefs::get().reduce_motion { 0 } else { 160 });
    let grid = AlbumGrid::new(|a| open(&a));
    let grid_page = widgets::vbox(10);
    // The page's head: its name and how many, then the order as a segmented switch.
    let toolbar = widgets::hbox(10);
    toolbar.add_css_class("list-toolbar");
    toolbar.add_css_class("page-head");
    toolbar.append(&widgets::label("Albums", "page-head-title"));
    let summary = widgets::label("", "page-head-count");
    summary.set_hexpand(true);
    summary.set_xalign(0.0);
    toolbar.append(&summary);
    // store::ALBUM_SORTS, with a shorter name for Recently added to fit the switch.
    let sorts: Vec<(String, String)> = store::ALBUM_SORTS
        .iter()
        .map(|(id, name)| (id.to_string(), if *id == "added" { "Added" } else { *name }.to_string()))
        .collect();
    let fill = {
        let (grid, summary) = (grid.clone(), summary.clone());
        move || {
            let albums = store::sorted_albums(&crate::prefs::get().album_sort);
            summary.set_text(&fmt::thousands(albums.len()));
            grid.set(&albums);
        }
    };
    let f = fill.clone();
    let sort = nyx_ui::widgets::segmented(&sorts, &crate::prefs::get().album_sort, move |id| {
        crate::prefs::update(|p| p.album_sort = id);
        f();
    });
    let sort_sync = sort.clone();
    store::subscribe(&sort, move |change| {
        if change != store::Change::Library { return; }
        let selected = crate::prefs::get().album_sort;
        let mut child = sort_sync.first_child();
        for (id, _) in store::ALBUM_SORTS {
            let Some(widget) = child else { break };
            child = widget.next_sibling();
            if *id == selected && let Ok(button) = widget.downcast::<gtk::ToggleButton>() {
                button.set_active(true);
            }
        }
    });
    sort.set_tooltip_text(Some("Order the albums"));
    toolbar.append(&sort);
    grid_page.append(&toolbar);
    grid_page.append(&grid.root);
    inner.add_named(&grid_page, Some("grid"));
    let detail = widgets::vbox(0);
    detail.set_vexpand(true);
    inner.add_named(&detail, Some("detail"));
    page.body.append(&library_stack(&inner));
    fill();
    store::subscribe(&inner, move |c| {
        if c == store::Change::Plays && crate::prefs::get().album_sort == "plays" {
            fill();
        }
        if c != store::Change::Library {
            return;
        }
        fill();
        // Re-open the album being viewed with fresh data, or fall back to the grid.
        let key = UI.with(|u| u.borrow().as_ref().map(|u| u.open_key.clone())).unwrap_or_default();
        if !key.is_empty() {
            match store::album(&key) {
                Some(a) => open(&a),
                None => back(),
            }
        }
    });
    UI.with(|u| *u.borrow_mut() = Some(Ui { inner, detail, open_key: String::new() }));
}

fn back() {
    UI.with(|u| {
        if let Some(u) = u.borrow_mut().as_mut() {
            u.inner.set_visible_child_name("grid");
            u.open_key.clear();
        }
    });
}

/// Go to an album from anywhere.
pub fn show(key: &str) {
    window::navigate("albums");
    if let Some(a) = store::album(key) {
        open(&a);
    }
}

fn open(album: &Rc<Album>) {
    let Some(detail) = UI.with(|u| u.borrow().as_ref().map(|u| u.detail.clone())) else { return };
    while let Some(c) = detail.first_child() {
        detail.remove(&c);
    }
    detail.append(&views::back_button("Albums", back));

    let cover = Cover::new(if window::narrow() { 140 } else { 200 }, false);
    cover.set_key(&album.art);
    let mut meta = Vec::new();
    if let Some(y) = album.year {
        meta.push(y.to_string());
    }
    meta.push(fmt::count(album.tracks.len(), "song", "songs"));
    meta.push(fmt::total(album.duration));
    let a = album.clone();
    let actions = views::play_buttons(move || a.tracks.iter().map(|t| t.path.clone()).collect());
    let (header, _) = views::detail_header(
        Some(&cover),
        "Album",
        &album.title,
        &views::artist_link(&album.artist),
        &meta.join(" · "),
        &actions,
    );
    detail.append(&header);

    let various = album.tracks.iter().any(|t| t.artist != album.artist);
    let cols: &'static [Col] = if various {
        &[Col::Num, Col::Title, Col::Artist, Col::Time, Col::Plays]
    } else {
        &[Col::Num, Col::Title, Col::Time, Col::Plays]
    };
    let discs: std::collections::HashSet<u32> = album.tracks.iter().map(|t| t.disc_no.unwrap_or(1)).collect();
    let table = TrackTable::new(Options { cols, sortable: false, disc_sections: discs.len() > 1, ..Default::default() });
    table.set(&album.tracks);
    detail.append(&table.root);

    UI.with(|u| {
        if let Some(u) = u.borrow_mut().as_mut() {
            u.open_key = album.key.clone();
            u.inner.set_visible_child_name("detail");
        }
    });
}
