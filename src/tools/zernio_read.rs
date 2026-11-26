//! Read-only tools: `read_x_post`, `read_youtube_video` (direct extractors) and the Zernio read tools.

use super::*;
use crate::context::Access;
use crate::extract::ExtractionRequest;
use crate::pipeline::content_hash;
use crate::urls::{detect_source_type, parse_and_normalize, SourceType};
use crate::zernio::{validate_connection, ReadResource};
use serde::Deserialize;

#[derive(Deserialize)]
struct UrlInput {
    url: String,
}

pub async fn read_source(state: &AppState, call: &Call<'_>, input: Value, kind: &str) -> ApiResult<ToolOutput> {
    let input: UrlInput = parse_input(input)?;
    let url = parse_and_normalize(&input.url, &state.url_policy)?;
    let st = detect_source_type(&url, "auto");
    let expected = if kind == "x" { SourceType::XPost } else { SourceType::YoutubeVideo };
    if st != expected {
        let (tool, example) =
            if kind == "x" { ("read_x_post", "https://x.com/<user>/status/<id>") } else { ("read_youtube_video", "https://www.youtube.com/watch?v=<id>") };
        return Err(ApiError::validation(format!(
            "{tool} only reads {} URLs like {example} (got host '{}')",
            expected.as_str(),
            url.host_str().unwrap_or("?")
        ))
        .with_details(json!({ "expected": expected.as_str(), "detected": st.as_str() })));
    }
    let req = ExtractionRequest {
        source_id: new_id("adhoc"),
        tenant_id: call.scope.tenant_id.clone(),
        brand_id: call.scope.brand_id.clone(),
        url,
        source_type: st,
        want_screenshot: false,
    };
    let c = state.extractors.extract(&req).await?;
    let mut v = serde_json::to_value(&c).map_err(ApiError::internal)?;
    v["url"] = json!(req.url.as_str());
    v["content_hash"] = json!(content_hash(&c.text));
    ToolOutput::new(&v)
}

#[derive(Deserialize)]
struct ReadInput {
    zernio_profile_id: String,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
}

pub async fn read_zernio(state: &AppState, call: &Call<'_>, input: Value, resource: ReadResource) -> ApiResult<ToolOutput> {
    let input: ReadInput = parse_input(input)?;
    let channels: Vec<String> = match &input.platform {
        Some(p) => resolve_channels(call.ctx, std::slice::from_ref(p))?,
        None => call.ctx.zernio.as_ref().map(|z| z.connected_accounts.iter().map(|a| crate::channels::canonical(&a.platform)).collect()).unwrap_or_default(),
    };
    if channels.is_empty() {
        return Err(ApiError::new(ErrorCode::ZernioConnectionMissing, "no connected accounts are available for this brand")
            .with_details(json!({ "brand_id": call.scope.brand_id })));
    }
    // Read access is enough here — and nothing in this path can write.
    validate_connection(call.ctx, &input.zernio_profile_id, &channels, Access::Read)?;
    let zernio = state.zernio.as_ref().ok_or_else(|| ApiError::internal("Zernio integration is not configured (ZERNIO_API_KEY)"))?;
    state.rate_limit_provider("zernio")?;

    let limit = input.limit.unwrap_or(25).clamp(1, 100).to_string();
    let mut q: Vec<(&str, &str)> = vec![("limit", limit.as_str())];
    let platform = input.platform.as_deref().map(crate::zernio::zernio_platform);
    if let Some(p) = platform.as_deref() {
        q.push(("platform", p));
    }
    if let Some(c) = input.cursor.as_deref() {
        q.push(("cursor", c));
    }
    let data = zernio.read(resource, &input.zernio_profile_id, &q).await?;
    ToolOutput::new(&json!({ "zernio_profile_id": input.zernio_profile_id, "platforms": channels, "data": data }))
}
