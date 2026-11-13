# Music

A music player for [Omarchy](https://omarchy.org). It plays your music library and
shows it as a live spectrum. It takes its colors from your Omarchy theme and fits a
half-screen tile.

## What it does

- **Home** opens the app: albums and playlists to jump back into, new podcast episodes
  with how much is left, your favorite stations (with what's on air while one plays)
  and the songs you play most.
- **Library:** scans your music folders (MP3, FLAC, Ogg Vorbis, Opus, AAC/M4A, WAV,
  AIFF, APE, WavPack, Musepack and more) into a local database. Later scans only read
  new or changed files, and it picks up music added, removed or renamed within a minute.
  - **Songs:** one sortable table of everything, with a filter box. Right-click the
    header to choose columns, including genre, date added and format (codec, sample
    rate and bit depth or bitrate). Tables keep their sort between sessions.
  - **Albums:** a cover grid, sorted by artist, title, year, recently added or most
    played. Covers come from the files' embedded art or a `cover.jpg` / `folder.jpg`
    beside them. Hover a cover to play the album; right-click it for more.
  - Multi-disc albums list each disc under its own heading.
  - **Artists** and **Genres**, each opening into their albums.
  - **Folders:** your music as it's laid out on disk.
  - **Recent and most played:** recently added albums, and the songs you played lately
    and most, on one page.
  - **Search** (<kbd>Ctrl</kbd>+<kbd>F</kbd>) across songs, albums and artists. Accents
    don't matter: "beyonce" finds Beyoncé.
- **Radio:** about 58,000 stations from the [Radio Browser](https://www.radio-browser.info)
  directory, no account needed.
  - Browse the most popular stations, or by genre or country, or search by name.
  - Star stations to keep them as favorites, grouped by genre or by country.
  - Add your own stations by their stream address or a `.pls` / `.m3u` link.
  - Shows the song a station is playing, when the station sends it.
- **Podcasts:** your shows across the top, marked when they have new episodes, and the
  chosen one below with Play latest and its episodes.
  - **Find a podcast:** Apple's top shows, its 19 categories and search, with no account
    or key.
  - Subscribe from the directory or by a show's RSS feed address. New episodes are
    checked for when Music opens.
  - Episodes pick up where you left off, and show how much is left.
  - Download episodes to play offline.
- Stations and episodes go in the queue and in playlists alongside your songs, and
  pick up again after a restart.
- **Now playing:** the cover and song over a blurred copy of the cover, a full-width
  spectrum analyzer or the lyrics, and what's up next.
  - Lyrics come from an `.lrc` file with the same name as the song, or from its tags.
    Timed lyrics follow the song; click a line to go there.
  - Optionally, its accents take the cover's color.
  - The analyzer draws 40 Hz–16 kHz on a log scale, in sync with what you hear.
  - Styles: bars, line or mirrored, with peak markers.
- **Playback:** gapless, crossfade (2 to 10 seconds; songs that run on within an album
  stay gapless), shuffle, repeat (all or one song), and ReplayGain volume leveling by
  song or album. It remembers the queue and position between sessions.
- **Sleep timer:** pause in 15 minutes to 1½ hours (fading out first), or at the end
  of the song or album.
- **Up next and playlists:** the queue sits at the top of Playlists in the sidebar, and
  opens from the player bar too.
  - Play next or add to the queue from any song's right-click menu.
  - Drag songs to reorder the queue or a playlist.
  - Removing songs or clearing the queue can be undone.
  - Make playlists from songs or from the queue, or drag songs onto a playlist in the
    sidebar.
  - Import and export M3U.
- **Equalizer:** ten bands and a preamp, with presets and your own saved ones. It opens from the player bar.
- **Media keys and the bar:** MPRIS, so media keys, `playerctl` and status bars control
  it and show the song and cover.
- **Notifications** when the song changes while the window is in the background.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/design-nexus/nyx-music/main/install.sh | bash -s -- music
```

This installs GTK 4 and GStreamer if they're missing, builds with Cargo, and installs
`nyx-music` to `~/.local/bin` along with a launcher entry. Add `-s -- --default` after
`bash` to also make it the default app for audio files.

To remove it, run the same line with `uninstall.sh` in place of `install.sh`. Add
`-s -- --purge` to also remove its settings, library database, playlists and downloaded
episodes. Your music files are never touched.

## Usage

```
nyx-music [OPTIONS] [FILES…]
```

| Option | What |
| --- | --- |
| `FILES…` | Play these songs, folders or `.m3u` playlists, or a stream's web address |
| `--enqueue` | Add `FILES` to the queue instead |
| `--section ID` | Open a page: `home`, `now-playing`, `songs`, `albums`, `artists`, `genres`, `folders`, `recent` (or one of its lists: `recently-added`, `recently-played`, `most-played`), `radio`, `podcasts`, `queue`, `equalizer`, `settings` |
| `--toggle` | Close the window if it's open, otherwise open it |
| `--play-pause`, `--play`, `--pause`, `--stop`, `--next`, `--previous` | Control playback without raising the window |
| `--sleep WHEN` | Set the sleep timer: a number of minutes, `song`, `album` or `off` |

| Key | What |
| --- | --- |
| <kbd>Space</kbd> | Play or pause |
| <kbd>Ctrl</kbd>+<kbd>←</kbd> / <kbd>→</kbd> | Previous / next song |
| <kbd>←</kbd> / <kbd>→</kbd> | Back / forward 5 seconds |
| <kbd>Ctrl</kbd>+<kbd>↑</kbd> / <kbd>↓</kbd> | Volume up / down |
| <kbd>M</kbd> | Mute or unmute |
| <kbd>S</kbd> | Shuffle on or off |
| <kbd>R</kbd> | Repeat: off, all, this song |
| <kbd>Ctrl</kbd>+<kbd>F</kbd> | Search the library |
| <kbd>Ctrl</kbd>+<kbd>L</kbd> | Now playing |
| <kbd>Ctrl</kbd>+<kbd>1</kbd>–<kbd>9</kbd> | Go to a page in the sidebar |
| <kbd>Alt</kbd>+<kbd>←</kbd> or the mouse's back button | Back |
| <kbd>Ctrl</kbd>+<kbd>B</kbd> | Collapse or expand the sidebar |
| <kbd>F1</kbd> or <kbd>?</kbd> | Show the shortcuts |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Close |

## Files

| Path | What |
| --- | --- |
| `~/.config/nyx-music/settings.toml` | Preferences: library folders, playback, equalizer, theme |
| `~/.config/nyx-music/themes/*.toml` | Your own themes |
| `~/.local/share/nyx-music/library.db` | The library, play counts, playlists, favorite stations and podcast subscriptions |
| `~/.local/share/nyx-music/podcasts/` | Downloaded episodes |
| `~/.local/share/nyx-music/state.json` | The queue and position, for resuming |
| `~/.cache/nyx-music/covers/` | Album covers, station logos and podcast art, scaled |

## License

MIT
