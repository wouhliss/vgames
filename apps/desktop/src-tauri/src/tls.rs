//! Select TLS cryptography before a startup task opens its first connection.
//!
//! HTTP enables aws-lc while the updater and SQL client enable ring. Rustls cannot
//! infer a process default when both are present; reconnecting realtime after an
//! update must not depend on the updater having initialized ring in the old process.

pub(crate) fn initialize() {
    // A provider can already be installed by an embedding host. Installation is
    // atomic and leaves that provider intact; otherwise match the updater's ring.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    #[test]
    fn startup_initializes_tls_in_a_fresh_process() {
        const CHILD: &str = "VGAMES_TLS_STARTUP_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tls::tests::startup_initializes_tls_in_a_fresh_process",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert!(rustls::crypto::CryptoProvider::get_default().is_none());
        super::initialize();
        let provider = rustls::crypto::CryptoProvider::get_default().unwrap();
        assert_eq!(
            provider.cipher_suites,
            rustls::crypto::ring::default_provider().cipher_suites
        );
        // This is the builder realtime uses; without explicit selection, the
        // combined ring/aws-lc feature graph panics here in a new process.
        let _ = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        super::initialize();
        assert!(std::ptr::eq(
            provider,
            rustls::crypto::CryptoProvider::get_default().unwrap()
        ));
    }
}
