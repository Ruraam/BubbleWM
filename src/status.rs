//! Background session probes for the built-in BubbleOS bar.
//!
//! NetworkManager, PipeWire and PulseAudio queries can block for hundreds of
//! milliseconds. They therefore run on a dedicated worker thread and the
//! compositor only polls immutable snapshots from its own event loop.

use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const NETWORK_INTERVAL: Duration = Duration::from_secs(9);
const SHUTDOWN_SLICE: Duration = Duration::from_millis(100);

/// A Wi-Fi access point reported by NetworkManager.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u8,
    pub secured: bool,
    pub active: bool,
}

/// A NetworkManager connection profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedConnection {
    pub name: String,
    pub kind: String,
    pub active: bool,
}

/// A Bluetooth device known to BlueZ.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BluetoothDevice {
    pub address: String,
    pub name: String,
    pub connected: bool,
    pub trusted: bool,
}

/// Wireless and audio state displayed by the bar and its network menu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionStatus {
    /// Active Wi-Fi SSID. `None` means no active wireless connection.
    pub ssid: Option<String>,
    /// Default sink volume in percent.
    pub volume: Option<u8>,
    pub muted: bool,
    /// True when NetworkManager answered a probe.
    pub network_manager: bool,
    /// True when the Wi-Fi radio is enabled.
    pub wifi_enabled: bool,
    /// Signal quality of the active connection.
    pub signal: Option<u8>,
    /// Access points discovered by NetworkManager.
    pub networks: Vec<WifiNetwork>,
    /// Saved connection profiles, including wired and VPN links.
    pub saved: Vec<SavedConnection>,
    /// True when BlueZ answered a probe.
    pub bluetooth_available: bool,
    /// True when the Bluetooth adapter is powered.
    pub bluetooth_enabled: bool,
    /// Paired Bluetooth devices, connected ones first.
    pub bluetooth_devices: Vec<BluetoothDevice>,
}

/// Non-blocking status worker owned by the compositor.
pub struct StatusRuntime {
    updates: Receiver<SessionStatus>,
    refresh_requests: Sender<()>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl StatusRuntime {
    pub fn start() -> Self {
        let (sender, updates) = mpsc::channel();
        let (refresh_requests, refresh_receiver) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let worker = thread::Builder::new()
            .name("bubblewm-status".into())
            .spawn(move || {
                let mut previous = SessionStatus::default();
                let mut network_age = Duration::ZERO;
                let mut network = probe_network();
                let mut bluetooth = probe_bluetooth();
                loop {
                    if worker_shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    let audio = probe_volume();
                    let mut status = SessionStatus {
                        ssid: network
                            .networks
                            .iter()
                            .find(|entry| entry.active && !entry.ssid.is_empty())
                            .map(|entry| entry.ssid.clone()),
                        volume: audio.map(|(volume, _)| volume),
                        muted: audio.map(|(_, muted)| muted).unwrap_or(false),
                        network_manager: network.network_manager,
                        wifi_enabled: network.wifi_enabled,
                        signal: network
                            .networks
                            .iter()
                            .find(|entry| entry.active)
                            .map(|entry| entry.signal),
                        networks: network.networks.clone(),
                        saved: network.saved.clone(),
                        bluetooth_available: bluetooth.available,
                        bluetooth_enabled: bluetooth.enabled,
                        bluetooth_devices: bluetooth.devices.clone(),
                    };
                    if status.ssid.is_none() {
                        status.ssid = probe_ssid_fallback();
                    }
                    if status != previous {
                        if sender.send(status.clone()).is_err() {
                            break;
                        }
                        previous = status;
                    }

                    let mut waited = Duration::ZERO;
                    loop {
                        let remaining = POLL_INTERVAL.saturating_sub(waited);
                        match refresh_receiver.recv_timeout(remaining.min(SHUTDOWN_SLICE)) {
                            Ok(()) => {
                                // Drain burst requests and refresh immediately.
                                while refresh_receiver.try_recv().is_ok() {}
                                network = probe_network();
                                bluetooth = probe_bluetooth();
                                network_age = Duration::ZERO;
                                break;
                            }
                            Err(RecvTimeoutError::Timeout) => {
                                if worker_shutdown.load(Ordering::Relaxed) {
                                    return;
                                }
                                waited += SHUTDOWN_SLICE;
                                if waited >= POLL_INTERVAL {
                                    network_age += POLL_INTERVAL;
                                    if network_age >= NETWORK_INTERVAL {
                                        network = probe_network();
                                        bluetooth = probe_bluetooth();
                                        network_age = Duration::ZERO;
                                    }
                                    break;
                                }
                            }
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                }
            })
            .ok();
        Self {
            updates,
            refresh_requests,
            shutdown,
            worker,
        }
    }

    /// Returns the newest snapshot, or `None` when nothing changed.
    pub fn poll(&self) -> Option<SessionStatus> {
        match self.updates.try_recv() {
            Ok(status) => Some(status),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    /// Asks the worker to re-probe NetworkManager immediately.
    pub fn refresh(&self) {
        let _ = self.refresh_requests.send(());
    }
}

impl Drop for StatusRuntime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct NetworkSnapshot {
    network_manager: bool,
    wifi_enabled: bool,
    networks: Vec<WifiNetwork>,
    saved: Vec<SavedConnection>,
}

impl Default for NetworkSnapshot {
    fn default() -> Self {
        Self {
            network_manager: false,
            wifi_enabled: false,
            networks: Vec::new(),
            saved: Vec::new(),
        }
    }
}

fn probe_network() -> NetworkSnapshot {
    let mut snapshot = NetworkSnapshot::default();

    let Ok(radio) = Command::new("nmcli").args(["-t", "radio", "wifi"]).output() else {
        return snapshot;
    };
    if !radio.status.success() {
        return snapshot;
    }
    snapshot.network_manager = true;
    snapshot.wifi_enabled = String::from_utf8_lossy(&radio.stdout)
        .trim()
        .eq_ignore_ascii_case("enabled");

    if let Ok(output) = Command::new("nmcli")
        .args([
            "-t",
            "-f",
            "IN-USE,SSID,SIGNAL,SECURITY",
            "device",
            "wifi",
            "list",
            "--rescan",
            "no",
        ])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let fields = split_nmcli(line);
            if fields.len() < 4 {
                continue;
            }
            let active = fields[0].trim() == "*";
            let ssid = fields[1].trim().to_owned();
            let signal = fields[2].trim().parse::<u8>().unwrap_or(0).min(100);
            let security = fields[3].trim();
            let secured = !security.is_empty() && security != "--";
            if ssid.is_empty() {
                continue;
            }
            if let Some(existing) = snapshot
                .networks
                .iter_mut()
                .find(|entry| entry.ssid == ssid)
            {
                if signal > existing.signal {
                    existing.signal = signal;
                }
                existing.active |= active;
                existing.secured |= secured;
                continue;
            }
            snapshot.networks.push(WifiNetwork {
                ssid,
                signal,
                secured,
                active,
            });
        }
    }

    if let Ok(output) = Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE,ACTIVE", "connection", "show"])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let fields = split_nmcli(line);
            if fields.len() < 3 {
                continue;
            }
            let name = fields[0].trim().to_owned();
            if name.is_empty() {
                continue;
            }
            snapshot.saved.push(SavedConnection {
                name,
                kind: fields[1].trim().to_owned(),
                active: matches!(fields[2].trim(), "yes" | "*"),
            });
        }
    }

    snapshot
}

/// Splits an `nmcli -t` line on unescaped colons.
fn split_nmcli(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ':' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(character);
        }
    }
    fields.push(current);
    fields
}

struct BluetoothSnapshot {
    available: bool,
    enabled: bool,
    devices: Vec<BluetoothDevice>,
}

impl Default for BluetoothSnapshot {
    fn default() -> Self {
        Self {
            available: false,
            enabled: false,
            devices: Vec::new(),
        }
    }
}

fn probe_bluetooth() -> BluetoothSnapshot {
    let mut snapshot = BluetoothSnapshot::default();

    let Ok(show) = Command::new("bluetoothctl").arg("show").output() else {
        return snapshot;
    };
    if !show.status.success() {
        return snapshot;
    }
    let text = String::from_utf8_lossy(&show.stdout);
    if !text.contains("Controller") {
        return snapshot;
    }
    snapshot.available = true;
    snapshot.enabled = text
        .lines()
        .map(str::trim)
        .any(|line| line.eq_ignore_ascii_case("Powered: yes"));

    let connected = Command::new("bluetoothctl")
        .args(["devices", "Connected"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let connected = connected
        .lines()
        .filter_map(parse_bluetooth_device)
        .map(|device| device.address)
        .collect::<Vec<_>>();

    if let Ok(output) = Command::new("bluetoothctl").arg("devices").output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let Some(device) = parse_bluetooth_device(line) else {
                continue;
            };
            if snapshot
                .devices
                .iter()
                .any(|entry| entry.address == device.address)
            {
                continue;
            }
            snapshot.devices.push(BluetoothDevice {
                connected: connected.contains(&device.address),
                ..device
            });
        }
    }

    snapshot.devices.sort_by(|left, right| {
        right
            .connected
            .cmp(&left.connected)
            .then(left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    snapshot
}

fn parse_bluetooth_device(line: &str) -> Option<BluetoothDevice> {
    let line = line.trim();
    let mut fields = line.splitn(3, char::is_whitespace);
    if !fields.next()?.eq_ignore_ascii_case("Device") {
        return None;
    }
    let address = fields.next()?.trim().to_owned();
    let name = fields.next().unwrap_or("").trim().to_owned();
    if address.is_empty() {
        return None;
    }
    Some(BluetoothDevice {
        address,
        name: if name.is_empty() {
            "Appareil Bluetooth".into()
        } else {
            name
        },
        connected: false,
        trusted: false,
    })
}

fn probe_ssid_fallback() -> Option<String> {
    // Fallbacks for minimal images without nmcli.
    if let Ok(output) = Command::new("iwgetid").arg("-r").output()
        && output.status.success()
    {
        let ssid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !ssid.is_empty() {
            return Some(ssid);
        }
    }
    None
}

fn probe_volume() -> Option<(u8, bool)> {
    if let Ok(output) = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        let volume = text
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<f32>().ok())
            .map(|value| (value * 100.0).round().clamp(0.0, 150.0) as u8);
        if let Some(volume) = volume {
            return Some((volume, text.contains("[MUTED]")));
        }
    }

    if let Ok(output) = Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        let volume = text.split_whitespace().find_map(|word| {
            word.strip_suffix('%')
                .and_then(|value| value.parse::<u8>().ok())
        });
        if let Some(volume) = volume {
            let muted = Command::new("pactl")
                .args(["get-sink-mute", "@DEFAULT_SINK@"])
                .output()
                .map(|output| String::from_utf8_lossy(&output.stdout).contains("yes"))
                .unwrap_or(false);
            return Some((volume, muted));
        }
    }

    None
}

/// Toggles the default sink mute state from the bar click handler.
pub fn toggle_mute() {
    if Command::new("wpctl")
        .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        return;
    }
    let _ = Command::new("pactl")
        .args(["set-sink-mute", "@DEFAULT_SINK@", "toggle"])
        .status();
}

/// Enables or disables the Wi-Fi radio.
pub fn set_wifi_enabled(enabled: bool) {
    let _ = Command::new("nmcli")
        .args(["radio", "wifi", if enabled { "on" } else { "off" }])
        .status();
}

/// Connects to an open or already-known network.
/// Powers the Bluetooth adapter on or off.
pub fn set_bluetooth_enabled(enabled: bool) {
    let _ = Command::new("bluetoothctl")
        .args(["power", if enabled { "on" } else { "off" }])
        .output();
}

/// Connects a paired Bluetooth device.
pub fn connect_bluetooth(address: &str) {
    let _ = Command::new("bluetoothctl")
        .args(["connect", address])
        .output();
}

/// Disconnects a Bluetooth device.
pub fn disconnect_bluetooth(address: &str) {
    let _ = Command::new("bluetoothctl")
        .args(["disconnect", address])
        .output();
}

/// Starts a short background scan for nearby Bluetooth devices.
pub fn scan_bluetooth() {
    let _ = Command::new("sh")
        .arg("-c")
        .arg("bluetoothctl --timeout 8 scan on >/dev/null 2>&1")
        .spawn();
}

pub fn connect_wifi(ssid: &str) {
    let _ = Command::new("nmcli")
        .args(["device", "wifi", "connect", ssid])
        .status();
}

/// Connects to a saved connection profile by name.
pub fn connect_saved(name: &str) {
    let _ = Command::new("nmcli")
        .args(["connection", "up", name])
        .status();
}

/// Disconnects the active Wi-Fi device.
pub fn disconnect_wifi() {
    if let Ok(output) = Command::new("nmcli")
        .args(["-t", "-f", "DEVICE,TYPE,STATE", "device", "status"])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let fields = split_nmcli(line);
            if fields.len() >= 3 && fields[1] == "wifi" && fields[2].starts_with("connected") {
                let _ = Command::new("nmcli")
                    .args(["device", "disconnect", fields[0].trim()])
                    .status();
                return;
            }
        }
    }
    let _ = Command::new("nmcli")
        .args(["radio", "wifi", "off"])
        .status();
}

/// Opens `nmcli --ask` in a terminal so the user can type a passphrase.
pub fn connect_wifi_ask(terminal: &str, ssid: &str) {
    let command = format!(
        "exec {} -e nmcli --ask device wifi connect {}",
        terminal,
        shell_quote(ssid)
    );
    let _ = Command::new("sh").arg("-c").arg(command).spawn();
}

/// Opens the NetworkManager text interface in a terminal.
pub fn open_network_settings(terminal: &str) {
    let command = format!("exec {} -e nmtui", terminal);
    let _ = Command::new("sh").arg("-c").arg(command).spawn();
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
