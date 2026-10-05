// The settings file: key=value lines in ~/.config/nebula/settings (the toolbar's state
// and the settings screen's, kept between runs as stock's app keeps its preferences).

use gtk::glib;
use std::collections::HashMap;
use std::path::PathBuf;

fn path() -> PathBuf {
    glib::user_config_dir().join("nebula").join("settings")
}

pub fn load() -> HashMap<String, String> {
    // first run under this name: the settings it had as l16-camera2
    if !path().exists() {
        let old = glib::user_config_dir().join("l16-camera2").join("settings");
        if let Ok(text) = std::fs::read_to_string(&old) {
            save(&text);
        }
    }
    let text = std::fs::read_to_string(path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

pub fn save(text: &str) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&p, text) {
        eprintln!("nebula: saving settings: {e}");
    }
}
