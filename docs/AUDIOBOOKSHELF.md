# Audiobookshelf in Music

Music supports an optional native Audiobookshelf connection alongside local music,
Radio Browser, and existing RSS subscriptions. The integration targets Audiobookshelf
2.37.2. Public HTTPS reachability and that version were confirmed on Ken's server;
authenticated playback and cross-device use still need live qualification.

## Connection and libraries

Open Audiobooks in the sidebar and choose **Connect Audiobookshelf**, or open
Settings → Audiobookshelf. Enter the server address, username, and password, then
choose **Sign in**. Before sign-in, only the connection form is shown. After sign-in,
settings are organized into Connection, Libraries, Playback, Synchronization, and
Downloads cards. Errors appear beside the connection form and can be dismissed.
The password is entered in a masked field. Music uses the native JWT login/refresh flow.
The password is not stored. Refresh tokens are stored through the desktop Secret
Service (`secret-tool`); the access token stays in memory. A working desktop keyring
is required. TLS certificate checks remain enabled. Reverse-proxy base paths and
Tailscale addresses are accepted. Redirects are not followed with account credentials.
OIDC-only servers are outside this implementation.

Book libraries are selected after the first connection and can then be deselected
individually. Podcast libraries are optional and disabled initially. Disconnecting
removes the saved refresh credential and pauses downloads while retaining cached
metadata, downloaded files, bookmarks, and pending progress for that account.

Audiobooks is always visible in the sidebar. Before setup it offers a direct
connection action; empty-library, expired-session, and offline states offer the
appropriate next step. Downloaded media remains accessible through Downloads.
The library offers paginated browsing and search, recently added, in-progress and
completed views, genres, series, and collections. Details show available metadata,
plain-text descriptions, chapters, bookmarks, and playback/download actions.
Home includes cached recently added books, Continue Listening, and the last audiobook.
Music's existing library section IDs, playlists, radio, and RSS subscriptions remain.

## Playback and progress

Spoken media uses the existing GStreamer engine in an independent playback context;
the music queue is retained while a book is active. Books never enter the ordinary
music shuffle/crossfade queue. Chapters and multipart files share one timeline.
Now playing, the player bar, media keys, and MPRIS control the active context.
Rate adjustment is 0.5–3×, using `scaletempo` when available to preserve pitch.
Default skip intervals are 15 seconds backward and 30 forward. The existing sleep
timer applies to books, with an optional default duration in provider settings.

Local checkpoints are saved every 10 seconds of active playback and on seek, pause,
stop, switching, and shutdown. Online sessions report every 15 seconds and flush on
pause/stop. Switching closes the online session and reconciles its durable cumulative
snapshot; offline sessions have stable UUIDs so replay updates the same session.
Shutdown always retains a durable checkpoint; an interrupted final HTTP request is
replayed after reconnect. Nothing depends on a successful network request to retain
local listening progress.

Before uploading, Music compares server progress with its last acknowledged baseline.
A remote-only change is adopted. If both sides changed, both positions remain stored
and book details offer **Use server** or **Use this device**. Deliberate rewinds are
not replaced by a "furthest position wins" rule. Completion follows server library
thresholds, with explicit completed/unfinished actions in details.

Audiobookshelf's online sync endpoint does not expose an atomic compare-and-swap.
Music checks before and after writes and preserves detected conflicts, but a truly
simultaneous server write still has a race window. Client clock differences can also
affect the server's timestamp-based offline-session acceptance. These are not claims
of conflict-free simultaneous playback.

## Downloads and podcasts

Download individual books or episodes from details. Downloads shows byte progress,
pause/resume/cancel, offline playback, and removal, plus existing RSS downloads.
HTTP Range/If-Range requests resume validated partial files; an ignored range restarts
that file. Completed files are flushed and atomically renamed. Interrupted jobs become
paused after restart. An audiobook is available offline once all parts are complete.
The explicit **Play offline** action opens local media without an initial server
request. Ordinary Resume checks remote progress, with a short fallback timeout for
already downloaded media. Pending progress retries once per minute while idle.
No libraries are downloaded automatically.

Audiobookshelf shows and episodes retain server/account/item/episode identities.
When an identical feed is already subscribed through RSS, its RSS detail offers the
Audiobookshelf source and the extra provider list hides the duplicate. Titles alone
are never used to merge shows or subscriptions.

## Storage and compatibility

- `library.db`: additive `abs_records` table for provider configuration, metadata,
  download manifests, bookmarks, progress baselines, conflicts, and durable sessions.
- `~/.local/share/nyx-music/audiobookshelf/`: application-managed media and `.part` files,
  isolated by account/item identity. XDG data overrides are honored.
- Existing cover-cache directory: authenticated artwork cached under non-secret keys;
  concurrent requests for the same cover are combined.
- Desktop Secret Service: refresh credentials only; no tokens are written to the
  database, saved music queue, MPRIS metadata, or application diagnostic messages.

Custom download-root migration and automatic local audiobook classification are not
included. The existing music scanner and its interpretation of music folders remain.
Other accounts' downloads are retained but not exposed as the current account's media.

## Validation and remaining qualification

Automated coverage includes native token refresh/retry against a local HTTP fixture,
base-path validation, response envelopes, account/episode identities, multipart
metadata, chapter file boundaries, reconciliation including rewinds, acknowledgement
of newer checkpoints, podcast parsing, and invalid/ignored Range responses. Existing
application unit tests cover music queue, radio/podcast parsing, database, and spectrum
behavior. Validation passed: 67 application unit tests (including nine new provider tests),
`cargo clippy --locked --all-targets -- -D warnings`, `git diff --check`, and a
locked release build. The installer installs the binary, launcher, and icon.

The following still require secure sign-in and live/manual use: real authentication
expiry and keyring interaction, multiple/large libraries, real codecs and multipart
transitions, speed/chapter controls, full downloads and interruption/resumption,
restart/offline playback, cross-device resume and conflict choices, server outages,
and responsive/accessibility/theme review. Compilation and fixture tests do not prove
those workflows. The app is installed without opening or restarting it.

API references:
[API reference](https://api.audiobookshelf.org/),
[native JWT flow](https://github.com/advplyr/audiobookshelf/discussions/4460),
[v2.37.2 session implementation](https://github.com/advplyr/audiobookshelf/blob/v2.37.2/server/managers/PlaybackSessionManager.js).
