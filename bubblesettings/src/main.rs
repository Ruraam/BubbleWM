//! BubbleSettings: independent GTK4 settings shell.
//!
//! The Appearance form edits the shared, validated BubbleWM configuration.
//! Writes are atomic and explicitly require a compositor restart; palette
//! generation remains owned by BubbleColor inside BubbleWM.

use std::{
    cell::RefCell,
    fs::File,
    io::Read,
    path::Path,
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

    let parent_window: gtk::Window = window.clone().upcast();
    let appearance = build_appearance_page(&parent_window);
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

fn build_appearance_page(parent: &gtk::Window) -> AppearancePage {
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

    let wallpaper_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let browse_wallpaper = gtk::Button::with_label("Parcourir…");
    wallpaper_row.append(&wallpaper_path);
    wallpaper_row.append(&browse_wallpaper);
    connect_wallpaper_picker(parent, &browse_wallpaper, &wallpaper_path);

    attach_setting(&settings_grid, 0, "Schéma de couleurs", &color_scheme);
    attach_setting(&settings_grid, 1, "Style de palette", &palette_variant);
    settings_grid.attach(&bubble_color_enabled, 1, 2, 1, 1);
    settings_grid.attach(&enforce_contrast, 1, 3, 1, 1);
    attach_setting(&settings_grid, 4, "Fond d’écran", &wallpaper_row);
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
                    match validate_wallpaper_path(wallpaper) {
                        Ok(path) => Some(path),
                        Err(error) => {
                            save_status.set_text(&format!("Fond d’écran invalide : {error}"));
                            return;
                        }
                    }
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

fn connect_wallpaper_picker(parent: &gtk::Window, button: &gtk::Button, entry: &gtk::Entry) {
    let parent = parent.clone();
    let entry = entry.clone();
    button.connect_clicked(move |_| {
        let dialog = gtk::FileChooserNative::new(
            Some("Choisir un fond d’écran"),
            Some(&parent),
            gtk::FileChooserAction::Open,
            Some("Ouvrir"),
            Some("Annuler"),
        );
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images PNG, JPEG et WebP"));
        filter.add_mime_type("image/png");
        filter.add_mime_type("image/jpeg");
        filter.add_mime_type("image/webp");
        dialog.add_filter(&filter);

        let entry = entry.clone();
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                match dialog.file().and_then(|file| file.path()) {
                    Some(path) => match validate_wallpaper_path(&path.to_string_lossy()) {
                        Ok(path) => {
                            entry.set_text(&path);
                            entry.set_tooltip_text(None);
                        }
                        Err(error) => {
                            entry.set_text("");
                            entry.set_tooltip_text(Some(&format!("Image refusée : {error}")));
                        }
                    },
                    None => {
                        entry.set_text("");
                    }
                }
            }
            dialog.destroy();
        });
        dialog.show();
    });
}

/// Accept only existing, readable, bounded local images supported by BubbleColor.
/// The compositor's image decoder performs the full format/decompression checks
/// later on its bounded worker thread, never in the GTK event loop.
fn validate_wallpaper_path(value: &str) -> Result<String, String> {
    let path = Path::new(value.trim());
    if !path.is_absolute() {
        return Err("le chemin doit être absolu".into());
    }

    let canonical = path
        .canonicalize()
        .map_err(|error| format!("impossible d’accéder au fichier : {error}"))?;
    let mut file = File::open(&canonical)
        .map_err(|error| format!("fichier non lisible : {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("impossible de lire les informations du fichier : {error}"))?;
    if !metadata.is_file() {
        return Err("la sélection n’est pas un fichier régulier".into());
    }
    if metadata.len() > 64 * 1024 * 1024 {
        return Err("l’image dépasse la limite de 64 Mio".into());
    }

    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !["png", "jpg", "jpeg", "webp"].contains(&extension.as_str()) {
        return Err("formats acceptés : PNG, JPEG et WebP".into());
    }

    // Reject obvious extension spoofing without decoding image data in GTK.
    // Full decoding and decompression limits remain the compositor's job.
    let mut header = Vec::with_capacity(12);
    file.by_ref()
        .take(12)
        .read_to_end(&mut header)
        .map_err(|error| format!("impossible de lire l’en-tête de l’image : {error}"))?;
    let valid_signature = match extension.as_str() {
        "png" => header.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => header.starts_with(b"\xff\xd8\xff"),
        "webp" => {
            header.len() >= 12 && &header[0..4] == b"RIFF" && &header[8..12] == b"WEBP"
        }
        _ => false,
    };
    if !valid_signature {
        return Err("le contenu du fichier ne correspond pas à son extension".into());
    }

    Ok(canonical.to_string_lossy().into_owned())
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

#[cfg(test)]
mod wallpaper_validation_tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "bubble-settings-wallpaper-{}-{}",
                std::process::id(),
                NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn file(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
            let path = self.0.join(name);
            fs::write(&path, bytes).expect("write test image");
            path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const PNG_HEADER: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPEG_HEADER: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIF";
    const WEBP_HEADER: &[u8] = b"RIFF\x00\x00\x00\x00WEBP";

    #[test]
    fn accepts_supported_image_signatures_and_case_insensitive_extensions() {
        let dir = TestDir::new();
        for (name, header) in [
            ("wallpaper.PNG", PNG_HEADER),
            ("wallpaper.jpg", JPEG_HEADER),
            ("wallpaper.jpeg", JPEG_HEADER),
            ("wallpaper.webp", WEBP_HEADER),
        ] {
            let path = dir.file(name, header);
            assert_eq!(
                validate_wallpaper_path(path.to_str().expect("UTF-8 path")).unwrap(),
                path.canonicalize().unwrap().to_string_lossy().into_owned()
            );
        }
    }

    #[test]
    fn rejects_relative_and_missing_paths() {
        assert!(validate_wallpaper_path("wallpaper.png").is_err());
        let dir = TestDir::new();
        let missing = dir.0.join("missing.png");
        assert!(validate_wallpaper_path(missing.to_str().unwrap()).is_err());
    }

    #[test]
    fn rejects_directories_and_unsupported_extensions() {
        let dir = TestDir::new();
        assert!(validate_wallpaper_path(dir.0.to_str().unwrap()).is_err());
        let path = dir.file("wallpaper.png.txt", PNG_HEADER);
        assert!(validate_wallpaper_path(path.to_str().unwrap()).is_err());
    }

    #[test]
    fn rejects_extension_spoofing_and_truncated_headers() {
        let dir = TestDir::new();
        let spoofed = dir.file("wallpaper.png", b"not a PNG image");
        assert!(validate_wallpaper_path(spoofed.to_str().unwrap()).is_err());
        let truncated = dir.file("truncated.webp", b"RIFF");
        assert!(validate_wallpaper_path(truncated.to_str().unwrap()).is_err());
    }

    #[test]
    fn rejects_files_larger_than_64_mib() {
        let dir = TestDir::new();
        let path = dir.0.join("oversized.png");
        let file = File::create(&path).expect("create sparse file");
        file.set_len(64 * 1024 * 1024 + 1)
            .expect("size sparse file");
        assert!(validate_wallpaper_path(path.to_str().unwrap())
            .unwrap_err()
            .contains("64 Mio"));
    }
}

