//! Command line configuration front-end used by `bubble-settings`.
//!
//! The tool edits the same typed TOML file as the compositor, validates the
//! result before writing it atomically, and asks a running compositor to
//! reload through the local IPC socket.

use anyhow::{Context, Result, bail};
use bubble_ipc::{Request, Response, unix};
use bubblewm::config::Config;
use std::{env, path::PathBuf, process::ExitCode};

const USAGE: &str = "\
bubblewm-config — configure BubbleWM

Usage:
  bubblewm-config [--config PATH] get <key>
  bubblewm-config [--config PATH] set <key> <value>
  bubblewm-config [--config PATH] unset <key>
  bubblewm-config [--config PATH] list
  bubblewm-config [--config PATH] reset
  bubblewm-config [--config PATH] apply

Keys use dotted TOML paths, for example:
  effects.animations
  effects.animation_duration
  decoration.corner_radius
  theme.palette_style
  bar.show_wifi

Values are parsed as TOML when possible and fall back to plain strings.
`apply` reloads a running compositor through its control socket.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bubblewm-config: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut arguments = env::args().skip(1).peekable();
    let mut explicit_path = None;
    let mut command = None;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("bubblewm-config {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--config" => {
                explicit_path = Some(PathBuf::from(
                    arguments.next().context("--config requires a path")?,
                ));
            }
            other if other.starts_with('-') => bail!("unknown option: {other}"),
            other => {
                command = Some(other.to_owned());
                break;
            }
        }
    }

    let command = command.context("missing command; try --help")?;
    let path = explicit_path.unwrap_or_else(Config::default_path);

    match command.as_str() {
        "get" => {
            let key = arguments.next().context("get requires a key")?;
            let raw = load_raw(&path)?;
            let value =
                lookup(&raw, &key).with_context(|| format!("unknown configuration key: {key}"))?;
            println!("{}", display_value(value)?);
            Ok(())
        }
        "set" => {
            let key = arguments.next().context("set requires a key")?;
            let value = arguments.next().context("set requires a value")?;
            let mut raw = load_raw(&path)?;
            let parsed = parse_value(&value);
            insert(&mut raw, &key, parsed)
                .with_context(|| format!("cannot set configuration key: {key}"))?;
            let config = decode(&raw)?;
            config.save(Some(&path))?;
            Ok(())
        }
        "unset" => {
            let key = arguments.next().context("unset requires a key")?;
            let mut raw = load_raw(&path)?;
            let defaults = toml::Value::try_from(Config::default())
                .context("cannot serialize default configuration")?;
            match lookup(&defaults, &key).cloned() {
                Some(default) => insert(&mut raw, &key, default),
                None => remove(&mut raw, &key),
            }
            .with_context(|| format!("cannot unset configuration key: {key}"))?;
            let config = decode(&raw)?;
            config.save(Some(&path))?;
            Ok(())
        }
        "list" => {
            let raw = load_raw(&path)?;
            let mut lines = Vec::new();
            flatten(&raw, String::new(), &mut lines);
            lines.sort_by(|left, right| left.0.cmp(&right.0));
            for (key, value) in lines {
                println!("{key} {value}");
            }
            Ok(())
        }
        "reset" => {
            Config::default().save(Some(&path))?;
            println!("{}", path.display());
            Ok(())
        }
        "apply" => apply(),
        other => bail!("unknown command: {other}; try --help"),
    }
}

fn load_raw(path: &PathBuf) -> Result<toml::Value> {
    if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read configuration: {}", path.display()))?;
        return toml::from_str(&text)
            .with_context(|| format!("invalid TOML configuration: {}", path.display()));
    }
    toml::Value::try_from(Config::default()).context("cannot serialize default configuration")
}

fn decode(raw: &toml::Value) -> Result<Config> {
    let config: Config = raw
        .clone()
        .try_into()
        .context("configuration value has the wrong type")?;
    config.validate()?;
    Ok(config)
}

fn parse_value(value: &str) -> toml::Value {
    // A bare scalar is not a valid TOML document, so parse it as the value of
    // a synthetic key. Plain words such as `zoom` fall back to a string.
    toml::from_str::<toml::Table>(&format!("value = {value}"))
        .ok()
        .and_then(|mut table| table.remove("value"))
        .unwrap_or_else(|| toml::Value::String(value.to_owned()))
}

fn lookup<'a>(root: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    let mut current = root;
    for segment in key.split('.').filter(|segment| !segment.is_empty()) {
        current = current.get(segment)?;
    }
    Some(current)
}

fn insert(root: &mut toml::Value, key: &str, value: toml::Value) -> Result<()> {
    let segments = key
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        bail!("empty key");
    }
    let mut current = root;
    for segment in &segments[..segments.len() - 1] {
        let table = current
            .as_table_mut()
            .context("configuration path is not a table")?;
        current = table
            .entry((*segment).to_owned())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    }
    let table = current
        .as_table_mut()
        .context("configuration path is not a table")?;
    table.insert(segments[segments.len() - 1].to_owned(), value);
    Ok(())
}

fn remove(root: &mut toml::Value, key: &str) -> Result<()> {
    let segments = key
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        bail!("empty key");
    }
    let mut current = root;
    for segment in &segments[..segments.len() - 1] {
        let Some(next) = current.get_mut(*segment) else {
            return Ok(());
        };
        current = next;
    }
    if let Some(table) = current.as_table_mut() {
        table.remove(segments[segments.len() - 1]);
    }
    Ok(())
}

fn display_value(value: &toml::Value) -> Result<String> {
    match value {
        toml::Value::String(text) => Ok(text.clone()),
        toml::Value::Integer(number) => Ok(number.to_string()),
        toml::Value::Float(number) => Ok(number.to_string()),
        toml::Value::Boolean(flag) => Ok(flag.to_string()),
        toml::Value::Datetime(datetime) => Ok(datetime.to_string()),
        _ => bail!("value is not a scalar"),
    }
}

fn flatten(value: &toml::Value, prefix: String, output: &mut Vec<(String, String)>) {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(child, path, output);
            }
        }
        toml::Value::Array(items) => {
            if let Ok(text) = display_value(value) {
                output.push((prefix, text));
            } else {
                for (index, item) in items.iter().enumerate() {
                    flatten(item, format!("{prefix}.{index}"), output);
                }
            }
        }
        _ => {
            if let Ok(text) = display_value(value) {
                output.push((prefix, text));
            }
        }
    }
}

fn apply() -> Result<()> {
    let socket = unix::default_socket_path().map_err(|error| anyhow::anyhow!("{error}"))?;
    let response = unix::send_request(&socket, Request::ReloadConfig)
        .map_err(|error| anyhow::anyhow!("cannot reach BubbleWM: {error}"))?;
    match response {
        Response::Success { .. } => Ok(()),
        Response::Failure { error } => bail!("BubbleWM rejected the reload: {}", error.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> toml::Value {
        toml::Value::try_from(Config::default()).expect("default configuration")
    }

    #[test]
    fn set_and_get_nested_keys() {
        let mut raw = sample();
        insert(&mut raw, "effects.animation_duration", parse_value("420")).expect("insert");
        assert_eq!(
            lookup(&raw, "effects.animation_duration"),
            Some(&toml::Value::Integer(420))
        );
        let config = decode(&raw).expect("valid configuration");
        assert_eq!(config.effects.animation_duration, 420);
    }

    #[test]
    fn unset_restores_the_default() {
        let mut raw = sample();
        insert(&mut raw, "effects.open_effect", parse_value("burn")).expect("insert");
        let defaults = sample();
        let default = lookup(&defaults, "effects.open_effect")
            .cloned()
            .expect("default");
        insert(&mut raw, "effects.open_effect", default).expect("restore");
        let config = decode(&raw).expect("valid configuration");
        assert_eq!(
            config.effects.open_effect,
            Config::default().effects.open_effect
        );
    }

    #[test]
    fn plain_words_become_strings() {
        assert_eq!(parse_value("cube"), toml::Value::String("cube".to_owned()));
        assert_eq!(parse_value("true"), toml::Value::Boolean(true));
        assert_eq!(parse_value("12"), toml::Value::Integer(12));
        assert_eq!(
            parse_value("#22d3ee"),
            toml::Value::String("#22d3ee".to_owned())
        );
    }

    #[test]
    fn invalid_values_are_rejected_before_saving() {
        let mut raw = sample();
        insert(&mut raw, "effects.animation_duration", parse_value("99999")).expect("insert");
        assert!(decode(&raw).is_err());
    }
}
