//! The one place HTTP clients start. Linux reqwest is built without a rustls crypto provider, so a
//! client built any other way panics in `build()`; this attaches ring, which GPUI already links.
//! Clippy rejects reqwest's named constructors but cannot name its `Default` impls, so
//! `Client::default()` and `ClientBuilder::default()` still compile and panic the same way.

use reqwest::{Client, ClientBuilder};

/// Which certificate authorities a client trusts.
pub(crate) enum Trust {
    /// The system's, for the internet.
    System,
    /// None: the client speaks plain HTTP on loopback, so on Linux it never reads the system's
    /// certificates, which a minimal install may lack.
    Nobody,
}

/// Starts an HTTP client builder trusting `trust`: rustls on ring on Linux, with the platform
/// verifier for [`Trust::System`], and the operating system's TLS stack elsewhere.
///
/// # Errors
/// Fails on Linux when [`Trust::System`] finds no CA certificate in the system trust store.
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
pub(crate) fn client_builder(trust: Trust) -> anyhow::Result<ClientBuilder> {
    #[cfg(target_os = "linux")]
    {
        use std::sync::Arc;

        use rustls_platform_verifier::BuilderVerifierExt as _;

        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?;
        let mut config = match trust {
            Trust::System => config.with_platform_verifier()?,
            Trust::Nobody => config.with_root_certificates(rustls::RootCertStore::empty()),
        }
        .with_no_client_auth();
        // reqwest offers HTTP/1.1 alone without its http2 feature, and sets this for its own configs.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Client::builder().tls_backend_preconfigured(config))
    }
    #[cfg(not(target_os = "linux"))]
    Ok(match trust {
        Trust::System => Client::builder(),
        Trust::Nobody => Client::builder().tls_certs_only([]),
    })
}
