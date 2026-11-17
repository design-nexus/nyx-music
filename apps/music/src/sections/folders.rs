//! Folders: the library as it's laid out on disk. A folder opens into all the
//! songs under it, in album order.

use super::{library_stack, scan_banner};
use crate::library::{Track, store};
use crate::tracklist::{Col, Options, TrackTable};
use crate::widgets::{self, Page};
use crate::{fmt, paths, views};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub children: Vec<Rc<Node>>,
    /// Songs in this folder and below.
    pub count: usize,
}

/// Build the folder tree under each library root from the songs' paths.
pub fn tree(roots: &[PathBuf], tracks: &[Rc<Track>]) -> Vec<Rc<Node>> {
    fn build(dir: &Path, name: String, counts: &BTreeMap<PathBuf, usize>) -> Node {
        // Direct subfolders that hold music somewhere below.
        let mut subs: BTreeMap<PathBuf, ()> = BTreeMap::new();
        for d in counts.keys() {
            if let Ok(rest) = d.strip_prefix(dir)
                && let Some(first) = rest.components().next()
            {
                subs.insert(dir.join(first), ());
            }
        }
        let count = counts.iter().filter(|(d, _)| d.starts_with(dir)).map(|(_, n)| n).sum();
        let mut children: Vec<Rc<Node>> = subs
            .into_keys()
            .map(|p| {
                let n = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                Rc::new(build(&p, n, counts))
            })
            .collect();
        children.sort_by_key(|c| c.name.to_lowercase());
        Node { path: dir.to_path_buf(), name, children, count }
    }
    let mut counts: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for t in tracks {
        if let Some(d) = t.path.parent() {
            *counts.entry(d.to_path_buf()).or_default() += 1;
        }
    }
    roots.iter().map(|r| Rc::new(build(r, paths::pretty(r), &counts))).filter(|n| n.count > 0).collect()
}

fn node_of(obj: &glib::Object) -> Option<Rc<Node>> {
    let obj = obj.downcast_ref::<gtk::TreeListRow>().and_then(|r| r.item()).unwrap_or_else(|| obj.clone());
    obj.downcast_ref::<glib::BoxedAnyObject>().map(|b| b.borrow::<Rc<Node>>().clone())
}

fn model_for(nodes: &[Rc<Node>]) -> gio::ListStore {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    for n in nodes {
        store.append(&glib::BoxedAnyObject::new(n.clone()));
    }
    store
}

pub fn build(page: &Page) {
    page.body.append(&super::music_chips("folders"));
    page.body.append(&scan_banner());
    let inner = gtk::Stack::new();
    inner.set_transition_type(gtk::StackTransitionType::Crossfade);

    let roots = model_for(&[]);
    let tree_model = gtk::TreeListModel::new(roots.clone(), false, false, |obj| {
        let node = node_of(obj)?;
        if node.children.is_empty() { None } else { Some(model_for(&node.children).upcast()) }
    });
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let expander = gtk::TreeExpander::new();
        let row = widgets::hbox(10);
        row.add_css_class("folder-row");
        row.append(&gtk::Image::from_icon_name("folder-symbolic"));
        let name = widgets::label("", "");
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let count = widgets::label("", "cell-dim");
        count.add_css_class("mono");
        row.append(&name);
        row.append(&count);
        expander.set_child(Some(&row));
        item.set_child(Some(&expander));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let (Some(row), Some(expander)) =
            (item.item().and_downcast::<gtk::TreeListRow>(), item.child().and_downcast::<gtk::TreeExpander>())
        else {
            return;
        };
        expander.set_list_row(Some(&row));
        let Some(node) = row.item().and_then(|o| node_of(&o)) else { return };
        if let Some(bx) = expander.child()
            && let Some(name) = bx.first_child().and_then(|i| i.next_sibling()).and_downcast::<gtk::Label>()
            && let Some(count) = name.next_sibling().and_downcast::<gtk::Label>()
        {
            name.set_text(&node.name);
            name.set_tooltip_text(Some(&paths::pretty(&node.path)));
            count.set_text(&fmt::thousands(node.count));
        }
    });
    let selection = gtk::SingleSelection::new(Some(tree_model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let list = gtk::ListView::new(Some(selection), Some(factory));
    list.add_css_class("bucket-list");
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&list)
        .vexpand(true)
        .build();
    let card = widgets::vbox(0);
    card.add_css_class("table-card");
    card.set_overflow(gtk::Overflow::Hidden);
    card.set_vexpand(true);
    card.append(&scroll);
    let browse = widgets::vbox(0);
    browse.append(&widgets::label("Click a folder to see its songs; use the arrows to look inside.", "page-hint"));
    browse.append(&card);
    inner.add_named(&browse, Some("tree"));

    // ----- Folder view -----
    let detail = widgets::vbox(0);
    detail.set_vexpand(true);
    let i2 = inner.clone();
    detail.append(&views::back_button("Folders", move || i2.set_visible_child_name("tree")));
    let open: Rc<std::cell::RefCell<Vec<PathBuf>>> = Rc::default();
    let o = open.clone();
    let actions = views::play_buttons(move || o.borrow().clone());
    let meta = widgets::label("", "dim");
    meta.add_css_class("mono");
    meta.add_css_class("detail-meta");
    let (header, title) = views::detail_header(None, "Folder", "", meta.upcast_ref(), "", &actions);
    detail.append(&header);
    let table = TrackTable::new(Options {
        cols: &[Col::Indicator, Col::Title, Col::Artist, Col::Album, Col::Time],
        sortable: false,
        key: Some("folder"),
        ..Default::default()
    });
    let filter = views::filter_entry(&table, "Filter this folder");
    let spacer = widgets::hbox(0);
    spacer.set_hexpand(true);
    actions.append(&spacer);
    actions.append(&filter);
    detail.append(&table.root);
    inner.add_named(&detail, Some("detail"));

    let (i2, t2) = (inner.clone(), table.clone());
    list.set_single_click_activate(true);
    list.connect_activate(move |_, pos| {
        let Some(node) = tree_model.item(pos).and_then(|o| node_of(&o)) else { return };
        let mut tracks: Vec<Rc<Track>> = store::tracks().into_iter().filter(|t| t.path.starts_with(&node.path)).collect();
        tracks.sort_by(|a, b| {
            (a.path.parent(), a.disc_no.unwrap_or(1), a.track_no.unwrap_or(u32::MAX), &a.path).cmp(&(
                b.path.parent(),
                b.disc_no.unwrap_or(1),
                b.track_no.unwrap_or(u32::MAX),
                &b.path,
            ))
        });
        title.set_text(&node.name);
        let secs: f64 = tracks.iter().map(|t| t.duration).sum();
        meta.set_text(&format!(
            "{} · {} · {}",
            paths::pretty(&node.path),
            fmt::count(tracks.len(), "song", "songs"),
            fmt::total(secs)
        ));
        *open.borrow_mut() = tracks.iter().map(|t| t.path.clone()).collect();
        filter.set_text("");
        t2.set(&tracks);
        i2.set_visible_child_name("detail");
    });
    page.body.append(&library_stack(&inner));

    let refresh = move || {
        let nodes = tree(&store::roots(), &store::tracks());
        let objs: Vec<glib::BoxedAnyObject> = nodes.into_iter().map(glib::BoxedAnyObject::new).collect();
        roots.splice(0, roots.n_items(), &objs);
    };
    refresh();
    store::subscribe(&inner, move |c| {
        if c == store::Change::Library {
            refresh();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_tree_with_counts() {
        let t = |p: &str| Rc::new(Track { path: p.into(), ..Default::default() });
        let tracks = vec![t("/m/A/x/1.mp3"), t("/m/A/x/2.mp3"), t("/m/A/3.mp3"), t("/m/B/4.mp3")];
        let roots = tree(&[PathBuf::from("/m"), PathBuf::from("/empty")], &tracks);
        assert_eq!(roots.len(), 1);
        let m = &roots[0];
        assert_eq!(m.count, 4);
        assert_eq!(m.children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["A", "B"]);
        assert_eq!(m.children[0].count, 3);
        assert_eq!(m.children[0].children[0].name, "x");
        assert_eq!(m.children[0].children[0].count, 2);
    }
}
