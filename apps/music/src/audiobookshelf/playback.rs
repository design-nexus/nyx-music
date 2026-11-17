//! Spoken media uses the existing GStreamer engine with a book-wide timeline.
//! All GTK and engine operations stay on the main thread; HTTP runs on the provider runtime.
use super::{api, config, downloads, model::*, notify, set_status, storage};
use crate::{
    library::{Kind, Track},
    player::{self, Event, State, engine::Engine},
};
use gtk::glib;
use reqwest::Method;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::Instant,
};

struct Playing {
    book: Book,
    engine: Engine,
    sources: Vec<String>,
    offsets: Vec<f64>,
    part: usize,
    position: f64,
    state: State,
    speed: f64,
    pending: Option<f64>,
    buffering: bool,
    session: Option<String>,
    local_id: String,
    started: i64,
    start_position: f64,
    listened: f64,
    unsynced: f64,
    last: Instant,
    checkpoint: Instant,
    sync_at: Instant,
    syncing: bool,
}
struct Prepared {
    book: Book,
    sources: Vec<String>,
    offsets: Vec<f64>,
    session: Option<String>,
    position: f64,
}
thread_local! {
    static ACTIVE:RefCell<Option<Playing>>=const{RefCell::new(None)};
    static GENERATION:Cell<u64>=const{Cell::new(0)};
}
fn with<R>(f: impl FnOnce(&mut Playing) -> R) -> Option<R> {
    ACTIVE.with(|p| p.borrow_mut().as_mut().map(f))
}
pub fn active() -> bool {
    ACTIVE.with(|p| p.borrow().is_some())
}
pub fn current_book() -> Option<Book> {
    with(|p| p.book.clone())
}
pub fn track() -> Option<Rc<Track>> {
    with(|p| {
        Rc::new(Track {
            kind: Kind::Audiobook,
            path: PathBuf::from(p.book.id.key()),
            title: p.book.title.clone(),
            artist: p.book.author.clone(),
            album: p.book.series.clone(),
            duration: p.book.duration,
            art: p.book.art.clone(),
            ..Track::default()
        })
    })
}
pub fn state() -> State {
    with(|p| p.state).unwrap_or(State::Stopped)
}
pub fn position() -> f64 {
    with(|p| p.position).unwrap_or_default()
}
pub fn duration() -> f64 {
    with(|p| p.book.duration).unwrap_or_default()
}
pub fn rate() -> f64 {
    with(|p| p.speed).unwrap_or(1.0)
}
fn emit(e: Event) {
    player::provider_event(e);
}
pub fn init() {
    glib::timeout_add_local(std::time::Duration::from_millis(250), || {
        tick();
        glib::ControlFlow::Continue
    });
    // Retry only pending work, at a modest interval; browsing metadata is never polled.
    glib::timeout_add_local(std::time::Duration::from_secs(60), || {
        if !active() && super::has_pending() {
            super::sync_pending();
        }
        glib::ControlFlow::Continue
    });
}
pub fn start(book: Book) {
    start_at(book, None, true);
}
pub fn start_at(book: Book, requested: Option<f64>, autoplay: bool) {
    start_impl(book, requested, autoplay, false);
}
pub fn start_offline(book: Book) {
    start_impl(book, None, true, true);
}
fn start_impl(book: Book, requested: Option<f64>, autoplay: bool, offline_only: bool) {
    if active() {
        deactivate();
    } else {
        player::pause();
    }
    let generation = GENERATION.with(|g| {
        let n = g.get() + 1;
        g.set(n);
        n
    });
    let c = config();
    set_status("Opening audiobook…");
    api::run(
        async move {
            let offline = downloads::files(&book);
            let book = if offline.is_some() {
                storage::get::<Download>("download", &book.id.key()).book
            } else {
                api::detail(&c, &book.id).await?
            };
            anyhow::ensure!(!offline_only || offline.is_some(), "Download this item before playing offline.");
            if c.connected && c.sync && !offline_only {
                if offline.is_some() {
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_secs(3), super::sync_one(&c, &book.id, None, 0.0)).await;
                } else {
                    super::sync_one(&c, &book.id, None, 0.0).await?;
                }
            }
            let progress = storage::progress(&book.id);
            anyhow::ensure!(
                progress.conflict.is_none(),
                "Two listening positions changed. Choose a position in the book details."
            );
            let position = requested.unwrap_or(if progress.finished { 0.0 } else { progress.position });
            if let Some(files) = offline {
                let sources =
                    files.iter().map(|f| glib::filename_to_uri(f, None).map(|u| u.to_string()).unwrap_or_default()).collect();
                return Ok(Prepared {
                    offsets: book.files.iter().map(|f| f.offset).collect(),
                    book,
                    sources,
                    session: None,
                    position,
                });
            }
            let path = format!(
                "api/items/{}/play{}",
                api::encode(&book.id.item),
                book.id.episode.as_ref().map(|e| format!("/{}", api::encode(e))).unwrap_or_default()
            );
            let v=api::json(&c,Method::POST,&path,Some(&json!({"deviceInfo":{"deviceId":c.connection,"clientName":"Music","clientVersion":env!("CARGO_PKG_VERSION"),"osName":"Linux"},"mediaPlayer":"GStreamer","forceDirectPlay":true}))).await?;
            let session = v["id"].as_str().ok_or_else(|| anyhow::anyhow!("No playback session returned."))?.to_string();
            let tracks = v["audioTracks"].as_array().ok_or_else(|| anyhow::anyhow!("This book has no playable audio."))?;
            anyhow::ensure!(!tracks.is_empty(), "This book has no playable audio.");
            let sources = tracks
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    format!(
                        "{}public/session/{}/track/{}",
                        c.server,
                        api::encode(&session),
                        t["index"].as_u64().unwrap_or(i as u64 + 1)
                    )
                })
                .collect();
            let offsets = tracks.iter().map(|t| t["startOffset"].as_f64().unwrap_or_default()).collect();
            Ok(Prepared { book, sources, offsets, session: Some(session), position })
        },
        move |r| {
            if GENERATION.with(|g| g.get()) != generation {
                if let Ok(p) = r {
                    close(p.session);
                }
                return;
            }
            match r {
                Ok(p) => {
                    if let Err(e) = activate(p, autoplay) {
                        set_status(e.to_string());
                    }
                }
                Err(e) => set_status(e.to_string()),
            }
        },
    );
}
fn activate(p: Prepared, autoplay: bool) -> anyhow::Result<()> {
    let mut engine = Engine::new()?;
    engine.id = 2;
    engine.watch(on_message);
    let c = config();
    let speed = c.speed.clamp(0.5, 3.0);
    let part = p.offsets.iter().rposition(|o| p.position >= *o).unwrap_or(0).min(p.sources.len().saturating_sub(1));
    anyhow::ensure!(!p.sources.is_empty(), "No playable audio files.");
    engine.load(&p.sources[part]);
    let pref = crate::prefs::get();
    engine.set_eq(&if pref.eq_enabled { pref.eq_bands } else { [0.0; 10] });
    engine.set_volume(player::linear_volume(pref.volume), pref.muted);
    let local_id = p.session.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let position = p.position.clamp(0.0, p.book.duration.max(0.0));
    let _ = storage::put("last", "book", &p.book);
    ACTIVE.with(|a| {
        *a.borrow_mut() = Some(Playing {
            book: p.book,
            engine,
            sources: p.sources,
            offsets: p.offsets,
            part,
            position,
            state: if autoplay { State::Playing } else { State::Paused },
            speed,
            pending: Some(position),
            buffering: false,
            session: p.session,
            local_id,
            started: storage::now(),
            start_position: position,
            listened: 0.0,
            unsynced: 0.0,
            last: Instant::now(),
            checkpoint: Instant::now(),
            sync_at: Instant::now(),
            syncing: false,
        })
    });
    with(|p| {
        if autoplay {
            p.engine.play();
        } else {
            p.engine.pause();
        }
    });
    if autoplay && c.sleep_minutes > 0 {
        player::set_sleep(player::Sleep::At(Instant::now() + std::time::Duration::from_secs(c.sleep_minutes as u64 * 60)));
    }
    if let Some(book) = current_book() {
        super::cover(&book, || {});
    }
    set_status("Ready");
    emit(Event::Track);
    emit(Event::State);
    emit(Event::Seeked);
    notify();
    Ok(())
}
fn on_message(msg: &gst::Message) {
    use gst::MessageView;
    match msg.view() {
        MessageView::AsyncDone(_) => {
            if let Some(position) = with(|p| p.pending.take()).flatten() {
                with(|p| {
                    let _ = p.engine.seek_rate(position - p.offsets[p.part], p.speed);
                });
            }
        }
        MessageView::Buffering(b) => {
            with(|p| {
                p.buffering = b.percent() < 100;
                if p.state == State::Playing {
                    if p.buffering {
                        p.engine.pause();
                    } else {
                        p.engine.play();
                    }
                }
            });
        }
        MessageView::Eos(_) => {
            let next = with(|p| if p.part + 1 < p.sources.len() { Some(p.offsets[p.part + 1]) } else { None }).flatten();
            if let Some(pos) = next {
                seek(pos);
            } else {
                checkpoint(true);
                if matches!(player::sleep(), player::Sleep::EndOfSong | player::Sleep::EndOfAlbum) {
                    player::set_sleep(player::Sleep::Off);
                }
                with(|p| {
                    p.position = p.book.duration;
                    p.state = State::Stopped;
                    p.engine.stop();
                });
                sync();
                emit(Event::State);
                notify();
            }
        }
        MessageView::Error(_) => {
            pause();
            set_status("Audiobook playback stopped. Check the connection and resume from the saved position.");
        }
        _ => {}
    }
}
fn tick() {
    let Some((save, synchronize)) = with(|p| {
        let dt = p.last.elapsed().as_secs_f64().min(1.0);
        p.last = Instant::now();
        if p.state != State::Playing {
            return (false, false);
        }
        if !p.buffering && p.pending.is_none() {
            if let Some(pos) = p.engine.position() {
                p.position = (p.offsets[p.part] + pos).min(p.book.duration);
            }
            p.listened += dt;
            p.unsynced += dt;
        }
        let pref = crate::prefs::get();
        p.engine.set_volume(player::linear_volume(pref.volume), pref.muted);
        p.engine.set_gain(
            10.0f64.powf(if pref.eq_enabled { pref.eq_preamp / 20.0 } else { 0.0 })
                * player::sleep_remaining().map_or(1.0, |s| (s / 8.0).clamp(0.0, 1.0)),
        );
        (p.checkpoint.elapsed().as_secs() >= 10, p.sync_at.elapsed().as_secs() >= 15)
    }) else {
        return;
    };
    if save || synchronize {
        checkpoint(false);
    }
    if synchronize {
        sync();
    }
    if let Some(remaining) = player::sleep_remaining()
        && remaining <= 0.0
        && state() == State::Playing
    {
        pause();
        player::set_sleep(player::Sleep::Off);
    }
    emit(Event::Position);
}
pub fn checkpoint(finished: bool) {
    let data = with(|p| {
        p.checkpoint = Instant::now();
        (p.book.clone(), p.position, p.local_id.clone(), p.started, p.start_position, p.listened)
    });
    let Some((b, position, id, started, start_position, listened)) = data else {
        return;
    };
    let previous = storage::progress(&b.id);
    let previous_listened = previous.session.as_ref().and_then(|s| s["timeListening"].as_f64()).unwrap_or(0.0);
    if !finished && (previous.position - position).abs() < 0.01 && (previous_listened - listened).abs() < 0.01 {
        return;
    }

    let result = storage::change_progress(&b.id, |p| {
        p.position = if finished { b.duration } else { position };
        p.finished = finished || (p.finished && position >= b.duration - 0.5);
        p.updated = storage::now();
        p.dirty = true;
        p.revision += 1;
        p.session = Some(
            json!({"id":id,"libraryItemId":b.id.item,"episodeId":b.id.episode,"libraryId":b.library,"mediaType":if b.podcast{"podcast"}else{"book"},"mediaMetadata":{"title":b.title},"displayTitle":b.title,"displayAuthor":b.author,"playMethod":3,"duration":b.duration,"startTime":start_position,"currentTime":p.position,"timeListening":listened,"startedAt":started,"updatedAt":p.updated}),
        );
    });
    if let Err(e) = result {
        set_status(format!("Couldn't save audiobook progress: {e}"));
    }
}
fn sync() {
    let c = config();
    if !c.connected || !c.sync {
        return;
    }
    let data = with(|p| {
        if p.syncing {
            return None;
        }
        p.syncing = true;
        p.sync_at = Instant::now();
        Some((p.book.id.clone(), p.session.clone(), p.unsynced))
    })
    .flatten();
    let Some((id, session, delta)) = data else {
        return;
    };
    let current_id = id.clone();
    api::run(async move { super::sync_one(&c, &id, session, delta).await }, move |r| {
        let failed = r.is_err();
        with(|p| {
            if p.book.id == current_id {
                p.syncing = false;
                if !failed {
                    p.unsynced = (p.unsynced - delta).max(0.0);
                } else {
                    p.session = None;
                }
            }
        });
        if let Err(e) = r {
            set_status(e.to_string());
        } else {
            if storage::progress(&current_id).conflict.is_some() {
                set_status("Listening positions changed on both devices. Open book details to choose one.");
            }
            notify();
            if current_book().is_some_and(|b| b.id == current_id) && state() != State::Playing {
                let p = storage::progress(&current_id);
                if p.dirty && p.conflict.is_none() {
                    sync();
                }
            }
        }
    });
}
pub fn play() {
    let Some(book) = current_book() else {
        return;
    };
    if state() == State::Stopped || with(|p| p.session.is_none() && !p.sources[0].starts_with("file:")).unwrap_or(false) {
        start_at(book, None, true);
        return;
    }
    // Refresh the remote position before resuming an online book.
    if config().connected && config().sync {
        start_at(book, None, true);
        return;
    }
    with(|p| {
        p.engine.play();
        p.state = State::Playing;
        p.last = Instant::now();
    });
    emit(Event::State);
}
pub fn pause() {
    with(|p| {
        p.engine.pause();
        p.state = State::Paused;
    });
    checkpoint(false);
    sync();
    emit(Event::State);
}
pub fn stop() {
    pause();
    with(|p| {
        p.engine.stop();
        p.state = State::Stopped;
    });
    emit(Event::State);
}
pub fn seek(secs: f64) {
    seek_inner(secs, true);
}
fn seek_inner(secs: f64, save: bool) {
    if !secs.is_finite() {
        return;
    }
    with(|p| {
        let secs = secs.clamp(0.0, p.book.duration.max(0.0));
        let part = p.offsets.iter().rposition(|o| secs >= *o).unwrap_or(0).min(p.sources.len() - 1);
        if part != p.part {
            p.part = part;
            p.engine.load(&p.sources[part]);
            p.pending = Some(secs);
            if p.state == State::Playing {
                p.engine.play();
            } else {
                p.engine.pause();
            }
        } else {
            let _ = p.engine.seek_rate(secs - p.offsets[part], p.speed);
        }
        p.position = secs;
    });
    if save {
        checkpoint(false);
    }
    emit(Event::Seeked);
}
pub fn set_rate(rate: f64) {
    if !rate.is_finite() {
        return;
    }
    with(|p| {
        p.speed = rate.clamp(0.5, 3.0);
        let _ = p.engine.seek_rate(p.position - p.offsets[p.part], p.speed);
    });
    emit(Event::Options);
}
pub fn chapter(next: bool) {
    let target = with(|p| {
        if next {
            p.book.chapters.iter().find(|c| c.start > p.position + 1.0).map(|c| c.start)
        } else {
            p.book.chapters.iter().rev().find(|c| c.start < p.position - 3.0).map(|c| c.start).or(Some(0.0))
        }
    })
    .flatten();
    if let Some(t) = target {
        seek(t);
    }
}
fn close(session: Option<String>) {
    if let Some(session) = session {
        let c = config();
        api::run(
            async move {
                api::checked(
                    api::request(&c, Method::POST, &format!("api/session/{}/close", api::encode(&session)), None).await?,
                )?;
                Ok(())
            },
            |_| {},
        );
    }
}
pub fn deactivate() {
    GENERATION.with(|g| g.set(g.get() + 1));
    if !active() {
        return;
    }
    checkpoint(false);
    let data = ACTIVE.with(|a| a.borrow_mut().take());
    if let Some(mut p) = data {
        p.engine.stop();
        let c = config();
        let id = p.book.id.clone();
        let session = p.session.clone();
        api::run(
            async move {
                if let Some(s) = session {
                    let _ = api::request(&c, Method::POST, &format!("api/session/{}/close", api::encode(&s)), None).await;
                }
                super::sync_one(&c, &id, None, 0.0).await
            },
            |r| {
                if let Err(e) = r {
                    set_status(e.to_string());
                }
                notify();
            },
        );
    }
    emit(Event::Track);
    emit(Event::State);
}
pub fn resolved(id: &MediaId, position: f64) {
    if current_book().is_some_and(|b| b.id == *id) {
        seek_inner(position, false);
    }
}
pub fn shutdown() {
    checkpoint(false); /* Durable local session is replayed after reconnect, even if shutdown interrupts HTTP. */
}
pub fn bookmark() {
    if let Some(b) = current_book() {
        let mut marks: Vec<Bookmark> = storage::get("bookmarks", &b.id.key());
        let pos = position();
        marks.push(Bookmark { position: pos, title: format!("Bookmark at {}", crate::fmt::time(pos)), pending: true });
        let _ = storage::put("bookmarks", &b.id.key(), &marks);
        super::ui::sync_bookmarks(b);
        notify();
    }
}

pub fn output(id: &str) {
    with(|p| {
        p.engine.set_output(id);
        p.pending = Some(p.position);
        if p.state == State::Playing {
            p.engine.play();
        } else {
            p.engine.pause();
        }
    });
}
pub fn equalizer() {
    let pref = crate::prefs::get();
    with(|p| p.engine.set_eq(&if pref.eq_enabled { pref.eq_bands } else { [0.0; 10] }));
}

pub fn synchronize() {
    if active() {
        checkpoint(false);
        sync();
    }
}
