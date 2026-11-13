//! BubbleSettings: independent GTK4 settings shell.
//!
//! The first page consumes the compositor's semantic BubbleColor snapshot over
//! the authenticated local IPC socket. It never decodes wallpapers or runs the
//! BubbleColor engine itself.

use std::{
    cell::RefCell,
    sync::mpsc::{self, TryRecvError},
    thread,
    time::Duration,
};

use bubble_ipc::{Request, Response, ResponseData, ThemePalette, unix::UnixIpcClient};
use gtk::prelude::*;

fn main() -> glib::ExitCode {
    let application = gtk::Application::builder()
        .application_id("org.bubbleos.BubbleSettings")
        .build();

    application.connect_activate(build_ui);
    application.run()
}

fn build_ui(application: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("BubbleSettings")
        .default_width(900)
        .default_height(620)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    let sidebar = gtk::StackSidebar::builder()
        .stack(&stack)
        .width_request(190)
        .build();

    let appearance = build_appearance_page();
    stack.add_titled(&appearance.page, Some("appearance"), "Apparence");

    let about = gtk::Box::new(gtk::Orientation::Vertical, 12);
    about.set_margin_top(28);
    about.set_margin_bottom(28);
    about.set_margin_start(32);
    about.set_margin_end(32);
    about.append(&heading("À propos de BubbleSettings"));
    about.append(&gtk::Label::new(Some(
        "Configuration graphique indépendante de BubbleWM. Les réglages sont appliqués par les services du bureau, pas par cette interface.",
    )));
    stack.add_titled(&about, Some("about"), "À propos");

    root.append(&sidebar);
    root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    root.append(&stack);
    window.set_child(Some(&root));
    window.present();

    start_palette_monitor(appearance.palette_grid, appearance.status);
}

struct AppearancePage {
    page: gtk::Box,
    palette_grid: gtk::Grid,
    status: gtk::Label,
}

fn build_appearance_page() -> AppearancePage {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.set_margin_top(28);
    page.set_margin_bottom(28);
    page.set_margin_start(32);
    page.set_margin_end(32);

    page.append(&heading("Apparence"));
    page.append(&gtk::Label::new(Some(
        "Aperçu de la palette sémantique partagée par BubbleWM et BubblePanel.",
    )));

    let palette_title = heading("Palette BubbleColor");
    palette_title.set_margin_top(12);
    page.append(&palette_title);

    let status = gtk::Label::new(Some("Connexion au compositeur BubbleWM…"));
    status.set_halign(gtk::Align::Start);
    page.append(&status);

    let palette_grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(12)
        .hexpand(true)
        .build();
    page.append(&palette_grid);

    let note = gtk::Label::new(Some(
        "BubbleSettings ne traite pas le fond d’écran : il affiche uniquement les couleurs calculées et publiées par BubbleColor.",
    ));
    note.set_wrap(true);
    note.set_halign(gtk::Align::Start);
    note.set_margin_top(16);
    page.append(&note);

    AppearancePage {
        page,
        palette_grid,
        status,
    }
}

fn heading(value: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(value));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("title-2");
    label
}

fn start_palette_monitor(grid: gtk::Grid, status: gtk::Label) {
    let (sender, receiver) = mpsc::sync_channel(1);

    // IPC is blocking by design (bounded two-second socket timeout), so keep it
    // off GTK's main thread. The UI only receives immutable palette snapshots.
    thread::spawn(move || {
        loop {
            let result = match UnixIpcClient::connect_default() {
                Ok(mut client) => client.request(Request::GetThemePalette),
                Err(error) => {
                    let _ = sender.try_send(Err(error));
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            let _ = sender.try_send(result);
            thread::sleep(Duration::from_secs(1));
        }
    });

    let grid = RefCell::new(grid);
    let status = RefCell::new(status);
    glib::timeout_add_seconds_local(1, move || {
        match receiver.try_recv() {
            Ok(Ok(Response::Success {
                data:
                    ResponseData::ThemePalette {
                        palette: Some(palette),
                    },
            })) if palette.schema_version == 1 => {
                render_palette(&grid.borrow(), &palette);
                status.borrow().set_text(&format!(
                    "Connecté à BubbleWM · génération {}",
                    palette.generation
                ));
            }
            Ok(Ok(Response::Success {
                data: ResponseData::ThemePalette { palette: None },
            })) => {
                clear_palette(&grid.borrow());
                status.borrow().set_text(
                    "BubbleColor est actif, mais aucune palette n’est encore disponible.",
                );
            }
            Ok(Ok(Response::Failure { error })) => {
                clear_palette(&grid.borrow());
                status
                    .borrow()
                    .set_text(&format!("BubbleWM : {}", error.message));
            }
            Ok(Ok(_)) => {
                clear_palette(&grid.borrow());
                status
                    .borrow()
                    .set_text("Réponse IPC inattendue de BubbleWM.");
            }
            Ok(Err(error)) => {
                clear_palette(&grid.borrow());
                status
                    .borrow()
                    .set_text(&format!("BubbleWM indisponible : {error}"));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                status
                    .borrow()
                    .set_text("Le service de palette est arrêté.");
                return glib::ControlFlow::Break;
            }
        }
        glib::ControlFlow::Continue
    });
}

fn clear_palette(grid: &gtk::Grid) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
}

fn render_palette(grid: &gtk::Grid, palette: &ThemePalette) {
    clear_palette(grid);
    let entries = [
        ("Primaire", palette.primary),
        ("Conteneur primaire", palette.primary_container),
        ("Texte sur conteneur", palette.on_primary_container),
        ("Surface", palette.surface),
        ("Texte", palette.on_surface),
        ("Surface secondaire", palette.surface_variant),
        ("Contour", palette.outline),
        ("Avertissement", palette.warning),
        ("Succès", palette.success),
        ("Erreur", palette.error),
    ];

    for (index, (name, rgb)) in entries.iter().enumerate() {
        let hex = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
        let label = gtk::Label::new(None);
        label.set_markup(&format!(
            "<span background=\"{hex}\" foreground=\"#FFFFFF\">   {name}   </span>\n<small>{hex}</small>"
        ));
        label.set_margin_top(12);
        label.set_margin_bottom(12);
        label.set_margin_start(8);
        label.set_margin_end(8);
        let frame = gtk::Frame::new(Some(name));
        frame.set_child(Some(&label));
        grid.attach(&frame, (index % 2) as i32, (index / 2) as i32, 1, 1);
    }
}
