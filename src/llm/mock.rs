//! Deterministic offline provider for development and tests. Produces schema-valid output
//! derived from the request payload so the whole pipeline can run without network access.

use super::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct MockProvider;

fn sentences(text: &str, n: usize) -> Vec<String> {
    // Split at sentence ends (". ", "! ", "? ", newlines), not at every '.', so "1.78.0" and "e.g." survive.
    let mut out = vec![];
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let sentence_end = matches!(c, '.' | '!' | '?') && chars.get(i + 1).is_none_or(|nx| nx.is_whitespace());
        if c == '\n' || sentence_end {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out.into_iter().map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|s| s.len() > 20).take(n).collect()
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn name(&self) -> &'static str {
        "mock"
    }
    fn model(&self) -> &str {
        "mock-1"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let u = &req.user;
        let out: Value = match req.purpose {
            "classify_relevance" => {
                let text = u["text"].as_str().unwrap_or_default();
                json!({ "relevant": text.len() > 50, "score": if text.len() > 50 { 0.9 } else { 0.1 }, "reason": "mock" })
            }
            "report" => {
                let all: String =
                    u["sources"].as_array().map(|a| a.iter().filter_map(|s| s["text"].as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                let pts = sentences(&all, 5);
                let channels = u["requested_channels"].clone();
                json!({
                    "key_points": if pts.is_empty() { vec!["Source content summarised".to_string()] } else { pts.clone() },
                    "audience_takeaways": pts.iter().take(2).map(|p| format!("Readers learn: {}", clip(p, 80))).collect::<Vec<_>>(),
                    "recommended_angle": pts.first().map(|p| clip(p, 120)).unwrap_or_else(|| "Share the key insight".into()),
                    "content_pillars": ["education", "authority"],
                    "recommended_channels": if channels.as_array().is_some_and(|c| !c.is_empty()) { channels } else { json!(["linkedin", "x"]) },
                    "risks": [],
                    "confidence": 0.8
                })
            }
            "social_draft" | "repurpose" => {
                let angle = u["report"]["recommended_angle"].as_str().or_else(|| u["source_text"].as_str()).unwrap_or("Check this out");
                let points: Vec<String> =
                    u["report"]["key_points"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                let targets = u["targets"].as_array().cloned().unwrap_or_default();
                let hashtags = u["include_hashtags"].as_bool().unwrap_or(true);
                let cta = u["include_cta"].as_bool().unwrap_or(true);
                let variants: Vec<Value> = targets
                    .iter()
                    .map(|t| {
                        let ch = t["channel"].as_str().unwrap_or("linkedin");
                        let fmt = t["format"].as_str().unwrap_or("text");
                        let limit = t["max_chars"].as_u64().unwrap_or(280) as usize;
                        let mut cta_txt = if cta { "Learn more at the link." } else { "" }.to_string();
                        let tags: Vec<String> = if hashtags { vec!["#marketing".into(), "#growth".into()] } else { vec![] };
                        let reserve = cta_txt.len() + tags.join(" ").len() + 6;
                        let caption = clip(angle, limit.saturating_sub(reserve).max(20));
                        if limit < 120 { cta_txt.clear(); }
                        let parts: Vec<String> = if matches!(fmt, "thread" | "carousel") { points.iter().take(4).map(|p| clip(p, limit.min(240))).collect() } else { vec![] };
                        json!({ "channel": ch, "format": fmt, "caption": caption, "hashtags": tags, "cta": if cta_txt.is_empty() { Value::Null } else { json!(cta_txt) }, "parts": parts, "alt_text": clip(angle, 100) })
                    })
                    .collect();
                json!({ "variants": variants })
            }
            "brand_rules_check" => json!({ "violations": [] }),
            "image_prompt" => {
                let c = u["caption"].as_str().unwrap_or("brand image");
                json!({ "prompt": format!("Clean editorial illustration: {}", clip(c, 120)), "alt_text": clip(c, 100) })
            }
            other => return Err(LlmError::Permanent(format!("mock has no handler for '{other}'"))),
        };
        let text = out.to_string();
        Ok(LlmResponse {
            input_tokens: (req.user.to_string().len() / 4) as u64,
            output_tokens: (text.len() / 4) as u64,
            text,
            provider: "mock".into(),
            model: "mock-1".into(),
            cost_usd: 0.0,
            latency_ms: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::sentences;

    #[test]
    fn keeps_version_numbers_and_abbreviations_together() {
        let s = sentences("The Rust team announces version 1.78.0 today for everyone. Second sentence is also long enough to keep! Short.", 5);
        assert_eq!(s[0], "The Rust team announces version 1.78.0 today for everyone");
        assert_eq!(s.len(), 2, "{s:?}");
    }
}
