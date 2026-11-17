//! Connection-first settings. Widgets stay mounted while provider state changes.
use super::{ConnectionPhase, api, config, connection, libraries, save_config, storage, subscribe};
use crate::{
    fmt,
    widgets::{self, Page},
};
use gtk::prelude::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

fn text(value: &str, class: &str) -> gtk::Label {
    let l = widgets::label(value, class);
    l.set_xalign(0.0);
    l.set_wrap(true);
    l
}
fn action(label: &str, f: impl Fn() + 'static) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.connect_clicked(move |_| f());
    b
}
fn card(parent: &gtk::Box, title: &str, description: &str, icon: &str) -> gtk::Box {
    let body = widgets::vbox(12);
    body.add_css_class("card");
    body.add_css_class("abs-settings-card");
    let head = widgets::hbox(8);
    head.add_css_class("abs-card-heading");
    head.append(&gtk::Image::from_icon_name(icon));
    head.append(&text(title, "card-title"));
    body.append(&head);
    if !description.is_empty() {
        body.append(&text(description, "dim"));
    }
    parent.append(&body);
    body
}
fn field(parent: &gtk::Box, title: &str, widget: &impl IsA<gtk::Widget>) {
    let column = widgets::vbox(6);
    let label = gtk::Label::new(Some(title));
    label.set_use_underline(true);
    label.set_xalign(0.0);
    label.set_mnemonic_widget(Some(widget));
    label.add_css_class("settings-option-title");
    column.append(&label);
    widget.set_hexpand(true);
    column.append(widget);
    parent.append(&column);
}
fn amount(value: f64, min: f64, max: f64, step: f64, unit: &str) -> (gtk::SpinButton, gtk::Box) {
    let spin = gtk::SpinButton::with_range(min, max, step);
    spin.set_value(value);
    spin.set_width_chars(4);
    let control = widgets::hbox(6);
    control.set_valign(gtk::Align::Center);
    control.append(&spin);
    control.append(&text(unit, "dim"));
    (spin, control)
}
pub fn open() {
    crate::settings_dialog::open_group("Audiobookshelf");
}
pub fn open_downloads() {
    nyx_ui::settings_card::close();
    crate::window::navigate("downloads");
}

pub fn build(page: &Page) {
    let group = page.group("Audiobookshelf");
    group.list.remove_css_class("group-list");
    group.list.add_css_class("abs-settings");
    group.list.set_spacing(16);
    let c = config();
    let updating = Rc::new(Cell::new(false));
    let editing = Rc::new(Cell::new(!c.connected));
    let intro = text("Connect your Audiobookshelf server to browse books and keep your place across devices.", "dim");
    group.add(&intro);
    let account = card(&group.list, "Connection", "", "network-server-symbolic");
    let summary = widgets::vbox(4);
    let account_name = text("", "settings-option-title");
    let address = text("", "dim");
    address.set_selectable(true);
    summary.append(&account_name);
    summary.append(&address);
    account.append(&summary);
    let state_line = widgets::hbox(8);
    let spinner = gtk::Spinner::new();
    let state_label = text("", "dim");
    state_line.append(&spinner);
    state_line.append(&state_label);
    account.append(&state_line);
    let form = widgets::vbox(12);
    let server = gtk::Entry::builder().placeholder_text("https://your-server.example").text(&c.server).build();
    server.set_input_purpose(gtk::InputPurpose::Url);
    server.update_property(&[gtk::accessible::Property::Label("Server address")]);
    field(&form, "_Server address", &server);
    form.append(&text("Use the address you open in your browser.", "dim"));
    let username = gtk::Entry::builder().text(&c.username).build();
    username.update_property(&[gtk::accessible::Property::Label("Username")]);
    field(&form, "_Username", &username);
    let password = gtk::PasswordEntry::builder().show_peek_icon(true).build();
    password.update_property(&[gtk::accessible::Property::Label("Password")]);
    field(&form, "_Password", &password);
    form.append(&text("Your password is only used to sign in. The connection is saved securely in your desktop keyring.", "dim"));
    let sign_in = gtk::Button::with_label("Sign in");
    sign_in.add_css_class("suggested-action");
    sign_in.set_halign(gtk::Align::Start);
    let cancel = gtk::Button::with_label("Cancel");
    let form_actions = widgets::hbox(8);
    form_actions.append(&sign_in);
    form_actions.append(&cancel);
    form.append(&form_actions);
    account.append(&form);
    let local_error: Rc<RefCell<Option<String>>> = Rc::default();
    let error_box = widgets::vbox(8);
    error_box.add_css_class("abs-connection-error");
    let error_label = text("", "error");
    error_label.set_selectable(true);
    error_box.append(&error_label);
    let error_actions = widgets::hbox(8);
    let retry = action("Retry connection", super::refresh);
    error_actions.append(&retry);
    let dismiss = gtk::Button::with_label("Dismiss");
    error_actions.append(&dismiss);
    error_box.append(&error_actions);
    account.append(&error_box);
    let management = gtk::Expander::builder().label("Connection options").build();
    let management_actions = widgets::vbox(8);
    let change = gtk::Button::with_label("Change connection");
    change.set_halign(gtk::Align::Start);
    management_actions.append(&change);
    management_actions.append(&text("Disconnecting keeps your downloads and listening progress on this device.", "dim"));
    let disconnect = widgets::two_click("Disconnect", "Click again to disconnect", super::disconnect);
    disconnect.set_halign(gtk::Align::Start);
    management_actions.append(&disconnect);
    management.set_child(Some(&management_actions));
    account.append(&management);
    let offline_downloads = action("Open downloads", open_downloads);
    offline_downloads.set_halign(gtk::Align::Start);
    account.append(&offline_downloads);

    let options = widgets::vbox(16);
    group.add(&options);
    let library_card =
        card(&options, "Libraries", "Choose the audiobook libraries you want to browse.", "accessories-dictionary-symbolic");
    let library_status = text("", "dim");
    library_card.append(&library_status);
    let library_list = widgets::vbox(8);
    library_card.append(&library_list);
    let refresh_libraries = widgets::icon_button("view-refresh-symbolic", "Refresh libraries");
    refresh_libraries.connect_clicked(|_| super::refresh());
    let library_heading = library_card.first_child().and_downcast::<gtk::Box>().expect("card heading");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    library_heading.append(&spacer);
    library_heading.append(&refresh_libraries);
    let guard = updating.clone();
    let (podcast_row, podcasts) = widgets::switch_row(
        "Include podcasts",
        "Show server podcasts alongside your RSS subscriptions.",
        c.podcasts,
        move |on| {
            if guard.get() {
                return;
            }
            let mut c = config();
            c.podcasts = on;
            save_config(c);
        },
    );
    library_card.append(&podcast_row);

    let playback = card(
        &options,
        "Playback",
        "Defaults for audiobooks. Music playback settings stay separate.",
        "media-playback-start-symbolic",
    );
    let (speed, speed_control) = amount(c.speed, 0.5, 3.0, 0.05, "×");
    speed.set_digits(2);
    speed.update_property(&[gtk::accessible::Property::Label("Default audiobook speed")]);
    playback.append(&widgets::row("Default speed", "Used when you start a book.", Some(speed_control.upcast_ref())));
    let (back, back_control) = amount(c.backward, 5.0, 120.0, 5.0, "sec");
    back.update_property(&[gtk::accessible::Property::Label("Skip backward in seconds")]);
    playback.append(&widgets::row("Skip backward", "", Some(back_control.upcast_ref())));
    let (forward, forward_control) = amount(c.forward, 5.0, 120.0, 5.0, "sec");
    forward.update_property(&[gtk::accessible::Property::Label("Skip forward in seconds")]);
    playback.append(&widgets::row("Skip forward", "", Some(forward_control.upcast_ref())));
    let guard = updating.clone();
    let (sleep_row, sleep) =
        widgets::switch_row("Sleep timer", "Pause automatically after starting a book.", c.sleep_minutes > 0, move |on| {
            if guard.get() {
                return;
            }
            let mut c = config();
            c.sleep_minutes = if on { 30 } else { 0 };
            save_config(c);
        });
    playback.append(&sleep_row);
    let (minutes, minutes_control) = amount(c.sleep_minutes.max(5) as f64, 5.0, 180.0, 5.0, "min");
    minutes.update_property(&[gtk::accessible::Property::Label("Default sleep timer in minutes")]);
    let duration_row = widgets::row("Pause after", "", Some(minutes_control.upcast_ref()));
    playback.append(&duration_row);
    for (spin, field) in [(speed.clone(), 0), (back.clone(), 1), (forward.clone(), 2), (minutes.clone(), 3)] {
        let guard = updating.clone();
        spin.connect_value_changed(move |s| {
            if guard.get() {
                return;
            }
            let mut c = config();
            match field {
                0 => c.speed = s.value(),
                1 => c.backward = s.value(),
                2 => c.forward = s.value(),
                _ => c.sleep_minutes = s.value() as u32,
            }
            save_config(c);
        });
    }
    let synchronization = card(
        &options,
        "Synchronization",
        "Keep your listening position and completion status across devices.",
        "view-refresh-symbolic",
    );
    let guard = updating.clone();
    let (row, sync) =
        widgets::switch_row("Sync listening progress", "Offline changes are kept until you reconnect.", c.sync, move |on| {
            if guard.get() {
                return;
            }
            let mut c = config();
            c.sync = on;
            save_config(c);
        });
    synchronization.append(&row);
    let sync_info = text("", "dim");
    synchronization.append(&sync_info);
    let sync_now = action("Sync now", super::sync_all);
    sync_now.set_halign(gtk::Align::Start);
    synchronization.append(&sync_now);
    let downloads = card(&options, "Downloads", "Books and episodes saved for offline listening.", "folder-download-symbolic");
    let usage = text("", "settings-option-title");
    downloads.append(&usage);
    let manage = action("Manage downloads", open_downloads);
    manage.set_halign(gtk::Align::Start);
    downloads.append(&manage);
    let location = gtk::Expander::builder().label("Storage location").build();
    let path = text(&crate::paths::pretty(&crate::paths::data_dir().join("audiobookshelf")), "dim");
    path.set_selectable(true);
    location.set_child(Some(&path));
    downloads.append(&location);

    let checks: Rc<RefCell<Vec<(String, gtk::CheckButton)>>> = Rc::default();
    let library_signature = Rc::new(RefCell::new(String::new()));
    let previous_phase = Rc::new(Cell::new(connection().phase));
    let refresh: Rc<dyn Fn()> = Rc::new({
        let cancel = cancel.clone();
        let (server, username, password, sign_in) = (server.clone(), username.clone(), password.clone(), sign_in.clone());
        let local_error = local_error.clone();
        let editing = editing.clone();
        let updating = updating.clone();
        move || {
            if updating.replace(true) {
                return;
            }
            let c = config();
            let state = connection();
            let phase = state.phase;
            if previous_phase.replace(phase) == ConnectionPhase::SigningIn && c.connected && phase == ConnectionPhase::Connected {
                editing.set(false);
            }
            if !c.connected || matches!(phase, ConnectionPhase::NeedsSignIn | ConnectionPhase::SignInFailed) {
                editing.set(true);
            }
            let show_form = editing.get();
            let busy = phase.busy();
            form.set_visible(show_form);
            form.set_sensitive(!busy);
            summary.set_visible(c.connected && !show_form);
            account_name.set_text(&format!("Signed in as {}", c.username));
            address.set_text(&c.server);
            management.set_visible(c.connected && !show_form);
            management.set_sensitive(!busy);
            cancel.set_visible(c.connected && !busy && phase != ConnectionPhase::NeedsSignIn);
            if !show_form {
                if server.text() != c.server {
                    server.set_text(&c.server);
                }
                if username.text() != c.username {
                    username.set_text(&c.username);
                }
            }
            sign_in.set_sensitive(
                !busy && !server.text().trim().is_empty() && !username.text().trim().is_empty() && !password.text().is_empty(),
            );
            sign_in.set_label(if phase == ConnectionPhase::SigningIn { "Signing in…" } else { "Sign in" });
            state_line.set_visible(
                !matches!(phase, ConnectionPhase::Disconnected | ConnectionPhase::NeedsSignIn) && !show_form || busy,
            );
            state_label.set_text(match phase {
                ConnectionPhase::SigningIn => "Connecting to your server…",
                ConnectionPhase::LoadingLibraries => "Loading your libraries…",
                ConnectionPhase::Connected => "Connected",
                ConnectionPhase::Offline => "Server unavailable · your downloads are still available",
                _ => "",
            });
            spinner.set_visible(busy);
            spinner.set_spinning(busy);
            let error = local_error.borrow().clone().or(state.error);
            error_box.set_visible(error.is_some());
            error_label.set_text(error.as_deref().unwrap_or_default());
            retry.set_visible(phase == ConnectionPhase::Offline && !show_form);
            retry.set_sensitive(!busy);
            options.set_visible(c.connected && !show_form);
            offline_downloads.set_visible(show_form && !storage::downloads(&c).is_empty());
            let libs: Vec<_> = libraries().into_iter().filter(|l| l.media_type == "book").collect();
            let signature =
                format!("{}/{}/{:?}", c.connection, c.user, libs.iter().map(|l| (&l.id, &l.name)).collect::<Vec<_>>());
            if *library_signature.borrow() != signature {
                *library_signature.borrow_mut() = signature;
                while let Some(w) = library_list.first_child() {
                    library_list.remove(&w);
                }
                checks.borrow_mut().clear();
                for lib in &libs {
                    let check = gtk::CheckButton::new();
                    check.set_child(Some(&text(&lib.name, "")));
                    check.update_property(&[gtk::accessible::Property::Label(&lib.name)]);
                    let id = lib.id.clone();
                    let guard = updating.clone();
                    check.connect_toggled(move |w| {
                        if guard.get() {
                            return;
                        }
                        let mut c = config();
                        c.library_selection_set = true;
                        c.libraries.retain(|l| l != &id);
                        if w.is_active() {
                            c.libraries.push(id.clone());
                        }
                        save_config(c);
                    });
                    library_list.append(&check);
                    checks.borrow_mut().push((lib.id.clone(), check));
                }
            }
            for (id, check) in checks.borrow().iter() {
                check.set_active(c.libraries.contains(id));
            }
            library_list.set_sensitive(!busy);
            refresh_libraries.set_sensitive(!busy);
            library_status.set_text(if phase == ConnectionPhase::LoadingLibraries {
                "Looking for audiobook libraries…"
            } else if libs.is_empty() {
                "No audiobook libraries are available for this account."
            } else if c.libraries.is_empty() {
                "Select at least one library to start browsing."
            } else {
                "Selected libraries appear in Audiobooks."
            });
            podcasts.set_active(c.podcasts);
            sync.set_active(c.sync);
            sleep.set_active(c.sleep_minutes > 0);
            duration_row.set_visible(c.sleep_minutes > 0);
            for (spin, value) in
                [(&speed, c.speed), (&back, c.backward), (&forward, c.forward), (&minutes, c.sleep_minutes.max(5) as f64)]
            {
                if (spin.value() - value).abs() > 0.001 {
                    spin.set_value(value);
                }
            }
            let (pending, conflicts) = storage::progress_summary(&c);
            let last = if c.last_sync > 0 { fmt::date(c.last_sync / 1000) } else { "Not yet synced".into() };
            sync_info.set_text(&format!("Last successful sync: {last}\n{pending} pending · {conflicts} need a position choice"));
            sync_now.set_sensitive(c.sync && phase == ConnectionPhase::Connected);
            let saved = storage::downloads(&c);
            let bytes: u64 = saved.iter().map(|d| d.done).sum();
            usage.set_text(&if saved.is_empty() {
                "No downloads yet".into()
            } else {
                format!("{} saved items · {:.1} MB used", saved.len(), bytes as f64 / 1_000_000.0)
            });
            updating.set(false);
        }
    });
    let f = refresh.clone();
    subscribe(&group.wrapper, move || f());
    let f = refresh.clone();
    let edit = editing.clone();
    let err = local_error.clone();
    change.connect_clicked(move |_| {
        edit.set(true);
        *err.borrow_mut() = None;
        f();
    });
    let f = refresh.clone();
    let edit = editing.clone();
    let err = local_error.clone();
    let pass = password.clone();
    cancel.connect_clicked(move |_| {
        *err.borrow_mut() = None;
        pass.set_text("");
        super::restore_saved_connection();
        edit.set(false);
        f();
    });
    let err = local_error.clone();
    let f = refresh.clone();
    dismiss.connect_clicked(move |_| {
        *err.borrow_mut() = None;
        super::dismiss_connection_error();
        f();
    });
    // Editing credentials only updates form validity; it never reloads library metadata.
    let validate: Rc<dyn Fn()> = Rc::new({
        let (server, username, password, submit) = (server.clone(), username.clone(), password.clone(), sign_in.clone());
        move || {
            submit.set_sensitive(
                !connection().phase.busy()
                    && !server.text().trim().is_empty()
                    && !username.text().trim().is_empty()
                    && !password.text().is_empty(),
            )
        }
    });
    for entry in [&server, &username] {
        let validate = validate.clone();
        entry.connect_changed(move |_| validate());
    }
    let validate = validate.clone();
    password.connect_changed(move |_| validate());
    let (server2, user2, pass2) = (server.clone(), username.clone(), password.clone());
    let err = local_error.clone();
    let f = refresh.clone();
    sign_in.connect_clicked(move |_| {
        if connection().phase.busy() {
            return;
        }
        if let Err(error) = api::server_url(&server2.text()) {
            *err.borrow_mut() = Some(error.to_string());
            f();
            server2.grab_focus();
            return;
        }
        *err.borrow_mut() = None;
        let secret = pass2.text().to_string();
        pass2.set_text("");
        super::login(server2.text().trim().into(), user2.text().trim().into(), secret);
    });
    let user_focus = username.clone();
    server.connect_activate(move |_| {
        user_focus.grab_focus();
    });
    let pass_focus = password.clone();
    username.connect_activate(move |_| {
        pass_focus.grab_focus();
    });
    let submit = sign_in.clone();
    password.connect_activate(move |_| {
        if submit.is_sensitive() {
            submit.emit_clicked();
        }
    });
    refresh();
}
