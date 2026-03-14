#![allow(dead_code)]

use crate::broker_protocol::{BrokerRequest, BrokerResponse};
use crate::errors::CliError;

pub fn broker_enabled() -> bool {
    std::env::var("SAURON_BROKER")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub async fn submit(request: BrokerRequest) -> Result<BrokerResponse, CliError> {
    // Phase-1 broker compatibility path: echo-style in-process passthrough.
    // The CLI can opt into this path early while the resident broker is developed.
    Ok(BrokerResponse {
        request_id: request.request_id,
        ok: true,
        payload: request.payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_disabled_by_default() {
        std::env::remove_var("SAURON_BROKER");
        assert!(!broker_enabled());
    }
}
