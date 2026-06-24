//! Provider/model route catalog.
//!
//! Provider metadata answers "who is this provider?" and model metadata
//! answers "what is this model?". This module is the narrow join: it maps a
//! semantic model choice to the wire id used by a concrete provider.

use crate::config::{
    ApiProvider, DEFAULT_DEEPINFRA_FLASH_MODEL, DEFAULT_DEEPINFRA_MODEL,
    DEFAULT_NOVITA_FLASH_MODEL, DEFAULT_NOVITA_MODEL, DEFAULT_NVIDIA_NIM_FLASH_MODEL,
    DEFAULT_NVIDIA_NIM_MODEL, DEFAULT_OPENROUTER_FLASH_MODEL, DEFAULT_OPENROUTER_MODEL,
    DEFAULT_SGLANG_FLASH_MODEL, DEFAULT_SGLANG_MODEL, DEFAULT_SILICONFLOW_FLASH_MODEL,
    DEFAULT_SILICONFLOW_MODEL, DEFAULT_TEXT_MODEL, DEFAULT_TOGETHER_MODEL,
    DEFAULT_VLLM_FLASH_MODEL, DEFAULT_VLLM_MODEL, DEFAULT_VOLCENGINE_FLASH_MODEL,
    DEFAULT_VOLCENGINE_MODEL,
};

pub(crate) const DEFAULT_TOGETHER_FLASH_MODEL: &str = "deepseek-ai/DeepSeek-V4-Flash";

const CANONICAL_DEEPSEEK_V4_PRO: &str = "deepseek-v4-pro";
const CANONICAL_DEEPSEEK_V4_FLASH: &str = "deepseek-v4-flash";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteModelRole {
    /// Best-quality/default model for provider-aware coding and reasoning.
    Primary,
    /// Faster or cheaper sibling for simple turns and exploratory workers.
    Fast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteModel {
    pub(crate) role: RouteModelRole,
    pub(crate) canonical_id: &'static str,
    pub(crate) wire_id: &'static str,
}

const DEEPSEEK_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_TEXT_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: CANONICAL_DEEPSEEK_V4_FLASH,
    },
];
const NVIDIA_NIM_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_NVIDIA_NIM_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: DEFAULT_NVIDIA_NIM_FLASH_MODEL,
    },
];
const OPENROUTER_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_OPENROUTER_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: DEFAULT_OPENROUTER_FLASH_MODEL,
    },
];
const NOVITA_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_NOVITA_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: DEFAULT_NOVITA_FLASH_MODEL,
    },
];
const FIREWORKS_ROUTES: &[RouteModel] = &[RouteModel {
    role: RouteModelRole::Primary,
    canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
    wire_id: crate::config::DEFAULT_FIREWORKS_MODEL,
}];
const SILICONFLOW_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_SILICONFLOW_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: DEFAULT_SILICONFLOW_FLASH_MODEL,
    },
];
const SGLANG_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_SGLANG_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: DEFAULT_SGLANG_FLASH_MODEL,
    },
];
const VLLM_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_VLLM_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: DEFAULT_VLLM_FLASH_MODEL,
    },
];
const VOLCENGINE_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_VOLCENGINE_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: DEFAULT_VOLCENGINE_FLASH_MODEL,
    },
];
const DEEPINFRA_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_DEEPINFRA_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: DEFAULT_DEEPINFRA_FLASH_MODEL,
    },
];
const TOGETHER_ROUTES: &[RouteModel] = &[
    RouteModel {
        role: RouteModelRole::Primary,
        canonical_id: CANONICAL_DEEPSEEK_V4_PRO,
        wire_id: DEFAULT_TOGETHER_MODEL,
    },
    RouteModel {
        role: RouteModelRole::Fast,
        canonical_id: CANONICAL_DEEPSEEK_V4_FLASH,
        wire_id: DEFAULT_TOGETHER_FLASH_MODEL,
    },
];

#[must_use]
pub(crate) fn route_models_for_provider(provider: ApiProvider) -> &'static [RouteModel] {
    match provider {
        ApiProvider::Deepseek | ApiProvider::DeepseekCN => DEEPSEEK_ROUTES,
        ApiProvider::NvidiaNim => NVIDIA_NIM_ROUTES,
        ApiProvider::Openrouter => OPENROUTER_ROUTES,
        ApiProvider::Novita => NOVITA_ROUTES,
        ApiProvider::Fireworks => FIREWORKS_ROUTES,
        ApiProvider::Siliconflow | ApiProvider::SiliconflowCn => SILICONFLOW_ROUTES,
        ApiProvider::Sglang => SGLANG_ROUTES,
        ApiProvider::Vllm => VLLM_ROUTES,
        ApiProvider::Volcengine => VOLCENGINE_ROUTES,
        ApiProvider::Deepinfra => DEEPINFRA_ROUTES,
        ApiProvider::Together => TOGETHER_ROUTES,
        _ => &[],
    }
}

#[must_use]
pub(crate) fn provider_can_route_model(provider: ApiProvider, model: &str) -> bool {
    canonical_route_model_id(model).is_some_and(|canonical| {
        route_models_for_provider(provider)
            .iter()
            .any(|route| route.canonical_id == canonical)
    })
}

#[must_use]
pub(crate) fn static_model_ids_for_provider(provider: ApiProvider) -> Option<Vec<&'static str>> {
    let routes = route_models_for_provider(provider);
    (!routes.is_empty()).then(|| routes.iter().map(|route| route.wire_id).collect())
}

#[must_use]
pub(crate) fn wire_model_for_provider(provider: ApiProvider, model: &str) -> Option<&'static str> {
    let canonical = canonical_route_model_id(model)?;
    route_models_for_provider(provider)
        .iter()
        .find(|route| route.canonical_id == canonical)
        .map(|route| route.wire_id)
}

#[must_use]
pub(crate) fn role_model_for_provider(
    provider: ApiProvider,
    role: RouteModelRole,
) -> Option<&'static str> {
    route_models_for_provider(provider)
        .iter()
        .find(|route| route.role == role)
        .map(|route| route.wire_id)
}

#[must_use]
pub(crate) fn primary_fast_pair_for_provider(
    provider: ApiProvider,
) -> Option<(&'static str, &'static str)> {
    Some((
        role_model_for_provider(provider, RouteModelRole::Primary)?,
        role_model_for_provider(provider, RouteModelRole::Fast)?,
    ))
}

fn canonical_route_model_id(model: &str) -> Option<&'static str> {
    match model.trim().to_ascii_lowercase().as_str() {
        "deepseek-v4-pro"
        | "deepseek-v4pro"
        | "deepseek-ai/deepseek-v4-pro"
        | "deepseek-ai/deepseek-v4pro"
        | "deepseek/deepseek-v4-pro"
        | "deepseek/deepseek-v4pro"
        | "deepseek-reasoner"
        | "deepseek-r1" => Some(CANONICAL_DEEPSEEK_V4_PRO),
        "deepseek-v4-flash"
        | "deepseek-v4flash"
        | "deepseek-v4"
        | "deepseek-ai/deepseek-v4-flash"
        | "deepseek-ai/deepseek-v4flash"
        | "deepseek/deepseek-v4-flash"
        | "deepseek/deepseek-v4flash"
        | "deepseek-chat"
        | "deepseek-v3" => Some(CANONICAL_DEEPSEEK_V4_FLASH),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn together_route_rows_are_role_based() {
        assert!(provider_can_route_model(
            ApiProvider::Together,
            "deepseek-v4-pro"
        ));
        assert_eq!(
            role_model_for_provider(ApiProvider::Together, RouteModelRole::Primary),
            Some(DEFAULT_TOGETHER_MODEL)
        );
        assert_eq!(
            role_model_for_provider(ApiProvider::Together, RouteModelRole::Fast),
            Some(DEFAULT_TOGETHER_FLASH_MODEL)
        );
        assert_eq!(
            static_model_ids_for_provider(ApiProvider::Together),
            Some(vec![DEFAULT_TOGETHER_MODEL, DEFAULT_TOGETHER_FLASH_MODEL])
        );
    }

    #[test]
    fn direct_provider_without_route_rows_cannot_route_deepseek_family() {
        assert!(!provider_can_route_model(
            ApiProvider::Zai,
            "deepseek-v4-pro"
        ));
    }
}
