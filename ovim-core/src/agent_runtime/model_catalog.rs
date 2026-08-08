//! Provider-independent model routing for delegated agents.
//!
//! A catalog route is a configured profile/model pair, not merely a model
//! name. Profiles may carry different credentials, endpoints, scopes, and
//! tool allowlists even when they name the same provider model.

use crate::ai::{AiConfig, AiProviderKind};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

const MAX_REASONING_EFFORT_LEN: usize = 32;

/// A provider-defined reasoning effort with a stable, validated wire value.
///
/// Ovim recognizes the common `none`, `minimal`, `low`, `medium`, `high`, and
/// `xhigh` and `max` values but deliberately does not close the type over that list.
/// Provider metadata remains authoritative for whether a model accepts a
/// syntactically valid value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReasoningEffort(String);

impl ReasoningEffort {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidReasoningEffort> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_REASONING_EFFORT_LEN
            && value.bytes().enumerate().all(|(index, byte)| match byte {
                b'a'..=b'z' => true,
                b'0'..=b'9' | b'_' | b'-' => index > 0,
                _ => false,
            });
        if !valid {
            return Err(InvalidReasoningEffort(value));
        }
        Ok(Self(value))
    }

    pub fn none() -> Self {
        Self("none".into())
    }

    pub fn minimal() -> Self {
        Self("minimal".into())
    }

    pub fn low() -> Self {
        Self("low".into())
    }

    pub fn medium() -> Self {
        Self("medium".into())
    }

    pub fn high() -> Self {
        Self("high".into())
    }

    pub fn xhigh() -> Self {
        Self("xhigh".into())
    }

    pub fn max() -> Self {
        Self("max".into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ReasoningEffort {
    type Err = InvalidReasoningEffort;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for ReasoningEffort {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ReasoningEffort {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidReasoningEffort(String);

impl fmt::Display for InvalidReasoningEffort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid reasoning effort {:?}; expected 1-{MAX_REASONING_EFFORT_LEN} lowercase ASCII letters, digits, '-' or '_'",
            self.0
        )
    }
}

impl std::error::Error for InvalidReasoningEffort {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelFallbackPolicy {
    FailClosed,
    Explicit {
        catalog_model_id: String,
        reasoning_effort: ReasoningEffort,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestedModelRoute {
    pub catalog_model_id: String,
    pub reasoning_effort: ReasoningEffort,
    pub fallback_policy: ModelFallbackPolicy,
}

impl RequestedModelRoute {
    pub fn exact(catalog_model_id: impl Into<String>, reasoning_effort: ReasoningEffort) -> Self {
        Self {
            catalog_model_id: catalog_model_id.into(),
            reasoning_effort,
            fallback_policy: ModelFallbackPolicy::FailClosed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelRouteResolution {
    Exact,
    ConfiguredFallback,
    /// Only produced while reading version-one dispatch history, which did
    /// not persist profile/provider/catalog identity.
    HistoricV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedModelRoute {
    pub catalog_generation: String,
    pub catalog_model_id: String,
    pub profile_name: String,
    pub provider: String,
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
    pub resolution: ModelRouteResolution,
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentModelCatalogEntry {
    pub id: String,
    pub provider: AiProviderKind,
    pub profile_name: String,
    pub model: String,
    pub supported_reasoning_efforts: BTreeSet<ReasoningEffort>,
    pub default_reasoning_effort: ReasoningEffort,
    pub supports_tools: bool,
    pub available: bool,
}

/// Optional provider discovery result for one exact configured profile/model.
///
/// Ovim does not currently have live provider model discovery. This overlay
/// makes that missing metadata explicit and gives later adapters a stable
/// insertion point without teaching routing to guess from model prefixes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderModelMetadata {
    pub profile_name: String,
    pub provider: AiProviderKind,
    pub model: String,
    pub supported_reasoning_efforts: BTreeSet<ReasoningEffort>,
    pub default_reasoning_effort: ReasoningEffort,
    pub supports_tools: bool,
    pub available: bool,
}

/// Built-in capability metadata for Ovim's first-party direct-Codex routes.
///
/// Keep model capability knowledge here rather than smuggling one profile's
/// default effort into the delegated-agent contract.
pub fn builtin_subagent_model_metadata(config: &AiConfig) -> Vec<ProviderModelMetadata> {
    config
        .profiles
        .values()
        .filter_map(|profile| {
            if profile.provider != AiProviderKind::Codex {
                return None;
            }
            let (efforts, default) = match profile.model.as_str() {
                "gpt-5.6-luna" => (vec![ReasoningEffort::max()], ReasoningEffort::max()),
                "gpt-5.6-terra" => (
                    vec![
                        ReasoningEffort::low(),
                        ReasoningEffort::medium(),
                        ReasoningEffort::high(),
                        ReasoningEffort::xhigh(),
                        ReasoningEffort::max(),
                    ],
                    ReasoningEffort::medium(),
                ),
                "gpt-5.6-sol" => (
                    vec![
                        ReasoningEffort::low(),
                        ReasoningEffort::medium(),
                        ReasoningEffort::high(),
                        ReasoningEffort::xhigh(),
                        ReasoningEffort::max(),
                    ],
                    ReasoningEffort::high(),
                ),
                _ => return None,
            };
            Some(ProviderModelMetadata {
                profile_name: profile.name.clone(),
                provider: profile.provider,
                model: profile.model.clone(),
                supported_reasoning_efforts: efforts.into_iter().collect(),
                default_reasoning_effort: default,
                supports_tools: true,
                available: true,
            })
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct SubagentModelCatalog {
    generation: String,
    entries: BTreeMap<String, SubagentModelCatalogEntry>,
}

impl SubagentModelCatalog {
    /// Build a conservative catalog from today's profile configuration.
    ///
    /// Since profiles expose one configured effort rather than a supported
    /// effort matrix, reasoning-capable providers advertise only that value.
    /// A missing value means `none`. Anthropic and Ollama currently ignore the
    /// setting in Ovim's adapters, so they advertise only `none`.
    pub fn from_config(config: &AiConfig) -> Result<Self, ModelCatalogError> {
        Self::from_config_with_metadata(config, std::iter::empty())
    }

    pub fn from_config_with_metadata(
        config: &AiConfig,
        metadata: impl IntoIterator<Item = ProviderModelMetadata>,
    ) -> Result<Self, ModelCatalogError> {
        let mut metadata_by_profile = BTreeMap::new();
        for item in metadata {
            if metadata_by_profile
                .insert(item.profile_name.clone(), item)
                .is_some()
            {
                return Err(ModelCatalogError::DuplicateMetadata);
            }
        }

        let mut entries = BTreeMap::new();
        let mut profile_names = config.profiles.keys().collect::<Vec<_>>();
        profile_names.sort();
        for profile_name in profile_names {
            let profile = &config.profiles[profile_name];
            if profile_name.trim().is_empty() || profile.model.trim().is_empty() {
                return Err(ModelCatalogError::InvalidProfile {
                    profile_name: profile_name.clone(),
                    detail: "profile and model names must be non-empty".into(),
                });
            }
            let discovered = metadata_by_profile.remove(profile_name);
            let (supported_reasoning_efforts, default_reasoning_effort, supports_tools, available) =
                if let Some(discovered) = discovered {
                    if discovered.provider != profile.provider || discovered.model != profile.model
                    {
                        return Err(ModelCatalogError::MetadataMismatch {
                            profile_name: profile_name.clone(),
                        });
                    }
                    validate_effort_matrix(
                        profile_name,
                        &discovered.supported_reasoning_efforts,
                        &discovered.default_reasoning_effort,
                    )?;
                    (
                        discovered.supported_reasoning_efforts,
                        discovered.default_reasoning_effort,
                        discovered.supports_tools,
                        discovered.available,
                    )
                } else {
                    let effort = configured_effort(profile_name, profile)?;
                    let child_supported = profile.provider != AiProviderKind::CodexAppServer;
                    (
                        BTreeSet::from([effort.clone()]),
                        effort,
                        child_supported,
                        child_supported,
                    )
                };
            let id = catalog_model_id(profile_name, &profile.model);
            if supported_reasoning_efforts.is_empty() {
                continue;
            }
            let default_reasoning_effort =
                if supported_reasoning_efforts.contains(&default_reasoning_effort) {
                    default_reasoning_effort
                } else {
                    supported_reasoning_efforts
                        .iter()
                        .next()
                        .expect("empty effort sets were skipped")
                        .clone()
                };
            let entry = SubagentModelCatalogEntry {
                id: id.clone(),
                provider: profile.provider,
                profile_name: profile_name.clone(),
                model: profile.model.clone(),
                supported_reasoning_efforts,
                default_reasoning_effort,
                supports_tools,
                available,
            };
            if entries.insert(id.clone(), entry).is_some() {
                return Err(ModelCatalogError::AmbiguousModelId(id));
            }
        }
        if let Some((profile_name, _)) = metadata_by_profile.into_iter().next() {
            return Err(ModelCatalogError::UnknownMetadataProfile(profile_name));
        }

        let generation = catalog_generation(&entries);
        Ok(Self {
            generation,
            entries,
        })
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn entries(&self) -> impl ExactSizeIterator<Item = &SubagentModelCatalogEntry> {
        self.entries.values()
    }

    pub fn entry(&self, id: &str) -> Option<&SubagentModelCatalogEntry> {
        self.entries.get(id)
    }

    pub fn resolve(
        &self,
        requested: &RequestedModelRoute,
        requires_tools: bool,
    ) -> Result<ResolvedModelRoute, ModelRouteError> {
        let requested_entry = self
            .entries
            .get(&requested.catalog_model_id)
            .ok_or_else(|| ModelRouteError::UnknownModel {
                catalog_model_id: requested.catalog_model_id.clone(),
                available: self.entries.keys().cloned().collect(),
            })?;
        validate_requested_effort(requested_entry, &requested.reasoning_effort)?;

        let exact_problem = if !requested_entry.available {
            Some("requested model is unavailable")
        } else if requires_tools && !requested_entry.supports_tools {
            Some("requested model does not support tools")
        } else {
            None
        };
        if exact_problem.is_none() {
            return Ok(self.resolved(
                requested_entry,
                requested.reasoning_effort.clone(),
                ModelRouteResolution::Exact,
                None,
            ));
        }

        let reason = exact_problem.expect("checked above");
        let ModelFallbackPolicy::Explicit {
            catalog_model_id,
            reasoning_effort,
        } = &requested.fallback_policy
        else {
            return Err(if !requested_entry.available {
                ModelRouteError::UnavailableModel(requested_entry.id.clone())
            } else {
                ModelRouteError::ToolIncompatible(requested_entry.id.clone())
            });
        };
        let fallback = self.entries.get(catalog_model_id).ok_or_else(|| {
            ModelRouteError::UnknownFallbackModel {
                catalog_model_id: catalog_model_id.clone(),
            }
        })?;
        validate_requested_effort(fallback, reasoning_effort).map_err(|error| match error {
            ModelRouteError::InvalidEffort {
                requested,
                supported,
                ..
            } => ModelRouteError::InvalidFallbackEffort {
                catalog_model_id: fallback.id.clone(),
                requested,
                supported,
            },
            other => other,
        })?;
        if !fallback.available {
            return Err(ModelRouteError::UnavailableFallbackModel(
                fallback.id.clone(),
            ));
        }
        if requires_tools && !fallback.supports_tools {
            return Err(ModelRouteError::ToolIncompatibleFallback(
                fallback.id.clone(),
            ));
        }
        Ok(self.resolved(
            fallback,
            reasoning_effort.clone(),
            ModelRouteResolution::ConfiguredFallback,
            Some(reason.into()),
        ))
    }

    fn resolved(
        &self,
        entry: &SubagentModelCatalogEntry,
        reasoning_effort: ReasoningEffort,
        resolution: ModelRouteResolution,
        fallback_reason: Option<String>,
    ) -> ResolvedModelRoute {
        ResolvedModelRoute {
            catalog_generation: self.generation.clone(),
            catalog_model_id: entry.id.clone(),
            profile_name: entry.profile_name.clone(),
            provider: entry.provider.to_string(),
            model: entry.model.clone(),
            reasoning_effort,
            resolution,
            fallback_reason,
        }
    }
}

fn configured_effort(
    profile_name: &str,
    profile: &crate::ai::AiProfileConfig,
) -> Result<ReasoningEffort, ModelCatalogError> {
    let configured = profile.reasoning_effort.as_deref().unwrap_or("none");
    let effort = ReasoningEffort::new(configured).map_err(|source| {
        ModelCatalogError::InvalidConfiguredEffort {
            profile_name: profile_name.into(),
            source,
        }
    })?;
    if matches!(
        profile.provider,
        AiProviderKind::Anthropic | AiProviderKind::Ollama
    ) && effort != ReasoningEffort::none()
    {
        return Err(ModelCatalogError::UnsupportedConfiguredEffort {
            profile_name: profile_name.into(),
            provider: profile.provider,
            effort,
        });
    }
    Ok(effort)
}

fn validate_effort_matrix(
    profile_name: &str,
    supported: &BTreeSet<ReasoningEffort>,
    default: &ReasoningEffort,
) -> Result<(), ModelCatalogError> {
    if supported.is_empty() {
        return Err(ModelCatalogError::InvalidProfile {
            profile_name: profile_name.into(),
            detail: "provider metadata has no supported reasoning efforts".into(),
        });
    }
    if !supported.contains(default) {
        return Err(ModelCatalogError::InvalidProfile {
            profile_name: profile_name.into(),
            detail: format!("default effort {default} is not in the supported effort set"),
        });
    }
    Ok(())
}

fn validate_requested_effort(
    entry: &SubagentModelCatalogEntry,
    requested: &ReasoningEffort,
) -> Result<(), ModelRouteError> {
    if entry.supported_reasoning_efforts.contains(requested) {
        return Ok(());
    }
    Err(ModelRouteError::InvalidEffort {
        catalog_model_id: entry.id.clone(),
        requested: requested.clone(),
        supported: entry.supported_reasoning_efforts.iter().cloned().collect(),
    })
}

pub fn catalog_model_id(profile_name: &str, model: &str) -> String {
    format!(
        "{}/{}",
        encode_catalog_segment(profile_name),
        encode_catalog_segment(model)
    )
}

fn encode_catalog_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn catalog_generation(entries: &BTreeMap<String, SubagentModelCatalogEntry>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ovim-subagent-model-catalog-v1\0");
    for entry in entries.values() {
        for value in [
            entry.id.as_str(),
            &entry.provider.to_string(),
            entry.profile_name.as_str(),
            entry.model.as_str(),
            entry.default_reasoning_effort.as_str(),
        ] {
            digest.update(value.as_bytes());
            digest.update(b"\0");
        }
        for effort in &entry.supported_reasoning_efforts {
            digest.update(effort.as_str().as_bytes());
            digest.update(b"\0");
        }
        digest.update([u8::from(entry.supports_tools), u8::from(entry.available)]);
    }
    format!("sha256:{:x}", digest.finalize())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelCatalogError {
    DuplicateMetadata,
    UnknownMetadataProfile(String),
    MetadataMismatch {
        profile_name: String,
    },
    InvalidProfile {
        profile_name: String,
        detail: String,
    },
    InvalidConfiguredEffort {
        profile_name: String,
        source: InvalidReasoningEffort,
    },
    UnsupportedConfiguredEffort {
        profile_name: String,
        provider: AiProviderKind,
        effort: ReasoningEffort,
    },
    AmbiguousModelId(String),
}

impl fmt::Display for ModelCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateMetadata => formatter.write_str("provider metadata repeats a profile"),
            Self::UnknownMetadataProfile(profile) => {
                write!(formatter, "provider metadata references unknown profile {profile:?}")
            }
            Self::MetadataMismatch { profile_name } => write!(
                formatter,
                "provider metadata does not match configured provider/model for profile {profile_name:?}"
            ),
            Self::InvalidProfile {
                profile_name,
                detail,
            } => write!(formatter, "invalid model profile {profile_name:?}: {detail}"),
            Self::InvalidConfiguredEffort {
                profile_name,
                source,
            } => write!(
                formatter,
                "profile {profile_name:?} has an invalid reasoning effort: {source}"
            ),
            Self::UnsupportedConfiguredEffort {
                profile_name,
                provider,
                effort,
            } => write!(
                formatter,
                "profile {profile_name:?} configures effort {effort} for {provider}, whose current Ovim adapter does not apply reasoning effort"
            ),
            Self::AmbiguousModelId(id) => write!(formatter, "catalog model ID {id:?} is ambiguous"),
        }
    }
}

impl std::error::Error for ModelCatalogError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelRouteError {
    UnknownModel {
        catalog_model_id: String,
        available: Vec<String>,
    },
    InvalidEffort {
        catalog_model_id: String,
        requested: ReasoningEffort,
        supported: Vec<ReasoningEffort>,
    },
    UnavailableModel(String),
    ToolIncompatible(String),
    UnknownFallbackModel {
        catalog_model_id: String,
    },
    InvalidFallbackEffort {
        catalog_model_id: String,
        requested: ReasoningEffort,
        supported: Vec<ReasoningEffort>,
    },
    UnavailableFallbackModel(String),
    ToolIncompatibleFallback(String),
}

impl fmt::Display for ModelRouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModel {
                catalog_model_id,
                available,
            } => write!(
                formatter,
                "unknown catalog model {catalog_model_id:?}; available models: {}",
                available.join(", ")
            ),
            Self::InvalidEffort {
                catalog_model_id,
                requested,
                supported,
            } => write!(
                formatter,
                "model {catalog_model_id:?} does not support effort {requested}; valid efforts: {}",
                display_efforts(supported)
            ),
            Self::UnavailableModel(id) => write!(formatter, "model {id:?} is unavailable"),
            Self::ToolIncompatible(id) => {
                write!(formatter, "model {id:?} does not support required tools")
            }
            Self::UnknownFallbackModel { catalog_model_id } => {
                write!(formatter, "configured fallback model {catalog_model_id:?} is unknown")
            }
            Self::InvalidFallbackEffort {
                catalog_model_id,
                requested,
                supported,
            } => write!(
                formatter,
                "fallback model {catalog_model_id:?} does not support effort {requested}; valid efforts: {}",
                display_efforts(supported)
            ),
            Self::UnavailableFallbackModel(id) => {
                write!(formatter, "configured fallback model {id:?} is unavailable")
            }
            Self::ToolIncompatibleFallback(id) => write!(
                formatter,
                "configured fallback model {id:?} does not support required tools"
            ),
        }
    }
}

impl std::error::Error for ModelRouteError {}

fn display_efforts(efforts: &[ReasoningEffort]) -> String {
    efforts
        .iter()
        .map(ReasoningEffort::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{AiProfileConfig, PROFILE_LOCAL};

    fn config_with_profiles(profiles: Vec<(&str, AiProviderKind, &str, Option<&str>)>) -> AiConfig {
        let mut config = AiConfig::default();
        let template = config.profiles.remove(PROFILE_LOCAL).unwrap();
        config.profiles.clear();
        for (name, provider, model, effort) in profiles {
            let mut profile = AiProfileConfig {
                name: name.into(),
                provider,
                model: model.into(),
                ..template.clone()
            };
            profile.reasoning_effort = effort.map(str::to_owned);
            config.profiles.insert(name.into(), profile);
        }
        config
    }

    fn metadata(
        profile_name: &str,
        model: &str,
        efforts: &[ReasoningEffort],
        available: bool,
    ) -> ProviderModelMetadata {
        ProviderModelMetadata {
            profile_name: profile_name.into(),
            provider: AiProviderKind::Codex,
            model: model.into(),
            supported_reasoning_efforts: efforts.iter().cloned().collect(),
            default_reasoning_effort: efforts[0].clone(),
            supports_tools: true,
            available,
        }
    }

    #[test]
    fn builtin_codex_routes_encode_effort_guidance() {
        let config = config_with_profiles(vec![
            ("luna", AiProviderKind::Codex, "gpt-5.6-luna", Some("low")),
            ("terra", AiProviderKind::Codex, "gpt-5.6-terra", Some("low")),
            ("sol", AiProviderKind::Codex, "gpt-5.6-sol", Some("low")),
        ]);
        let metadata = builtin_subagent_model_metadata(&config);
        let catalog = SubagentModelCatalog::from_config_with_metadata(&config, metadata).unwrap();

        let luna = catalog
            .entry(&catalog_model_id("luna", "gpt-5.6-luna"))
            .unwrap();
        assert_eq!(
            luna.supported_reasoning_efforts,
            BTreeSet::from([ReasoningEffort::max()])
        );
        assert_eq!(luna.default_reasoning_effort, ReasoningEffort::max());

        for (profile, model, default) in [
            ("terra", "gpt-5.6-terra", ReasoningEffort::medium()),
            ("sol", "gpt-5.6-sol", ReasoningEffort::high()),
        ] {
            let entry = catalog.entry(&catalog_model_id(profile, model)).unwrap();
            assert!(entry
                .supported_reasoning_efforts
                .contains(&ReasoningEffort::low()));
            assert!(entry
                .supported_reasoning_efforts
                .contains(&ReasoningEffort::max()));
            assert_eq!(entry.default_reasoning_effort, default);
        }
    }

    #[test]
    fn resolves_exact_profile_model_and_effort() {
        let config = config_with_profiles(vec![(
            "codex-default",
            AiProviderKind::Codex,
            "gpt-5.6-terra",
            Some("low"),
        )]);
        let catalog = SubagentModelCatalog::from_config(&config).unwrap();
        let id = catalog_model_id("codex-default", "gpt-5.6-terra");
        let resolved = catalog
            .resolve(
                &RequestedModelRoute::exact(&id, ReasoningEffort::low()),
                true,
            )
            .unwrap();

        assert_eq!(resolved.catalog_model_id, id);
        assert_eq!(resolved.profile_name, "codex-default");
        assert_eq!(resolved.provider, "codex");
        assert_eq!(resolved.model, "gpt-5.6-terra");
        assert_eq!(resolved.reasoning_effort, ReasoningEffort::low());
        assert_eq!(resolved.resolution, ModelRouteResolution::Exact);
        assert!(resolved.fallback_reason.is_none());
    }

    #[test]
    fn rejects_syntactically_invalid_and_model_invalid_effort() {
        assert!(ReasoningEffort::new("HIGH!").is_err());
        let config = config_with_profiles(vec![(
            "codex-default",
            AiProviderKind::Codex,
            "gpt-5.6-terra",
            Some("low"),
        )]);
        let catalog = SubagentModelCatalog::from_config(&config).unwrap();
        let id = catalog_model_id("codex-default", "gpt-5.6-terra");
        assert!(matches!(
            catalog.resolve(
                &RequestedModelRoute::exact(&id, ReasoningEffort::high()),
                true
            ),
            Err(ModelRouteError::InvalidEffort {
                requested,
                supported,
                ..
            }) if requested == ReasoningEffort::high()
                && supported == vec![ReasoningEffort::low()]
        ));
    }

    #[test]
    fn same_model_in_distinct_profiles_has_unambiguous_routes() {
        let config = config_with_profiles(vec![
            (
                "codex/fast",
                AiProviderKind::Codex,
                "org/gpt-5.6-terra",
                Some("low"),
            ),
            (
                "codex-safe",
                AiProviderKind::Codex,
                "org/gpt-5.6-terra",
                Some("low"),
            ),
        ]);
        let catalog = SubagentModelCatalog::from_config(&config).unwrap();
        let ids = catalog
            .entries()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();

        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert!(ids.iter().any(|id| id.starts_with("codex%2Ffast/")));
        assert!(ids.iter().all(|id| id.matches('/').count() == 1));
    }

    #[test]
    fn unavailable_model_fails_closed_without_allocating_a_guess() {
        let config = config_with_profiles(vec![(
            "codex-default",
            AiProviderKind::Codex,
            "gpt-5.6-terra",
            Some("low"),
        )]);
        let catalog = SubagentModelCatalog::from_config_with_metadata(
            &config,
            [metadata(
                "codex-default",
                "gpt-5.6-terra",
                &[ReasoningEffort::low()],
                false,
            )],
        )
        .unwrap();
        let id = catalog_model_id("codex-default", "gpt-5.6-terra");

        assert_eq!(
            catalog
                .resolve(
                    &RequestedModelRoute::exact(&id, ReasoningEffort::low()),
                    true
                )
                .unwrap_err(),
            ModelRouteError::UnavailableModel(id)
        );
    }

    #[test]
    fn explicit_fallback_is_visible_and_uses_its_own_effort() {
        let config = config_with_profiles(vec![
            (
                "preferred",
                AiProviderKind::Codex,
                "gpt-5.6-sol",
                Some("high"),
            ),
            (
                "fallback",
                AiProviderKind::Codex,
                "gpt-5.6-terra",
                Some("low"),
            ),
        ]);
        let catalog = SubagentModelCatalog::from_config_with_metadata(
            &config,
            [
                metadata(
                    "preferred",
                    "gpt-5.6-sol",
                    &[ReasoningEffort::high()],
                    false,
                ),
                metadata("fallback", "gpt-5.6-terra", &[ReasoningEffort::low()], true),
            ],
        )
        .unwrap();
        let preferred = catalog_model_id("preferred", "gpt-5.6-sol");
        let fallback = catalog_model_id("fallback", "gpt-5.6-terra");
        let resolved = catalog
            .resolve(
                &RequestedModelRoute {
                    catalog_model_id: preferred,
                    reasoning_effort: ReasoningEffort::high(),
                    fallback_policy: ModelFallbackPolicy::Explicit {
                        catalog_model_id: fallback.clone(),
                        reasoning_effort: ReasoningEffort::low(),
                    },
                },
                true,
            )
            .unwrap();

        assert_eq!(resolved.catalog_model_id, fallback);
        assert_eq!(resolved.reasoning_effort, ReasoningEffort::low());
        assert_eq!(
            resolved.resolution,
            ModelRouteResolution::ConfiguredFallback
        );
        assert_eq!(
            resolved.fallback_reason.as_deref(),
            Some("requested model is unavailable")
        );
    }
}
