//! Klien HTTP ke server API (login memakai token Bearer). Sertifikat server DIKUNCI lewat `ca_file` bila diisi.
use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::Method;
use serde_json::Value;

use crate::config::ClientConfig;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub message: String,
    /// Kode HTTP bila error dari server (401 = login tidak berlaku lagi). Dipakai fase K3 (resume_login).
    #[allow(dead_code)]
    pub status: Option<u16>,
}

impl ApiError {
    pub fn new(message: impl Into<String>) -> Self {
        ApiError { message: message.into(), status: None }
    }
}

pub struct Http {
    client: Client,
    base: String,
    token: Mutex<Option<String>>,
}

/// Pesan error dari server: `detail` bisa teks, daftar {msg}, atau {message}.
pub fn detail(body: &Value, status: u16) -> String {
    match &body["detail"] {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Array(a) if !a.is_empty() => a
            .iter()
            .map(|d| d["msg"].as_str().map(str::to_string).unwrap_or_else(|| d.to_string()))
            .collect::<Vec<_>>()
            .join("; "),
        Value::Object(o) => o.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("Server error ({status})")),
        _ => format!("Server error ({status})"),
    }
}

impl Http {
    pub fn new(cfg: &ClientConfig) -> Result<Http, String> {
        let mut b = Client::builder().timeout(Duration::from_secs(30));
        if let Some(ca) = &cfg.ca_file {
            let pem = std::fs::read(ca).map_err(|e| format!("ca_file tidak terbaca ({ca}): {e}"))?;
            b = b.use_preconfigured_tls(cloudpc_common::tls::pinned_tls(&pem)?);
        } else if !cfg.verify_tls {
            b = b.danger_accept_invalid_certs(true);
        }
        let client = b.build().map_err(|e| format!("Gagal menyiapkan klien HTTP: {e}"))?;
        Ok(Http { client, base: cfg.api_url.trim_end_matches('/').to_string(), token: Mutex::new(None) })
    }

    pub fn set_token(&self, token: Option<String>) {
        *self.token.lock().unwrap() = token;
    }

    pub fn token(&self) -> Option<String> {
        self.token.lock().unwrap().clone()
    }

    /// Permintaan mentah untuk unggahan: mengembalikan (kode, isi) tanpa menganggap 4xx sebagai error, dan membedakan
    /// kegagalan jaringan (Err) supaya unggahan bisa dicoba ulang otomatis.
    pub fn request_raw(&self, method: Method, path: &str, body: Option<Vec<u8>>, headers: &[(&str, String)],
                       timeout: Option<Duration>) -> Result<(u16, Value), String> {
        let mut rb = self.client.request(method, format!("{}{}", self.base, path));
        if let Some(t) = self.token() {
            rb = rb.bearer_auth(t);
        }
        for (k, v) in headers {
            rb = rb.header(*k, v.as_str());
        }
        if let Some(b) = body {
            rb = rb.body(b);
        }
        if let Some(t) = timeout {
            rb = rb.timeout(t);
        }
        let r = rb.send().map_err(|e| e.to_string())?;
        let code = r.status().as_u16();
        Ok((code, r.json().unwrap_or(Value::Null)))
    }

    /// GET untuk unduhan besar (opsional Range). Status diperiksa pemanggil; 416 berarti .partial sudah lengkap.
    pub fn get_stream(&self, path: &str, range: Option<String>) -> Result<Response, ApiError> {
        let mut rb = self.client.get(format!("{}{}", self.base, path)).timeout(Duration::from_secs(6 * 3600));
        if let Some(t) = self.token() {
            rb = rb.bearer_auth(t);
        }
        if let Some(r) = range {
            rb = rb.header("Range", r);
        }
        rb.send().map_err(|_| ApiError::new("Unduhan terputus. Coba unduh lagi; bagian yang sudah ada akan dilanjutkan."))
    }

    pub fn call(&self, method: Method, path: &str, json: Option<&Value>) -> Result<Value, ApiError> {
        let mut rb = self.client.request(method, format!("{}{}", self.base, path));
        if let Some(t) = self.token() {
            rb = rb.bearer_auth(t);
        }
        if let Some(j) = json {
            rb = rb.json(j);
        }
        let r = rb.send().map_err(|_| ApiError::new("Tidak bisa terhubung ke server. Periksa koneksi internet."))?;
        let status = r.status().as_u16();
        let body: Value = r.json().unwrap_or(Value::Null);
        if status >= 400 {
            return Err(ApiError { message: detail(&body, status), status: Some(status) });
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detail_formats() {
        assert_eq!(detail(&json!({"detail": "Salah"}), 400), "Salah");
        assert_eq!(detail(&json!({"detail": [{"msg": "a"}, {"msg": "b"}]}), 422), "a; b");
        assert_eq!(detail(&json!({"detail": {"message": "x"}}), 409), "x");
        assert_eq!(detail(&Value::Null, 500), "Server error (500)");
    }
}
