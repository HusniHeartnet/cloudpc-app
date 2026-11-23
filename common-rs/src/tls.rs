//! Pin sertifikat server: dipakai agent dan aplikasi pemain untuk berbicara dengan API di VPS.
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, SignatureScheme};

/// Mengunci SATU sertifikat server (pin per sertifikat), bukan memvalidasi rantai CA. Sertifikat self-signed di VPS
/// bertanda CA:TRUE; OpenSSL (agent/klien Python) menerimanya, tetapi rustls menolak CA sebagai sertifikat server
/// (CaUsedAsEndEntity). Pin per sertifikat lebih ketat: hanya sertifikat yang persis sama diterima, dan tanda tangan
/// handshake tetap diverifikasi terhadap kunci publiknya. Nama host/IP dan masa berlaku sengaja tidak diperiksa,
/// karena yang dipercaya adalah kunci itu sendiri.
#[derive(Debug)]
struct PinnedCert {
    der: Vec<u8>,
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _inter: &[CertificateDer<'_>], _name: &ServerName<'_>,
                          _ocsp: &[u8], _now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.der.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct)
        -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct)
        -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Konfigurasi TLS klien yang hanya mempercayai sertifikat pertama di `pem`.
pub fn pinned_tls(pem: &[u8]) -> Result<ClientConfig, String> {
    let cert = CertificateDer::from_pem_slice(pem).map_err(|e| format!("ca_file bukan PEM yang valid: {e}"))?;
    let provider = ring::default_provider();
    let verifier = PinnedCert { der: cert.as_ref().to_vec(), algs: provider.signature_verification_algorithms };
    Ok(ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_garbage_pem() {
        assert!(pinned_tls(b"bukan pem").is_err());
    }
}
