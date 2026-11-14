use bubble_theme_engine::{generate_from_path, ColorMode, PalettePersonality, ThemeOptions};
use std::path::PathBuf;

fn usage() {
    eprintln!(
        "Usage: bubble-theme generate --wallpaper <FILE> [--output <FILE>] [--mode dark|light] [--personality balanced|vivid|muted]\n\nWrites a versioned BubbleOS theme JSON document to stdout or --output."
    );
}

fn main() {
    if let Err(error) = run() {
        eprintln!("bubble-theme: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        usage();
        return Err("missing command".into());
    };
    if command == "--help" || command == "-h" {
        usage();
        return Ok(());
    }
    if command != "generate" {
        usage();
        return Err(format!("unknown command: {command}").into());
    }

    let mut wallpaper: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut options = ThemeOptions::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--wallpaper" => {
                wallpaper = Some(args.next().ok_or("--wallpaper requires a path")?.into())
            }
            "--output" => output = Some(args.next().ok_or("--output requires a path")?.into()),
            "--mode" => {
                options.mode = match args.next().as_deref() {
                    Some("dark") => ColorMode::Dark,
                    Some("light") => ColorMode::Light,
                    _ => return Err("--mode must be dark or light".into()),
                };
            }
            "--personality" => {
                options.personality = match args.next().as_deref() {
                    Some("balanced") => PalettePersonality::Balanced,
                    Some("vivid") => PalettePersonality::Vivid,
                    Some("muted") => PalettePersonality::Muted,
                    _ => return Err("--personality must be balanced, vivid or muted".into()),
                };
            }
            "--help" | "-h" => {
                usage();
                return Ok(());
            }
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }

    let wallpaper = wallpaper.ok_or("--wallpaper is required")?;
    let theme = generate_from_path(&wallpaper, &options)?;
    let json = serde_json::to_string_pretty(&theme)?;

    if let Some(path) = output {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, format!("{json}\n"))?;
        std::fs::rename(temp, path)?;
    } else {
        println!("{json}");
    }
    Ok(())
}
