//! The song table used by Songs, albums, playlists, the queue and search:
//! sortable columns, multi-select, a right-click menu, double-click to play,
//! and drag-and-drop onto playlists in the sidebar.

use crate::indicator::Indicator;
use crate::library::{Kind, Track, store};
use crate::sections::{albums, playlist, podcasts};
use crate::{cmd, fmt, player, prefs, widgets, window};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    /// A speaker on the song that's playing.
    Indicator,
    /// Track number (album) or position (playlists, queue); a speaker when playing.
    Num,
    Title,
    Artist,
    Album,
    Genre,
    Year,
    /// When the file joined the library (its modification time).
    Added,
    Time,
    Plays,
    /// Codec and quality: "FLAC · 44.1 kHz · 16-bit", "MP3 · 320 kbps".
    Format,
    /// When it was last played through.
    Played,
}

/// The columns a table with a `key` can show or hide, in display order.
const OPTIONAL: [Col; 8] = [Col::Artist, Col::Album, Col::Genre, Col::Year, Col::Added, Col::Time, Col::Plays, Col::Format];

impl Col {
    fn title(self) -> &'static str {
        match self {
            Col::Indicator | Col::Num => "",
            Col::Title => "Title",
            Col::Artist => "Artist",
            Col::Album => "Album",
            Col::Genre => "Genre",
            Col::Year => "Year",
            Col::Added => "Added",
            Col::Time => "Time",
            Col::Plays => "Plays",
            Col::Format => "Format",
            Col::Played => "Played",
        }
    }

    /// What settings.toml calls it.
    fn id(self) -> &'static str {
        match self {
            Col::Indicator => "indicator",
            Col::Num => "num",
            Col::Title => "title",
            Col::Artist => "artist",
            Col::Album => "album",
            Col::Genre => "genre",
            Col::Year => "year",
            Col::Added => "added",
            Col::Time => "time",
            Col::Plays => "plays",
            Col::Format => "format",
            Col::Played => "played",
        }
    }

    fn from_id(id: &str) -> Option<Col> {
        [Col::Title].into_iter().chain(OPTIONAL).find(|c| c.id() == id)
    }

    fn hidden_when_narrow(self) -> bool {
        matches!(self, Col::Year | Col::Plays | Col::Genre | Col::Added | Col::Format)
    }

    fn numeric(self) -> bool {
        matches!(self, Col::Num | Col::Year | Col::Time | Col::Plays | Col::Added | Col::Played)
    }
}

/// "FLAC · 44.1 kHz · 16-bit" for lossless files, "MP3 · 320 kbps" otherwise.
pub fn format_of(t: &Track) -> String {
    if t.is_remote() {
        return String::new();
    }
    let ext = t.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
    let mut parts = vec![ext];
    if t.bit_depth > 0 {
        if t.sample_rate > 0 {
            let khz = t.sample_rate as f64 / 1000.0;
            parts.push(if khz.fract() == 0.0 { format!("{khz:.0} kHz") } else { format!("{khz:.1} kHz") });
        }
        parts.push(format!("{}-bit", t.bit_depth));
    } else if t.bitrate > 0 {
        parts.push(format!("{} kbps", t.bitrate));
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" · ")
}

pub struct Row {
    pub track: Rc<Track>,
    /// Position in the list given to `set` (queue index, playlist position).
    pub index: usize,
}

pub type Extra = (&'static str, Rc<dyn Fn(Vec<usize>)>);
/// Move the rows at these indexes to just before the given index.
pub type Reorder = Rc<dyn Fn(Vec<usize>, usize)>;

pub struct Options {
    pub cols: &'static [Col],
    pub sortable: bool,
    /// Numbers in `Num` are positions (1, 2, 3…) rather than track numbers.
    pub positions: bool,
    /// Instead of playing the list from the activated row.
    pub on_activate: Option<Rc<dyn Fn(usize)>>,
    /// Extra menu items acting on the selected rows' indexes.
    pub extra: Vec<Extra>,
    /// A "DISC N" header above each disc (multi-disc albums).
    pub disc_sections: bool,
    /// Rows can be dragged to a new place (queue, playlists; unsorted tables only).
    pub reorder: Option<Reorder>,
    /// Names this table in settings.toml, so its sort and chosen columns are
    /// kept, and lets a right-click on the header choose the columns.
    pub key: Option<&'static str>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            cols: &[Col::Indicator, Col::Title, Col::Artist, Col::Album, Col::Year, Col::Time, Col::Plays],
            sortable: true,
            positions: false,
            on_activate: None,
            extra: Vec::new(),
            disc_sections: false,
            reorder: None,
            key: None,
        }
    }
}

#[derive(Clone)]
pub struct TrackTable {
    pub root: gtk::Box,
    pub view: gtk::ColumnView,
    store: gio::ListStore,
    sorted: gtk::SortListModel,
    selection: gtk::MultiSelection,
    filter: gtk::CustomFilter,
    terms: Rc<RefCell<Vec<String>>>,
    columns: Columns,
}

/// Every column a table has, and which of them the user wants shown.
#[derive(Clone)]
struct Columns {
    all: Rc<Vec<(Col, gtk::ColumnViewColumn)>>,
    chosen: Rc<RefCell<Vec<Col>>>,
}

impl Columns {
    fn apply(&self, narrow: bool) {
        let chosen = self.chosen.borrow();
        for (col, c) in self.all.iter() {
            let wanted = !OPTIONAL.contains(col) || chosen.contains(col);
            c.set_visible(wanted && !(narrow && col.hidden_when_narrow()));
        }
    }
}

thread_local! {
    static TABLES: RefCell<Vec<(glib::WeakRef<gtk::ColumnView>, Columns)>> = const { RefCell::new(Vec::new()) };
    /// The table a drag started in and the rows it carries, for reordering.
    static DRAGGING: RefCell<Option<(glib::WeakRef<gtk::ColumnView>, Vec<usize>)>> = const { RefCell::new(None) };
}

/// The row widget a cell sits in, which shows the drop line.
fn row_widget(cell: &gtk::Widget) -> Option<gtk::Widget> {
    let mut w = cell.parent();
    while let Some(p) = w {
        if p.css_name() == "row" {
            return Some(p);
        }
        w = p.parent();
    }
    None
}

fn row_of(obj: &glib::Object) -> Option<std::cell::Ref<'_, Row>> {
    obj.downcast_ref::<glib::BoxedAnyObject>().map(|b| b.borrow::<Row>())
}

fn cmp_text(a: &str, b: &str) -> Ordering {
    store::sort_key(a).cmp(&store::sort_key(b))
}

fn compare(col: Col, a: &Track, b: &Track) -> Ordering {
    let album_order = |a: &Track, b: &Track| {
        (a.disc_no.unwrap_or(1), a.track_no.unwrap_or(u32::MAX)).cmp(&(b.disc_no.unwrap_or(1), b.track_no.unwrap_or(u32::MAX)))
    };
    match col {
        Col::Title => cmp_text(&a.title, &b.title),
        Col::Artist => cmp_text(&a.artist, &b.artist)
            .then_with(|| a.year.cmp(&b.year))
            .then_with(|| cmp_text(&a.album, &b.album))
            .then_with(|| album_order(a, b)),
        Col::Album => cmp_text(&a.album, &b.album).then_with(|| album_order(a, b)),
        Col::Year => a.year.cmp(&b.year).then_with(|| cmp_text(&a.album, &b.album)).then_with(|| album_order(a, b)),
        Col::Time => a.duration.total_cmp(&b.duration),
        Col::Plays => a.plays.get().cmp(&b.plays.get()),
        Col::Genre => cmp_text(&a.genre, &b.genre).then_with(|| cmp_text(&a.artist, &b.artist)),
        Col::Added => a.mtime.cmp(&b.mtime),
        Col::Format => format_of(a).cmp(&format_of(b)),
        Col::Played => a.last_played.get().cmp(&b.last_played.get()),
        Col::Indicator | Col::Num => Ordering::Equal,
    }
}

fn is_playing(t: &Track) -> bool {
    player::current().is_some_and(|c| c.path == t.path)
}

impl TrackTable {
    pub fn new(opts: Options) -> TrackTable {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let terms: Rc<RefCell<Vec<String>>> = Rc::default();
        let tt = terms.clone();
        let filter = gtk::CustomFilter::new(move |o| {
            let terms = tt.borrow();
            terms.is_empty()
                || row_of(o).is_some_and(|r| {
                    let hay = r.track.haystack();
                    terms.iter().all(|t| hay.contains(t.as_str()))
                })
        });
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), None::<gtk::Sorter>);
        let selection = gtk::MultiSelection::new(Some(sorted.clone()));
        let view = gtk::ColumnView::new(Some(selection.clone()));
        view.add_css_class("track-table");
        view.set_reorderable(false);
        view.set_show_column_separators(false);
        let opts = Rc::new(opts);

        // With a key, every optional column exists and the chosen ones show.
        let cols: Vec<Col> = match opts.key {
            Some(_) => {
                let lead = opts.cols.iter().copied().filter(|c| !OPTIONAL.contains(c));
                lead.chain(OPTIONAL).collect()
            }
            None => opts.cols.to_vec(),
        };
        let saved = opts.key.and_then(|k| prefs::get().table_columns.get(k).cloned());
        let chosen: Vec<Col> = match saved {
            Some(ids) => ids.iter().filter_map(|i| Col::from_id(i)).collect(),
            None => opts.cols.to_vec(),
        };

        let mut table = TrackTable {
            root: widgets::vbox(0),
            view: view.clone(),
            store,
            sorted: sorted.clone(),
            selection,
            filter,
            terms,
            columns: Columns { all: Rc::default(), chosen: Rc::new(RefCell::new(chosen)) },
        };
        let mut built = Vec::new();

        let mut default_sort: Option<gtk::ColumnViewColumn> = None;
        let saved_sort = opts.key.and_then(|k| prefs::get().table_sort.get(k).cloned()).unwrap_or_default();
        let (saved_col, saved_desc) = match saved_sort.split_once(':') {
            Some((c, dir)) => (Col::from_id(c), dir == "desc"),
            None => (Col::from_id(&saved_sort), false),
        };
        let mut restore_sort: Option<gtk::ColumnViewColumn> = None;
        for &col in &cols {
            let factory = gtk::SignalListItemFactory::new();
            let t = table.clone();
            let o = opts.clone();
            factory.connect_setup(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
                let child: gtk::Widget = match col {
                    Col::Indicator => {
                        let i = Indicator::new();
                        let area = i.area.clone();
                        unsafe { area.set_data("indicator", i) };
                        area.upcast()
                    }
                    // The number, or the playing mark over it.
                    Col::Num => {
                        let l = widgets::label("", "mono");
                        l.add_css_class("cell-dim");
                        l.set_xalign(1.0);
                        let i = Indicator::new();
                        i.area.set_halign(gtk::Align::End);
                        i.area.set_margin_end(2);
                        let o = gtk::Overlay::new();
                        o.set_child(Some(&l));
                        o.add_overlay(&i.area);
                        unsafe { o.set_data("indicator", i) };
                        o.upcast()
                    }
                    _ => {
                        let l = widgets::label("", "");
                        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                        if col.numeric() {
                            l.add_css_class("mono");
                            l.add_css_class("cell-dim");
                            l.set_xalign(1.0);
                        }
                        if col == Col::Format || col == Col::Genre {
                            l.add_css_class("cell-dim");
                        }
                        l.upcast()
                    }
                };
                child.set_hexpand(true);
                t.attach_row_gestures(&child, item, &o);
                item.set_child(Some(&child));
            });
            let o = opts.clone();
            factory.connect_bind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
                let (Some(obj), Some(child)) = (item.item(), item.child()) else { return };
                let Some(row) = row_of(&obj) else { return };
                let t = &row.track;
                let playing = is_playing(t);
                if let Some(i) = unsafe { child.data::<Indicator>("indicator") } {
                    unsafe { i.as_ref() }.set_on(playing);
                }
                if col == Col::Indicator {
                    return;
                }
                let label = match col {
                    Col::Num => child.downcast_ref::<gtk::Overlay>().and_then(|o| o.child()).and_downcast::<gtk::Label>(),
                    _ => child.downcast_ref::<gtk::Label>().cloned(),
                };
                let Some(l) = label else { return };
                let text = match col {
                    Col::Num => {
                        if playing {
                            String::new()
                        } else if o.positions {
                            (row.index + 1).to_string()
                        } else {
                            t.track_no.map(|n| n.to_string()).unwrap_or_default()
                        }
                    }
                    Col::Title => t.title.clone(),
                    Col::Artist => t.artist.clone(),
                    Col::Album => t.album.clone(),
                    Col::Year => t.year.map(|y| y.to_string()).unwrap_or_default(),
                    Col::Time if t.is_live() => "LIVE".to_string(),
                    Col::Time if t.duration <= 0.0 && t.is_remote() => String::new(),
                    Col::Time => fmt::time(t.duration),
                    Col::Plays => {
                        let p = t.plays.get();
                        if p == 0 { String::new() } else { p.to_string() }
                    }
                    Col::Genre => t.genre.clone(),
                    Col::Added if t.is_remote() || t.mtime <= 0 => String::new(),
                    Col::Added => fmt::date(t.mtime),
                    Col::Format => format_of(t),
                    Col::Played if t.last_played.get() <= 0 => String::new(),
                    Col::Played => fmt::date(t.last_played.get()),
                    Col::Indicator => String::new(),
                };
                l.set_text(&text);
                if col == Col::Title {
                    if playing {
                        l.add_css_class("accent-text");
                    } else {
                        l.remove_css_class("accent-text");
                    }
                }
                if matches!(col, Col::Title | Col::Artist | Col::Album | Col::Genre | Col::Format) {
                    l.set_tooltip_text(Some(&text));
                }
            });
            let c = gtk::ColumnViewColumn::new(Some(col.title()), Some(factory));
            c.set_resizable(!matches!(col, Col::Indicator | Col::Num));
            match col {
                Col::Indicator => c.set_fixed_width(34),
                Col::Num => c.set_fixed_width(48),
                Col::Year => c.set_fixed_width(70),
                Col::Time => c.set_fixed_width(74),
                Col::Plays => c.set_fixed_width(70),
                Col::Added | Col::Played => c.set_fixed_width(110),
                Col::Genre => c.set_fixed_width(130),
                Col::Format => c.set_fixed_width(180),
                Col::Title | Col::Artist | Col::Album => c.set_expand(true),
            }
            if opts.sortable && !matches!(col, Col::Indicator | Col::Num) {
                c.set_sorter(Some(&gtk::CustomSorter::new(move |a, b| match (row_of(a), row_of(b)) {
                    (Some(a), Some(b)) => compare(col, &a.track, &b.track).into(),
                    _ => gtk::Ordering::Equal,
                })));
                if col == Col::Artist {
                    default_sort = Some(c.clone());
                }
                if Some(col) == saved_col {
                    restore_sort = Some(c.clone());
                }
            }
            view.append_column(&c);
            built.push((col, c));
        }
        table.columns.all = Rc::new(built);
        if opts.sortable {
            sorted.set_sorter(view.sorter().as_ref());
            let (start, desc) = match restore_sort {
                Some(c) => (Some(c), saved_desc),
                None => (default_sort, false),
            };
            if let Some(c) = start {
                view.sort_by_column(Some(&c), if desc { gtk::SortType::Descending } else { gtk::SortType::Ascending });
            }
            if let (Some(key), Some(sorter)) = (opts.key, view.sorter().and_downcast::<gtk::ColumnViewSorter>()) {
                let all = table.columns.all.clone();
                sorter.connect_changed(move |s, _| {
                    let Some(col) = s.primary_sort_column().and_then(|c| all.iter().find(|(_, x)| *x == c).map(|(col, _)| *col))
                    else {
                        return;
                    };
                    let dir = if s.primary_sort_order() == gtk::SortType::Descending { ":desc" } else { "" };
                    let value = format!("{}{dir}", col.id());
                    prefs::update(|p| {
                        p.table_sort.insert(key.to_string(), value);
                    });
                });
            }
        }
        if let Some(key) = opts.key {
            table.column_menu(key);
        }

        if opts.disc_sections {
            sorted.set_section_sorter(Some(&gtk::CustomSorter::new(|a, b| match (row_of(a), row_of(b)) {
                (Some(a), Some(b)) => a.track.disc_no.unwrap_or(1).cmp(&b.track.disc_no.unwrap_or(1)).into(),
                _ => gtk::Ordering::Equal,
            })));
            let headers = gtk::SignalListItemFactory::new();
            headers.connect_setup(|_, item| {
                if let Some(h) = item.downcast_ref::<gtk::ListHeader>() {
                    h.set_child(Some(&widgets::label("", "disc-header")));
                }
            });
            headers.connect_bind(|_, item| {
                let Some(h) = item.downcast_ref::<gtk::ListHeader>() else { return };
                let disc = h.item().and_then(|o| row_of(&o).map(|r| r.track.disc_no.unwrap_or(1)));
                if let (Some(d), Some(l)) = (disc, h.child().and_downcast::<gtk::Label>()) {
                    l.set_text(&format!("DISC {d}"));
                }
            });
            view.set_header_factory(Some(&headers));
        }

        let t = table.clone();
        let o = opts.clone();
        view.connect_activate(move |_, pos| t.activate(pos, &o));

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&view)
            .vexpand(true)
            .build();
        table.root.add_css_class("table-card");
        table.root.set_overflow(gtk::Overflow::Hidden);
        table.root.set_vexpand(true);
        table.root.append(&scroll);

        // Re-bind on song changes so the playing marker follows.
        let weak = table.store.downgrade();
        player::subscribe(&view, move |e| {
            if e == player::Event::Track
                && let Some(s) = weak.upgrade()
            {
                let n = s.n_items();
                s.items_changed(0, n, n);
            }
        });
        TABLES.with(|t| t.borrow_mut().push((view.downgrade(), table.columns.clone())));
        table.columns.apply(window::narrow());
        table
    }

    /// Show only rows matching every word of `text` (accents and case ignored).
    pub fn set_filter(&self, text: &str) {
        *self.terms.borrow_mut() = store::fold(text).split_whitespace().map(str::to_string).collect();
        self.filter.changed(gtk::FilterChange::Different);
    }

    /// Rows showing (after the filter).
    pub fn shown(&self) -> u32 {
        self.sorted.n_items()
    }

    /// A right-click on the header picks which columns show.
    fn column_menu(&self, key: &'static str) {
        let Some(header) = self.view.first_child() else { return };
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        let columns = self.columns.clone();
        click.connect_pressed(move |g, _, x, y| {
            let Some(anchor) = g.widget() else { return };
            let pop = gtk::Popover::new();
            pop.set_has_arrow(false);
            pop.set_parent(&anchor);
            pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            pop.add_css_class("menu-popover");
            let list = widgets::vbox(2);
            list.append(&widgets::label("Columns", "menu-heading-inline"));
            for (col, _) in columns.all.iter().filter(|(c, _)| OPTIONAL.contains(c)) {
                let check = gtk::CheckButton::with_label(col.title());
                check.set_active(columns.chosen.borrow().contains(col));
                let (columns, col) = (columns.clone(), *col);
                check.connect_toggled(move |b| {
                    {
                        let mut chosen = columns.chosen.borrow_mut();
                        chosen.retain(|c| *c != col);
                        if b.is_active() {
                            chosen.push(col);
                        }
                    }
                    let ids: Vec<String> = columns.chosen.borrow().iter().map(|c| c.id().to_string()).collect();
                    prefs::update(|p| {
                        p.table_columns.insert(key.to_string(), ids);
                    });
                    columns.apply(window::narrow());
                });
                list.append(&check);
            }
            pop.set_child(Some(&list));
            let anchor_weak = anchor.downgrade();
            pop.connect_closed(move |p| {
                let (p, a) = (p.clone(), anchor_weak.clone());
                glib::idle_add_local_once(move || {
                    if a.upgrade().is_some() {
                        p.unparent();
                    }
                });
            });
            pop.popup();
        });
        header.add_controller(click);
    }

    pub fn set(&self, tracks: &[Rc<Track>]) {
        let objs: Vec<glib::BoxedAnyObject> =
            tracks.iter().enumerate().map(|(index, t)| glib::BoxedAnyObject::new(Row { track: t.clone(), index })).collect();
        self.store.splice(0, self.store.n_items(), &objs);
    }

    pub fn len(&self) -> u32 {
        self.store.n_items()
    }

    /// Paths in the order they're shown.
    pub fn paths(&self) -> Vec<PathBuf> {
        (0..self.sorted.n_items())
            .filter_map(|i| self.sorted.item(i))
            .filter_map(|o| row_of(&o).map(|r| r.track.path.clone()))
            .collect()
    }

    pub fn focus(&self) {
        if self.sorted.n_items() > 0 {
            self.view.scroll_to(0, None, gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT, None);
        }
        self.view.grab_focus();
    }

    fn activate(&self, pos: u32, opts: &Options) {
        let Some(obj) = self.sorted.item(pos) else { return };
        let Some(index) = row_of(&obj).map(|r| r.index) else { return };
        if let Some(f) = &opts.on_activate {
            f(index);
        } else {
            player::play_tracks(self.paths(), pos as usize);
        }
    }

    /// Selected rows (in view order) — or just the clicked one if it isn't selected.
    fn targets(&self, clicked: u32) -> Vec<(u32, PathBuf, usize)> {
        let collect = |i: u32| self.sorted.item(i).and_then(|o| row_of(&o).map(|r| (i, r.track.path.clone(), r.index)));
        if !self.selection.is_selected(clicked) {
            self.selection.select_item(clicked, true);
            return collect(clicked).into_iter().collect();
        }
        let set = self.selection.selection();
        let mut out = Vec::new();
        if let Some((iter, first)) = gtk::BitsetIter::init_first(&set) {
            out.extend(collect(first));
            for i in iter {
                out.extend(collect(i));
            }
        }
        out
    }

    fn attach_row_gestures(&self, child: &gtk::Widget, item: &gtk::ListItem, opts: &Rc<Options>) {
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        let t = self.clone();
        let o = opts.clone();
        let item_weak = item.downgrade();
        let w = child.downgrade();
        click.connect_pressed(move |_, _, x, y| {
            let (Some(item), Some(w)) = (item_weak.upgrade(), w.upgrade()) else { return };
            let pos = item.position();
            if pos == gtk::INVALID_LIST_POSITION {
                return;
            }
            t.menu(&w, x, y, pos, &o);
        });
        child.add_controller(click);

        // Drag songs onto a playlist in the sidebar.
        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::COPY);
        let t = self.clone();
        let item_weak = item.downgrade();
        drag.connect_prepare(move |_, _, _| {
            let item = item_weak.upgrade()?;
            let pos = item.position();
            if pos == gtk::INVALID_LIST_POSITION {
                return None;
            }
            let targets = t.targets(pos);
            let rows = targets.iter().map(|(_, _, i)| *i).collect();
            DRAGGING.with(|d| *d.borrow_mut() = Some((t.view.downgrade(), rows)));
            let paths: Vec<String> = targets.into_iter().map(|(_, p, _)| p.to_string_lossy().into_owned()).collect();
            Some(gdk::ContentProvider::for_value(&playlist::drag_payload(&paths).to_value()))
        });
        drag.connect_drag_end(|_, _, _| DRAGGING.with(|d| *d.borrow_mut() = None));
        child.add_controller(drag);

        if let Some(reorder) = opts.reorder.clone() {
            self.accept_reorder(child, item, reorder);
        }
    }

    /// Dropping rows of this same table on a row moves them above or below it.
    fn accept_reorder(&self, child: &gtk::Widget, item: &gtk::ListItem, reorder: Reorder) {
        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::COPY);
        let from_here = {
            let view = self.view.downgrade();
            move || {
                DRAGGING.with(|d| {
                    d.borrow()
                        .as_ref()
                        .filter(|(v, _)| v.upgrade().is_some() && v.upgrade() == view.upgrade())
                        .map(|(_, r)| r.clone())
                })
            }
        };
        let below = |w: &gtk::Widget, y: f64| y > w.height() as f64 / 2.0;
        let mark = move |w: &gtk::Widget, y: Option<f64>| {
            if let Some(row) = row_widget(w) {
                row.remove_css_class("drop-above");
                row.remove_css_class("drop-below");
                if let Some(y) = y {
                    row.add_css_class(if below(w, y) { "drop-below" } else { "drop-above" });
                }
            }
        };
        let f = from_here.clone();
        target.connect_motion(move |t, _, y| {
            if f().is_none() {
                return gdk::DragAction::empty();
            }
            if let Some(w) = t.widget() {
                mark(&w, Some(y));
            }
            gdk::DragAction::COPY
        });
        target.connect_leave(move |t| {
            if let Some(w) = t.widget() {
                mark(&w, None);
            }
        });
        let item_weak = item.downgrade();
        target.connect_drop(move |t, _, _, y| {
            let Some(w) = t.widget() else { return false };
            mark(&w, None);
            let (Some(rows), Some(item)) = (from_here(), item_weak.upgrade()) else { return false };
            // The row's place in the list given to `set`, whatever the filter shows.
            let Some(index) = item.item().and_then(|o| row_of(&o).map(|r| r.index)) else { return false };
            let to = index + usize::from(below(&w, y));
            reorder(rows, to);
            true
        });
        child.add_controller(target);
    }

    fn menu(&self, anchor: &gtk::Widget, x: f64, y: f64, clicked: u32, opts: &Rc<Options>) {
        let targets = self.targets(clicked);
        if targets.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = targets.iter().map(|(_, p, _)| p.clone()).collect();
        let indexes: Vec<usize> = targets.iter().map(|(_, _, i)| *i).collect();
        let first = store::track_for(&paths[0]);

        let pop = gtk::Popover::new();
        pop.set_has_arrow(false);
        pop.set_parent(anchor);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.add_css_class("menu-popover");
        let pages = gtk::Stack::new();
        pages.set_vhomogeneous(false);
        pages.set_hhomogeneous(false);
        let main = widgets::vbox(1);
        let item = |label: &str, f: Box<dyn Fn()>| {
            let b = gtk::Button::with_label(label);
            b.add_css_class("flat");
            b.add_css_class("menu-item");
            if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                l.set_xalign(0.0);
            }
            let pop = pop.clone();
            b.connect_clicked(move |_| {
                pop.popdown();
                f();
            });
            b
        };

        let (t, o, p) = (self.clone(), opts.clone(), paths.clone());
        main.append(&item(
            "Play",
            Box::new(move || {
                if p.len() == 1 {
                    t.activate(clicked, &o);
                } else {
                    player::play_tracks(p.clone(), 0);
                }
            }),
        ));
        let p = paths.clone();
        main.append(&item(
            "Play next",
            Box::new(move || {
                player::play_next(p.clone());
                window::toast(&added_text(p.len(), "to play next"));
            }),
        ));
        let p = paths.clone();
        main.append(&item(
            "Add to queue",
            Box::new(move || {
                player::enqueue(p.clone());
                window::toast(&added_text(p.len(), "to the queue"));
            }),
        ));
        let to_playlist = gtk::Button::new();
        to_playlist.add_css_class("flat");
        to_playlist.add_css_class("menu-item");
        let row = widgets::hbox(8);
        let l = widgets::label("Add to playlist", "");
        l.set_hexpand(true);
        row.append(&l);
        row.append(&gtk::Image::from_icon_name("pan-end-symbolic"));
        to_playlist.set_child(Some(&row));
        let pg = pages.clone();
        to_playlist.connect_clicked(move |_| pg.set_visible_child_name("playlists"));
        main.append(&to_playlist);
        main.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        match first.kind {
            Kind::File => {
                let key = first.album_key();
                main.append(&item("Show album", Box::new(move || albums::show(&key))));
                let dir = first.path.parent().map(|d| d.to_path_buf());
                main.append(&item(
                    "Open folder",
                    Box::new(move || {
                        if let Some(d) = &dir {
                            cmd::spawn(&["xdg-open", &d.to_string_lossy()]);
                        }
                    }),
                ));
            }
            Kind::Episode if !first.feed.is_empty() => {
                let feed = first.feed.clone();
                main.append(&item("Show podcast", Box::new(move || podcasts::show(&feed))));
            }
            Kind::Station => main.append(&item("Show radio", Box::new(|| window::navigate("radio")))),
            Kind::Audiobook | Kind::Episode => {}
        }
        if !opts.extra.is_empty() {
            main.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            for (label, f) in &opts.extra {
                let (f, idx) = (f.clone(), indexes.clone());
                main.append(&item(label, Box::new(move || f(idx.clone()))));
            }
        }
        pages.add_named(&main, Some("main"));

        // Second page: the playlists.
        let lists = widgets::vbox(1);
        let back = gtk::Button::new();
        back.add_css_class("flat");
        back.add_css_class("menu-item");
        let row = widgets::hbox(8);
        row.append(&gtk::Image::from_icon_name("pan-start-symbolic"));
        row.append(&widgets::label("Add to playlist", "menu-heading-inline"));
        back.set_child(Some(&row));
        let pg = pages.clone();
        back.connect_clicked(move |_| pg.set_visible_child_name("main"));
        lists.append(&back);
        lists.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let p = paths.clone();
        lists.append(&item("New playlist…", Box::new(move || playlist::new_dialog(p.clone()))));
        let scroll_box = widgets::vbox(1);
        for pl in store::playlists() {
            let p = paths.clone();
            scroll_box.append(&item(&pl.name, Box::new(move || playlist::append(pl.id, p.clone()))));
        }
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .propagate_natural_height(true)
            .max_content_height(320)
            .child(&scroll_box)
            .build();
        lists.append(&scroll);
        pages.add_named(&lists, Some("playlists"));

        pop.set_child(Some(&pages));
        let anchor_weak = anchor.downgrade();
        pop.connect_closed(move |p| {
            let p = p.clone();
            let a = anchor_weak.clone();
            glib::idle_add_local_once(move || {
                if a.upgrade().is_some() {
                    p.unparent();
                }
            });
        });
        pop.popup();
    }
}

pub fn added_text(n: usize, where_: &str) -> String {
    format!("Added {} {where_}.", fmt::count(n, "song", "songs"))
}

/// Hide the less important columns in a half-screen window.
pub fn set_narrow(narrow: bool) {
    TABLES.with(|t| {
        t.borrow_mut().retain(|(w, _)| w.upgrade().is_some());
        for (_, columns) in t.borrow().iter() {
            columns.apply(narrow);
        }
    });
}
