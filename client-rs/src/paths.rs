//! Lokasi folder per sistem operasi (sama dengan client/cloudpc_client/paths.py), supaya preferensi dan log dibagi
//! dengan aplikasi Python di komputer yang sama.
use std::path::PathBuf;

pub fn app_data_dir() -> PathBuf {
    // Untuk tes: jangan menyentuh preferensi aplikasi yang sebenarnya.
    if let Some(p) = std::env::var_os("CLOUDPC_APP_DATA") {
        return PathBuf::from(p);
    }
    if cfg!(target_os = "macos") {
        return home().join("Library").join("Application Support").join("CloudPC");
    }
    if cfg!(windows) {
        let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home);
        return base.join("CloudPC");
    }
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local").join("share")).join("CloudPC")
}

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default()
}
