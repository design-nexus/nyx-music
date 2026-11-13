//! Music's actions on the control interface (`nyx_core::control`, API 2), for the `nyx`
//! command, Glimpse and Nyx Assistant: find and play from the library, queue, volume,
//! read what's playing, and the live `playback` and `playlists` topics Glimpse's Music
//! panel shows. Worked out only while something watches.

use crate::library::{store, Track};
use crate::player::{self, queue::Repeat, State};
use gtk::prelude::*;
use nyx_core::control::{text, Reply};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

/// Music's description: its actions, topics and Glimpse panel (`app_id` is the running
/// ID, which a test copy changes).
pub fn description(app_id: &str) -> Value {
    let what = json!({"enum": ["track", "album", "artist", "playlist"], "description": "what the name is"});
    let alias = |word: &str, action: &str| json!({"word": word, "arguments": {"action": action}});
    json!({
        "tools": [
            {"name": "music.search", "description": "Search the music library: tracks, albums, artists and playlists matching the words.",
             "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"], "additionalProperties": false},
             "annotations": {"title": "Search music"}, "x-nyx": {"risk": "read_only", "positional": ["query"], "aliases": ["find"], "result": "object"}},
            {"name": "music.play", "description": "Play a track, album, artist or playlist from the library (by name), optionally shuffled.",
             "inputSchema": {"type": "object", "properties": {"what": what, "name": {"type": "string"}, "shuffle": {"type": "boolean"}}, "required": ["what", "name"], "additionalProperties": false},
             "annotations": {"title": "Play"}, "x-nyx": {"risk": "writes", "positional": ["what", "name"], "result": "object"}},
            {"name": "music.queue", "description": "Add a track, album, artist or playlist to the queue: next, or at the end.",
             "inputSchema": {"type": "object", "properties": {"what": what, "name": {"type": "string"}, "next": {"type": "boolean"}}, "required": ["what", "name"], "additionalProperties": false},
             "annotations": {"title": "Add to the queue"}, "x-nyx": {"risk": "writes", "positional": ["what", "name"], "result": "object"}},
            {"name": "music.status", "description": "What Music is playing: track, artist, album, playing or paused, shuffle, repeat, what's next.",
             "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
             "annotations": {"title": "Now playing"}, "x-nyx": {"icon": "audio-x-generic-symbolic", "risk": "read_only", "aliases": ["now"], "result": "object"}},
            {"name": "music.control", "description": "Play, pause, toggle, next, previous or stop in Music; or set shuffle on/off.",
             "inputSchema": {"type": "object", "properties": {"action": {"enum": ["play", "pause", "toggle", "next", "previous", "stop", "shuffle_on", "shuffle_off"]}}, "required": ["action"], "additionalProperties": false},
             "annotations": {"title": "Playback control"},
             "x-nyx": {"risk": "writes", "positional": ["action"], "result": "object",
                       "aliases": [alias("pause", "pause"), alias("resume", "play"), alias("toggle", "toggle"), alias("next", "next"), alias("previous", "previous"), alias("prev", "previous"), alias("stop", "stop")]}},
            {"name": "music.volume", "description": "Set Music's volume (0 to 100), or mute and unmute it.",
             "inputSchema": {"type": "object", "properties": {"level": {"type": "number", "minimum": 0, "maximum": 100}, "muted": {"type": "boolean"}}, "additionalProperties": false},
             "annotations": {"title": "Volume"}, "x-nyx": {"risk": "writes", "positional": ["level"], "aliases": ["vol"], "result": "object"}},
            {"name": "music.outputs", "description": "The audio outputs Music can play through, the one in use marked.",
             "inputSchema": {"type": "object", "properties": {"prefix": {"type": "string"}, "limit": {"type": "integer", "minimum": 1}}, "additionalProperties": false},
             "annotations": {"title": "Audio outputs", "readOnlyHint": true}, "x-nyx": {"risk": "read_only", "aliases": ["outputs", "speakers"], "result": "list"}},
            {"name": "music.output", "description": "Play through an audio output (its name or ID from music.outputs), or default for the system's.",
             "inputSchema": {"type": "object", "properties": {"output": {"type": "string", "minLength": 1}}, "required": ["output"], "additionalProperties": false},
             "annotations": {"title": "Choose the output"},
             "x-nyx": {"risk": "writes", "confirmation": "never", "positional": ["output"], "aliases": ["through"], "suggest": {"output": "music.outputs"}, "result": "message"}},
            {"name": "music.playlists", "description": "The playlists in the music library.",
             "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
             "annotations": {"title": "Playlists"}, "x-nyx": {"risk": "read_only", "result": "list"}},
            {"name": "music.open", "description": "Open Music's window, at a page when given.",
             "inputSchema": {"type": "object", "properties": {"section": {"enum": ["home", "now-playing", "songs", "albums", "artists", "genres", "folders", "recent", "recently-added", "recently-played", "most-played", "radio", "podcasts", "queue", "equalizer", "settings"]}}, "additionalProperties": false},
             "annotations": {"title": "Open Music"}, "x-nyx": {"risk": "writes", "confirmation": "never", "positional": ["section"], "result": "none"}}
        ],
        "nyx": {
            "api": 2, "app": app_id, "name": "Music", "icon": crate::APP_ID, "namespaces": ["music"], "aliases": ["songs"],
            "bus_name": app_id, "activation": "dbus", "default": "music.search", "open": {"action": "music.open"},
            "topics": [{"name": "playback"}, {"name": "playlists"}],
            "panel": {"id": "music", "topics": ["playback", "playlists"], "size": {"min": [420, 320], "preferred": [500, 380]}}
        }
    })
}

thread_local! {
    static SERVICE: RefCell<Option<nyx_core::control::Service>> = const { RefCell::new(None) };
    static WATCHED: Cell<(bool, bool)> = const { Cell::new((false, false)) };
    static LAST_SECOND: Cell<i64> = const { Cell::new(-1) };
}

/// Register the interface (from `connect_startup`) and keep the topics current while watched.
pub fn serve(app: &gtk::Application) {
    let id = app.application_id().map(|s| s.to_string()).unwrap_or_else(|| crate::APP_ID.to_string());
    let app_for = app.clone();
    let Some(service) = nyx_core::control::serve_with(app, description(&id), move |request, answer| {
        handle_with(&app_for, &request.action, request.arguments, Box::new(move |r| answer(r.map_err(|e| nyx_actions::Error::new(nyx_actions::ErrorKind::Failed, e)))));
    }) else {
        return;
    };
    service.on_watch(|topic, on| {
        WATCHED.with(|w| {
            let (p, l) = w.get();
            w.set(match topic {
                "playback" => (on, l),
                "playlists" => (p, on),
                _ => (p, l),
            })
        });
        if on {
            publish(topic);
        }
    });
    SERVICE.with(|s| *s.borrow_mut() = Some(service));
    player::subscribe_global(|e| {
        if !WATCHED.with(|w| w.get().0) {
            return;
        }
        // The position ticks often; a second is enough for a compact view.
        if e == player::Event::Position {
            let second = player::position() as i64;
            if LAST_SECOND.with(|l| l.replace(second)) == second {
                return;
            }
        }
        publish("playback");
    });
}

fn publish(topic: &str) {
    SERVICE.with(|s| {
        let Some(service) = s.borrow().clone() else { return };
        match topic {
            "playback" => service.publish("playback", playback()),
            "playlists" => {
                let lists: Vec<Value> = store::playlists().into_iter().map(|p| json!({"id": p.id, "name": p.name})).collect();
                service.publish("playlists", json!({"items": lists}));
            }
            _ => {}
        }
    });
}

/// What the panel shows: the song, its cover, where it is, and the player's options.
/// The output in use, by name, as last seen (listing outputs takes a moment).
fn output_name() -> String {
    thread_local! {
        static SEEN: RefCell<(String, String)> = const { RefCell::new((String::new(), String::new())) };
    }
    let chosen = crate::prefs::get().output;
    if let Some(name) = SEEN.with(|s| (s.borrow().0 == chosen && !s.borrow().1.is_empty()).then(|| s.borrow().1.clone())) {
        return name;
    }
    let all = crate::player::engine::outputs();
    let name = all.iter().find(|o| if chosen.is_empty() { o.default } else { o.id == chosen }).map(|o| o.name.clone()).unwrap_or_default();
    SEEN.with(|s| *s.borrow_mut() = (chosen, name.clone()));
    name
}

fn playback() -> Value {
    let (volume, muted) = {
        let p = crate::prefs::get();
        (p.volume, p.muted)
    };
    let mut v = status();
    v["position"] = json!(player::position().floor());
    v["duration"] = json!(player::duration().round());
    v["volume"] = json!((volume * 100.0).round());
    v["muted"] = json!(muted);
    v["output"] = json!(output_name());
    v["can_next"] = json!(player::can_next());
    v["can_previous"] = json!(player::can_previous());
    v["art"] = json!(player::current()
        .filter(|t| !t.art.is_empty())
        .map(|t| crate::library::art::file(&t.art, true))
        .filter(|f| f.exists())
        .map(|f| f.display().to_string())
        .unwrap_or_default());
    v
}

/// A headless Music (started for a call) is held while it plays, so it quits when it's
/// idle, not halfway through a song.
pub fn hold_while_playing(app: &gtk::Application) {
    thread_local! {
        static HOLD: RefCell<Option<gtk::gio::ApplicationHoldGuard>> = const { RefCell::new(None) };
    }
    let app = app.clone();
    player::subscribe_global(move |e| {
        if e != player::Event::State {
            return;
        }
        let playing = player::state() == State::Playing;
        HOLD.with(|h| {
            let mut h = h.borrow_mut();
            if playing && h.is_none() {
                *h = Some(app.hold());
            } else if !playing {
                *h = None;
            }
        });
    });
}

fn track(t: &Track) -> Value {
    json!({"title": t.title, "artist": t.artist, "album": t.album, "seconds": t.duration.round()})
}

/// How well `name` fits `query`: 3 the same, 2 starts with it, 1 contains it.
fn fit(name: &str, query: &str) -> u8 {
    let (n, q) = (name.to_lowercase(), query.trim().to_lowercase());
    if q.is_empty() {
        0
    } else if n == q {
        3
    } else if n.starts_with(&q) {
        2
    } else if n.contains(&q) {
        1
    } else {
        0
    }
}

/// The one best match among `items`, or an error naming the candidates.
fn best<T>(items: Vec<T>, name: &str, label: impl Fn(&T) -> String) -> Result<T, String> {
    let top = items.iter().map(|i| fit(&label(i), name)).max().unwrap_or(0);
    if top == 0 {
        return Err(format!("nothing in the library matches “{name}”"));
    }
    let mut found: Vec<T> = items.into_iter().filter(|i| fit(&label(i), name) == top).collect();
    if found.len() > 1 {
        let names: Vec<String> = found.iter().take(5).map(&label).collect();
        let mut unique = names.clone();
        unique.dedup();
        if unique.len() > 1 {
            return Err(format!("several match: {}", names.join("; ")));
        }
    }
    Ok(found.remove(0))
}

fn paths(tracks: &[Rc<Track>]) -> Vec<PathBuf> {
    tracks.iter().map(|t| t.path.clone()).collect()
}

/// The tracks `what`/`name` names, handed to `then` (a playlist is read from the database).
fn resolve(args: &Value, then: impl FnOnce(Result<(String, Vec<Rc<Track>>), String>) + 'static) {
    let (what, name) = match (text(args, "what"), text(args, "name")) {
        (Ok(w), Ok(n)) => (w, n),
        (Err(e), _) | (_, Err(e)) => return then(Err(e)),
    };
    let found = match what.as_str() {
        "track" => best(store::tracks(), &name, |t| t.title.clone()).map(|t| (format!("{} by {}", t.title, t.artist), vec![t])),
        "album" => best(store::albums(), &name, |a| a.title.clone()).map(|a| (format!("{} by {}", a.title, a.artist), a.tracks.clone())),
        "artist" => best(store::artists(), &name, |b| b.name.clone()).map(|b| (b.name.clone(), b.albums.iter().flat_map(|a| a.tracks.clone()).collect())),
        "playlist" => match best(store::playlists(), &name, |p| p.name.clone()) {
            Ok(p) => {
                let label = p.name.clone();
                return store::playlist_tracks(p.id, move |tracks| then(Ok((label, tracks))));
            }
            Err(e) => Err(e),
        },
        other => Err(format!("unknown kind {other}")),
    };
    then(found)
}

fn status() -> Value {
    let state = match player::state() {
        State::Playing => "playing",
        State::Paused => "paused",
        State::Stopped => "stopped",
    };
    let repeat = match player::repeat() {
        Repeat::Off => "off",
        Repeat::All => "all",
        Repeat::One => "one",
    };
    let next: Vec<Value> = player::upcoming(5).iter().map(|p| track(&store::track_for(p))).collect();
    json!({
        "state": state,
        "track": player::current().map(|t| track(&t)),
        "position": player::position().round(),
        "shuffle": player::shuffle(),
        "repeat": repeat,
        "next": next,
    })
}

fn handle_with(app: &gtk::Application, name: &str, args: Value, reply: Reply) {
    match name {
        "music.volume" => {
            if let Some(level) = args["level"].as_f64() {
                player::set_volume(level / 100.0);
            }
            if let Some(muted) = args["muted"].as_bool() {
                player::set_muted(muted);
            }
            let p = crate::prefs::get();
            reply(Ok(json!({"volume": (p.volume * 100.0).round(), "muted": p.muted})))
        }
        "music.outputs" => {
            let prefix = args["prefix"].as_str().unwrap_or("").to_lowercase();
            let chosen = crate::prefs::get().output;
            let items: Vec<Value> = crate::player::engine::outputs()
                .into_iter()
                .filter(|o| o.name.to_lowercase().starts_with(&prefix))
                .map(|o| {
                    let current = if chosen.is_empty() { o.default } else { o.id == chosen };
                    json!({"title": o.name, "value": o.name, "id": o.id, "subtitle": if o.default { "System default" } else { "" }, "current": current})
                })
                .take(args["limit"].as_u64().unwrap_or(50) as usize)
                .collect();
            reply(Ok(json!({"items": items})))
        }
        "music.output" => {
            let wanted = args["output"].as_str().unwrap_or("").trim().to_lowercase();
            if wanted == "default" {
                crate::player::set_output("");
                return reply(Ok(json!({"message": "Music plays through the system's default output"})));
            }
            let all = crate::player::engine::outputs();
            let found = all.iter().find(|o| o.id.to_lowercase() == wanted || o.name.to_lowercase() == wanted).or_else(|| {
                let starting: Vec<_> = all.iter().filter(|o| o.name.to_lowercase().starts_with(&wanted)).collect();
                if starting.len() == 1 { Some(starting[0]) } else { None }
            });
            match found {
                Some(o) => {
                    crate::player::set_output(&o.id);
                    reply(Ok(json!({"message": format!("Music plays through {}", o.name)})))
                }
                None => reply(Err(format!("No single output is called “{wanted}”; music outputs lists them"))),
            }
        }
        "music.playlists" => {
            let items: Vec<Value> = store::playlists().into_iter().map(|p| json!({"title": p.name, "id": p.id})).collect();
            reply(Ok(json!({"items": items})))
        }
        "music.open" => {
            crate::window::present(app, args["section"].as_str());
            reply(Ok(Value::Null))
        }
        _ => handle(name, args, reply),
    }
}

pub fn handle(name: &str, args: Value, reply: Reply) {
    match name {
        "music.search" => reply((|| {
            let q = text(&args, "query")?;
            let hit = |s: &str| fit(s, &q) > 0;
            let tracks: Vec<Value> = store::tracks().iter().filter(|t| hit(&t.title)).take(10).map(|t| track(t)).collect();
            let albums: Vec<Value> = store::albums().iter().filter(|a| hit(&a.title)).take(10).map(|a| json!({"title": a.title, "artist": a.artist, "year": a.year})).collect();
            let artists: Vec<String> = store::artists().into_iter().filter(|b| hit(&b.name)).take(10).map(|b| b.name).collect();
            let playlists: Vec<String> = store::playlists().into_iter().filter(|p| hit(&p.name)).take(10).map(|p| p.name).collect();
            Ok(json!({"tracks": tracks, "albums": albums, "artists": artists, "playlists": playlists}))
        })()),
        "music.play" => {
            let shuffle = args["shuffle"] == true;
            resolve(&args, move |found| {
                reply(found.and_then(|(label, tracks)| {
                    if tracks.is_empty() {
                        return Err(format!("{label} has no tracks"));
                    }
                    if shuffle {
                        player::shuffle_tracks(paths(&tracks));
                    } else {
                        player::play_tracks(paths(&tracks), 0);
                    }
                    Ok(json!({"playing": label, "tracks": tracks.len(), "shuffled": shuffle}))
                }))
            });
        }
        "music.queue" => {
            let next = args["next"] == true;
            resolve(&args, move |found| {
                reply(found.and_then(|(label, tracks)| {
                    if tracks.is_empty() {
                        return Err(format!("{label} has no tracks"));
                    }
                    if next {
                        player::play_next(paths(&tracks));
                    } else {
                        player::enqueue(paths(&tracks));
                    }
                    Ok(json!({"queued": label, "tracks": tracks.len(), "next": next}))
                }))
            });
        }
        "music.status" => reply(Ok(status())),
        "music.control" => reply((|| {
            match text(&args, "action")?.as_str() {
                "play" => player::play(),
                "pause" => player::pause(),
                "toggle" => player::toggle(),
                "next" => player::next(),
                "previous" => player::previous(),
                "stop" => player::stop(),
                "shuffle_on" => player::set_shuffle(true),
                "shuffle_off" => player::set_shuffle(false),
                other => return Err(format!("unknown action {other}")),
            }
            Ok(status())
        })()),
        _ => reply(Err(format!("unknown tool {name}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_description_is_api_2() {
        let d = nyx_actions::Description::from_value(&description(crate::APP_ID), None).unwrap();
        assert_eq!(d.app.panel.as_ref().map(|p| p.id.as_str()), Some("music"));
        assert_eq!(d.action("music.status").unwrap().risk, nyx_actions::Risk::ReadOnly);
        assert!(!d.action("music.open").unwrap().asks(), "opening the window never asks");
    }

    #[test]
    fn best_match_prefers_exact_then_start() {
        let names = || vec!["Blue Train".to_string(), "Blue".to_string(), "Kind of Blue".to_string()];
        assert_eq!(best(names(), "blue", |s| s.clone()).unwrap(), "Blue");
        assert_eq!(best(names(), "blue t", |s| s.clone()).unwrap(), "Blue Train");
        assert_eq!(best(names(), "kind", |s| s.clone()).unwrap(), "Kind of Blue");
        assert!(best(names(), "red", |s| s.clone()).is_err());
        let two = vec!["Live at Leeds".to_string(), "Live at Pompeii".to_string()];
        assert!(best(two, "live", |s| s.clone()).unwrap_err().starts_with("several match"));
    }
}
