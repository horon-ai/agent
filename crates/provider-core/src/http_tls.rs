//! The TLS configuration every `reqwest` client in this crate is built with.

/// Give `builder` the process-wide platform TLS configuration.
///
/// The trust set and protocol list live in the platform adapter. A client built
/// without this configuration has no trusted roots at all, which is what keeps
/// an accidental plain builder from reaching the network.
pub(crate) fn with_platform_tls(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    builder.use_preconfigured_tls(horon_agent_platform::tls_client_config().as_ref().clone())
}

/// Last-resort client: the same TLS configuration and no other settings.
///
/// Only a builder setting can fail a client, never the TLS configuration, so
/// this build has nothing left to reject. Should it fail anyway, the default
/// client trusts no root at all and therefore refuses every HTTPS request
/// instead of accepting one it cannot verify.
pub(crate) fn fallback_client() -> reqwest::Client {
    match with_platform_tls(reqwest::Client::builder()).build() {
        Ok(client) => client,
        Err(err) => {
            eprintln!("horon-agent: cannot build an HTTP client with the platform TLS configuration: {err}");
            // Fail closed: the default client trusts no root, so every HTTPS
            // request is refused instead of being sent unverified.
            reqwest::Client::default()
        }
    }
}
