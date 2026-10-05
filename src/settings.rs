// The settings file: key=value lines in ~/.config/l16-camera2/settings (the toolbar's state
// and the settings screen's, kept between runs as stock's app keeps its preferences).

use gtk::glib;
use std::collections::HashMap;
use std::path::PathBuf;

fn path() -> PathBuf {
    glib::user_config_dir().join("l16-camera2").join("settings")
}

pub fn load() -> HashMap<String, String> {
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
        eprintln!("l16-camera2: saving settings: {e}");
    }
}
