//! The main window: the Nyx header (the app and the sidebar's toggle, search, the
//! mini player and settings), the navigation sidebar with the library's size at
//! its foot, and beside it a stack of pages built the first time they're shown
//! over the player bar.

use crate::library::store;
use crate::sections::{self, Section};
use crate::widgets;
use crate::{fmt, panel_dialog, player, playerbar, prefs, settings_dialog, theme};
use gtk::prelude::*;
use nyx_ui::chrome;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

struct Ui {
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    nav: gtk::Box,
    nav_items: HashMap<String, gtk::Button>,
    /// The app's name in the header; the icon alone says it in a narrow window.
    app_title: Option<gtk::Widget>,
    playlist_box: gtk::Box,
    search: gtk::SearchEntry,
    /// The current page's name, in the top bar.
    pages: HashMap<String, gtk::Widget>,
    sections: Vec<Section>,
    current: String,
    /// Where Esc in the search goes back to.
    before_search: String,
    /// Pages visited, for Back.
    history: Vec<String>,
    overlay: gtk::Overlay,
}

thread_local! {
    static UI: RefCell<Option<Rc<RefCell<Ui>>>> = const { RefCell::new(None) };
    static NARROW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// A playlist asked for before the playlists finished loading.
    static PENDING: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn ui() -> Option<Rc<RefCell<Ui>>> {
    UI.with(|u| u.borrow().clone())
}

pub fn present(app: &gtk::Application, section: Option<&str>) {
    if let Some(ui) = ui() {
        let window = ui.borrow().window.clone();
        if let Some(s) = section {
            navigate(s);
        }
        window.present();
        return;
    }
    theme::install();
    install_icons();
    build(app);
    let start = section.map(String::from).unwrap_or_else(|| nyx_theme::start_page("home", &prefs::get().last_section));
    navigate(&start);
    // Developer aid: MUSIC_SNAPSHOT=/path.png renders the window to a PNG
    // (invisibly) and quits, so layouts can be checked without a visible window.
    if let Some(out) = std::env::var_os("MUSIC_SNAPSHOT") {
        snapshot_and_quit(app, std::path::PathBuf::from(out));
        return;
    }
    if let Some(ui) = ui() {
        ui.borrow().window.present();
    }
    if prefs::take_broken() {
        toast("Your settings file couldn't be read, so defaults are in use. The old file is kept as settings.toml.bak.");
    }
}

/// Our own symbolic icons, for things the icon theme has no glyph for.
/// They're written to the cache once and added to the icon search path.
fn install_icons() {
    const ICONS: &[(&str, &str)] = &[
        ("music-note-symbolic.svg", include_str!("../data/icons/music-note-symbolic.svg")),
        ("music-playlist-symbolic.svg", include_str!("../data/icons/music-playlist-symbolic.svg")),
        ("music-queue-symbolic.svg", include_str!("../data/icons/music-queue-symbolic.svg")),
        ("music-equalizer-symbolic.svg", include_str!("../data/icons/music-equalizer-symbolic.svg")),
        ("music-genre-symbolic.svg", include_str!("../data/icons/music-genre-symbolic.svg")),
        ("music-spectrum-symbolic.svg", include_str!("../data/icons/music-spectrum-symbolic.svg")),
        ("music-radio-symbolic.svg", include_str!("../data/icons/music-radio-symbolic.svg")),
        ("music-podcast-symbolic.svg", include_str!("../data/icons/music-podcast-symbolic.svg")),
        ("music-new-symbolic.svg", include_str!("../data/icons/music-new-symbolic.svg")),
    ];
    let dir = crate::paths::cache_dir().join("icons");
    for (name, svg) in ICONS {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*svg) {
            let _ = crate::cmd::atomic_write(&path, svg);
        }
    }
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(&dir);
    }
}

/// Where a group starts in the icon rail: a short bar in its colour.
fn rail_divider(section: &str) -> gtk::Separator {
    let bar = gtk::Separator::new(gtk::Orientation::Horizontal);
    bar.add_css_class("nav-divider");
    bar.add_css_class("compact-show");
    bar.add_css_class(&format!("section-{section}"));
    bar.set_visible(false);
    bar
}

/// A sidebar group's heading in its section's colour (Library the app's accent,
/// the others from the theme's `[sections.music]`); Playlists has its + for a new one.
fn group_heading(group: &str) -> (gtk::Separator, gtk::Box) {
    let section = if group == "Library" { nyx_ui::sidebar::ACCENT_SECTION.to_string() } else { group.to_lowercase() };
    let heading = if group == "Playlists" {
        nyx_ui::sidebar::group_label_with_add(group, false, "New playlist, or import one", || {
            sections::playlist::new_dialog(Vec::new())
        })
    } else {
        nyx_ui::sidebar::group_label(group, false)
    };
    nyx_ui::sidebar::set_section(&heading, &section);
    (rail_divider(&section), heading)
}

fn build(app: &gtk::Application) {
    let window =
        gtk::ApplicationWindow::builder().application(app).title("Music").default_width(1120).default_height(800).build();
    window.add_css_class("music-window");
    // No client-side titlebar: Hyprland manages the window.
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    nyx_ui::keys::install(&window, nyx_ui::keys::Common { settings: Some(settings_dialog::open), ..Default::default() });
    window.set_icon_name(Some("io.github.design_nexus.nyx.Music"));
    // Closing Music stops the music, even when a call started it in the background first.
    let a = app.clone();
    window.connect_close_request(move |_| {
        a.quit();
        glib::Propagation::Stop
    });

    let sections = sections::all();

    // ----- Sidebar -----
    let nav = gtk::Box::new(gtk::Orientation::Vertical, 0);
    nav.add_css_class("settings-navigation");
    nav.set_hexpand(false);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut nav_items = HashMap::new();
    let mut counts = HashMap::new();
    let playlist_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut last_group = "";
    for s in sections.iter().filter(|s| s.nav) {
        // Home and Now playing need no heading: they're what the sidebar starts with.
        if s.group != last_group {
            if !s.group.is_empty() {
                let (divider, heading) = group_heading(s.group);
                list.append(&divider);
                list.append(&heading);
            }
            last_group = s.group;
        }
        let item = nyx_ui::sidebar::nav_item(s.icon, s.title, s.description);
        let id = s.id;
        item.button.connect_clicked(move |_| navigate(id));
        list.append(&item.button);
        nav_items.insert(s.id.to_string(), item.button.clone());
        if COUNTED.contains(&s.id) {
            counts.insert(s.id, item);
        }
        if s.id == "queue" {
            list.append(&playlist_box);
        }
    }
    let counts = Rc::new(counts);
    refresh_counts(&counts);
    {
        let c = counts.clone();
        store::subscribe(&list, move |_| refresh_counts(&c));
        let c = counts.clone();
        crate::online::subscribe(&list, move |_| refresh_counts(&c));
        let c = counts.clone();
        player::subscribe(&list, move |e| {
            if e == player::Event::Queue {
                refresh_counts(&c);
            }
        });
    }
    let nav_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls with wheel, trackpad and keyboard; no visible scrollbar.
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&list)
        .build();
    nav.append(&nav_scroll);
    nav.append(&library_readout());

    // ----- Content -----
    let stack = gtk::Stack::new();
    stack.add_css_class("settings-content");
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 160 });

    let search = search_entry();
    let bar = playerbar::build();

    // The header: the app and the sidebar's toggle, search, the mini player, Settings.
    let header = chrome::app_header(&window, "music-note-symbolic", "Music");
    let collapse = nyx_ui::widgets::bar_button("sidebar-show-symbolic", "Collapse the sidebar (Ctrl+B)");
    collapse.connect_clicked(|_| toggle_sidebar());
    header.start.append(&collapse);
    header.center.append(&search);
    let mini = nyx_ui::widgets::bar_button("view-restore-symbolic", "Mini player (Ctrl+M)");
    mini.connect_clicked(|_| toggle_mini_player());
    header.end.append(&mini);
    header.end.append(&chrome::settings_button(settings_dialog::open));

    // The player bar runs across the window's foot, under the sidebar and the pages.
    let frame = nyx_ui::frame::AppFrame::new(&header.root);
    frame.set_sidebar(&nav);
    frame.set_page(&stack);
    frame.set_foot(&bar);
    let body = frame.body.clone();
    // The frame's minimum width doesn't hold the window open: a tiled window gets
    // the compositor's width even while the wide layout is wider, and goes narrow.
    let body_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .vexpand(true)
        .child(&frame.root)
        .build();

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&body_scroll));
    window.set_child(Some(&overlay));

    install_keys(&window, &search);

    // Narrow windows (a tiled half-screen) get an icon-only sidebar: below 980,
    // or whenever the wide layout needs more than the window has.
    let wide_min = Rc::new(std::cell::Cell::new(0));
    let apply_width = {
        let (nav, body, bar) = (nav.clone(), body.clone(), bar.clone());
        move |w: &gtk::ApplicationWindow| {
            let width = if w.width() > 0 { w.width() } else { w.default_width() };
            if !NARROW.with(|n| n.get()) {
                let min = |w: &gtk::Box| w.measure(gtk::Orientation::Horizontal, -1).0;
                wide_min.set(min(&body).max(min(&bar)));
            }
            let narrow = width > 0 && (width < 980 || width < wide_min.get());
            settings_dialog::fit(w);
            panel_dialog::fit(w);
            if narrow == NARROW.with(|n| n.get()) && nav.has_css_class("sized") {
                return;
            }
            nav.add_css_class("sized");
            set_narrow(narrow);
            apply_compact(&nav, narrow || prefs::get().sidebar_collapsed);
        }
    };
    let aw = apply_width.clone();
    window.connect_default_width_notify(move |w| aw(w));
    let aw = apply_width.clone();
    window.connect_realize(move |w| aw(w));
    // Tiled windows are resized by the compositor without touching the default
    // size: an invisible layer over the whole window reports each real size
    // change, and the layout follows on the next frame.
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    probe.set_can_focus(false);
    overlay.add_overlay(&probe);
    overlay.set_measure_overlay(&probe, false);
    {
        let (aw, w2) = (apply_width.clone(), window.clone());
        probe.connect_resize(move |_, _, _| {
            let (aw, w2) = (aw.clone(), w2.clone());
            // After this layout pass, when the window's width is the new one.
            glib::idle_add_local_once(move || aw(&w2));
        });
    }
    // A slow fallback, in case a resize slips by.
    let w2 = window.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
        apply_width(&w2);
        glib::ControlFlow::Continue
    });

    let ui = Ui {
        window,
        stack,
        nav,
        nav_items,
        app_title: find_class(header.root.upcast_ref(), "app-title"),
        playlist_box: playlist_box.clone(),
        search,
        pages: HashMap::new(),
        sections,
        current: String::new(),
        before_search: String::new(),
        history: Vec::new(),
        overlay,
    };
    UI.with(|u| *u.borrow_mut() = Some(Rc::new(RefCell::new(ui))));

    store::subscribe(&playlist_box, |c| {
        if c == store::Change::Playlists {
            refresh_playlists();
        }
    });
    refresh_playlists();
}

fn find_class(w: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    if w.has_css_class(class) {
        return Some(w.clone());
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(found) = find_class(&c, class) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

const SEARCH_HINT: &str = "Search songs, albums and artists  (Ctrl+F)";
const SEARCH_HINT_NARROW: &str = "Search music";

/// The library search, in the middle of the header.
fn search_entry() -> gtk::SearchEntry {
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(SEARCH_HINT));
    search.update_property(&[gtk::accessible::Property::Label("Search the library")]);
    search.add_css_class("bar-search");
    search.set_width_chars(40);
    search.connect_search_changed(|e| on_search(&e.text()));
    search.connect_activate(|_| sections::search::focus_results());
    search.connect_stop_search(|e| e.set_text(""));
    search
}

/// Sections whose sidebar row shows how many things they hold (Podcasts: its new episodes).
const COUNTED: &[&str] = &["songs", "albums", "artists", "genres", "radio", "podcasts", "queue"];

fn refresh_counts(counts: &HashMap<&'static str, nyx_ui::sidebar::NavItem>) {
    let loaded = store::loaded();
    for (id, item) in counts {
        let n = match *id {
            "songs" if loaded => store::tracks().len(),
            "albums" if loaded => store::albums().len(),
            "artists" if loaded => store::artists().len(),
            "genres" if loaded => store::genres().len(),
            "radio" => crate::online::favourites().len(),
            "podcasts" => crate::online::new_total(),
            "queue" => player::queue_items().0.len(),
            _ => 0,
        };
        if *id == "podcasts" {
            item.set_badge(Some(n as u64));
        } else {
            item.set_count(Some(n as u64));
        }
        set_readout_text(&item.button, n);
    }
}

/// The readout in thousands (1,284), as the rest of the app writes counts.
fn set_readout_text(button: &gtk::Button, n: usize) {
    let Some(content) = button.child() else { return };
    let mut child = content.first_child();
    while let Some(c) = child {
        if c.has_css_class("nav-readout")
            && let Some(l) = c.downcast_ref::<gtk::Label>()
        {
            l.set_text(&if n > 0 { fmt::thousands(n) } else { String::new() });
            // A badge is a pill around its number, not a band the row's height.
            l.set_valign(gtk::Align::Center);
        }
        child = c.next_sibling();
    }
}

/// The sidebar's foot: how big the library is, and a scan while one runs.
fn library_readout() -> gtk::Label {
    let readout = widgets::label("", "status-readout");
    readout.add_css_class("nav-foot");
    readout.add_css_class("compact-hide");
    readout.set_xalign(0.0);
    readout.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let refresh = {
        let readout = readout.clone();
        move || {
            let tracks = store::tracks();
            let mut text = match store::scanning() {
                Some((done, total)) if total > 0 => format!("Scanning {}% · ", done * 100 / total),
                Some(_) => "Scanning · ".to_string(),
                None => String::new(),
            };
            if store::loaded() {
                let secs: f64 = tracks.iter().map(|t| t.duration).sum();
                text.push_str(&fmt::count(tracks.len(), "song", "songs"));
                if secs > 0.0 {
                    text.push_str(" · ");
                    text.push_str(&fmt::total(secs));
                }
            }
            readout.set_text(&text);
        }
    };
    refresh();
    store::subscribe(&readout, move |_| refresh());
    readout
}

/// The sidebar shows only icons: hide the labels (everything marked
/// `compact-hide`), show what's marked `compact-show`, centre the icons and the toggle.
fn apply_compact(nav: &gtk::Box, compact: bool) {
    if compact {
        nav.add_css_class("compact");
    } else {
        nav.remove_css_class("compact");
    }
    set_compact_hidden(nav, compact);
}

pub fn toggle_sidebar() {
    prefs::update(|p| p.sidebar_collapsed = !p.sidebar_collapsed);
    let Some(ui) = ui() else { return };
    let nav = ui.borrow().nav.clone();
    apply_compact(&nav, NARROW.with(|n| n.get()) || prefs::get().sidebar_collapsed);
}

fn set_compact_hidden(root: &gtk::Box, compact: bool) {
    fn walk(w: &gtk::Widget, compact: bool) {
        if w.has_css_class("compact-hide") {
            // An empty count stays hidden when the labels come back.
            let empty = w.has_css_class("nav-readout") && w.downcast_ref::<gtk::Label>().is_some_and(|l| l.text().is_empty());
            w.set_visible(!compact && !empty);
        }
        if w.has_css_class("compact-show") {
            w.set_visible(compact);
        }
        // Icon-only: centre the icon in its pill, and the toggle in the column.
        if w.has_css_class("nav-item") {
            if compact {
                w.add_css_class("compact");
            } else {
                w.remove_css_class("compact");
            }
            if let Some(content) = w.downcast_ref::<gtk::Button>().and_then(|b| b.child()) {
                content.set_halign(if compact { gtk::Align::Center } else { gtk::Align::Fill });
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c, compact);
            child = c.next_sibling();
        }
    }
    walk(root.upcast_ref(), compact);
}

fn install_keys(window: &gtk::ApplicationWindow, search: &gtk::SearchEntry) {
    // Capture phase: these work wherever focus is, except while typing.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s2 = search.clone();
    let w2 = window.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let typing = gtk::prelude::GtkWindowExt::focus(&w2).is_some_and(|f| {
            f.is::<gtk::Text>() || f.ancestor(gtk::Entry::static_type()).is_some() || f.is::<gtk::SearchEntry>()
        });
        let alt = mods.contains(gdk::ModifierType::ALT_MASK);
        let plain = !ctrl && !alt;
        let done = glib::Propagation::Stop;
        if panel_dialog::is_open() && key == gdk::Key::Escape {
            panel_dialog::close();
            return done;
        }
        if settings_dialog::is_open() {
            return match key {
                gdk::Key::Escape => {
                    settings_dialog::escape();
                    done
                }
                gdk::Key::f if ctrl => {
                    settings_dialog::focus_search();
                    done
                }
                gdk::Key::q | gdk::Key::w if ctrl => {
                    w2.close();
                    done
                }
                _ => glib::Propagation::Proceed,
            };
        }
        match key {
            gdk::Key::f if ctrl => {
                s2.grab_focus();
                done
            }
            gdk::Key::F1 => {
                show_shortcuts();
                done
            }
            gdk::Key::b if ctrl => {
                toggle_sidebar();
                done
            }
            gdk::Key::q | gdk::Key::w if ctrl => {
                w2.close();
                done
            }
            gdk::Key::l if ctrl => {
                navigate("now-playing");
                done
            }
            gdk::Key::m if ctrl => {
                toggle_mini_player();
                done
            }
            gdk::Key::Up | gdk::Key::Down if ctrl && !typing => {
                let step = if key == gdk::Key::Up { 0.05 } else { -0.05 };
                player::set_volume(prefs::get().volume + step);
                done
            }
            gdk::Key::Left if alt => {
                back();
                done
            }
            _ if ctrl && !alt && key.to_unicode().and_then(|c| c.to_digit(10)).is_some_and(|d| d >= 1) => {
                let n = key.to_unicode().and_then(|c| c.to_digit(10)).unwrap_or(1) as usize;
                let id = ui().and_then(|u| u.borrow().sections.iter().filter(|s| s.nav).nth(n - 1).map(|s| s.id));
                if let Some(id) = id {
                    navigate(id);
                }
                done
            }
            gdk::Key::space if !typing => {
                player::toggle();
                done
            }
            gdk::Key::m if plain && !typing => {
                player::set_muted(!prefs::get().muted);
                done
            }
            gdk::Key::s if plain && !typing => {
                player::set_shuffle(!player::shuffle());
                done
            }
            gdk::Key::r if plain && !typing => {
                player::cycle_repeat();
                done
            }
            gdk::Key::question if !ctrl && !typing => {
                show_shortcuts();
                done
            }
            gdk::Key::Left if ctrl && !typing => {
                player::previous();
                done
            }
            gdk::Key::Right if ctrl && !typing => {
                player::next();
                done
            }
            gdk::Key::Escape if !s2.text().is_empty() => {
                s2.set_text("");
                done
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(keys);

    // The mouse's back button.
    let mouse_back = gtk::GestureClick::new();
    mouse_back.set_button(8);
    mouse_back.set_propagation_phase(gtk::PropagationPhase::Capture);
    mouse_back.connect_pressed(|g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        back();
    });
    window.add_controller(mouse_back);

    // Bubble phase: plain arrows seek only when nothing focused used them.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, mods| {
        if !mods.is_empty() {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Left => {
                player::seek_by(-5.0);
                glib::Propagation::Stop
            }
            gdk::Key::Right => {
                player::seek_by(5.0);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(keys);
}

/// Every keyboard shortcut, for Settings and the `?` list.
pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Space"], "Play or pause"),
    (&["Ctrl", "←"], "Previous song"),
    (&["Ctrl", "→"], "Next song"),
    (&["←"], "Back 5 seconds"),
    (&["→"], "Forward 5 seconds"),
    (&["Ctrl", "↑"], "Volume up"),
    (&["Ctrl", "↓"], "Volume down"),
    (&["M"], "Mute or unmute"),
    (&["S"], "Shuffle on or off"),
    (&["R"], "Repeat: off, all, this song"),
    (&["Ctrl", "F"], "Search the library"),
    (&["Esc"], "Clear the search"),
    (&["Ctrl", "M"], "Mini player"),
    (&["Ctrl", "L"], "Now playing"),
    (&["Ctrl", "1–9"], "Go to a page in the sidebar"),
    (&["Alt", "←"], "Back"),
    (&["Ctrl", "B"], "Collapse or expand the sidebar"),
    (&["Ctrl", ","], "Settings"),
    (&["F1"], "Show these shortcuts (or ?)"),
    (&["Ctrl", "Q"], "Close"),
];

/// The shortcuts in a dialog.
pub fn show_shortcuts() {
    let (dialog, card) = widgets::dialog("Keyboard shortcuts", 520);
    let list = widgets::vbox(0);
    list.add_css_class("group-list");
    for (keys, what) in SHORTCUTS {
        list.append(&widgets::row(what, "", Some(widgets::key_caps(keys).upcast_ref())));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls without a scrollbar, which would cover the key caps.
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_height(true)
        .max_content_height(560)
        .child(&list)
        .build();
    card.append(&scroll);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    let d = dialog.clone();
    close.connect_clicked(move |_| d.close());
    card.append(&close);
    dialog.present();
}

/// Back: close the detail view open on this page (an album, an artist, a
/// podcast…) or, with none open, return to the page before.
pub fn back() {
    let Some(ui) = ui() else { return };
    let page = {
        let u = ui.borrow();
        u.pages.get(&u.current).cloned()
    };
    if let Some(button) = page.as_ref().and_then(visible_back_button) {
        button.emit_clicked();
        return;
    }
    let prev = ui.borrow_mut().history.pop();
    if let Some(id) = prev {
        go(&id, false);
    }
}

/// The player bar's song: open Now playing, or from there, go back to the
/// page before it.
pub fn toggle_now_playing() {
    if current() != "now-playing" {
        navigate("now-playing");
        return;
    }
    let prev = ui().and_then(|u| u.borrow_mut().history.pop());
    go(prev.as_deref().unwrap_or("home"), false);
}

/// The player bar's Lyrics: Now playing with its lyrics showing, or back from them.
pub fn toggle_lyrics() {
    if current() == "now-playing" && prefs::get().now_view == "lyrics" {
        toggle_now_playing();
        return;
    }
    sections::now_playing::show_lyrics();
    navigate("now-playing");
    page_changed("now-playing");
}

/// The player bar's Up next: the queue, or back from it.
pub fn toggle_queue() {
    if current() == "queue" {
        let prev = ui().and_then(|u| u.borrow_mut().history.pop());
        go(prev.as_deref().unwrap_or("home"), false);
    } else {
        navigate("queue");
    }
}

fn visible_back_button(w: &gtk::Widget) -> Option<gtk::Button> {
    if !w.is_mapped() {
        return None;
    }
    if w.has_css_class("back-button") {
        return w.downcast_ref::<gtk::Button>().cloned();
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(b) = visible_back_button(&c) {
            return Some(b);
        }
        child = c.next_sibling();
    }
    None
}

fn on_search(text: &str) {
    let Some(ui) = ui() else { return };
    let q = text.trim().to_string();
    if q.is_empty() {
        let back = std::mem::take(&mut ui.borrow_mut().before_search);
        if ui.borrow().current == "search" {
            navigate(if back.is_empty() { "home" } else { &back });
        }
        return;
    }
    if ui.borrow().current != "search" {
        let cur = ui.borrow().current.clone();
        ui.borrow_mut().before_search = cur;
    }
    navigate("search");
    sections::search::set_query(&q);
}

fn set_narrow(narrow: bool) {
    NARROW.with(|n| n.set(narrow));
    playerbar::set_narrow(narrow);
    crate::tracklist::set_narrow(narrow);
    let Some(ui) = ui() else { return };
    let u = ui.borrow();
    if let Some(t) = &u.app_title {
        t.set_visible(!narrow);
    }
    u.search.set_placeholder_text(Some(if narrow { SEARCH_HINT_NARROW } else { SEARCH_HINT }));
    u.search.set_width_chars(if narrow { 24 } else { 40 });
    for page in u.pages.values() {
        mark_page(page, narrow);
    }
}

pub fn narrow() -> bool {
    NARROW.with(|n| n.get())
}

fn mark_page(page: &gtk::Widget, narrow: bool) {
    let body = page
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(|s| s.child())
        .and_then(|v| v.first_child())
        .unwrap_or_else(|| page.clone());
    if narrow {
        body.add_css_class("narrow");
    } else {
        body.remove_css_class("narrow");
    }
}

fn ensure_built(id: &str) -> bool {
    let Some(ui) = ui() else { return false };
    if ui.borrow().pages.contains_key(id) {
        return true;
    }
    let page: gtk::Widget = if let Some(pid) = id.strip_prefix("playlist:").and_then(|p| p.parse::<i64>().ok()) {
        if !store::playlists().iter().any(|p| p.id == pid) {
            return false;
        }
        sections::playlist::build(pid)
    } else {
        let section = {
            let u = ui.borrow();
            u.sections.iter().find(|s| s.id == id).map(|s| (s.id, s.build, s.fill))
        };
        let Some((sid, build, fill)) = section else { return false };
        let page = widgets::page(sid);
        if fill {
            page.fill();
        }
        build(&page);
        page.root.upcast()
    };
    mark_page(&page, narrow());
    let stack = ui.borrow().stack.clone();
    stack.add_named(&page, Some(id));
    ui.borrow_mut().pages.insert(id.to_string(), page);
    true
}

pub fn navigate(id: &str) {
    go(id, true);
}

fn go(id: &str, record: bool) {
    let Some(ui) = ui() else { return };
    // Recently added, recently played and most played were pages of their own.
    if sections::smart::TABS.iter().any(|(t, _)| *t == id) {
        go("recent", record);
        sections::smart::set_tab(id);
        return;
    }
    // Settings and the equalizer are cards over the window, not pages.
    if id == "settings" || id == "equalizer" {
        if id == "settings" {
            settings_dialog::open();
        } else {
            open_equalizer();
        }
        if !ui.borrow().current.is_empty() {
            return;
        }
    }
    let id = if id == "settings" || id == "equalizer" { "home" } else { id };
    if id.starts_with("playlist:") && !store::loaded() {
        PENDING.with(|p| *p.borrow_mut() = Some(id.to_string()));
    }
    let id = if ensure_built(id) { id.to_string() } else { "home".to_string() };
    if !ensure_built(&id) {
        return;
    }
    let mut u = ui.borrow_mut();
    if record && u.current != id && !u.current.is_empty() && u.current != "search" {
        let prev = u.current.clone();
        u.history.push(prev);
        if u.history.len() > 50 {
            u.history.remove(0);
        }
    }
    if let Some(prev) = u.nav_items.get(&u.current) {
        prev.remove_css_class("active");
    }
    if let Some(b) = u.nav_items.get(&id) {
        b.add_css_class("active");
    }
    u.stack.set_visible_child_name(&id);
    u.current = id.clone();
    let title = page_title(&u.sections, &id);
    chrome::set_title(&u.window, "Music", &title);
    let search = u.search.clone();
    drop(u);
    page_changed(&id);
    if id != "search" {
        if !search.text().is_empty() {
            ui.borrow_mut().before_search.clear();
            search.set_text("");
        }
        prefs::update(|p| p.last_section = id);
    }
}

type PageListener = (glib::WeakRef<gtk::Widget>, Box<dyn Fn(&str)>);

thread_local! {
    static PAGE_LISTENERS: RefCell<Vec<PageListener>> = const { RefCell::new(Vec::new()) };
}

/// Call `f` with the page's id whenever the window shows another page, while `owner` is alive.
pub fn on_page(owner: &impl IsA<gtk::Widget>, f: impl Fn(&str) + 'static) {
    PAGE_LISTENERS.with(|l| l.borrow_mut().push((owner.upcast_ref::<gtk::Widget>().downgrade(), Box::new(f))));
}

fn page_changed(id: &str) {
    PAGE_LISTENERS.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|(w, _)| w.upgrade().is_some());
        for (_, f) in l.iter() {
            f(id);
        }
    });
}

thread_local! {
    static MINI: RefCell<Option<gtk::Window>> = const { RefCell::new(None) };
}

/// A small window with just the player bar. It can't be resized, so Hyprland
/// floats it; closing it (or Ctrl+M again) leaves the main window as it was.
pub fn toggle_mini_player() {
    if let Some(w) = MINI.with(|m| m.borrow_mut().take()) {
        w.close();
        return;
    }
    let Some(main) = ui().map(|u| u.borrow().window.clone()) else { return };
    let mini = gtk::Window::builder().title("Mini player").default_width(560).resizable(false).build();
    if let Some(app) = main.application() {
        mini.set_application(Some(&app));
    }
    mini.add_css_class("music-window");
    mini.add_css_class("mini-player");
    mini.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    mini.set_child(Some(&playerbar::build()));
    mini.connect_close_request(|_| {
        MINI.with(|m| m.borrow_mut().take());
        glib::Propagation::Proceed
    });
    let keys = gtk::EventControllerKey::new();
    let m = mini.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        match key {
            gdk::Key::m | gdk::Key::w if ctrl => m.close(),
            gdk::Key::Escape => m.close(),
            gdk::Key::space => player::toggle(),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    mini.add_controller(keys);
    mini.present();
    MINI.with(|m| *m.borrow_mut() = Some(mini));
}

/// The equalizer, as a card over the window.
pub fn open_equalizer() {
    panel_dialog::open("equalizer", "Equalizer", sections::equalizer::build);
}

/// A page's name for the top bar: its section's title or the playlist's name.
fn page_title(sections: &[Section], id: &str) -> String {
    if let Some(pid) = id.strip_prefix("playlist:").and_then(|p| p.parse::<i64>().ok()) {
        return store::playlists().into_iter().find(|p| p.id == pid).map(|p| p.name).unwrap_or_default();
    }
    sections.iter().find(|s| s.id == id).map(|s| s.title.to_string()).unwrap_or_default()
}

/// Drop a page so it's rebuilt next time (a deleted playlist).
pub fn remove_page(id: &str) {
    let Some(ui) = ui() else { return };
    let old = ui.borrow_mut().pages.remove(id);
    if let Some(old) = old {
        ui.borrow().stack.remove(&old);
    }
    if ui.borrow().current == id {
        navigate("queue");
    }
}

fn refresh_playlists() {
    let Some(ui) = ui() else { return };
    let bx = ui.borrow().playlist_box.clone();
    while let Some(c) = bx.first_child() {
        bx.remove(&c);
    }
    ui.borrow_mut().nav_items.retain(|k, _| !k.starts_with("playlist:"));
    let current = ui.borrow().current.clone();
    let mut items = HashMap::new();
    for p in store::playlists() {
        let item = nyx_ui::sidebar::nav_item("music-playlist-symbolic", &p.name, &p.name);
        let button = item.button.clone();
        // Icon-only, every playlist would look the same: show its initial instead.
        if let Some(content) = button.child().and_downcast::<gtk::Box>() {
            if let Some(icon) = content.first_child() {
                icon.add_css_class("compact-hide");
            }
            let initial = widgets::label(&initial_of(&p.name), "nav-initial");
            initial.set_valign(gtk::Align::Center);
            initial.add_css_class("compact-show");
            initial.set_visible(false);
            content.prepend(&initial);
        }
        let id = format!("playlist:{}", p.id);
        if id == current {
            button.add_css_class("active");
        }
        let target = id.clone();
        button.connect_clicked(move |_| navigate(&target));
        sections::playlist::accept_drops(&button, p.id);
        bx.append(&button);
        ui.borrow_mut().nav_items.insert(id, button);
        items.insert(p.id, item);
    }
    let nav = ui.borrow().nav.clone();
    set_compact_hidden(&nav, nav.has_css_class("compact"));
    sections::playlist::load_items(move |lists| {
        for (p, songs) in lists {
            if let Some(item) = items.get(&p.id) {
                item.set_count(Some(songs.len() as u64));
                set_readout_text(&item.button, songs.len());
            }
        }
        if let Some(u) = self::ui() {
            let nav = u.borrow().nav.clone();
            set_compact_hidden(&nav, nav.has_css_class("compact"));
        }
    });
    if let Some(id) = PENDING.with(|p| p.borrow_mut().take()) {
        navigate(&id);
    }
}

/// The first letter or digit of a name, for the icon-only sidebar.
fn initial_of(name: &str) -> String {
    name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "#".into())
}

/// Show a short message at the bottom of the window.
pub fn toast(message: &str) {
    show_toast(message, None);
}

/// A message with a button (Undo), shown a little longer.
pub fn toast_action(message: &str, label: &str, action: impl Fn() + 'static) {
    show_toast(message, Some((label, Box::new(action))));
}

thread_local! {
    /// The toast showing now; a new one replaces it.
    static TOAST: RefCell<Option<gtk::Box>> = const { RefCell::new(None) };
}

type ToastAction<'a> = Option<(&'a str, Box<dyn Fn()>)>;

fn show_toast(message: &str, action: ToastAction<'_>) {
    let Some(ui) = ui() else {
        eprintln!("nyx-music: {message}");
        return;
    };
    let overlay = ui.borrow().overlay.clone();
    if let Some(old) = TOAST.with(|t| t.borrow_mut().take())
        && old.parent().is_some()
    {
        overlay.remove_overlay(&old);
    }
    let label = gtk::Label::new(Some(message));
    label.set_wrap(true);
    label.set_max_width_chars(70);
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bx.add_css_class("toast");
    bx.append(&label);
    let has_action = action.is_some();
    if let Some((text, f)) = action {
        let b = gtk::Button::with_label(text);
        b.add_css_class("flat");
        b.add_css_class("toast-action");
        b.set_valign(gtk::Align::Center);
        let (o, w) = (overlay.clone(), bx.downgrade());
        b.connect_clicked(move |_| {
            f();
            if let Some(w) = w.upgrade()
                && w.parent().is_some()
            {
                o.remove_overlay(&w);
            }
        });
        bx.append(&b);
    }
    bx.set_halign(gtk::Align::Center);
    bx.set_valign(gtk::Align::End);
    overlay.add_overlay(&bx);
    TOAST.with(|t| *t.borrow_mut() = Some(bx.clone()));
    let ms = if has_action { 6000 } else { 3500 };
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
        if bx.parent().is_some() {
            overlay.remove_overlay(&bx);
        }
    });
}

pub fn current() -> String {
    ui().map(|u| u.borrow().current.clone()).unwrap_or_default()
}

/// The layer over the window, for toasts and the settings dialog.
pub fn overlay() -> Option<gtk::Overlay> {
    ui().map(|u| u.borrow().overlay.clone())
}

pub fn window() -> Option<gtk::ApplicationWindow> {
    ui().map(|u| u.borrow().window.clone())
}

pub fn is_active() -> bool {
    window().is_some_and(|w| w.is_active() && w.is_visible())
}

fn snapshot_and_quit(app: &gtk::Application, out: std::path::PathBuf) {
    let Some(ui) = ui() else { return };
    let window = ui.borrow().window.clone();
    window.set_opacity(0.01);
    // A distinct title lets a window rule float it at a set size for screenshots.
    window.set_title(Some("Music snapshot"));
    window.set_default_size(
        std::env::var("MUSIC_SNAPSHOT_W").ok().and_then(|v| v.parse().ok()).unwrap_or(1180),
        std::env::var("MUSIC_SNAPSHOT_H").ok().and_then(|v| v.parse().ok()).unwrap_or(820),
    );
    window.present();
    if std::env::var_os("MUSIC_SNAPSHOT_MAX").is_some() {
        window.maximize();
    }
    let app = app.clone();
    let delay: u64 = std::env::var("MUSIC_SNAPSHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
    if std::env::var_os("MUSIC_SNAPSHOT_PLAY").is_some() {
        glib::timeout_add_local_once(std::time::Duration::from_millis(300), player::play);
    }
    // MUSIC_SNAPSHOT_ALBUM=<part of a title> opens that album first.
    if let Ok(want) = std::env::var("MUSIC_SNAPSHOT_ALBUM") {
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay.saturating_sub(800)), move || {
            if let Some(a) = store::albums().into_iter().find(|a| a.title.contains(&want)) {
                sections::albums::show(&a.key);
            }
        });
    }
    glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
        if let Some(child) = window.child() {
            let paintable = gtk::WidgetPaintable::new(Some(&child));
            let (w, h) = (child.width(), child.height());
            let snapshot = gtk::Snapshot::new();
            snapshot.append_color(&gdk::RGBA::BLACK, &gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
            paintable.snapshot(&snapshot, w as f64, h as f64);
            if let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) {
                let texture = renderer.render_texture(node, None);
                match texture.save_to_png(&out) {
                    Ok(()) => println!("snapshot {w}x{h} -> {}", out.display()),
                    Err(e) => eprintln!("snapshot failed: {e}"),
                }
            }
        }
        player::save();
        app.quit();
    });
}
