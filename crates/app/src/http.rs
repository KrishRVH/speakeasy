//! The one place HTTP clients start. Linux reqwest is built without a rustls crypto provider, so a
//! client built any other way panics in `build()`; this attaches ring, which GPUI already links.
//! Clippy rejects reqwest's named constructors but cannot name its `Default` impls, so
//! `Client::default()` and `ClientBuilder::default()` still compile and panic the same way.

use reqwest::{Client, ClientBuilder};

/// Starts an HTTP client builder: rustls on ring with the platform verifier on Linux, and the
/// operating system's TLS stack elsewhere.
///
/// # Errors
/// Fails on Linux when the system trust store yields no CA certificate.
#[expect(
    clippy::disallowed_methods,
    reason = "the one place that constructs reqwest clients; Linux reqwest has no built-in rustls provider"
)]
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "callers share the Linux signature, whose TLS setup can fail"
    )
)]
pub(crate) fn client_builder() -> anyhow::Result<ClientBuilder> {
    #[cfg(target_os = "linux")]
    {
        use std::sync::Arc;

        use rustls_platform_verifier::BuilderVerifierExt as _;

        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
        // reqwest offers HTTP/1.1 alone without its http2 feature, and sets this for its own configs.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Client::builder().tls_backend_preconfigured(config))
    }
    #[cfg(not(target_os = "linux"))]
    Ok(Client::builder())
}
