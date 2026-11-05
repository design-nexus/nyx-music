//! Songs: the whole library in one sortable table.

use super::{library_stack, scan_banner};
use crate::library::store;
use crate::tracklist::{Options, TrackTable};
use crate::widgets::{self, Page};
use crate::{fmt, views};
use gtk::prelude::*;

pub fn build(page: &Page) {
    page.body.append(&scan_banner());
    let content = widgets::vbox(0);
    let toolbar = widgets::hbox(12);
    toolbar.add_css_class("list-toolbar");
    let summary = widgets::label("", "dim");
    summary.add_css_class("mono");
    summary.set_hexpand(true);
    summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
    toolbar.append(&summary);
    let table = TrackTable::new(Options { key: Some("songs"), ..Default::default() });
    let filter = views::filter_entry(&table, "Filter songs");
    toolbar.append(&filter);
    let t = table.clone();
    toolbar.append(&views::play_buttons(move || t.paths()));
    content.append(&toolbar);
    content.append(&table.root);
    page.body.append(&library_stack(&content));

    let show_summary = {
        let (summary, table) = (summary.clone(), table.clone());
        move || {
            let tracks = store::tracks();
            let shown = table.shown() as usize;
            summary.set_text(&if shown < tracks.len() {
                format!("{} of {}", fmt::thousands(shown), fmt::count(tracks.len(), "song", "songs"))
            } else {
                let secs: f64 = tracks.iter().map(|t| t.duration).sum();
                format!("{} · {}", fmt::count(tracks.len(), "song", "songs"), fmt::total(secs))
            });
        }
    };
    let s = show_summary.clone();
    filter.connect_search_changed(move |_| s());
    let refresh = move || {
        table.set(&store::tracks());
        show_summary();
    };
    refresh();
    store::subscribe(&content, move |c| {
        if c == store::Change::Library {
            refresh();
        }
    });
}
