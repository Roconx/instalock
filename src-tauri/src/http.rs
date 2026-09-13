//! The process's HTTP clients.
//!
//! There are exactly two, and the split is the point: certificate verification
//! is disabled for Riot's local APIs and **only** for those.
//!
//! Building a client per request repeats the TLS setup on the latency-critical
//! lock path, and neither config ever varies, so both are built once.

use std::sync::OnceLock;
use std::time::Duration;

/// Riot's local endpoints — the LCU on the lockfile port and the Live Client
/// Data API on 2999 — serve a self-signed certificate that nothing can
/// validate, so verification has to be off to talk to them at all. They're on
/// the loopback interface, so there is no network to be in the middle of.
static LOCAL: OnceLock<reqwest::Client> = OnceLock::new();

/// Community Dragon. A public CDN with a valid certificate, reached over the
/// real internet, so verification stays **on**: this fetch is what decides which
/// champion every pick and ban resolves to, and a tampered response would
/// silently redirect them.
static CDN: OnceLock<reqwest::Client> = OnceLock::new();

/// Client for the LCU and the Live Client Data API.
pub fn local() -> &'static reqwest::Client {
    LOCAL.get_or_init(|| {
        reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            // Without this a stalled LCU hangs a pick forever: the hover PATCH
            // never returns, the lock never runs, and nothing is ever logged.
            .timeout(Duration::from_secs(5))
            .build()
            .expect("failed to build local HTTP client")
    })
}

/// Client for Community Dragon. Longer timeout than `local`: these are real
/// internet round-trips for payloads of a few hundred KB, and nothing waiting
/// on them is on a countdown.
pub fn cdn() -> &'static reqwest::Client {
    CDN.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build CDN HTTP client")
    })
}
