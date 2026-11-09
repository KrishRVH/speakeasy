use reqwest::{Client, ClientBuilder};

pub(crate) enum Trust {
    System,
    Nobody,
}

#[expect(
    clippy::disallowed_methods,
    reason = "The one place that constructs reqwest clients; Linux reqwest has no built-in rustls provider"
)]
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "Callers share the Linux signature, whose TLS setup can fail"
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

        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Client::builder().tls_backend_preconfigured(config))
    }
    #[cfg(not(target_os = "linux"))]
    Ok(match trust {
        Trust::System => Client::builder(),
        Trust::Nobody => Client::builder().tls_certs_only([]),
    })
}
