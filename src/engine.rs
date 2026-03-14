#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    ChromeFull,
    ChromeLean,
    Lightpanda,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineCapabilities {
    pub supports_profiles: bool,
    pub supports_downloads: bool,
    pub supports_extensions: bool,
    pub supports_webgl: bool,
    pub supports_recording: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestedFeatures {
    pub needs_profiles: bool,
    pub needs_downloads: bool,
    pub needs_extensions: bool,
    pub needs_webgl: bool,
    pub needs_recording: bool,
    pub prefers_low_memory: bool,
}

pub fn capabilities(kind: EngineKind) -> EngineCapabilities {
    match kind {
        EngineKind::ChromeFull => EngineCapabilities {
            supports_profiles: true,
            supports_downloads: true,
            supports_extensions: true,
            supports_webgl: true,
            supports_recording: true,
        },
        EngineKind::ChromeLean => EngineCapabilities {
            supports_profiles: true,
            supports_downloads: false,
            supports_extensions: false,
            supports_webgl: false,
            supports_recording: false,
        },
        EngineKind::Lightpanda => EngineCapabilities {
            supports_profiles: false,
            supports_downloads: false,
            supports_extensions: false,
            supports_webgl: false,
            supports_recording: false,
        },
    }
}

pub fn choose_engine(requested: &RequestedFeatures) -> EngineKind {
    if requested.needs_profiles
        || requested.needs_downloads
        || requested.needs_extensions
        || requested.needs_webgl
        || requested.needs_recording
    {
        return EngineKind::ChromeFull;
    }

    if requested.prefers_low_memory {
        return EngineKind::Lightpanda;
    }

    EngineKind::ChromeLean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_engine_prefers_full_for_downloads() {
        let kind = choose_engine(&RequestedFeatures {
            needs_downloads: true,
            ..RequestedFeatures::default()
        });
        assert_eq!(kind, EngineKind::ChromeFull);
    }

    #[test]
    fn choose_engine_prefers_low_memory_when_compatible() {
        let kind = choose_engine(&RequestedFeatures {
            prefers_low_memory: true,
            ..RequestedFeatures::default()
        });
        assert_eq!(kind, EngineKind::Lightpanda);
    }
}
