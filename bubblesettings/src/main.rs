//! BubbleSettings: independent GTK4 settings shell.
//!
//! The Appearance form edits the shared, validated BubbleWM configuration.
//! Writes are atomic and explicitly require a compositor restart; palette
//! generation remains owned by BubbleColor inside BubbleWM.

use std::{
    cell::RefCell,
    rc::Rc,
    sync::mpsc::{self, TryRecvError},
    thread,
    time::Duration,
};

use bubble_ipc::{Request, Response, ResponseData, ThemePalette, unix::UnixIpcClient};
use bubblewm::config::{self, Config};
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
        .default_height(720)
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
        "Configuration graphique indépendante de BubbleWM. Les réglages sont enregistrés dans le fichier de configuration partagé.",
    )));
    stack.add_titled(&about, Some("about"), "À propos");

    root.append(&sidebar);
    root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    root.append(&stack);
    window.set_child(Some(&root));
    window.present();

    start_palette_monitor(appearance.palette_grid, appearance.palette_status);
}

struct AppearancePage {
    page: gtk::Box,
    palette_grid: gtk::Grid,
    palette_status: gtk::Label,
}

fn build_appearance_page() -> AppearancePage {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.set_margin_top(28);
    page.set_margin_bottom(28);
    page.set_margin_start(32);
    page.set_margin_end(32);

    page.append(&heading("Apparence"));
    page.append(&gtk::Label::new(Some(
        "Configurez le schéma de couleurs et les paramètres de génération BubbleColor.",
    )));

    let (initial_config, config_path, config_error) =
        match (config::load(None), config::default_path()) {
            (Ok(config), Ok(path)) => (config, Some(path), None),
            (Err(error), path_result) => (
                Config::default(),
                path_result.ok(),
                Some(format!("Configuration non chargée : {error}")),
            ),
            (Ok(config), Err(error)) => (
                config,
                None,
                Some(format!("Chemin de configuration indisponible : {error}")),
            ),
        };
    let can_save = config_path.is_some() && config_error.is_none();
    let config_state = Rc::new(RefCell::new(initial_config.clone()));

    let settings_title = heading("Paramètres du bureau");
    settings_title.set_margin_top(8);
    page.append(&settings_title);

    let settings_grid = gtk::Grid::builder()
        .column_spacing(16)
        .row_spacing(10)
        .hexpand(true)
        .build();

    let color_scheme = gtk::ComboBoxText::new();
    color_scheme.append(Some("dark"), "Sombre");
    color_scheme.append(Some("light"), "Clair");
    let _ = color_scheme.set_active_id(Some(initial_config.appearance.color_scheme.as_str()));

    let palette_variant = gtk::ComboBoxText::new();
    palette_variant.append(Some("subtle"), "Subtile");
    palette_variant.append(Some("balanced"), "Équilibrée");
    palette_variant.append(Some("vivid"), "Vive");
    palette_variant.append(Some("neon"), "Néon");
    let _ =
        palette_variant.set_active_id(Some(initial_config.bubble_color.palette_variant.as_str()));

    let bubble_color_enabled = gtk::CheckButton::with_label("Activer la génération BubbleColor");
    bubble_color_enabled.set_active(initial_config.bubble_color.enabled);

    let enforce_contrast = gtk::CheckButton::with_label("Renforcer le contraste");
    enforce_contrast.set_active(initial_config.bubble_color.enforce_contrast);

    let wallpaper_path = gtk::Entry::new();
    wallpaper_path.set_hexpand(true);
    wallpaper_path.set_placeholder_text(Some("/chemin/vers/fond-ecran.png"));
    wallpaper_path.set_text(initial_config.appearance.wallpaper.as_deref().unwrap_or(""));

    attach_setting(&settings_grid, 0, "Schéma de couleurs", &color_scheme);
    attach_setting(&settings_grid, 1, "Style de palette", &palette_variant);
    settings_grid.attach(&bubble_color_enabled, 1, 2, 1, 1);
    settings_grid.attach(&enforce_contrast, 1, 3, 1, 1);
    attach_setting(&settings_grid, 4, "Fond d’écran (chemin)", &wallpaper_path);
    page.append(&settings_grid);

    let restart_note = gtk::Label::new(Some(
        "Les modifications sont enregistrées de façon atomique. Elles seront prises en compte au prochain démarrage de BubbleWM ; aucun réglage n’est appliqué à chaud.",
    ));
    restart_note.set_wrap(true);
    restart_note.set_halign(gtk::Align::Start);
    restart_note.add_css_class("dim-label");
    page.append(&restart_note);

    let save_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let save_button = gtk::Button::with_label("Enregistrer les paramètres");
    save_button.set_sensitive(can_save);
    let save_status = gtk::Label::new(config_error.as_deref());
    save_status.set_wrap(true);
    save_status.set_halign(gtk::Align::Start);
    save_row.append(&save_button);
    save_row.append(&save_status);
    page.append(&save_row);

    if let Some(path) = config_path {
        if can_save {
            let config_state = Rc::clone(&config_state);
            let color_scheme = color_scheme.clone();
            let palette_variant = palette_variant.clone();
            let bubble_color_enabled = bubble_color_enabled.clone();
            let enforce_contrast = enforce_contrast.clone();
            let wallpaper_path = wallpaper_path.clone();
            let save_status = save_status.clone();

            save_button.connect_clicked(move |_| {
                let mut updated = config_state.borrow().clone();
                if let Some(value) = color_scheme.active_id() {
                    updated.appearance.color_scheme = value.to_string();
                }
                if let Some(value) = palette_variant.active_id() {
                    updated.bubble_color.palette_variant = value.to_string();
                }
                updated.bubble_color.enabled = bubble_color_enabled.is_active();
                updated.bubble_color.enforce_contrast = enforce_contrast.is_active();

                let wallpaper = wallpaper_path.text();
                let wallpaper = wallpaper.trim();
                updated.appearance.wallpaper = if wallpaper.is_empty() {
                    None
                } else {
                    Some(wallpaper.to_owned())
                };

                match config::save_atomic(&path, &updated) {
                    Ok(()) => {
                        *config_state.borrow_mut() = updated;
                        save_status.set_text(
                            "Configuration enregistrée. Redémarrez BubbleWM pour appliquer les changements.",
                        );
                    }
                    Err(error) => {
                        save_status.set_text(&format!("Échec de l’enregistrement : {error}"));
                    }
                }
            });
        }
    }

    let palette_title = heading("Aperçu de la palette BubbleColor");
    palette_title.set_margin_top(12);
    page.append(&palette_title);

    let palette_status = gtk::Label::new(Some("Connexion au compositeur BubbleWM…"));
    palette_status.set_halign(gtk::Align::Start);
    page.append(&palette_status);

    let palette_grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(12)
        .hexpand(true)
        .build();
    page.append(&palette_grid);

    let note = gtk::Label::new(Some(
        "L’aperçu affiche la dernière palette publiée par BubbleWM. BubbleSettings ne décode pas le fond d’écran et ne lance pas le moteur BubbleColor.",
    ));
    note.set_wrap(true);
    note.set_halign(gtk::Align::Start);
    note.set_margin_top(8);
    page.append(&note);

    AppearancePage {
        page,
        palette_grid,
        palette_status,
    }
}

fn attach_setting(grid: &gtk::Grid, row: i32, label: &str, control: &impl IsA<gtk::Widget>) {
    let label_widget = gtk::Label::new(Some(label));
    label_widget.set_halign(gtk::Align::Start);
    grid.attach(&label_widget, 0, row, 1, 1);
    grid.attach(control, 1, row, 1, 1);
}

fn heading(value: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(value));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("title-2");
    label
}

fn start_palette_monitor(grid: gtk::Grid, status: gtk::Label) {
    let (sender, receiver) = mpsc::sync_channel(1);

    // IPC is blocking by design (bounded socket timeout), so keep it off GTK's
    // main thread. The UI receives immutable palette snapshots only.
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
