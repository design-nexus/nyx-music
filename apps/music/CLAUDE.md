# Music — notes for working on this repo

- GTK4 (gtk4-rs 0.11) + Rust + GStreamer (gstreamer-rs 0.25). No libadwaita. The
  window is Nyx 2.0's one blended surface (see `STYLE.md`): the `nyx_ui::chrome::app_header`
  (the app and the sidebar's toggle, library search in the middle, mini player and
  settings at the end), the sidebar (Home and Now playing, then Library, Online and
  Playlists headings in their section colours with a bar each on the icon rail; Up next
  heads Playlists; the library's size at its foot) and beside it the pages over the
  player bar (Lyrics, Up next, the equalizer, sleep and volume at its right). No
  status bar. Home (`sections/home.rs`) opens the app; Podcasts is the row of your
  shows over the chosen one, with Find a podcast for the directory. Albums has a page head with a segmented sort; Now playing is two columns
  (cover and song; spectrum or lyrics and up next) over the blurred cover.
  Settings is a card over the window (`settings_dialog.rs`): it builds the settings
  page once and lists its groups (named by `Page::group`) on the left; `navigate("settings")`
  opens it. The equalizer is a card too (`panel_dialog.rs`), from the header or
  `navigate("equalizer")`. The theme, widgets, settings card and window frame come from
  the `nyx-*` crates. Every
  colour is a `@theme_*` token; the analyzer gets its colours from `theme::palette()`.
- Playback (`player/`): `engine.rs` wraps a `playbin` whose `audio-filter` is
  `audioconvert ! equalizer-10bands ! volume ! audioconvert ! spectrum`. The `volume`
  element carries ReplayGain × preamp; playbin's own volume is the user's (cubic).
  The bus is watched on the GTK main loop, so everything runs on the UI thread.
  Gapless: `about-to-finish` (streaming thread) swaps in `next_uri` and sets
  `switched`; the next STREAM_START advances the queue. `queue_changed()` must run
  after anything that changes what's next.
- Spectrum frames are queued with their running time and released by `current_frame()`
  only once the pipeline clock reaches them, so the bars match what's audible.
  `spectrum.rs` maps the bins to log bars (`map_bars`) and smooths them (`Smoother`);
  the tick callback stops itself when paused and idle.
- `player::mod` holds state; widgets subscribe with `player::subscribe(&widget, …)`
  (dropped with the widget) and the library with `store::subscribe`. Never hold a
  `with(...)` borrow while calling `emit`.
- Library: `scan.rs` runs on a thread, diffs (mtime, size) against SQLite and reads
  only what changed; files under a missing root are kept (unplugged drive). Covers are
  extracted once per album key into `~/.cache/nyx-music/covers/<fnv>{,-s}.jpg`.
  The UI keeps every track in memory (`store`), grouped into albums on load.
  `db.rs` opens a connection per call so it can run anywhere.
- Playlists store paths, not ids, so they survive rescans. Pages are `playlist:<id>`.
- Radio and podcasts (`online/`): a station or episode is a `Track` whose path is its
  `https://` address (`Kind::Station` / `Kind::Episode`), so the queue, playlists and
  `state.json` carry them unchanged; `store::track_for`/`find` hand such paths to
  `online`. Their details live in the `streams` table (plus `stations` for favourites
  and custom stations, `podcasts` for subscriptions) and in memory in `online::mod`.
  Items are only written to `streams` once queued, saved, played, downloaded or part of
  a subscription (`online::keep`, called by `player::play_tracks/enqueue/play_next` and
  the playlist actions). `radio.rs` is Radio Browser (a mirror is picked once per run),
  `podcast.rs` is Apple's charts/search plus RSS parsing (roxmltree), `http.rs` is
  blocking `ureq` run via `cmd::background`. Web pictures are cached like covers, keyed
  by `hash(url)` (`art::fetch`, `Cover::set_url`).
- Streams in the player: `MessageView::Tag` on a live station turns ICY "Artist - Title"
  into a copy of the current track (same path) and names bare stations from icy-name;
  `Buffering` pauses non-live streams until full. Episodes save their position every
  10 s, on pause and on quit, and resume through `pending_seek`.
- Crossfade: `Player` holds a second `Engine` (`spare`). Near a song's end
  `maybe_start_xfade` starts the next song there and `xfade_step` fades both gains
  (equal power); `finish_xfade` swaps the engines and advances the queue. Each engine's
  bus messages carry its `id` and are ignored unless it's the active one. Loading,
  seeking, pausing and stopping call `cancel_xfade`. Songs that run on within an album
  (`continues_album`) stay gapless. The sleep timer's fade (`Player.fade`) multiplies
  into `apply_gain`.
- Schema changes go in `db::migrate` (checked with `pragma_table_info`); setting
  `mtime = 0` makes the next scan reread every file to fill new columns.
- Song tables (`tracklist.rs`): `store → FilterListModel → SortListModel`. An
  `Options.key` keeps the sort and chosen columns in `prefs.table_sort` /
  `table_columns`. Drag-reorder hands back row indexes (`Row.index`), not view positions.
- MPRIS (`mpris.rs`) runs zbus' blocking API on its own thread: it reads a shared
  snapshot, sends commands back over a channel, and emits PropertiesChanged when the
  UI thread asks. Bus name `org.mpris.MediaPlayer2.music`.
- **Control** (`control.rs`): `nyx_core::control::serve_async` on the app's bus name, for
  Nyx Assistant: `music.search`, `play` / `queue` (by name; `store::playlist_tracks`
  answers later, hence async), `status`, `control`.
- Checks: `cargo clippy --all-targets -- -D warnings`, `cargo test`. Visual check:
  `MUSIC_SNAPSHOT=/tmp/x.png [MUSIC_SNAPSHOT_PLAY=1] music --section now-playing`
  (quit any running instance first, or run it under `dbus-run-session --` so a running
  one is left alone; it's single-instance). For tests, point
  `XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`XDG_CACHE_HOME` at scratch dirs with
  `library_folders` set, and use `MUSIC_AUDIO_SINK=fakesink` to play silently.
  `mode = "theme"`, `theme = "catppuccin-latte"` checks light mode.
  `MUSIC_SNAPSHOT_ALBUM=<part of a title>` opens that album first, and
  `MUSIC_SNAPSHOT_MAX=1` maximizes the window for a taller shot.
