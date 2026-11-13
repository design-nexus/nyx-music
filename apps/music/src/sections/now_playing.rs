//! Now playing: the cover, the song, a full-size spectrum analyzer and what's
//! up next.

use crate::library::art::Cover;
use crate::library::{Kind, lyrics, store};
use crate::player::{self, Event, State, engine};
use crate::widgets::{self, Page};
use crate::{fmt, prefs, spectrum};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

const UP_NEXT: usize = 5;

thread_local! {
    /// The Spectrum and Lyrics switch, once the page is built.
    static VIEW_SWITCH: RefCell<Option<gtk::Box>> = const { RefCell::new(None) };
}

/// Show the lyrics (now, or when the page is first built).
pub fn show_lyrics() {
    let switch = VIEW_SWITCH.with(|s| s.borrow().clone());
    match switch.and_then(|s| s.last_child()).and_downcast::<gtk::ToggleButton>() {
        Some(b) => b.set_active(true),
        _ => prefs::update(|p| p.now_view = "lyrics".into()),
    }
}

fn link(class: &str) -> (gtk::Button, gtk::Label) {
    let b = gtk::Button::new();
    b.add_css_class("flat");
    b.add_css_class("link-button");
    b.add_css_class(class);
    b.set_halign(gtk::Align::Start);
    let l = widgets::label("", "");
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    b.set_child(Some(&l));
    (b, l)
}

pub fn build(page: &Page) {
    if let Some(e) = player::engine_error() {
        page.body.append(&widgets::banner(&format!("Playback isn't available: {}", super::glib_escape(&e)), true));
    } else {
        for (el, what) in engine::missing() {
            page.body.append(&widgets::banner(
                &format!("The {what} needs GStreamer's <tt>{el}</tt> element, from <b>gst-plugins-good</b>. Install it and restart Music."),
                true,
            ));
        }
    }

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    let empty = widgets::empty_state(
        "music-spectrum-symbolic",
        "Nothing playing",
        "Pick an album or a song, or press <b>Space</b> to play your whole library.",
        Some(("Play everything", Box::new(player::play))),
    );
    stack.add_named(&empty, Some("empty"));

    // Two columns over a blurred copy of the cover: the cover and the song on the
    // left; the spectrum (or lyrics) and what's up next on the right. A half-screen
    // window stacks them.
    let cols = widgets::hbox(34);
    cols.add_css_class("now-cols");
    let left = widgets::vbox(18);
    left.add_css_class("now-left");
    let cover = Cover::new(300, false);
    left.append(&cover.root);
    let right = widgets::vbox(14);
    right.set_hexpand(true);
    cols.append(&left);
    cols.append(&right);
    let backdrop = gtk::DrawingArea::new();
    backdrop.add_css_class("now-backdrop");
    backdrop.set_vexpand(true);
    let frame = gtk::Overlay::new();
    frame.add_css_class("now-frame");
    frame.set_overflow(gtk::Overflow::Hidden);
    frame.set_child(Some(&backdrop));
    // The cover grows with the page, and shrinks in a half-screen window.
    let (c, cl, lf) = (cover.clone(), cols.clone(), left.clone());
    let last = std::rc::Rc::new(std::cell::Cell::new(0));
    backdrop.connect_resize(move |_, w, _| {
        let narrow = crate::window::narrow();
        let size = if narrow { 150 } else { (w as f64 * 0.26).clamp(220.0, 300.0) as i32 };
        if last.replace(size) != size {
            let (c, cl, lf) = (c.clone(), cl.clone(), lf.clone());
            gtk::glib::idle_add_local_once(move || {
                c.set_size(size);
                cl.set_orientation(if narrow { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
                lf.set_orientation(if narrow { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical });
            });
        }
    });
    let text = widgets::vbox(4);
    text.set_valign(gtk::Align::Start);
    let state = widgets::label("", "group-title");
    text.append(&state);
    let title = widgets::label("", "now-title");
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    title.set_lines(2);
    // Wraps under the cover instead of widening its column.
    title.set_max_width_chars(18);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&title);
    let (artist_btn, artist_lbl) = link("now-artist");
    text.append(&artist_btn);
    let (album_btn, album_lbl) = link("now-album");
    text.append(&album_btn);
    let meta = widgets::label("", "dim");
    meta.add_css_class("mono");
    meta.add_css_class("detail-meta");
    text.append(&meta);
    left.append(&text);

    // ----- Spectrum or lyrics -----
    let an_card = widgets::vbox(10);
    an_card.set_vexpand(true);
    let an_head = widgets::hbox(12);
    let has_spectrum = player::has_spectrum();
    let view = if has_spectrum { prefs::get().now_view } else { "lyrics".to_string() };
    let views = gtk::Stack::new();
    views.set_vexpand(true);
    views.set_transition_type(gtk::StackTransitionType::Crossfade);
    let styles = widgets::segmented(
        &widgets::opts(&[("bars", "Bars"), ("line", "Line"), ("mirror", "Mirror")]),
        &prefs::get().spectrum_style,
        |v| {
            prefs::update(|p| p.spectrum_style = v);
            spectrum::redraw_all();
        },
    );
    let switch = {
        let (views, styles) = (views.clone(), styles.clone());
        widgets::segmented(&widgets::opts(&[("spectrum", "Spectrum"), ("lyrics", "Lyrics")]), &view, move |v| {
            views.set_visible_child_name(&v);
            styles.set_visible(v == "spectrum");
            prefs::update(|p| p.now_view = v);
        })
    };
    switch.set_visible(has_spectrum);
    VIEW_SWITCH.with(|s| *s.borrow_mut() = Some(switch.clone()));
    switch.set_hexpand(true);
    switch.set_halign(gtk::Align::Start);
    an_head.append(&switch);
    styles.set_visible(view == "spectrum");
    an_head.append(&styles);
    an_card.append(&an_head);
    let analyzer = spectrum::analyzer(false);
    analyzer.set_vexpand(true);
    analyzer.set_size_request(-1, 200);
    views.add_named(&analyzer, Some("spectrum"));
    let lyrics = LyricsView::new();
    views.add_named(&lyrics.root, Some("lyrics"));
    views.set_visible_child_name(&view);
    an_card.append(&views);
    right.append(&an_card);

    // ----- Up next -----
    let next_group = widgets::vbox(6);
    next_group.append(&widgets::label("UP NEXT", "group-title"));
    let next_list = widgets::vbox(2);
    next_group.append(&next_list);
    right.append(&next_group);
    // Scrolls rather than grow taller than the window, which would push the
    // player bar off the bottom.
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&cols)
        .vexpand(true)
        .build();
    frame.add_overlay(&scroll);
    stack.add_named(&frame, Some("playing"));
    page.body.append(&stack);

    let artist_name: Rc<RefCell<String>> = Rc::default();
    let album_key: Rc<RefCell<String>> = Rc::default();
    let n = artist_name.clone();
    artist_btn.connect_clicked(move |_| super::artists::show(&n.borrow()));
    let k = album_key.clone();
    album_btn.connect_clicked(move |_| {
        let Some(t) = player::current() else { return };
        match t.kind {
            Kind::File => super::albums::show(&k.borrow()),
            Kind::Station => crate::window::navigate("radio"),
            Kind::Episode => super::podcasts::show(&t.feed),
        }
    });

    let refresh_next = move || {
        while let Some(c) = next_list.first_child() {
            next_list.remove(&c);
        }
        let (_, cursor) = player::queue_items();
        let upcoming = player::upcoming(UP_NEXT);
        next_group.set_visible(!upcoming.is_empty());
        let start = cursor.map_or(0, |c| c + 1);
        for (i, p) in upcoming.iter().enumerate() {
            let t = store::track_for(p);
            let b = gtk::Button::new();
            b.add_css_class("flat");
            b.add_css_class("next-row");
            let row = widgets::hbox(12);
            let num = widgets::label(&(i + 1).to_string(), "cell-dim");
            num.add_css_class("mono");
            num.set_width_chars(2);
            num.set_xalign(1.0);
            row.append(&num);
            let tt = widgets::label(&t.title, "");
            tt.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&tt);
            let ar = widgets::label(&t.artist, "dim");
            ar.set_ellipsize(gtk::pango::EllipsizeMode::End);
            ar.set_hexpand(true);
            row.append(&ar);
            let d = widgets::label(&if t.is_live() { "LIVE".into() } else { fmt::time(t.duration) }, "cell-dim");
            d.add_css_class("mono");
            row.append(&d);
            b.set_child(Some(&row));
            let idx = start + i;
            b.set_tooltip_text(Some("Play now"));
            b.connect_clicked(move |_| player::jump(idx));
            next_list.append(&b);
        }
    };

    let refresh = {
        let stack = stack.clone();
        let refresh_next = refresh_next.clone();
        move |e: Event| match e {
            Event::Track => {
                match player::current() {
                    Some(t) => {
                        stack.set_visible_child_name("playing");
                        cover.set_placeholder(crate::playerbar::placeholder_icon(&t));
                        cover.set_key(&t.art);
                        set_backdrop(&t.art);
                        lyrics.load(&t);
                        title.set_text(&t.title);
                        artist_lbl.set_text(&t.artist);
                        *artist_name.borrow_mut() = t.album_artist_or_artist().to_string();
                        album_lbl.set_text(&match (t.kind, t.year) {
                            (Kind::File, Some(y)) => format!("{} · {y}", t.album),
                            _ => t.album.clone(),
                        });
                        album_btn.set_visible(!t.album.is_empty());
                        *album_key.borrow_mut() = t.album_key();
                        album_btn.set_sensitive(t.is_remote() || store::album_of(&t).is_some());
                        artist_btn.set_sensitive(!t.is_remote() && store::find(&t.path).is_some());
                        let mut m = Vec::new();
                        match t.kind {
                            Kind::File => {
                                m.push(fmt::time(t.duration));
                                if let Some(ext) = t.path.extension() {
                                    m.push(ext.to_string_lossy().to_uppercase());
                                }
                            }
                            Kind::Station => {
                                m.push("LIVE".into());
                                m.push(crate::online::http::host(&t.path.to_string_lossy()).to_string());
                            }
                            Kind::Episode => {
                                if t.duration > 0.0 {
                                    m.push(fmt::time(t.duration));
                                }
                                let published = crate::online::info(&t.path).published;
                                if published > 0 {
                                    m.push(fmt::date(published));
                                }
                                m.push(if t.download.is_some() { "Downloaded".into() } else { "Streaming".into() });
                            }
                        }
                        let plays = t.plays.get();
                        if plays > 0 {
                            m.push(fmt::count(plays as usize, "play", "plays"));
                        }
                        meta.set_text(&m.join(" · "));
                    }
                    None => {
                        stack.set_visible_child_name("empty");
                        set_backdrop("");
                    }
                }
                refresh_next();
            }
            Event::State => state.set_text(match (player::state(), player::buffering()) {
                (State::Playing, Some(_)) => "BUFFERING",
                (State::Playing, None) if player::is_live() => "ON AIR",
                (State::Playing, None) => "PLAYING",
                (State::Paused, _) => "PAUSED",
                (State::Stopped, _) => "STOPPED",
            }),
            Event::Queue => refresh_next(),
            Event::Position | Event::Seeked => lyrics.follow(player::position()),
            _ => {}
        }
    };
    for e in [Event::Track, Event::State, Event::Seeked] {
        refresh(e);
    }
    let r = refresh.clone();
    player::subscribe(&page.body, move |e| {
        r(e);
        // The state line also says whether a station is live.
        if e == Event::Track {
            r(Event::State);
        }
    });
}

thread_local! {
    static BACKDROP: gtk::CssProvider = {
        let p = gtk::CssProvider::new();
        if let Some(d) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&d, &p, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
        }
        p
    };
}

/// Paint the hero's backdrop from this cover (blurred by the stylesheet), and
/// with "Colors from cover" on, tint the hero's accents with its tone.
fn set_backdrop(art: &str) {
    use crate::library::art;
    let path = [art::file(art, false), art::file(art, true)].into_iter().find(|p| !art.is_empty() && p.exists());
    let mut css = match &path {
        Some(p) => {
            let uri = gtk::glib::filename_to_uri(p, None).map(|u| u.to_string()).unwrap_or_default();
            format!(".now-backdrop {{ background-image: url(\"{uri}\"); }}\n")
        }
        None => ".now-backdrop { background-image: none; }\n".to_string(),
    };
    if prefs::get().cover_colours
        && let Some((r, g, b)) = path.as_ref().and_then(|_| art::tone(art))
    {
        let c = format!("rgb({},{},{})", (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8);
        css.push_str(&format!(
            ".now-hero .group-title, window.music-window .now-hero button.link-button.now-artist {{ color: {c}; }}\n\
             .now-hero-frame {{ box-shadow: inset 0 0 0 1px alpha({c}, 0.25); }}\n"
        ));
    }
    BACKDROP.with(|p| p.load_from_string(&css));
}

/// Lyrics for the song that's playing. Synced lyrics light up the current
/// line and keep it in the middle; click a line to go there.
#[derive(Clone)]
struct LyricsView {
    root: gtk::Stack,
    scroll: gtk::ScrolledWindow,
    lines: gtk::Box,
    text: Rc<RefCell<Option<lyrics::Lyrics>>>,
    rows: Rc<RefCell<Vec<gtk::Widget>>>,
    current: Rc<std::cell::Cell<Option<usize>>>,
    path: Rc<RefCell<std::path::PathBuf>>,
}

impl LyricsView {
    fn new() -> LyricsView {
        let root = gtk::Stack::new();
        root.add_css_class("lyrics");
        let lines = widgets::vbox(2);
        lines.add_css_class("lyrics-lines");
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&lines)
            .vexpand(true)
            .build();
        root.add_named(&scroll, Some("lines"));
        root.add_named(
            &widgets::empty_state(
                "music-note-symbolic",
                "No lyrics for this song",
                "Put a <tt>.lrc</tt> file with the same name next to the song, or add lyrics to its tags.",
                None,
            ),
            Some("none"),
        );
        root.set_visible_child_name("none");
        LyricsView {
            root,
            scroll,
            lines,
            text: Rc::default(),
            rows: Rc::default(),
            current: Rc::default(),
            path: Rc::default(),
        }
    }

    fn load(&self, t: &crate::library::Track) {
        if *self.path.borrow() == t.path {
            return;
        }
        *self.path.borrow_mut() = t.path.clone();
        self.show(None);
        if t.is_remote() {
            return;
        }
        let (me, path, want) = (self.clone(), t.path.clone(), t.path.clone());
        crate::cmd::background(
            move || lyrics::read(&path),
            move |l| {
                // Still the same song?
                if *me.path.borrow() == want {
                    me.show(l);
                }
            },
        );
    }

    fn show(&self, l: Option<lyrics::Lyrics>) {
        while let Some(c) = self.lines.first_child() {
            self.lines.remove(&c);
        }
        self.rows.borrow_mut().clear();
        self.current.set(None);
        self.scroll.vadjustment().set_value(0.0);
        let Some(l) = l else {
            *self.text.borrow_mut() = None;
            self.root.set_visible_child_name("none");
            return;
        };
        for (secs, line) in &l.lines {
            let label = widgets::label(line, "lyric-line");
            label.set_wrap(true);
            label.set_xalign(0.0);
            let row: gtk::Widget = if l.synced {
                let b = gtk::Button::new();
                b.add_css_class("flat");
                b.add_css_class("lyric-button");
                b.set_child(Some(&label));
                let secs = *secs;
                b.connect_clicked(move |_| player::seek(secs));
                b.upcast()
            } else {
                label.add_css_class("unsynced");
                label.set_selectable(true);
                label.upcast()
            };
            self.lines.append(&row);
            self.rows.borrow_mut().push(row);
        }
        *self.text.borrow_mut() = Some(l);
        self.root.set_visible_child_name("lines");
        self.follow(player::position());
    }

    /// Light the line at `secs` and scroll it to the middle.
    fn follow(&self, secs: f64) {
        let at = self.text.borrow().as_ref().and_then(|l| lyrics::line_at(l, secs));
        if at == self.current.get() {
            return;
        }
        let rows = self.rows.borrow();
        if let Some(old) = self.current.get().and_then(|i| rows.get(i)) {
            old.remove_css_class("current");
        }
        self.current.set(at);
        let Some(row) = at.and_then(|i| rows.get(i)) else { return };
        row.add_css_class("current");
        if let Some(p) = row.compute_point(&self.lines, &gtk::graphene::Point::new(0.0, 0.0)) {
            let adj = self.scroll.vadjustment();
            let target = p.y() as f64 + row.height() as f64 / 2.0 - adj.page_size() / 2.0;
            adj.set_value(target.clamp(0.0, (adj.upper() - adj.page_size()).max(0.0)));
        }
    }
}
