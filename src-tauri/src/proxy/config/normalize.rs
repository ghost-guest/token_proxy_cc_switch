use std::collections::{HashMap, HashSet};

use super::{
    model_mapping::compile_model_mappings, HeaderOverride, ProviderUpstreams, UpstreamConfig,
    UpstreamGroup, UpstreamOverrides, UpstreamRuntime,
};
use axum::http::header::{HeaderName, HeaderValue};

#[derive(Clone)]
pub(super) struct NormalizedUpstream {
    pub(super) provider: String,
    pub(super) runtime: UpstreamRuntime,
}

pub(super) fn fill_missing_upstream_indices(
    upstreams: &mut [UpstreamConfig],
) -> Result<(), String> {
    let mut max_index: Option<i32> = None;
    for upstream in upstreams.iter() {
        if let Some(index) = upstream.index {
            max_index = Some(max_index.map_or(index, |current| current.max(index)));
        }
    }
    let mut next_index = match max_index {
        Some(value) => value
            .checked_add(1)
            .ok_or_else(|| "Upstream index is out of range.".to_string())?,
        None => 0,
    };
    for upstream in upstreams.iter_mut() {
        if upstream.index.is_none() {
            upstream.index = Some(assign_next_index(&mut next_index)?);
        }
    }
    Ok(())
}

pub(super) fn normalize_upstreams(
    upstreams: &[UpstreamConfig],
) -> Result<Vec<NormalizedUpstream>, String> {
    let max_index = scan_upstream_indices(upstreams)?;
    let mut next_index = match max_index {
        Some(value) => value
            .checked_add(1)
            .ok_or_else(|| "Upstream index is out of range.".to_string())?,
        None => 0,
    };
    let mut normalized = Vec::with_capacity(upstreams.len());
    for (order, upstream) in upstreams.iter().enumerate() {
        if let Some(entry) = normalize_single_upstream(upstream, order, &mut next_index)? {
            normalized.push(entry);
        }
    }
    Ok(normalized)
}

pub(super) fn build_provider_upstreams(
    upstreams: Vec<NormalizedUpstream>,
) -> Result<HashMap<String, ProviderUpstreams>, String> {
    let mut grouped: HashMap<String, Vec<UpstreamRuntime>> = HashMap::new();
    for upstream in upstreams {
        grouped
            .entry(upstream.provider)
            .or_default()
            .push(upstream.runtime);
    }
    let mut output = HashMap::new();
    for (provider, upstreams) in grouped {
        let groups = group_upstreams_by_priority(upstreams);
        output.insert(provider, ProviderUpstreams { groups });
    }
    Ok(output)
}

fn assign_next_index(next_index: &mut i32) -> Result<i32, String> {
    let current = *next_index;
    *next_index = next_index
        .checked_add(1)
        .ok_or_else(|| "Upstream index is out of range.".to_string())?;
    Ok(current)
}

fn group_upstreams_by_priority(mut upstreams: Vec<UpstreamRuntime>) -> Vec<UpstreamGroup> {
    upstreams.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.index.cmp(&right.index))
            .then_with(|| left.order().cmp(&right.order()))
    });
    let mut groups: Vec<UpstreamGroup> = Vec::new();
    for upstream in upstreams {
        match groups.last_mut() {
            Some(group) if group.priority == upstream.priority => group.items.push(upstream),
            _ => groups.push(UpstreamGroup {
                priority: upstream.priority,
                items: vec![upstream],
            }),
        }
    }
    groups
}

fn scan_upstream_indices(upstreams: &[UpstreamConfig]) -> Result<Option<i32>, String> {
    let mut seen_ids = HashSet::new();
    let mut max_index = None::<i32>;
    for upstream in upstreams {
        let id = upstream.id.trim();
        if id.is_empty() {
            return Err("Upstream id cannot be empty.".to_string());
        }
        if !seen_ids.insert(id.to_string()) {
            return Err(format!("Upstream id already exists: {id}."));
        }
        if let Some(index) = upstream.index {
            max_index = Some(max_index.map_or(index, |current| current.max(index)));
        }
    }
    Ok(max_index)
}

fn normalize_single_upstream(
    upstream: &UpstreamConfig,
    order: usize,
    next_index: &mut i32,
) -> Result<Option<NormalizedUpstream>, String> {
    if !upstream.enabled {
        return Ok(None);
    }
    let provider = upstream.provider.trim();
    if provider.is_empty() {
        return Err(format!(
            "Upstream {} provider cannot be empty.",
            upstream.id
        ));
    }
    let base_url = upstream.base_url.trim();
    if base_url.is_empty() {
        return Err(format!(
            "Upstream {} base_url cannot be empty.",
            upstream.id
        ));
    }
    // When index is missing, assign sequentially after the global max for stable ordering.
    let index = match upstream.index {
        Some(value) => value,
        None => assign_next_index(next_index)?,
    };
    let api_key = upstream
        .api_key
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let model_mappings = compile_model_mappings(&upstream.id, &upstream.model_mappings)?;
    let header_overrides = normalize_header_overrides(upstream.overrides.as_ref())?;
    let runtime = UpstreamRuntime {
        id: upstream.id.trim().to_string(),
        base_url: base_url.to_string(),
        api_key,
        priority: upstream.priority.unwrap_or(0),
        index,
        model_mappings,
        header_overrides,
        order,
    };
    Ok(Some(NormalizedUpstream {
        provider: provider.to_string(),
        runtime,
    }))
}

fn normalize_header_overrides(
    overrides: Option<&UpstreamOverrides>,
) -> Result<Option<Vec<HeaderOverride>>, String> {
    let Some(overrides) = overrides else {
        return Ok(None);
    };
    if overrides.header.is_empty() {
        return Ok(None);
    }

    let mut normalized = Vec::with_capacity(overrides.header.len());
    for (raw_name, raw_value) in &overrides.header {
        let trimmed = raw_name.trim();
        let name = HeaderName::from_bytes(trimmed.as_bytes())
            .map_err(|_| format!("Invalid header name in overrides: {raw_name}"))?;

        let value: Option<HeaderValue> = match raw_value {
            Some(value) => {
                if value.is_empty() {
                    // 允许空字符串，代表设置为空值。
                    Some(HeaderValue::from_str("").map_err(|_| {
                        format!("Invalid header value for {raw_name}")
                    })?)
                } else {
                    Some(HeaderValue::from_str(value).map_err(|_| {
                        format!("Invalid header value for {raw_name}")
                    })?)
                }
            }
            None => None,
        };

        normalized.push(HeaderOverride { name, value });
    }

    // 用户输入大小写混合时，保持用户写法；应用阶段再做覆盖策略。
    Ok(Some(normalized))
}
