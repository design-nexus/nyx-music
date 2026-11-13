//! Playlists: one page each, plus making, importing, exporting and dropping
//! songs onto them in the sidebar.

use crate::library::{db, m3u, store};
use crate::tracklist::{Col, Options, TrackTable};
use crate::widgets;
use crate::{fmt, views, window};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

const DRAG_PREFIX: &str = "nyx-music-paths\n";

/// Every playlist with its songs, read off the UI thread.
pub fn load_items(done: impl FnOnce(Vec<(db::Playlist, Vec<PathBuf>)>) + 'static) {
    let lists = store::playlists();
    crate::cmd::background(
        move || -> anyhow::Result<Vec<(db::Playlist, Vec<PathBuf>)>> {
            let c = db::open()?;
            lists
                .into_iter()
                .map(|p| {
                    let items = db::playlist_paths(&c, p.id)?;
                    Ok((p, items))
                })
                .collect()
        },
        |r| done(r.unwrap_or_default()),
    );
}

/// What a song drag carries: a marker line, then one path per line.
pub fn drag_payload(paths: &[String]) -> String {
    format!("{DRAG_PREFIX}{}", paths.join("\n"))
}

fn parse_payload(s: &str) -> Option<Vec<PathBuf>> {
    let rest = s.strip_prefix(DRAG_PREFIX)?;
    Some(rest.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
}

/// Let songs be dropped onto a playlist's sidebar entry.
pub fn accept_drops(button: &gtk::Button, id: i64) {
    let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::COPY);
    target.connect_drop(move |_, value, _, _| {
        let Some(paths) = value.get::<String>().ok().and_then(|s| parse_payload(&s)) else { return false };
        append(id, paths);
        true
    });
    let b = button.clone();
    target.connect_enter(move |_, _, _| {
        b.add_css_class("drop-hover");
        gdk::DragAction::COPY
    });
    let b = button.clone();
    target.connect_leave(move |_| b.remove_css_class("drop-hover"));
    button.add_controller(target);
}

fn name_of(id: i64) -> String {
    store::playlists().into_iter().find(|p| p.id == id).map(|p| p.name).unwrap_or_default()
}

pub fn append(id: i64, paths: Vec<PathBuf>) {
    crate::online::keep(&paths);
    let n = paths.len();
    let name = name_of(id);
    store::edit_playlists(
        move |c| db::append_playlist(c, id, &paths),
        move || {
            window::toast(&format!("Added {} to {name}.", fmt::count(n, "song", "songs")));
        },
    );
}

/// Ask for a name and make a playlist (with `paths` in it, if any). With no
/// songs it also offers importing an M3U file.
pub fn new_dialog(paths: Vec<PathBuf>) {
    crate::online::keep(&paths);
    let from_songs = !paths.is_empty();
    let desc = if from_songs { format!("With {}.", fmt::count(paths.len(), "song", "songs")) } else { String::new() };
    let extra = widgets::ask_text("New playlist", &desc, "", "Create", move |name| {
        let p = paths.clone();
        store::edit_playlists(
            move |c| db::create_playlist(c, &name, &p).map(|_| ()),
            move || {
                // Open the newest playlist (the highest id).
                if let Some(id) = store::playlists().iter().map(|p| p.id).max() {
                    window::navigate(&format!("playlist:{id}"));
                }
            },
        );
    });
    if !from_songs {
        let import = gtk::Button::with_label("Import a playlist file…");
        import.add_css_class("flat");
        import.set_halign(gtk::Align::Start);
        import.connect_clicked(|b| {
            if let Some(w) = b.root().and_downcast::<gtk::Window>() {
                w.close();
            }
            import_dialog();
        });
        extra.append(&import);
    }
}

pub fn import_dialog() {
    let dialog = gtk::FileDialog::builder().title("Import playlist").modal(true).build();
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&widgets::m3u_filter());
    dialog.set_filters(Some(&filters));
    dialog.open(window::window().as_ref(), gio::Cancellable::NONE, |res| {
        let Ok(file) = res else { return };
        let Some(path) = file.path() else { return };
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported".into());
        let Ok(bytes) = std::fs::read(&path) else {
            window::toast("Couldn't read that file.");
            return;
        };
        let text = String::from_utf8_lossy(&bytes);
        let base = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let items: Vec<PathBuf> = m3u::parse(&text, &base).into_iter().filter(|p| p.exists()).collect();
        if items.is_empty() {
            window::toast("None of the songs in that playlist were found.");
            return;
        }
        let n = items.len();
        store::edit_playlists(
            move |c| db::create_playlist(c, &name, &items).map(|_| ()),
            move || {
                window::toast(&format!("Imported {}.", fmt::count(n, "song", "songs")));
                if let Some(id) = store::playlists().iter().map(|p| p.id).max() {
                    window::navigate(&format!("playlist:{id}"));
                }
            },
        );
    });
}

fn export(id: i64, tracks: Vec<Rc<crate::library::Track>>) {
    let dialog =
        gtk::FileDialog::builder().title("Export playlist").modal(true).initial_name(format!("{}.m3u8", name_of(id))).build();
    dialog.save(window::window().as_ref(), gio::Cancellable::NONE, move |res| {
        let Ok(file) = res else { return };
        let Some(path) = file.path() else { return };
        let owned: Vec<crate::library::Track> = tracks.iter().map(|t| (**t).clone()).collect();
        let base = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        match crate::cmd::atomic_write(&path, m3u::write(&owned, &base)) {
            Ok(()) => window::toast(&format!("Saved {}.", crate::paths::pretty(&path))),
            Err(e) => window::toast(&format!("Couldn't save the playlist: {e}")),
        }
    });
}

/// The page for one playlist.
pub fn build(id: i64) -> gtk::Widget {
    let page = widgets::page(&format!("playlist-{id}"));
    page.fill();
    let current: Rc<RefCell<Vec<Rc<crate::library::Track>>>> = Rc::default();

    let toolbar = widgets::hbox(8);
    toolbar.add_css_class("toolbar");
    let c = current.clone();
    let play = views::play_buttons(move || c.borrow().iter().map(|t| t.path.clone()).collect());
    toolbar.append(&play);
    let spacer = widgets::hbox(0);
    spacer.set_hexpand(true);
    toolbar.append(&spacer);
    let rename = widgets::icon_button("document-edit-symbolic", "Rename");
    rename.connect_clicked(move |_| {
        widgets::ask_text("Rename playlist", "", &name_of(id), "Rename", move |new| {
            store::edit_playlists(
                move |c| db::rename_playlist(c, id, &new),
                move || {
                    let pid = format!("playlist:{id}");
                    window::remove_page(&pid);
                    window::navigate(&pid);
                },
            );
        });
    });
    toolbar.append(&rename);
    let c = current.clone();
    let exp = widgets::icon_button("document-save-symbolic", "Export as M3U");
    exp.connect_clicked(move |_| export(id, c.borrow().clone()));
    toolbar.append(&exp);
    let delete = widgets::two_click("Delete", "Click again to delete", move || {
        let name = name_of(id);
        store::edit_playlists(
            move |c| db::delete_playlist(c, id),
            move || {
                window::remove_page(&format!("playlist:{id}"));
                window::toast(&format!("Deleted {name}."));
            },
        );
    });
    toolbar.append(&delete);
    page.body.append(&toolbar);

    let summary = widgets::label("", "dim");
    summary.add_css_class("mono");
    summary.add_css_class("detail-meta");
    summary.set_margin_bottom(10);
    page.body.append(&summary);

    let (c, r) = (current.clone(), current.clone());
    let table = TrackTable::new(Options {
        cols: &[Col::Num, Col::Title, Col::Artist, Col::Album, Col::Time],
        sortable: false,
        positions: true,
        key: Some("playlist"),
        extra: vec![(
            "Remove from playlist",
            Rc::new(move |idx: Vec<usize>| {
                let before: Vec<PathBuf> = c.borrow().iter().map(|t| t.path.clone()).collect();
                let keep: Vec<PathBuf> =
                    before.iter().enumerate().filter(|(i, _)| !idx.contains(i)).map(|(_, p)| p.clone()).collect();
                let n = idx.len();
                store::edit_playlists(
                    move |conn| db::set_playlist(conn, id, &keep),
                    move || {
                        let before = before.clone();
                        window::toast_action(
                            &format!("Removed {} from {}.", fmt::count(n, "song", "songs"), name_of(id)),
                            "Undo",
                            move || {
                                let b = before.clone();
                                store::edit_playlists(move |conn| db::set_playlist(conn, id, &b), || {});
                            },
                        );
                    },
                );
            }),
        )],
        reorder: Some(Rc::new(move |rows: Vec<usize>, to: usize| {
            let paths: Vec<PathBuf> = r.borrow().iter().map(|t| t.path.clone()).collect();
            let order = crate::player::queue::block_move(paths.len(), &rows, to);
            let moved: Vec<PathBuf> = order.iter().map(|&i| paths[i].clone()).collect();
            store::edit_playlists(move |conn| db::set_playlist(conn, id, &moved), || {});
        })),
        ..Default::default()
    });
    let empty = widgets::empty_state(
        "music-playlist-symbolic",
        "This playlist is empty",
        "Right-click songs and choose <b>Add to playlist</b>, or drag them onto it in the sidebar.",
        None,
    );
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_named(&table.root, Some("table"));
    stack.add_named(&empty, Some("empty"));
    page.body.append(&stack);
    let filter = views::filter_entry(&table, "Filter this playlist");
    toolbar.insert_child_after(&filter, Some(&spacer));

    let load = {
        let (current, table, stack) = (current.clone(), table.clone(), stack.clone());
        move || {
            let (current, table, stack, summary) = (current.clone(), table.clone(), stack.clone(), summary.clone());
            store::playlist_tracks(id, move |tracks| {
                let secs: f64 = tracks.iter().map(|t| t.duration).sum();
                summary.set_text(&format!("{} · {}", fmt::count(tracks.len(), "song", "songs"), fmt::total(secs)));
                stack.set_visible_child_name(if tracks.is_empty() { "empty" } else { "table" });
                table.set(&tracks);
                *current.borrow_mut() = tracks;
            });
        }
    };
    load();
    store::subscribe(&page.body, move |c| {
        if c == store::Change::Playlists || c == store::Change::Library {
            load();
        }
    });
    page.root.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_payload_round_trips() {
        let p = drag_payload(&["/a b.flac".into(), "/c.mp3".into()]);
        assert_eq!(parse_payload(&p), Some(vec![PathBuf::from("/a b.flac"), PathBuf::from("/c.mp3")]));
        assert_eq!(parse_payload("/etc/passwd"), None);
    }
}
