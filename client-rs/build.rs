use std::path::PathBuf;

/// Aplikasi HP tidak punya `client_config.json` di samping program, jadi config dan sertifikat server yang dikunci
/// ditanam saat build: CLOUDPC_MOBILE_CONFIG = path client_config.json, CLOUDPC_MOBILE_CA = path sertifikat (.pem).
/// Tanpa variabel itu file kosong yang ditanam (config bawaan). Di build desktop file ini tidak dipakai.
fn embed(var: &str, out_name: &str, fallback: &str) {
    println!("cargo:rerun-if-env-changed={var}");
    let content = match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{var}={}: {e}", PathBuf::from(&path).display()))
        }
        None => fallback.to_string(),
    };
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join(out_name);
    std::fs::write(&out, content).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}

fn main() {
    embed("CLOUDPC_MOBILE_CONFIG", "mobile_config.json", "{}");
    embed("CLOUDPC_MOBILE_CA", "mobile_ca.pem", "");
    tauri_build::build()
}
