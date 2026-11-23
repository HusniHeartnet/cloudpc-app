//! `client_config.json`: file yang sama dengan aplikasi Python (field asing diabaikan).
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub api_url: String,
    pub brand_name: String,
    pub moonlight_path: Option<String>,
    pub moonlight_app: String,
    /// Template perintah Moonlight ({host}, {pin}, {app}, {resolution}, {fps}, {bitrate_kbps}, {codec}, {display_mode});
    /// kosong = bawaan. Bisa diubah di client_config.json bila versi Moonlight berbeda.
    pub pair_args: Option<Vec<String>>,
    pub stream_args: Option<Vec<String>>,
    pub verify_tls: bool,
    /// Sertifikat server yang dikunci (self-signed, tanpa domain). Kalau diisi, HANYA sertifikat ini yang dipercaya.
    pub ca_file: Option<String>,
    pub heartbeat_interval: f64,
    pub poll_interval: f64,
    pub reconnect_grace_seconds: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            api_url: "http://localhost:8000".into(),
            brand_name: "CloudPC".into(),
            moonlight_path: None,
            moonlight_app: "Desktop".into(),
            pair_args: None,
            stream_args: None,
            verify_tls: true,
            ca_file: None,
            heartbeat_interval: 30.0,
            poll_interval: 1.0,
            reconnect_grace_seconds: 120,
        }
    }
}

impl ClientConfig {
    pub fn parse(raw: &str) -> Result<ClientConfig, String> {
        // Tahan BOM dari PowerShell 5.1.
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).map_err(|e| format!("client_config.json rusak: {e}"))
    }

    pub fn load(path: &Path) -> Result<ClientConfig, String> {
        if !path.exists() {
            return Ok(ClientConfig::default());
        }
        let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut cfg = Self::parse(&raw)?;
        cfg.ca_file = cfg.ca_file.filter(|c| !c.is_empty());
        if let Some(ca) = &cfg.ca_file {
            if !Path::new(ca).is_absolute() {
                // relatif terhadap folder client_config.json
                cfg.ca_file = Some(path.parent().unwrap_or(Path::new(".")).join(ca).to_string_lossy().into_owned());
            }
        }
        Ok(cfg)
    }
}

/// Urutan pencarian: variabel CLOUDPC_CLIENT_CONFIG, di samping .exe, lalu `client/client_config.json` di repo
/// (saat development, exe ada di <repo>/target/debug).
pub fn find_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("CLOUDPC_CLIENT_CONFIG") {
        return PathBuf::from(p);
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let beside = exe_dir.join("client_config.json");
    if beside.exists() {
        return beside;
    }
    // macOS: CloudPC.app/Contents/MacOS/<exe> -> file yang dibundel ada di Contents/Resources.
    if cfg!(target_os = "macos") {
        let res = exe_dir.parent().map(|p| p.join("Resources").join("client_config.json"));
        if let Some(r) = res.filter(|r| r.exists()) {
            return r;
        }
    }
    for dir in exe_dir.ancestors().take(5) {
        let dev = dir.join("client").join("client_config.json");
        if dev.exists() {
            return dev;
        }
    }
    beside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bom_defaults_and_ignores_unknown_fields() {
        let c = ClientConfig::parse("\u{feff}{\"api_url\":\"https://x\",\"pair_args\":[\"a\"]}").unwrap();
        assert_eq!(c.api_url, "https://x");
        assert!(c.verify_tls && c.brand_name == "CloudPC" && c.moonlight_app == "Desktop");
    }

    #[test]
    fn missing_file_gives_defaults_and_ca_is_relative_to_config() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(ClientConfig::load(&t.path().join("nope.json")).unwrap().api_url, "http://localhost:8000");
        let p = t.path().join("client_config.json");
        std::fs::write(&p, r#"{"api_url":"https://x","ca_file":"server.pem"}"#).unwrap();
        assert_eq!(PathBuf::from(ClientConfig::load(&p).unwrap().ca_file.unwrap()), t.path().join("server.pem"));
    }
}
