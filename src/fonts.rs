// The fonts the app is set in (DM Sans and DM Mono, SIL Open Font License: assets/fonts/OFL.txt),
// carried in the binary so nothing has to be installed: written once to the cache directory and
// added to fontconfig as the app's own fonts, before GTK has made its font map.

use gtk::glib;
use std::ffi::CString;
use std::os::raw::{c_int, c_void};

#[link(name = "fontconfig")]
extern "C" {
    fn FcConfigAppFontAddFile(config: *mut c_void, file: *const std::os::raw::c_char) -> c_int;
}

const FONTS: &[(&str, &[u8])] = &[
    ("DMSans-Variable.ttf", include_bytes!("../assets/fonts/DMSans-Variable.ttf")),
    ("DMMono-Light.ttf", include_bytes!("../assets/fonts/DMMono-Light.ttf")),
    ("DMMono-Regular.ttf", include_bytes!("../assets/fonts/DMMono-Regular.ttf")),
    ("DMMono-Medium.ttf", include_bytes!("../assets/fonts/DMMono-Medium.ttf")),
];

// where the files are kept: the cache directory (written again if one is missing or a different size)
fn dir() -> std::path::PathBuf {
    glib::user_cache_dir().join("nebula").join("fonts")
}

pub fn install() {
    let dir = dir();
    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!("nebula: fonts: no {}", dir.display());
        return;
    }
    for (name, bytes) in FONTS {
        let path = dir.join(name);
        if std::fs::metadata(&path).map_or(true, |m| m.len() != bytes.len() as u64) {
            // written aside and renamed: two instances (locked and not) can start together
            let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
            if let Err(e) = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &path)) {
                eprintln!("nebula: fonts: {}: {e}", path.display());
                let _ = std::fs::remove_file(&tmp);
                continue;
            }
        }
        if let Ok(c) = CString::new(path.to_string_lossy().as_bytes()) {
            // SAFETY: a valid C string; a null config is fontconfig's current one
            if unsafe { FcConfigAppFontAddFile(std::ptr::null_mut(), c.as_ptr()) } == 0 {
                eprintln!("nebula: fonts: fontconfig would not take {}", path.display());
            }
        }
    }
}
