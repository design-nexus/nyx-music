use super::{api, config, downloads, libraries, model::*, playback, save_config, set_status, storage, subscribe};
use crate::{
    fmt,
    library::art::Cover,
    widgets::{self, Page},
    window,
};
use gtk::{glib, prelude::*};
use reqwest::Method;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

fn clear(b: &gtk::Box) {
    while let Some(w) = b.first_child() {
        b.remove(&w);
    }
}
fn button(text: &str, f: impl Fn() + 'static) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.connect_clicked(move |_| f());
    b
}
fn note(text: &str) -> gtk::Label {
    let l = widgets::label(text, "dim-label");
    l.set_wrap(true);
    l.set_xalign(0.0);
    l
}
fn progress_text(b: &Book) -> String {
    let p = storage::progress(&b.id);
    if p.conflict.is_some() {
        "Choose listening position".into()
    } else if p.finished {
        "Completed".into()
    } else if p.position > 0.0 {
        format!(
            "{} listened · {} left{}",
            fmt::time(p.position),
            fmt::time((b.duration - p.position).max(0.0)),
            if p.dirty { " · pending sync" } else { "" }
        )
    } else {
        fmt::time(b.duration)
    }
}
fn book_row(book: &Book) -> gtk::Box {
    let row = widgets::hbox(12);
    row.set_margin_top(6);
    row.set_margin_bottom(6);
    let cover = Cover::new(56, true);
    cover.set_key(&book.art);
    row.append(&cover.root);
    let col = widgets::vbox(3);
    col.set_hexpand(true);
    let title = widgets::label(&book.title, "row-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    col.append(&title);
    col.append(&note(
        &[book.author.as_str(), book.series.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
    ));
    col.append(&note(&progress_text(book)));
    row.append(&col);
    let b = book.clone();
    let play = widgets::icon_button("media-playback-start-symbolic", "Resume listening");
    play.connect_clicked(move |_| playback::start(b.clone()));
    if !book.podcast || book.id.episode.is_some() {
        row.append(&play);
    }
    let key = book.art.clone();
    let root = cover.root.downgrade();
    super::cover(book, move || {
        if root.upgrade().is_some() {
            cover.set_key(&key);
        }
    });
    row
}

fn progress_fraction(book: &Book) -> f64 {
    let p = storage::progress(&book.id);
    if p.finished || book.duration <= 0.0 {
        0.0
    } else {
        (p.position / book.duration).clamp(0.0, 1.0)
    }
}

fn time_left(book: &Book) -> String {
    let p = storage::progress(&book.id);
    if p.finished {
        "Completed".into()
    } else if p.position > 0.0 {
        format!("{} left", fmt::time((book.duration - p.position).max(0.0)))
    } else {
        fmt::time(book.duration)
    }
}

fn is_downloaded(book: &Book) -> bool {
    storage::downloads(&config()).iter().any(|d| d.state == "complete" && d.book.id == book.id)
}

/// Wide continue-listening card: cover, title/author/left, play.
fn continue_card(book: &Book) -> gtk::Box {
    let card = widgets::hbox(12);
    card.add_css_class("abs-cont-card");
    let cover = Cover::new(72, true);
    cover.set_placeholder("accessories-dictionary-symbolic");
    cover.set_key(&book.art);
    card.append(&cover.root);
    let key = book.art.clone();
    let ready = cover.clone();
    super::cover(book, move || ready.set_key(&key));
    let meta = widgets::vbox(2);
    meta.set_hexpand(true);
    let title = widgets::label(&book.title, "abs-cont-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    meta.append(&title);
    let author = widgets::label(&book.author, "dim");
    author.set_xalign(0.0);
    author.set_ellipsize(gtk::pango::EllipsizeMode::End);
    meta.append(&author);
    let left = widgets::label(&time_left(book), "abs-cont-left");
    left.set_xalign(0.0);
    meta.append(&left);
    let frac = progress_fraction(book);
    if frac > 0.0 && frac < 1.0 {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("abs-pbar-mini");
        bar.set_fraction(frac);
        meta.append(&bar);
    }
    card.append(&meta);
    let b = book.clone();
    let play = widgets::icon_button("media-playback-start-symbolic", "Resume");
    play.add_css_class("abs-play-ib");
    play.connect_clicked(move |_| playback::start(b.clone()));
    card.append(&play);
    let open = book.clone();
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| show(open.clone()));
    card.add_controller(click);
    card.set_cursor_from_name(Some("pointer"));
    card
}

/// Cover tile for a series / genre / collection in the facet grid.
fn facet_tile(name: &str, subtitle: &str, art: Option<&str>, on_open: impl Fn() + 'static) -> gtk::Box {
    let card = widgets::vbox(0);
    card.add_css_class("abs-book");
    card.set_size_request(148, -1);
    card.set_hexpand(false);
    let cover = Cover::new(148, true);
    cover.set_placeholder("accessories-dictionary-symbolic");
    if let Some(key) = art {
        cover.set_key(key);
    }
    card.append(&cover.root);
    let title = widgets::label(name, "abs-book-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(16);
    title.set_tooltip_text(Some(name));
    card.append(&title);
    if !subtitle.is_empty() {
        let sub = widgets::label(subtitle, "abs-book-author");
        sub.set_xalign(0.0);
        sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sub.set_max_width_chars(16);
        card.append(&sub);
    }
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| on_open());
    card.add_controller(click);
    card.set_cursor_from_name(Some("pointer"));
    card
}

/// Cover tile for the library grid.
fn book_tile(book: &Book) -> gtk::Box {
    let card = widgets::vbox(0);
    card.add_css_class("abs-book");
    card.set_size_request(148, -1);
    card.set_hexpand(false);
    let art = gtk::Overlay::new();
    let cover = Cover::new(148, true);
    cover.set_placeholder("accessories-dictionary-symbolic");
    cover.set_key(&book.art);
    art.set_child(Some(&cover.root));
    if is_downloaded(book) {
        let off = gtk::Image::from_icon_name("folder-download-symbolic");
        off.set_pixel_size(12);
        off.add_css_class("abs-offline-dot");
        off.set_halign(gtk::Align::Start);
        off.set_valign(gtk::Align::Start);
        off.set_margin_start(8);
        off.set_margin_top(8);
        art.add_overlay(&off);
    }
    card.append(&art);
    let key = book.art.clone();
    let ready = cover.clone();
    super::cover(book, move || ready.set_key(&key));
    let title = widgets::label(&book.title, "abs-book-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(16);
    title.set_tooltip_text(Some(&book.title));
    if playback::current_book().is_some_and(|b| b.id == book.id) {
        title.add_css_class("accent-text");
    }
    card.append(&title);
    let sub = if book.series.is_empty() { book.author.as_str() } else { book.series.as_str() };
    let author = widgets::label(sub, "abs-book-author");
    author.set_xalign(0.0);
    author.set_ellipsize(gtk::pango::EllipsizeMode::End);
    author.set_max_width_chars(16);
    card.append(&author);
    let frac = progress_fraction(book);
    if frac > 0.0 && frac < 1.0 {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("abs-pbar-mini");
        bar.set_fraction(frac);
        card.append(&bar);
    }
    let open = book.clone();
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| show(open.clone()));
    card.add_controller(click);
    card.set_cursor_from_name(Some("pointer"));
    card
}

fn section_head(title: &str, count: &str, action: Option<(&str, Box<dyn Fn()>)>) -> gtk::Box {
    let row = widgets::hbox(10);
    row.add_css_class("abs-shead");
    let t = widgets::label(title, "abs-shead-title");
    row.append(&t);
    if !count.is_empty() {
        let s = widgets::label(count, "dim");
        row.append(&s);
    }
    let grow = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    grow.set_hexpand(true);
    row.append(&grow);
    if let Some((label, f)) = action {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.add_css_class("link-button");
        b.connect_clicked(move |_| f());
        row.append(&b);
    }
    row
}
#[derive(Clone)]
struct Setup {
    title: gtk::Label,
    description: gtk::Label,
    action: gtk::Button,
    downloads: gtk::Button,
    spinner: gtk::Spinner,
}
const FILTERS: &[&str] = &["All", "Recently added", "In progress", "Completed", "Series", "Genres", "Collections"];

#[derive(Clone)]
struct Browser {
    stack: gtk::Stack,
    detail: gtk::Box,
    library: gtk::DropDown,
    filter: Rc<Cell<u32>>,
    /// Drill-in facet (series / genre / collection id + name).
    facet: Rc<RefCell<Option<(String, String)>>>,
    query: gtk::SearchEntry,
    status: gtk::Label,
    more: gtk::Button,
    continue_wrap: gtk::Box,
    continue_row: gtk::Box,
    lib_title: gtk::Label,
    lib_count: gtk::Label,
    facet_back: gtk::Button,
    grid: gtk::FlowBox,
    ids: Rc<RefCell<Vec<String>>>,
    page: Rc<Cell<u64>>,
    generation: Rc<Cell<u64>>,
    podcast: bool,
    setup: Setup,
    retry: gtk::Button,
}

fn clear_grid(grid: &gtk::FlowBox) {
    while let Some(c) = grid.first_child() {
        grid.remove(&c);
    }
}

fn grid_len(grid: &gtk::FlowBox) -> usize {
    let mut n = 0;
    let mut child = grid.first_child();
    while let Some(c) = child {
        n += 1;
        child = c.next_sibling();
    }
    n
}

fn insert_tile(grid: &gtk::FlowBox, tile: &impl IsA<gtk::Widget>) {
    grid.insert(tile, -1);
    if let Some(child) = grid.last_child() {
        // Keep each cell cover-sized so leftover row width stays at the end, not between tiles.
        child.set_size_request(148, -1);
        child.set_hexpand(false);
        child.set_halign(gtk::Align::Start);
        child.set_valign(gtk::Align::Start);
    }
}
thread_local! {static BROWSER:RefCell<Option<Browser>>=const{RefCell::new(None)};}
pub fn build(page: &Page) {
    let browser = browser(false);
    page.body.append(&browser.stack);
    BROWSER.with(|b| *b.borrow_mut() = Some(browser));
}
pub fn podcasts_widget() -> gtk::Widget {
    let root = widgets::vbox(8);
    let enabled = Rc::new(Cell::new(false));
    let r = root.clone();
    let e = enabled.clone();
    let refresh = move || {
        let c = config();
        let available = c.podcasts && libraries().iter().any(|l| l.media_type == "podcast");
        if available == e.get() {
            return;
        }
        e.set(available);
        clear(&r);
        if available {
            r.append(&note("Audiobookshelf podcasts"));
            r.append(&browser(true).stack);
        }
    };
    refresh();
    subscribe(&root, refresh);
    root.upcast()
}
fn browser(podcast: bool) -> Browser {
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    let browse = widgets::vbox(10);
    let detail = widgets::vbox(12);
    let detail_scroll =
        gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&detail).vexpand(true).build();
    stack.add_named(&browse, Some("browse"));
    stack.add_named(&detail_scroll, Some("detail"));
    let setup_box = widgets::vbox(12);
    setup_box.set_valign(gtk::Align::Center);
    setup_box.set_halign(gtk::Align::Fill);
    setup_box.set_margin_start(24);
    setup_box.set_margin_end(24);
    let setup_icon = gtk::Image::from_icon_name("accessories-dictionary-symbolic");
    setup_icon.set_pixel_size(48);
    setup_icon.add_css_class("dim");
    setup_box.append(&setup_icon);
    let setup_title = widgets::label("Audiobooks", "section-title");
    setup_title.set_wrap(true);
    setup_box.append(&setup_title);
    let setup_description = note("");
    setup_description.set_xalign(0.5);
    setup_description.set_justify(gtk::Justification::Center);
    setup_box.append(&setup_description);
    let setup_spinner = gtk::Spinner::new();
    setup_spinner.set_halign(gtk::Align::Center);
    setup_box.append(&setup_spinner);
    let setup_action = gtk::Button::with_label("Connect Audiobookshelf");
    setup_action.add_css_class("suggested-action");
    setup_action.set_halign(gtk::Align::Center);
    setup_action.connect_clicked(|_| {
        let state = super::connection();
        if state.phase == super::ConnectionPhase::Offline
            || (config().connected
                && state.phase == super::ConnectionPhase::Connected
                && !libraries().iter().any(|l| l.media_type == "book"))
        {
            super::refresh();
        } else {
            super::settings::open();
        }
    });
    setup_box.append(&setup_action);
    let setup_downloads = button("Open downloads", super::settings::open_downloads);
    setup_downloads.set_halign(gtk::Align::Center);
    setup_box.append(&setup_downloads);
    let setup = Setup {
        title: setup_title,
        description: setup_description,
        action: setup_action,
        downloads: setup_downloads,
        spinner: setup_spinner,
    };
    stack.add_named(&setup_box, Some("setup"));
    let library = gtk::DropDown::from_strings(&[]);
    library.set_tooltip_text(Some("Audiobookshelf library"));
    library.add_css_class("abs-library-dd");
    library.set_hexpand(false);
    library.set_halign(gtk::Align::Start);
    library.set_size_request(120, -1);
    let filter = Rc::new(Cell::new(0u32));
    let chips = widgets::hbox(6);
    chips.add_css_class("chips");
    chips.set_hexpand(true);
    let query = gtk::SearchEntry::new();
    query.set_placeholder_text(Some("Search this library"));
    query.set_size_request(200, -1);
    query.set_hexpand(false);
    let connection = widgets::hbox(8);
    connection.add_css_class("abs-banner");
    let label = note("");
    label.set_hexpand(true);
    connection.append(&label);
    let retry_connection = button("Retry connection", super::refresh);
    connection.append(&retry_connection);
    connection.append(&button("Dismiss", super::dismiss_connection_error));
    let line = connection.clone();
    subscribe(&connection, move || {
        let state = super::connection();
        let show = state.error.is_some() && state.phase == super::ConnectionPhase::Offline;
        line.set_visible(show);
        label.set_text(state.error.as_deref().unwrap_or_default());
        retry_connection.set_sensitive(!state.phase.busy());
    });
    connection.set_visible(false);
    browse.append(&connection);
    let top = widgets::hbox(8);
    top.add_css_class("abs-ptool");
    top.append(&library);
    top.append(&chips);
    top.append(&query);
    browse.append(&top);
    let status = note("");
    let status_row = widgets::hbox(8);
    status.set_hexpand(true);
    status_row.append(&status);
    let retry = gtk::Button::with_label("Retry");
    retry.set_visible(false);
    status_row.append(&retry);
    browse.append(&status_row);
    let main = widgets::vbox(0);
    main.add_css_class("abs-main");
    let continue_wrap = widgets::vbox(0);
    continue_wrap.set_visible(!podcast);
    let continue_row = widgets::hbox(12);
    continue_row.add_css_class("abs-cont-strip");
    let cont_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&continue_row)
        .build();
    cont_scroll.set_propagate_natural_height(true);
    let continue_head = section_head("Continue listening", "", None);
    continue_wrap.append(&continue_head);
    continue_wrap.append(&cont_scroll);
    main.append(&continue_wrap);
    let lib_head = widgets::hbox(10);
    lib_head.add_css_class("abs-shead");
    let facet_back = gtk::Button::with_label("Series");
    facet_back.add_css_class("flat");
    facet_back.add_css_class("link-button");
    facet_back.set_visible(false);
    lib_head.append(&facet_back);
    let lib_title = widgets::label("Library", "abs-shead-title");
    lib_head.append(&lib_title);
    let lib_count = widgets::label("", "dim");
    lib_head.append(&lib_count);
    let grow = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    grow.set_hexpand(true);
    lib_head.append(&grow);
    main.append(&lib_head);
    let grid = gtk::FlowBox::new();
    grid.add_css_class("abs-agrid");
    grid.set_selection_mode(gtk::SelectionMode::None);
    grid.set_homogeneous(false);
    grid.set_max_children_per_line(10);
    grid.set_min_children_per_line(1);
    grid.set_row_spacing(18);
    grid.set_column_spacing(18);
    grid.set_halign(gtk::Align::Start);
    grid.set_hexpand(true);
    main.append(&grid);
    let more = gtk::Button::with_label("Show more");
    more.set_halign(gtk::Align::Center);
    more.set_margin_top(12);
    main.append(&more);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .min_content_height(if podcast { 220 } else { 120 })
        .child(&main)
        .build();
    browse.append(&scroll);
    let ui = Browser {
        stack,
        detail,
        library,
        filter: filter.clone(),
        facet: Rc::default(),
        query,
        status,
        more,
        continue_wrap,
        continue_row,
        lib_title,
        lib_count,
        facet_back: facet_back.clone(),
        grid,
        ids: Rc::default(),
        page: Rc::new(Cell::new(0)),
        generation: Rc::new(Cell::new(0)),
        podcast,
        setup,
        retry,
    };
    // Filter chips (All · Recently added · …). Index order matches FILTERS / load().
    for (i, label) in FILTERS.iter().enumerate() {
        let b = gtk::Button::with_label(label);
        b.add_css_class("chip");
        if i == 0 {
            b.add_css_class("selected");
        }
        let u = ui.clone();
        let chips_box = chips.clone();
        b.connect_clicked(move |btn| {
            u.filter.set(i as u32);
            *u.facet.borrow_mut() = None;
            let mut child = chips_box.first_child();
            while let Some(c) = child {
                c.remove_css_class("selected");
                child = c.next_sibling();
            }
            btn.add_css_class("selected");
            fill_continue(&u);
            load(&u, true);
        });
        chips.append(&b);
    }
    let u = ui.clone();
    ui.retry.connect_clicked(move |_| load(&u, true));
    let u = ui.clone();
    ui.more.connect_clicked(move |_| {
        u.page.set(u.page.get() + 1);
        load(&u, false);
    });
    let u = ui.clone();
    ui.library.connect_selected_notify(move |_| {
        *u.facet.borrow_mut() = None;
        fill_continue(&u);
        load(&u, true);
    });
    let u = ui.clone();
    ui.query.connect_search_changed(move |_| load(&u, true));
    let u = ui.clone();
    ui.facet_back.connect_clicked(move |_| {
        *u.facet.borrow_mut() = None;
        load(&u, true);
    });
    let u = ui.clone();
    subscribe(&ui.stack, move || {
        update_libraries(&u);
        fill_continue(&u);
    });
    if podcast {
        let u = ui.clone();
        crate::online::subscribe(&ui.stack, move |change| {
            if change == crate::online::Change::Podcasts {
                load(&u, true);
            }
        });
    }
    // "See all" on Continue → In progress chip.
    let see = gtk::Button::with_label("See all");
    see.add_css_class("flat");
    see.add_css_class("link-button");
    let u = ui.clone();
    let chips_box = chips.clone();
    see.connect_clicked(move |_| {
        u.filter.set(2);
        *u.facet.borrow_mut() = None;
        let mut child = chips_box.first_child();
        let mut i = 0u32;
        while let Some(c) = child {
            if i == 2 {
                c.add_css_class("selected");
            } else {
                c.remove_css_class("selected");
            }
            child = c.next_sibling();
            i += 1;
        }
        fill_continue(&u);
        load(&u, true);
    });
    continue_head.append(&see);
    update_libraries(&ui);
    fill_continue(&ui);
    ui
}

fn fill_continue(u: &Browser) {
    if u.podcast {
        u.continue_wrap.set_visible(false);
        return;
    }
    clear(&u.continue_row);
    // Mockup hides Continue on Series / Genres / Collections.
    let show = u.filter.get() == 0 && u.query.text().trim().is_empty() && u.facet.borrow().is_none();
    u.continue_wrap.set_visible(show);
    if !show {
        return;
    }
    let c = config();
    let progress = storage::progress_map(&c);
    let mut books: Vec<_> = storage::books(&c)
        .into_iter()
        .filter(|b| !b.podcast && b.id.episode.is_none() && c.libraries.contains(&b.library))
        .collect();
    books.sort_by_key(|b| std::cmp::Reverse(progress.get(&b.id.key()).map_or(0, |p| p.updated)));
    let mut n = 0;
    for b in books {
        let p = progress.get(&b.id.key()).cloned().unwrap_or_default();
        if p.position > 0.0 && !p.finished {
            u.continue_row.append(&continue_card(&b));
            n += 1;
            if n >= 4 {
                break;
            }
        }
    }
    u.continue_wrap.set_visible(n > 0);
}
fn update_libraries(u: &Browser) {
    let c = config();
    let libs: Vec<_> = libraries()
        .into_iter()
        .filter(|l| {
            if u.podcast {
                l.media_type == "podcast" && c.podcasts
            } else {
                l.media_type == "book" && c.libraries.contains(&l.id)
            }
        })
        .collect();
    let ids: Vec<_> = libs.iter().map(|l| l.id.clone()).collect();
    u.library.set_visible(ids.len() >= 2);
    if ids != *u.ids.borrow() {
        *u.ids.borrow_mut() = ids;
        u.library.set_model(Some(&gtk::StringList::new(&libs.iter().map(|l| l.name.as_str()).collect::<Vec<_>>())));
        load(u, true);
    }
    let phase = super::connection().phase;
    let downloaded = storage::downloads(&c);
    let needs_setup = !c.connected || libs.is_empty() || phase == super::ConnectionPhase::NeedsSignIn;
    let keep_offline_detail = u.stack.visible_child_name().as_deref() == Some("detail")
        && downloaded.iter().any(|d| d.state == "complete" && d.book.id.key() == u.detail.widget_name());
    if needs_setup && !keep_offline_detail {
        u.stack.set_visible_child_name("setup");
        let (title, description, action) = match phase {
            super::ConnectionPhase::SigningIn => ("Signing in…", "Connecting to your Audiobookshelf server.", ""),
            super::ConnectionPhase::LoadingLibraries => {
                ("Loading your libraries…", "Your books will appear here when the connection is ready.", "")
            }
            super::ConnectionPhase::NeedsSignIn | super::ConnectionPhase::SignInFailed => (
                "Sign in to continue",
                "Reconnect your account to browse your libraries. Your downloads and listening progress are kept.",
                "Sign in",
            ),
            super::ConnectionPhase::Offline => (
                "Server unavailable",
                "Try connecting again, or listen to books already saved on this device.",
                "Retry connection",
            ),
            _ if !c.connected => (
                "Audiobooks",
                "Connect your Audiobookshelf server to browse books, resume listening, and save books for offline playback.",
                "Connect Audiobookshelf",
            ),
            _ if libraries().iter().any(|l| l.media_type == "book") => (
                "Choose your audiobook libraries",
                "Select at least one library in Audiobookshelf settings to start browsing.",
                "Choose libraries",
            ),
            _ => (
                "No audiobook libraries",
                "This account has no audiobook libraries. Refresh after a library has been added on your server.",
                "Refresh libraries",
            ),
        };
        u.setup.title.set_text(title);
        u.setup.description.set_text(description);
        u.setup.action.set_label(action);
        u.setup.action.set_visible(!phase.busy());
        u.setup.spinner.set_visible(phase.busy());
        u.setup.spinner.set_spinning(phase.busy());
        u.setup.downloads.set_visible(!downloaded.is_empty());
        u.more.set_visible(false);
    } else if !needs_setup && u.stack.visible_child_name().as_deref() == Some("setup") {
        u.stack.set_visible_child_name("browse");
    }
}
fn selected_library(u: &Browser) -> Option<String> {
    let ids = u.ids.borrow();
    if ids.is_empty() {
        return None;
    }
    if ids.len() == 1 {
        return ids.first().cloned();
    }
    ids.get(u.library.selected() as usize).cloned()
}

fn update_heading(u: &Browser) {
    let mode = u.filter.get();
    let facet = u.facet.borrow().clone();
    let drilled = facet.is_some() && mode >= 4;
    u.facet_back.set_visible(drilled);
    if drilled {
        let label = match mode {
            4 => "Series",
            5 => "Genres",
            _ => "Collections",
        };
        u.facet_back.set_label(label);
        u.lib_title.set_text(&facet.as_ref().map(|(_, n)| n.as_str()).unwrap_or(""));
    } else {
        let title = match mode {
            4 => "Series",
            5 => "Genres",
            6 => "Collections",
            _ => "Library",
        };
        u.lib_title.set_text(title);
    }
}

fn open_facet(u: &Browser, id: String, name: String) {
    *u.facet.borrow_mut() = Some((id, name));
    load(u, true);
}

/// Parse facet rows from ABS filterdata / collections / series list responses.
fn parse_facets(values: Option<&Vec<serde_json::Value>>) -> Vec<(String, String, String, Option<String>)> {
    // (id, name, subtitle, art_key)
    values
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    if let Some(s) = v.as_str() {
                        return Some((s.to_string(), s.to_string(), String::new(), None));
                    }
                    let name = v["name"].as_str().unwrap_or_default().to_string();
                    if name.is_empty() {
                        return None;
                    }
                    let id = v["id"].as_str().unwrap_or(name.as_str()).to_string();
                    let count = v["numBooks"]
                        .as_u64()
                        .or_else(|| v["books"].as_array().map(|b| b.len() as u64))
                        .unwrap_or(0);
                    let subtitle = if count > 0 { fmt::count(count as usize, "book", "books") } else { String::new() };
                    let art = v["books"].as_array().and_then(|books| books.first()).and_then(|b| {
                        let item = if b["libraryItem"].is_object() { &b["libraryItem"] } else { b };
                        let c = config();
                        let book = Book::parse(item, &c);
                        if book.id.item.is_empty() {
                            None
                        } else {
                            let _ = storage::cache_book(&book);
                            Some(book.art)
                        }
                    });
                    Some((id, name, subtitle, art))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn load(u: &Browser, reset: bool) {
    let Some(lib) = selected_library(u) else {
        return;
    };
    if reset {
        u.page.set(0);
    }
    let generation = u.generation.get() + 1;
    u.generation.set(generation);
    let page = u.page.get();
    let mode = u.filter.get();
    let query = u.query.text().trim().to_string();
    let facet = u.facet.borrow().clone();
    let c = config();
    let u = u.clone();
    let offline_config = c.clone();
    let offline_lib = lib.clone();
    let offline_query = query.to_lowercase();
    u.status.set_text("Loading…");
    u.retry.set_visible(false);
    u.more.set_sensitive(false);
    update_heading(&u);
    fill_continue(&u);

    // Facet overview grids (Series / Genres / Collections) — no drill-in yet.
    if mode >= 4 && facet.is_none() && query.is_empty() {
        let u2 = u.clone();
        api::run(
            async move {
                let path = match mode {
                    6 => format!("api/libraries/{}/collections?limit=1000", api::encode(&lib)),
                    4 => format!("api/libraries/{}/series?limit=1000&minified=1", api::encode(&lib)),
                    _ => format!("api/libraries/{}/filterdata", api::encode(&lib)),
                };
                let v = api::json(&c, Method::GET, &path, None).await?;
                let mut list = match mode {
                    6 => v["results"]
                        .as_array()
                        .or_else(|| v["collections"].as_array())
                        .or_else(|| v.as_array())
                        .cloned()
                        .unwrap_or_default(),
                    4 => v["results"]
                        .as_array()
                        .or_else(|| v["series"].as_array())
                        .or_else(|| v.as_array())
                        .cloned()
                        .unwrap_or_default(),
                    _ => v["genres"].as_array().cloned().unwrap_or_default(),
                };
                if mode == 4 && list.is_empty() {
                    let fd = api::json(&c, Method::GET, &format!("api/libraries/{}/filterdata", api::encode(&lib)), None).await?;
                    list = fd["series"].as_array().cloned().unwrap_or_default();
                }
                Ok(list)
            },
            move |r| {
                if u2.generation.get() != generation {
                    return;
                }
                u2.more.set_sensitive(true);
                u2.more.set_visible(false);
                u2.retry.set_visible(r.is_err());
                clear_grid(&u2.grid);
                match r {
                    Ok(list) => {
                        let facets = parse_facets(Some(&list));
                        let kind = match mode {
                            4 => "series",
                            5 => "genre",
                            _ => "collection",
                        };
                        u2.lib_count.set_text(&if facets.is_empty() {
                            String::new()
                        } else {
                            fmt::count(facets.len(), kind, &format!("{kind}s"))
                        });
                        for (id, name, sub, art) in facets {
                            let browser = u2.clone();
                            let id2 = id.clone();
                            let name2 = name.clone();
                            insert_tile(
                                &u2.grid,
                                &facet_tile(&name, &sub, art.as_deref(), move || open_facet(&browser, id2.clone(), name2.clone())),
                            );
                        }
                        u2.status.set_text(&if grid_len(&u2.grid) == 0 { "No matching items.".into() } else { String::new() });
                    }
                    Err(e) => u2.status.set_text(&e.to_string()),
                }
            },
        );
        return;
    }

    api::run(
        async move {
            let mut path = format!("api/libraries/{}/items?limit=60&page={page}&minified=1&include=progress", api::encode(&lib));
            match mode {
                1 => path.push_str("&sort=addedAt&desc=1"),
                2 | 3 => path.push_str(&format!(
                    "&filter=progress.{}",
                    api::encode(&glib::base64_encode(if mode == 2 { b"in-progress" } else { b"finished" }))
                )),
                4 | 5 => {
                    if let Some((id, _)) = &facet {
                        path.push_str(&format!(
                            "&filter={}.{}",
                            if mode == 4 { "series" } else { "genres" },
                            api::encode(&glib::base64_encode(id.as_bytes()))
                        ));
                    }
                }
                6 => {
                    if let Some((id, _)) = &facet {
                        path = format!("api/collections/{}", api::encode(id));
                    }
                }
                _ => path.push_str("&sort=media.metadata.title"),
            }
            if !query.is_empty() {
                path =
                    format!("api/libraries/{}/search?q={}&limit={}", api::encode(&lib), api::encode(&query), (page + 1) * 60 + 1);
            }
            let v = api::json(&c, Method::GET, &path, None).await?;
            let result_count = result_items(&v).len();
            let items = Some(result_items(&v));
            let books = items
                .map(|a| {
                    a.iter()
                        .skip(if query.is_empty() { 0 } else { page as usize * 60 })
                        .take(if mode == 6 && query.is_empty() { usize::MAX } else { 60 })
                        .map(|v| {
                            let v = if v["libraryItem"].is_object() { &v["libraryItem"] } else { v };
                            let b = Book::parse(v, &c);
                            let _ = storage::cache_book(&b);
                            if let Ok(remote) = serde_json::from_value::<ServerProgress>(v["userMediaProgress"].clone()) {
                                let _ = storage::change_progress(&b.id, |p| {
                                    if !p.dirty {
                                        p.position = remote.current_time;
                                        p.finished = remote.is_finished;
                                        p.base = remote;
                                    }
                                });
                            }
                            b
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let more = if query.is_empty() {
                mode != 6 && (page + 1) * 60 < v["total"].as_u64().unwrap_or(0)
            } else {
                result_count as u64 > (page + 1) * 60
            };
            Ok((books, more))
        },
        move |r| {
            if u.generation.get() != generation {
                return;
            }
            u.more.set_sensitive(true);
            u.retry.set_visible(r.is_err());
            let (books, more, message) = match r {
                Ok((b, m)) => (b, m, String::new()),
                Err(e) => {
                    let b = storage::books(&offline_config)
                        .into_iter()
                        .filter(|b| b.library == offline_lib && b.title.to_lowercase().contains(&offline_query))
                        .collect();
                    (b, false, format!("{e} Showing cached items."))
                }
            };
            if reset {
                clear_grid(&u.grid);
            }
            let books: Vec<_> = books
                .into_iter()
                .filter(|b| {
                    !b.id.item.is_empty()
                        && (!u.podcast
                            || !crate::online::podcasts()
                                .iter()
                                .any(|p| !b.feed.is_empty() && normalize_feed(&p.feed) == normalize_feed(&b.feed)))
                })
                .collect();
            for b in &books {
                insert_tile(&u.grid, &book_tile(b));
            }
            let total = grid_len(&u.grid);
            u.lib_count.set_text(&if total == 0 {
                String::new()
            } else {
                fmt::count(total, "book", "books")
            });
            u.more.set_visible(more);
            u.status.set_text(&if message.is_empty() {
                if total == 0 { "No matching items.".into() } else { String::new() }
            } else {
                message
            });
        },
    );
}
pub fn show(book: Book) {
    window::navigate("audiobooks");
    BROWSER.with(|b| {
        if let Some(u) = b.borrow().as_ref() {
            open_detail(u, book);
        }
    });
}
fn open_detail(u: &Browser, book: Book) {
    u.stack.set_visible_child_name("detail");
    render_detail(u, &book);
    let c = config();
    if !c.connected {
        return;
    }
    let id = book.id.clone();
    let expected = id.key();
    let u = u.clone();
    api::run(
        async move {
            let b = api::detail(&c, &id).await?;
            let _ = super::sync_one(&c, &id, None, 0.0).await;
            Ok(b)
        },
        move |r| match r {
            Ok(b) => {
                if u.detail.widget_name() == expected {
                    render_detail(&u, &b);
                }
            }
            Err(e) => set_status(e.to_string()),
        },
    );
}
fn render_detail(u: &Browser, book: &Book) {
    u.detail.set_widget_name(&book.id.key());
    clear(&u.detail);
    u.detail.set_margin_start(8);
    u.detail.set_margin_end(8);
    let stack = u.stack.clone();
    let back = gtk::Button::with_label("Library");
    back.add_css_class("flat");
    back.add_css_class("link-button");
    back.set_halign(gtk::Align::Start);
    back.connect_clicked(move |_| stack.set_visible_child_name("browse"));
    u.detail.append(&back);
    let head = widgets::hbox(18);
    head.add_css_class("abs-detail-hero");
    let cover = Cover::new(168, false);
    cover.set_key(&book.art);
    head.append(&cover.root);
    let cover_ready = cover.clone();
    let art_key = book.art.clone();
    super::cover(book, move || cover_ready.set_key(&art_key));
    let info = widgets::vbox(6);
    info.set_hexpand(true);
    let title = widgets::label(&book.title, "title-2");
    title.set_wrap(true);
    title.set_xalign(0.0);
    info.append(&title);
    if !book.author.is_empty() {
        let a = widgets::label(&book.author, "accent-text");
        a.set_xalign(0.0);
        info.append(&a);
    }
    if !book.narrator.is_empty() {
        info.append(&note(&format!("Narrated by {}", book.narrator)));
    }
    if !book.series.is_empty() {
        info.append(&note(&book.series));
    }
    if !book.genres.is_empty() {
        let tags = widgets::hbox(6);
        for g in &book.genres {
            let t = gtk::Label::new(Some(g));
            t.add_css_class("chip");
            tags.append(&t);
        }
        if is_downloaded(book) {
            let t = gtk::Label::new(Some("Downloaded"));
            t.add_css_class("chip");
            tags.append(&t);
        }
        info.append(&tags);
    }
    let frac = progress_fraction(book);
    if frac > 0.0 {
        let prog = widgets::vbox(4);
        let times = widgets::hbox(8);
        let p = storage::progress(&book.id);
        times.append(&widgets::label(&fmt::time(p.position), "dim"));
        let grow = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        grow.set_hexpand(true);
        times.append(&grow);
        times.append(&widgets::label(&time_left(book), "dim"));
        prog.append(&times);
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("abs-pbar-mini");
        bar.set_fraction(frac);
        prog.append(&bar);
        info.append(&prog);
    } else {
        info.append(&note(&progress_text(book)));
    }
    head.append(&info);
    u.detail.append(&head);
    let c = config();
    let p = storage::progress(&book.id);
    if let Some(remote) = p.conflict.clone() {
        u.detail.append(&note(&format!(
            "Listening position changed on both devices. This device: {}. Server: {}.",
            fmt::time(p.position),
            fmt::time(remote.current_time)
        )));
        let actions = widgets::hbox(8);
        let id = book.id.clone();
        let b = book.clone();
        let ui = u.clone();
        actions.append(&button("Use server", move || {
            super::resolve(id.clone(), false);
            render_detail(&ui, &b);
        }));
        let id = book.id.clone();
        let b = book.clone();
        let ui = u.clone();
        actions.append(&button("Use this device", move || {
            super::resolve(id.clone(), true);
            render_detail(&ui, &b);
        }));
        u.detail.append(&actions);
    }
    if !book.podcast || book.id.episode.is_some() {
        let actions = widgets::hbox(8);
        let b = book.clone();
        actions.append(&button("Resume", move || playback::start(b.clone())));
        let b = book.clone();
        actions.append(&button("Download", move || downloads::start(b.clone())));
        let id = book.id.clone();
        let duration = book.duration;
        actions.append(&button(if p.finished { "Mark unfinished" } else { "Mark completed" }, move || {
            if playback::current_book().is_some_and(|b| b.id == id) {
                playback::deactivate();
            }
            let mut pr = storage::progress(&id);
            pr.finished = !pr.finished;
            pr.position = if pr.finished { duration } else { 0.0 };
            pr.updated = storage::now();
            pr.dirty = true;
            pr.session = None;
            pr.revision += 1;
            let _ = storage::put("progress", &id.key(), &pr);
            super::sync_all();
            super::notify();
        }));
        u.detail.append(&actions);
        let controls = widgets::hbox(8);
        let target = book.clone();
        controls.append(&button(&format!("−{} s", c.backward), move || seek_target(&target, -config().backward)));
        let target = book.clone();
        controls.append(&button("Play / pause", move || {
            if playback::current_book().is_some_and(|b| b.id == target.id) {
                crate::player::toggle();
            } else {
                playback::start(target.clone());
            }
        }));
        let target = book.clone();
        controls.append(&button(&format!("+{} s", c.forward), move || seek_target(&target, config().forward)));
        let speed = gtk::SpinButton::with_range(0.5, 3.0, 0.05);
        speed.set_digits(2);
        speed.set_value(if playback::active() { playback::rate() } else { c.speed });
        speed.set_tooltip_text(Some("Playback speed"));
        let target = book.id.clone();
        speed.connect_value_changed(move |s| {
            let mut c = config();
            c.speed = s.value();
            save_config(c);
            if playback::current_book().is_some_and(|b| b.id == target) {
                playback::set_rate(s.value());
            }
        });
        controls.append(&speed);
        let target = book.clone();
        controls.append(&button("Bookmark", move || {
            if playback::current_book().is_some_and(|b| b.id == target.id) {
                playback::bookmark();
            } else {
                set_status("Start this book to add a bookmark at the current position.");
            }
        }));
        u.detail.append(&controls);
        let times = note("");
        let update = times.clone();
        let id = book.id.clone();
        crate::player::subscribe(&times, move |_| {
            if playback::current_book().is_some_and(|b| b.id == id) {
                update.set_text(&format!(
                    "{} elapsed · {} remaining",
                    fmt::time(playback::position()),
                    fmt::time((playback::duration() - playback::position()).max(0.0))
                ));
            }
        });
        u.detail.append(&times);
    }
    // Remote HTML is rendered as plain text, never interpreted as GTK markup.
    let description = plain_description(&book.description);
    u.detail.append(&note(&description));
    for (name, value) in &book.details {
        u.detail.append(&note(&format!("{name}: {value}")));
    }
    if book.podcast && book.id.episode.is_none() {
        u.detail.append(&note("Episodes"));
        for episode in &book.episodes {
            let b = episode.clone();
            let ui = u.clone();
            let btn = gtk::Button::new();
            btn.set_child(Some(&book_row(episode)));
            btn.connect_clicked(move |_| render_detail(&ui, &b));
            u.detail.append(&btn);
        }
    }
    if !book.chapters.is_empty() {
        u.detail.append(&note("Chapters"));
        for chapter in &book.chapters {
            let b = book.clone();
            let at = chapter.start;
            let id = b.id.clone();
            u.detail.append(&button(&format!("{}  {}", fmt::time(at), chapter.title), move || {
                if playback::current_book().is_some_and(|b| b.id == id) {
                    playback::seek(at);
                } else {
                    playback::start_at(b.clone(), Some(at), true);
                }
            }));
        }
    }
    let marks = super::bookmarks(book);
    if !marks.is_empty() {
        u.detail.append(&note("Bookmarks"));
        for m in marks {
            let b = book.clone();
            let pos = m.position;
            u.detail.append(&button(&m.title, move || playback::start_at(b.clone(), Some(pos), true)));
        }
    }
}
fn plain_description(s: &str) -> String {
    let wrapped = format!("<root>{s}</root>");
    if let Ok(doc) = roxmltree::Document::parse(&wrapped) {
        doc.descendants().filter_map(|n| if n.is_text() { n.text() } else { None }).collect::<Vec<_>>().join(" ")
    } else {
        s.to_string()
    }
}
pub fn sync_bookmarks(book: Book) {
    let c = config();
    if !c.connected {
        return;
    }
    api::run(async move { super::synchronize_bookmarks(&c, &book).await }, |r| {
        if let Err(e) = r {
            set_status(e.to_string());
        }
    });
}
pub fn downloads_page(page: &Page) {
    let list = widgets::vbox(10);
    page.body.append(&list);
    let l = list.clone();
    let refresh = move || {
        clear(&l);
        let downloads = storage::downloads(&config());
        let bytes: u64 = downloads.iter().map(|d| d.done).sum();
        l.append(&note(&format!("Audiobookshelf · {:.1} MiB stored", bytes as f64 / 1048576.0)));
        for d in downloads {
            let row = widgets::vbox(6);
            let b = d.book.clone();
            row.append(&button(&d.book.title, move || show(b.clone())));
            row.append(&note(&format!(
                "{} · {:.1} / {:.1} MiB {}",
                d.state,
                d.done as f64 / 1048576.0,
                d.total as f64 / 1048576.0,
                d.error
            )));
            let actions = widgets::hbox(8);
            if d.state == "downloading" {
                let id = d.book.id.clone();
                actions.append(&button("Pause", move || downloads::pause(&id, false)));
                let id = d.book.id.clone();
                actions.append(&button("Cancel", move || downloads::pause(&id, true)));
            } else if d.state == "complete" {
                let b = d.book.clone();
                actions.append(&button("Play offline", move || playback::start_offline(b.clone())));
            } else {
                let b = d.book.clone();
                actions.append(&button("Resume", move || downloads::start(b.clone())));
            }
            let id = d.book.id.clone();
            actions.append(&widgets::two_click("Remove", "Click again to delete", move || downloads::delete(&id)));
            row.append(&actions);
            l.append(&row);
        }
        for t in crate::online::downloaded() {
            let row = widgets::hbox(8);
            let path = t.path.clone();
            row.append(&button(&t.title, move || crate::player::play_tracks(vec![path.clone()], 0)));
            let path = t.path.clone();
            row.append(&widgets::two_click("Remove", "Click again to delete", move || crate::online::delete_download(&path)));
            l.append(&row);
        }
    };
    refresh();
    let f = Rc::new(refresh);
    let f2 = f.clone();
    subscribe(&list, move || f2());
    let f2 = f.clone();
    crate::online::subscribe(&list, move |_| f2());
    let weak = list.downgrade();
    glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
        if weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        if window::current() == "downloads" {
            f();
        }
        glib::ControlFlow::Continue
    });
}
/// Spoken Now playing: cover + meta on the left; chapters / bookmarks on the right.
pub fn now_playing() -> gtk::Box {
    let root = widgets::hbox(34);
    root.add_css_class("now-cols");
    root.set_margin_start(24);
    root.set_margin_end(24);
    root.set_margin_top(18);
    root.set_margin_bottom(18);

    let left = widgets::vbox(14);
    left.add_css_class("now-left");
    let cover = Cover::new(280, false);
    left.append(&cover.root);
    let title = widgets::label("", "now-title");
    title.set_wrap(true);
    title.set_xalign(0.0);
    left.append(&title);
    let author = widgets::label("", "now-artist");
    author.add_css_class("accent-text");
    author.set_xalign(0.0);
    left.append(&author);
    let narrator = widgets::label("", "dim");
    narrator.set_xalign(0.0);
    left.append(&narrator);
    let meta = widgets::label("", "dim");
    meta.set_xalign(0.0);
    left.append(&meta);
    let acts = widgets::hbox(8);
    acts.set_margin_top(8);
    let resume = gtk::Button::with_label("Resume");
    resume.add_css_class("suggested-action");
    resume.connect_clicked(|_| crate::player::toggle());
    acts.append(&resume);
    let details = gtk::Button::with_label("Details");
    details.connect_clicked(|_| {
        if let Some(b) = playback::current_book() {
            show(b);
        }
    });
    acts.append(&details);
    left.append(&acts);
    root.append(&left);

    let right = widgets::vbox(12);
    right.set_hexpand(true);
    let panel = widgets::vbox(10);
    panel.add_css_class("abs-now-panel");
    let view = Rc::new(Cell::new(0u32)); // 0 chapters, 1 bookmarks
    let seg = widgets::hbox(0);
    seg.add_css_class("segmented");
    let ch_btn = gtk::ToggleButton::with_label("Chapters");
    ch_btn.set_active(true);
    let bm_btn = gtk::ToggleButton::with_label("Bookmarks");
    bm_btn.set_group(Some(&ch_btn));
    seg.append(&ch_btn);
    seg.append(&bm_btn);
    panel.append(&seg);
    let times = widgets::hbox(8);
    let pos_l = widgets::label("0:00", "dim");
    pos_l.add_css_class("mono");
    let grow = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    grow.set_hexpand(true);
    let dur_l = widgets::label("0:00", "dim");
    dur_l.add_css_class("mono");
    times.append(&pos_l);
    times.append(&grow);
    times.append(&dur_l);
    panel.append(&times);
    let bar = gtk::ProgressBar::new();
    bar.add_css_class("abs-pbar-mini");
    panel.append(&bar);
    let tools = widgets::hbox(8);
    tools.append(&widgets::label("Speed", "dim"));
    let speed = gtk::Button::with_label(&format!("{:.2}×", config().speed));
    speed.add_css_class("chip");
    speed.connect_clicked(|_| {
        let next = match config().speed {
            s if (s - 1.0).abs() < 0.01 => 1.25,
            s if (s - 1.25).abs() < 0.01 => 1.5,
            s if (s - 1.5).abs() < 0.01 => 1.75,
            s if (s - 1.75).abs() < 0.01 => 2.0,
            _ => 1.0,
        };
        playback::set_rate(next);
        let mut c = config();
        c.speed = next;
        save_config(c);
    });
    tools.append(&speed);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    tools.append(&spacer);
    let add_bm = gtk::Button::with_label("Add bookmark");
    add_bm.connect_clicked(|_| playback::bookmark());
    tools.append(&add_bm);
    panel.append(&tools);
    right.append(&panel);

    let list_label = widgets::label("Chapters", "group-title");
    right.append(&list_label);
    let list = widgets::vbox(2);
    list.add_css_class("abs-now-list");
    let list_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&list)
        .build();
    right.append(&list_scroll);
    root.append(&right);

    let refill = {
        let list = list.clone();
        let list_label = list_label.clone();
        let view = view.clone();
        move || {
            clear(&list);
            let Some(b) = playback::current_book() else { return };
            let pos = playback::position();
            if view.get() == 0 {
                list_label.set_text("Chapters");
                for ch in &b.chapters {
                    let row = gtk::Button::new();
                    row.add_css_class("flat");
                    row.add_css_class("abs-ch");
                    if pos >= ch.start && b.chapters.iter().find(|c| c.start > ch.start).map_or(true, |n| pos < n.start) {
                        row.add_css_class("active");
                    }
                    let inner = widgets::hbox(12);
                    let tm = widgets::label(&fmt::time(ch.start), "dim");
                    tm.add_css_class("mono");
                    tm.set_width_chars(8);
                    inner.append(&tm);
                    let nm = widgets::label(&ch.title, "");
                    nm.set_xalign(0.0);
                    nm.set_hexpand(true);
                    nm.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    inner.append(&nm);
                    row.set_child(Some(&inner));
                    let at = ch.start;
                    let id = b.id.clone();
                    let book = b.clone();
                    row.connect_clicked(move |_| {
                        if playback::current_book().is_some_and(|x| x.id == id) {
                            playback::seek(at);
                        } else {
                            playback::start_at(book.clone(), Some(at), true);
                        }
                    });
                    list.append(&row);
                }
            } else {
                list_label.set_text("Bookmarks");
                for m in super::bookmarks(&b) {
                    let row = gtk::Button::new();
                    row.add_css_class("flat");
                    row.add_css_class("abs-ch");
                    let inner = widgets::hbox(12);
                    let tm = widgets::label(&fmt::time(m.position), "dim");
                    tm.add_css_class("mono");
                    tm.set_width_chars(8);
                    inner.append(&tm);
                    let nm = widgets::label(&m.title, "");
                    nm.set_xalign(0.0);
                    nm.set_hexpand(true);
                    nm.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    inner.append(&nm);
                    row.set_child(Some(&inner));
                    let book = b.clone();
                    let at = m.position;
                    row.connect_clicked(move |_| playback::start_at(book.clone(), Some(at), true));
                    list.append(&row);
                }
            }
        }
    };
    let v = view.clone();
    let r = refill.clone();
    ch_btn.connect_toggled(move |b| {
        if b.is_active() {
            v.set(0);
            r();
        }
    });
    let v = view.clone();
    let r = refill.clone();
    bm_btn.connect_toggled(move |b| {
        if b.is_active() {
            v.set(1);
            r();
        }
    });

    let update = {
        let refill = refill.clone();
        let speed = speed.clone();
        move |e: crate::player::Event| {
            if let Some(b) = playback::current_book() {
                cover.set_key(&b.art);
                title.set_text(&b.title);
                author.set_text(&b.author);
                narrator.set_text(&if b.narrator.is_empty() {
                    String::new()
                } else {
                    format!("Narrated by {}", b.narrator)
                });
                let pos = playback::position();
                let chapter = b.chapters.iter().rev().find(|c| c.start <= pos).map(|c| c.title.as_str()).unwrap_or("");
                let left = fmt::time((b.duration - pos).max(0.0));
                meta.set_text(&if chapter.is_empty() {
                    format!("{left} left")
                } else {
                    format!("{chapter} · {left} left")
                });
                pos_l.set_text(&fmt::time(pos));
                dur_l.set_text(&fmt::time(b.duration));
                bar.set_fraction(if b.duration > 0.0 { (pos / b.duration).clamp(0.0, 1.0) } else { 0.0 });
                speed.set_label(&format!("{:.2}×", playback::rate()));
                resume.set_label(if crate::player::state() == crate::player::State::Playing { "Pause" } else { "Resume" });
                if matches!(e, crate::player::Event::Track | crate::player::Event::Seeked | crate::player::Event::Queue) {
                    refill();
                }
            }
        }
    };
    update(crate::player::Event::Track);
    crate::player::subscribe(&root, update);
    subscribe(&root, {
        let refill = refill.clone();
        move || refill()
    });
    root
}

pub fn podcast_source(feed: &str) -> gtk::Box {
    let row = widgets::vbox(4);
    let feed = normalize_feed(feed);
    let r = row.clone();
    let refresh = move || {
        clear(&r);
        if !config().podcasts {
            return;
        }
        for b in storage::books(&config()).into_iter().filter(|b| b.podcast && normalize_feed(&b.feed) == feed) {
            let name = b.title.clone();
            r.append(&button(&format!("Audiobookshelf · {name}"), move || show(b.clone())));
        }
    };
    refresh();
    subscribe(&row, refresh);
    row
}
fn normalize_feed(feed: &str) -> String {
    if let Ok(mut u) = url::Url::parse(feed) {
        u.set_fragment(None);
        let path = u.path().trim_end_matches('/').to_string();
        u.set_path(&path);
        u.to_string()
    } else {
        feed.trim_end_matches('/').to_string()
    }
}

fn seek_target(book: &Book, delta: f64) {
    if playback::current_book().is_some_and(|b| b.id == book.id) {
        crate::player::seek_by(delta);
    } else {
        playback::start_at(book.clone(), Some((storage::progress(&book.id).position + delta).max(0.0)), true);
    }
}
