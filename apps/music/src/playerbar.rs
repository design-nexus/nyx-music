//! The bar under every page: what's playing, transport, seek, volume and a
//! small spectrum strip.

use crate::library::art::Cover;
use crate::player::{self, Event, State, queue::Repeat};
use crate::{fmt, prefs, spectrum, widgets, window};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    static NARROW_PARTS: RefCell<Vec<gtk::Widget>> = const { RefCell::new(Vec::new()) };
}

pub fn set_narrow(narrow: bool) {
    NARROW_PARTS.with(|p| {
        for w in p.borrow().iter() {
            w.set_visible(!narrow);
        }
    });
}

fn volume_icon(v: f64, muted: bool) -> &'static str {
    if muted || v <= 0.001 {
        "audio-volume-muted-symbolic"
    } else if v < 0.34 {
        "audio-volume-low-symbolic"
    } else if v < 0.67 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}

/// "Artist — Album", leaving out what's empty.
pub fn byline(t: &crate::library::Track) -> String {
    [t.artist.as_str(), t.album.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" — ")
}

/// The glyph a cover shows when there's no picture.
pub fn placeholder_icon(t: &crate::library::Track) -> &'static str {
    match t.kind {
        crate::library::Kind::Station => "music-radio-symbolic",
        crate::library::Kind::Episode => "music-podcast-symbolic",
        crate::library::Kind::File => "media-optical-symbolic",
    }
}

/// A vertical volume slider and a mute switch, for narrow windows.
fn volume_popover(anchor: &gtk::Button) -> gtk::Popover {
    let pop = gtk::Popover::new();
    pop.set_parent(anchor);
    pop.add_css_class("volume-popover");
    let bx = widgets::vbox(8);
    let slider = gtk::Scale::with_range(gtk::Orientation::Vertical, 0.0, 1.0, 0.01);
    slider.set_inverted(true);
    slider.set_draw_value(false);
    slider.set_size_request(-1, 140);
    slider.set_halign(gtk::Align::Center);
    slider.add_css_class("volume");
    slider.set_value(prefs::get().volume);
    slider.connect_change_value(|_, _, v| {
        let v = v.clamp(0.0, 1.0);
        player::set_volume(v);
        if prefs::get().muted && v > 0.0 {
            player::set_muted(false);
        }
        glib::Propagation::Proceed
    });
    let mute = gtk::ToggleButton::with_label("Mute");
    mute.set_active(prefs::get().muted);
    mute.connect_toggled(|b| {
        if b.is_active() != prefs::get().muted {
            player::set_muted(b.is_active());
        }
    });
    bx.append(&slider);
    bx.append(&mute);
    pop.set_child(Some(&bx));
    let (sl, m) = (slider.clone(), mute.clone());
    player::subscribe(&slider, move |e| {
        if e == Event::Options {
            let p = prefs::get();
            if (sl.value() - p.volume).abs() > 0.005 {
                sl.set_value(p.volume);
            }
            m.set_active(p.muted);
        }
    });
    pop
}

/// The sleep timer: a moon that's lit while a timer is set.
fn sleep_button() -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    button.set_icon_name("weather-clear-night-symbolic");
    button.add_css_class("flat");
    button.add_css_class("icon-button");
    button.add_css_class("sleep-button");
    button.set_focus_on_click(false);
    let pop = gtk::Popover::new();
    pop.add_css_class("menu-popover");
    let list = widgets::vbox(1);
    let heading = widgets::label("Sleep timer", "menu-heading-inline");
    heading.set_margin_start(10);
    heading.set_margin_top(4);
    heading.set_margin_bottom(4);
    list.append(&heading);
    // Minutes for a timed sleep; None for the other choices.
    let choices: [(&str, Option<u64>, player::Sleep); 8] = [
        ("Off", None, player::Sleep::Off),
        ("In 15 minutes", Some(15), player::Sleep::Off),
        ("In 30 minutes", Some(30), player::Sleep::Off),
        ("In 45 minutes", Some(45), player::Sleep::Off),
        ("In 1 hour", Some(60), player::Sleep::Off),
        ("In 1½ hours", Some(90), player::Sleep::Off),
        ("At the end of this song", None, player::Sleep::EndOfSong),
        ("At the end of this album", None, player::Sleep::EndOfAlbum),
    ];
    for (label, minutes, choice) in choices {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.add_css_class("menu-item");
        if let Some(l) = b.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }
        let p = pop.clone();
        b.connect_clicked(move |_| {
            p.popdown();
            // Timed choices start counting when picked.
            let s = minutes.map_or(choice, |m| {
                player::Sleep::At(std::time::Instant::now() + std::time::Duration::from_secs(m * 60))
            });
            player::set_sleep(s);
            if s != player::Sleep::Off {
                window::toast(&format!("Sleep timer: {}.", sleep_text(s)));
            }
        });
        list.append(&b);
    }
    pop.set_child(Some(&list));
    button.set_popover(Some(&pop));

    let refresh = {
        let button = button.clone();
        move || {
            let s = player::sleep();
            if s == player::Sleep::Off {
                button.remove_css_class("active");
                button.set_tooltip_text(Some("Sleep timer"));
            } else {
                button.add_css_class("active");
                button.set_tooltip_text(Some(&format!("Sleep timer: {}", sleep_text(s))));
            }
        }
    };
    refresh();
    player::subscribe(&button, move |e| {
        if matches!(e, Event::Options | Event::Position | Event::Track) {
            refresh();
        }
    });
    button
}

/// "pauses in 23 min", "pauses after this song"…
fn sleep_text(s: player::Sleep) -> String {
    match s {
        player::Sleep::Off => "off".into(),
        player::Sleep::At(_) => {
            let left = player::sleep_remaining().unwrap_or(0.0);
            let mins = (left / 60.0).ceil().max(1.0) as u64;
            if mins >= 60 && mins.is_multiple_of(60) {
                format!("pauses in {} h", mins / 60)
            } else if mins >= 60 {
                format!("pauses in {} h {} min", mins / 60, mins % 60)
            } else {
                format!("pauses in {mins} min")
            }
        }
        player::Sleep::EndOfSong => "pauses after this song".into(),
        player::Sleep::EndOfAlbum => "pauses after this album".into(),
    }
}

/// Says where a click goes: to Now playing, or back from it.
fn now_playing_tooltip(w: &impl IsA<gtk::Widget>) {
    w.set_has_tooltip(true);
    w.connect_query_tooltip(|_, _, _, _, tip| {
        tip.set_text(Some(if window::current() == "now-playing" { "Back" } else { "Open Now playing" }));
        true
    });
}

pub fn build() -> gtk::Box {
    let bar = widgets::hbox(16);
    bar.add_css_class("player-bar");

    // ----- Now playing -----
    let info = widgets::hbox(12);
    info.add_css_class("player-info");
    info.set_size_request(220, -1);
    let cover = Cover::new(40, true);
    cover.root.set_valign(gtk::Align::Center);
    info.append(&cover.root);
    let text = widgets::vbox(2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    let title = widgets::label("Nothing playing", "player-title");
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(28);
    let artist = widgets::label("", "player-artist");
    artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
    artist.set_max_width_chars(28);
    text.append(&title);
    text.append(&artist);
    info.append(&text);
    let click = gtk::GestureClick::new();
    click.connect_released(|_, _, _, _| window::toggle_now_playing());
    now_playing_tooltip(&info);
    info.add_controller(click);
    info.set_cursor_from_name(Some("pointer"));
    bar.append(&info);

    // ----- Transport + seek -----
    let center = widgets::vbox(2);
    center.set_hexpand(true);
    center.set_size_request(260, -1);
    center.set_valign(gtk::Align::Center);
    let controls = widgets::hbox(6);
    controls.set_halign(gtk::Align::Center);
    let shuffle = gtk::ToggleButton::new();
    shuffle.set_icon_name("media-playlist-shuffle-symbolic");
    shuffle.add_css_class("flat");
    shuffle.add_css_class("icon-button");
    shuffle.add_css_class("transport-toggle");
    shuffle.set_tooltip_text(Some("Shuffle"));
    let prev = widgets::icon_button("media-skip-backward-symbolic", "Previous (Ctrl+←)");
    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    play.add_css_class("play-button");
    play.set_tooltip_text(Some("Play (Space)"));
    let next = widgets::icon_button("media-skip-forward-symbolic", "Next (Ctrl+→)");
    let repeat = gtk::ToggleButton::new();
    repeat.set_icon_name("media-playlist-repeat-symbolic");
    repeat.add_css_class("flat");
    repeat.add_css_class("icon-button");
    repeat.add_css_class("transport-toggle");
    // Names for screen readers: GTK reads a tooltip only as the description, and the
    // repeat toggle's tooltip says its state.
    shuffle.update_property(&[gtk::accessible::Property::Label("Shuffle")]);
    play.update_property(&[gtk::accessible::Property::Label("Play")]);
    repeat.update_property(&[gtk::accessible::Property::Label("Repeat")]);
    for w in [shuffle.upcast_ref::<gtk::Widget>(), prev.upcast_ref(), play.upcast_ref(), next.upcast_ref(), repeat.upcast_ref()] {
        w.set_focus_on_click(false);
        controls.append(w);
    }
    center.append(&controls);

    let seek_row = widgets::hbox(10);
    let pos = widgets::label("0:00", "time-readout");
    pos.add_css_class("mono");
    pos.set_xalign(1.0);
    pos.set_width_chars(5);
    let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    seek.set_draw_value(false);
    seek.set_hexpand(true);
    seek.add_css_class("seek");
    seek.set_focus_on_click(false);
    let dur = widgets::label("0:00", "time-readout");
    dur.add_css_class("mono");
    dur.set_width_chars(5);
    seek_row.append(&pos);
    seek_row.append(&seek);
    seek_row.append(&dur);
    center.append(&seek_row);
    bar.append(&center);

    // ----- Spectrum strip + volume -----
    let right = widgets::hbox(8);
    right.set_valign(gtk::Align::Center);
    right.set_halign(gtk::Align::End);
    let strip = spectrum::analyzer(true);
    strip.set_size_request(110, 34);
    strip.set_hexpand(false);
    strip.set_valign(gtk::Align::Center);
    now_playing_tooltip(&strip);
    let sclick = gtk::GestureClick::new();
    sclick.connect_released(|_, _, _, _| window::toggle_now_playing());
    strip.add_controller(sclick);
    right.append(&strip);
    let sleep = sleep_button();
    right.append(&sleep);
    let p = prefs::get();
    let mute = widgets::icon_button(volume_icon(p.volume, p.muted), "Mute");
    mute.set_focus_on_click(false);
    right.append(&mute);
    let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    volume.set_draw_value(false);
    volume.set_value(p.volume);
    volume.set_size_request(100, -1);
    volume.add_css_class("volume");
    volume.set_focus_on_click(false);
    volume.set_tooltip_text(Some(&format!("{}%", (p.volume * 100.0).round())));
    right.append(&volume);
    bar.append(&right);
    NARROW_PARTS.with(|n| {
        let mut n = n.borrow_mut();
        n.push(strip.clone().upcast());
        n.push(volume.clone().upcast());
    });

    // ----- Wiring -----
    play.connect_clicked(|_| player::toggle());
    prev.connect_clicked(|_| player::previous());
    next.connect_clicked(|_| player::next());
    let syncing = Rc::new(Cell::new(false));
    let s = syncing.clone();
    shuffle.connect_toggled(move |b| {
        if !s.get() {
            player::set_shuffle(b.is_active());
        }
    });
    let s = syncing.clone();
    repeat.connect_toggled(move |_| {
        if !s.get() {
            player::cycle_repeat();
        }
    });
    // Narrow, the slider is hidden: the button opens one in a popover instead.
    let volume_pop = volume_popover(&mute);
    mute.connect_clicked(move |_| {
        if window::narrow() {
            volume_pop.popup();
        } else {
            player::set_muted(!prefs::get().muted);
        }
    });
    let middle = gtk::GestureClick::new();
    middle.set_button(gtk::gdk::BUTTON_MIDDLE);
    middle.connect_pressed(|_, _, _, _| player::set_muted(!prefs::get().muted));
    mute.add_controller(middle);
    volume.connect_change_value(|s, _, v| {
        let v = v.clamp(0.0, 1.0);
        player::set_volume(v);
        if prefs::get().muted && v > 0.0 {
            player::set_muted(false);
        }
        s.set_tooltip_text(Some(&format!("{}%", (v * 100.0).round())));
        glib::Propagation::Proceed
    });
    // Scroll on the volume icon nudges the volume.
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.connect_scroll(|_, _, dy| {
        player::set_volume((prefs::get().volume - dy * 0.05).clamp(0.0, 1.0));
        glib::Propagation::Stop
    });
    mute.add_controller(scroll);

    // Hovering the seek bar shows the time under the pointer.
    let hover = gtk::EventControllerMotion::new();
    hover.connect_motion(|c, x, _| {
        let Some(w) = c.widget() else { return };
        let d = player::duration();
        if player::is_live() || d <= 0.0 || w.width() <= 0 {
            w.set_tooltip_text(None);
            return;
        }
        // The trough runs the scale's full width.
        let frac = (x / w.width() as f64).clamp(0.0, 1.0);
        w.set_tooltip_text(Some(&fmt::time(frac * d)));
        w.trigger_tooltip_query();
    });
    seek.add_controller(hover);

    // While the user drags the seek bar, don't move it under them.
    let dragging: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    let d = dragging.clone();
    let pos_label = pos.clone();
    seek.connect_change_value(move |_, _, v| {
        let gen_ = d.get().wrapping_add(1);
        d.set(gen_);
        pos_label.set_text(&fmt::time(v));
        player::seek(v);
        let d2 = d.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
            if d2.get() == gen_ {
                d2.set(0);
            }
        });
        glib::Propagation::Proceed
    });

    let refresh = {
        let (cover, title, artist, play, shuffle, repeat, seek, pos, dur, mute, volume, syncing, prev, next) = (
            cover.clone(),
            title.clone(),
            artist.clone(),
            play.clone(),
            shuffle.clone(),
            repeat.clone(),
            seek.clone(),
            pos.clone(),
            dur.clone(),
            mute.clone(),
            volume.clone(),
            syncing.clone(),
            prev.clone(),
            next.clone(),
        );
        let dragging = dragging.clone();
        move |e: Event| match e {
            Event::Track => {
                match player::current() {
                    Some(t) => {
                        title.set_text(&t.title);
                        artist.set_text(&byline(&t));
                        title.set_tooltip_text(Some(&t.title));
                        cover.set_placeholder(placeholder_icon(&t));
                        cover.set_key(&t.art);
                    }
                    None => {
                        title.set_text("Nothing playing");
                        artist.set_text("");
                        cover.set_key("");
                    }
                }
                let live = player::is_live();
                seek.set_sensitive(!live);
                let d = player::duration();
                seek.set_range(0.0, d.max(1.0));
                if live {
                    seek.set_value(0.0);
                }
                dur.set_text(&if live { "LIVE".to_string() } else { fmt::time(d) });
                prev.set_sensitive(player::can_previous());
                next.set_sensitive(player::can_next());
            }
            Event::State => {
                if let Some(t) = player::current() {
                    artist.set_text(&match player::buffering() {
                        Some(p) if p > 0 => format!("Buffering… {p}%"),
                        Some(_) => "Connecting…".to_string(),
                        None => byline(&t),
                    });
                }
                let playing = player::state() == State::Playing;
                play.set_icon_name(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
                play.set_tooltip_text(Some(if playing { "Pause (Space)" } else { "Play (Space)" }));
                play.update_property(&[gtk::accessible::Property::Label(if playing { "Pause" } else { "Play" })]);
            }
            Event::Position | Event::Seeked => {
                if dragging.get() != 0 && e == Event::Position {
                    return;
                }
                if player::is_live() {
                    pos.set_text(&fmt::time(player::position()));
                    return;
                }
                let d = player::duration();
                if (seek.adjustment().upper() - d.max(1.0)).abs() > 0.5 {
                    seek.set_range(0.0, d.max(1.0));
                    dur.set_text(&fmt::time(d));
                }
                let p = player::position();
                seek.set_value(p);
                pos.set_text(&fmt::time(p));
            }
            Event::Queue => {
                prev.set_sensitive(player::can_previous());
                next.set_sensitive(player::can_next());
            }
            Event::Options => {
                syncing.set(true);
                shuffle.set_active(player::shuffle());
                let r = player::repeat();
                repeat.set_active(r != Repeat::Off);
                repeat.set_icon_name(if r == Repeat::One {
                    "media-playlist-repeat-song-symbolic"
                } else {
                    "media-playlist-repeat-symbolic"
                });
                repeat.set_tooltip_text(Some(match r {
                    Repeat::Off => "Repeat: off",
                    Repeat::All => "Repeat: all",
                    Repeat::One => "Repeat: this song",
                }));
                syncing.set(false);
                let p = prefs::get();
                mute.set_icon_name(volume_icon(p.volume, p.muted));
                let mute_name = match (window::narrow(), p.muted) {
                    (true, _) => "Volume",
                    (false, true) => "Unmute",
                    (false, false) => "Mute",
                };
                mute.set_tooltip_text(Some(mute_name));
                mute.update_property(&[gtk::accessible::Property::Label(mute_name)]);
                if (volume.value() - p.volume).abs() > 0.005 {
                    volume.set_value(p.volume);
                }
            }
        }
    };
    for e in [Event::Track, Event::State, Event::Seeked, Event::Queue, Event::Options] {
        refresh(e);
    }
    player::subscribe(&bar, refresh);
    bar
}
