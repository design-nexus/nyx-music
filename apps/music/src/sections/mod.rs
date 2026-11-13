//! Every page in the sidebar, in order. Playlists are added to the sidebar
//! as they're made (see `playlist`).

use crate::library::store;
use crate::widgets::{self, Page};
use crate::{fmt, paths};
use gtk::prelude::*;

pub mod albums;
pub mod artists;
pub mod equalizer;
pub mod folders;
pub mod home;
pub mod now_playing;
pub mod playlist;
pub mod podcasts;
pub mod queue;
pub mod radio;
pub mod search;
pub mod settings;
pub mod smart;
pub mod songs;

pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub group: &'static str,
    pub description: &'static str,
    pub build: fn(&Page),
    /// The page manages its own scrolling (tables, grids).
    pub fill: bool,
    /// Listed in the sidebar (search isn't).
    pub nav: bool,
}

pub fn all() -> Vec<Section> {
    let s = |id, title, icon, group, description, build, fill| Section {
        id,
        title,
        icon,
        group,
        description,
        build,
        fill,
        nav: true,
    };
    vec![
        s(
            "home",
            "Home",
            "go-home-symbolic",
            "",
            "Albums to jump back into, new episodes, your stations and what you play most.",
            home::build,
            false,
        ),
        s(
            "now-playing",
            "Now playing",
            "music-spectrum-symbolic",
            "",
            "The current song, its spectrum and what's up next.",
            now_playing::build,
            true,
        ),
        s("songs", "Songs", "music-note-symbolic", "Library", "Every song in your library.", songs::build, true),
        s("albums", "Albums", "media-optical-symbolic", "Library", "Your albums, by artist.", albums::build, true),
        s(
            "artists",
            "Artists",
            "avatar-default-symbolic",
            "Library",
            "Everyone in your library, and their albums.",
            artists::build_artists,
            true,
        ),
        s("genres", "Genres", "music-genre-symbolic", "Library", "Your music by genre.", artists::build_genres, true),
        s("folders", "Folders", "folder-music-symbolic", "Library", "Your music as it's laid out on disk.", folders::build, true),
        s(
            "recent",
            "Recent and most played",
            "document-open-recent-symbolic",
            "Library",
            "The albums that joined your library last, and the songs you've played lately and most.",
            smart::build,
            true,
        ),
        s(
            "radio",
            "Radio",
            "music-radio-symbolic",
            "Online",
            "Thousands of stations from around the world, by genre and country.",
            radio::build,
            true,
        ),
        s(
            "podcasts",
            "Podcasts",
            "music-podcast-symbolic",
            "Online",
            "Shows you follow, the charts, and every episode to stream or download.",
            podcasts::build,
            true,
        ),
        s("queue", "Up next", "music-queue-symbolic", "Playlists", "What's playing now and next.", queue::build, true),
        // Opened from the top bar as a card over the window, not listed in the sidebar.
        Section {
            nav: false,
            ..s(
                "equalizer",
                "Equalizer",
                "music-equalizer-symbolic",
                "Sound",
                "Shape the sound with ten bands and a preamp.",
                equalizer::build,
                false,
            )
        },
        // Opened from the top bar as a dialog, not listed in the sidebar.
        Section {
            nav: false,
            ..s(
                "settings",
                "Settings",
                "emblem-system-symbolic",
                "App",
                "Library folders, playback, the analyzer and this window.",
                settings::build,
                false,
            )
        },
        Section {
            nav: false,
            ..s(
                "search",
                "Search",
                "system-search-symbolic",
                "Library",
                "Songs, albums and artists matching your search.",
                search::build,
                true,
            )
        },
    ]
}

/// A banner that shows while the library is being scanned.
pub fn scan_banner() -> gtk::Box {
    let b = widgets::banner("", false);
    b.set_visible(false);
    b.set_margin_bottom(14);
    let label = b.last_child().and_downcast::<gtk::Label>();
    let refresh = {
        let b = b.clone();
        move || match store::scanning() {
            Some((done, total)) => {
                b.set_visible(true);
                if let Some(l) = &label {
                    l.set_markup(&if total == 0 {
                        "Looking for music…".to_string()
                    } else {
                        format!(
                            "Reading {} of {}…",
                            fmt::thousands(done),
                            fmt::count(total, "new or changed song", "new or changed songs")
                        )
                    });
                }
            }
            None => b.set_visible(false),
        }
    };
    refresh();
    store::subscribe(&b, move |c| {
        if c == store::Change::Scan {
            refresh();
        }
    });
    b
}

/// What to show when the library is empty: why, and how to fix it.
pub fn empty_library() -> gtk::Box {
    let roots = store::roots();
    let any = roots.iter().any(|r| r.is_dir());
    let shown = roots.first().map(|r| paths::pretty(r)).unwrap_or_else(|| "~/Music".into());
    if !any || roots.is_empty() {
        widgets::empty_state(
            "folder-music-symbolic",
            "No music folder",
            &format!("<tt>{}</tt> doesn't exist. Choose the folder your music is in.", glib_escape(&shown)),
            Some(("Choose folder", Box::new(settings::choose_folder))),
        )
    } else {
        widgets::empty_state(
            "folder-music-symbolic",
            "No music yet",
            &format!("No songs were found in <tt>{}</tt>. Add music there, or choose another folder.", glib_escape(&shown)),
            Some(("Choose folder", Box::new(settings::choose_folder))),
        )
    }
}

pub fn glib_escape(s: &str) -> String {
    gtk::glib::markup_escape_text(s).to_string()
}

/// A library view that swaps between its content and the empty state.
pub fn library_stack(content: &impl IsA<gtk::Widget>) -> gtk::Stack {
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_css_class("library-stack");
    stack.add_named(content, Some("content"));
    let empty_holder = widgets::vbox(0);
    empty_holder.set_vexpand(true);
    stack.add_named(&empty_holder, Some("empty"));
    let loading = gtk::Spinner::new();
    loading.set_spinning(true);
    loading.set_halign(gtk::Align::Center);
    loading.set_valign(gtk::Align::Center);
    loading.set_size_request(32, 32);
    stack.add_named(&loading, Some("loading"));
    let refresh = {
        let stack = stack.clone();
        move || {
            if !store::loaded() {
                stack.set_visible_child_name("loading");
            } else if let Some(e) = store::error() {
                while let Some(c) = empty_holder.first_child() {
                    empty_holder.remove(&c);
                }
                let msg = format!("<tt>{}</tt>", glib_escape(&e));
                empty_holder.append(&widgets::empty_state("dialog-warning-symbolic", "Couldn't open the library", &msg, None));
                stack.set_visible_child_name("empty");
            } else if store::tracks().is_empty() && store::scanning().is_none() {
                while let Some(c) = empty_holder.first_child() {
                    empty_holder.remove(&c);
                }
                empty_holder.append(&empty_library());
                stack.set_visible_child_name("empty");
            } else {
                stack.set_visible_child_name("content");
            }
        }
    };
    refresh();
    store::subscribe(&stack, move |_| refresh());
    stack
}
