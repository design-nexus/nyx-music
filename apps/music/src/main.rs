//! Music — a music player for Omarchy.

mod cmd;
mod control;
mod eq;
mod fmt;
mod indicator;
mod library;
mod mpris;
mod online;
mod panel_dialog;
mod paths;
mod player;
mod playerbar;
mod prefs;
mod sections;
mod settings_dialog;
mod spectrum;
mod theme;
mod tracklist;
mod views;
mod widgets;
mod window;

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::path::PathBuf;

pub const APP_ID: &str = "io.github.design_nexus.nyx.Music";

const USAGE: &str = "Usage: nyx-music [OPTIONS] [FILES…]\n\
\n\
  FILES…          play these songs, folders or .m3u playlists, or stream addresses\n\
  --enqueue       add FILES to the queue instead of playing them now\n\
  --section ID    open (or switch the open window) to a page: now-playing, songs, albums,\n\
                  artists, genres, folders, recently-added, recently-played, most-played,\n\
                  radio, podcasts, queue, equalizer, settings\n\
  --toggle        close the window if it's open, otherwise open it (for a keybinding)\n\
  --mini          open or close the mini player\n\
  --play-pause, --play, --pause, --stop, --next, --previous\n\
                  control playback in the running window\n\
  --sleep WHEN    pause after MINUTES, at the end of this song or album, or not:\n\
                  a number, song, album or off\n";

/// `--sleep 30`, `song`, `album` or `off`.
fn parse_sleep(when: &str) -> Option<player::Sleep> {
    match when {
        "off" => Some(player::Sleep::Off),
        "song" => Some(player::Sleep::EndOfSong),
        "album" => Some(player::Sleep::EndOfAlbum),
        n => n
            .parse::<f64>()
            .ok()
            .filter(|m| *m > 0.0 && m.is_finite())
            .map(|m| player::Sleep::At(std::time::Instant::now() + std::time::Duration::from_secs_f64(m * 60.0))),
    }
}

thread_local! {
    static STARTED: Cell<bool> = const { Cell::new(false) };
}

/// One-time setup in the primary instance: playback, library, MPRIS.
fn start(app: &gtk::Application) {
    if STARTED.with(|s| s.replace(true)) {
        return;
    }
    online::init();
    player::init();
    library::store::reload();
    // Let the window appear first, then look for new music.
    glib::timeout_add_local_once(std::time::Duration::from_millis(600), || library::store::rescan(false));
    library::store::start_watching();
    mpris::start(app);
}

/// Songs from the command line: files, folders (everything inside, in path
/// order) and M3U playlists.
fn expand(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for f in files {
        if f.is_dir() {
            let mut inside: Vec<PathBuf> = walkdir::WalkDir::new(f)
                .follow_links(true)
                .into_iter()
                .flatten()
                .filter(|e| e.file_type().is_file() && library::is_audio(e.path()))
                .map(|e| e.into_path())
                .collect();
            inside.sort();
            out.extend(inside);
        } else if f.extension().is_some_and(|e| e.eq_ignore_ascii_case("m3u") || e.eq_ignore_ascii_case("m3u8")) {
            if let Ok(bytes) = std::fs::read(f) {
                let base = f.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                out.extend(library::m3u::parse(&String::from_utf8_lossy(&bytes), &base).into_iter().filter(|p| p.exists()));
            }
        } else if f.is_file() {
            out.push(f.clone());
        }
    }
    out
}

fn main() -> glib::ExitCode {
    nyx_core::app::init(nyx_core::app::App { slug: "nyx-music", name: "Music" });
    nyx_core::migrate::from(&["nexus-music"]);
    let id = std::env::var("NYX_MUSIC_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    nyx_core::control::describe_if_asked(&control::description(&id));
    let mut args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }

    // GTK's Vulkan renderer enumerates every GPU at startup, which wakes a
    // sleeping discrete GPU on hybrid laptops. GL renders only on the one in use.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
    }

    // Started by D-Bus for an action (no window): GLib leaves this flag to apps that
    // handle their own command line.
    let service = args.iter().any(|a| a == "--gapplication-service");
    args.retain(|a| a != "--gapplication-service");
    let mut flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    if service {
        flags |= gio::ApplicationFlags::IS_SERVICE;
    }
    let app = gtk::Application::builder().application_id(&id).flags(flags).build();
    // The nyx command, Glimpse and Nyx Assistant play and read the library through this
    // (primary instance only).
    app.connect_startup(move |app| {
        control::serve(app);
        control::hold_while_playing(app);
        if service {
            start(app);
            app.set_inactivity_timeout(10_000);
        }
    });
    app.connect_command_line(|app, cl| {
        let argv: Vec<String> = cl.arguments().iter().map(|a| a.to_string_lossy().to_string()).collect();
        let has = |flag: &str| argv.iter().any(|a| a == flag);
        let value_of = |flag: &str| argv.iter().position(|a| a == flag).and_then(|i| argv.get(i + 1)).cloned();
        let section = value_of("--section");
        let sleep = value_of("--sleep");
        let mut files = Vec::new();
        let mut urls = Vec::new();
        let mut skip = true; // argv[0]
        for a in &argv {
            if std::mem::take(&mut skip) {
                continue;
            }
            if a == "--section" || a == "--sleep" {
                skip = true;
                continue;
            }
            if a.starts_with("--") {
                continue;
            }
            if a.starts_with("http://") || a.starts_with("https://") {
                urls.push(a.clone());
                continue;
            }
            // Relative paths resolve against the caller's directory, and URIs work too.
            if let Some(p) = cl.create_file_for_arg(a).path() {
                files.push(p);
            }
        }

        if has("--toggle")
            && let Some(w) = window::window()
            && w.is_visible()
        {
            w.close();
            return glib::ExitCode::SUCCESS;
        }
        start(app);
        let remote = ["--play-pause", "--play", "--pause", "--stop", "--next", "--previous", "--sleep"].iter().any(|f| has(f));
        // Playback commands to a running window shouldn't pop it up.
        if !(remote && window::window().is_some() && files.is_empty() && urls.is_empty() && section.is_none()) {
            window::present(app, section.as_deref());
        }
        if has("--mini") {
            window::toggle_mini_player();
        }
        if has("--play-pause") {
            player::toggle();
        }
        if has("--play") {
            player::play();
        }
        if has("--pause") {
            player::pause();
        }
        if has("--stop") {
            player::stop();
        }
        if has("--next") {
            player::next();
        }
        if has("--previous") {
            player::previous();
        }
        if let Some(when) = &sleep {
            match parse_sleep(when) {
                Some(s) => player::set_sleep(s),
                None => {
                    eprintln!("nyx-music: --sleep takes minutes, song, album or off");
                    return glib::ExitCode::FAILURE;
                }
            }
        }
        let songs = expand(&files);
        if !songs.is_empty() {
            if has("--enqueue") {
                let n = songs.len();
                player::enqueue(songs);
                window::toast(&tracklist::added_text(n, "to the queue"));
            } else {
                player::play_tracks(songs, 0);
            }
        }
        if !urls.is_empty() {
            online::open_urls(urls, has("--enqueue"));
        }
        glib::ExitCode::SUCCESS
    });
    app.connect_shutdown(|_| {
        player::save();
        prefs::flush();
        player::stop();
    });
    app.run_with_args(&args)
}
