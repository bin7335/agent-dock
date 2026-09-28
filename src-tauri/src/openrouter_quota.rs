use serde_json::{json, Value};

pub fn is_openrouter(url: &str) -> bool {
    matches!(url.trim_end_matches('/'), "https://openrouter.ai/api/v1" | "https://openrouter.ai")
}

pub async fn fetch(name: &str) -> Result<Value, String> {
    let raw = std::fs::read_to_string(crate::ocx_home()?.join("config.json"))
        .map_err(|_| "OpenRouter 설정을 읽을 수 없습니다".to_string())?;
    let config: Value = serde_json::from_str(&raw).map_err(|_| "Opencodex 설정 형식 오류")?;
    let provider = &config["providers"][name];
    if !provider["baseUrl"].as_str().map(is_openrouter).unwrap_or(false) {
        return Err("OpenRouter 공식 주소 설정이 필요합니다".into());
    }
    let stored = provider["apiKey"].as_str().unwrap_or("").trim();
    let key = if let Some(env) = stored.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        std::env::var(env).map_err(|_| "OpenRouter 키 환경변수를 읽을 수 없습니다")?
    } else { stored.to_owned() };
    if key.is_empty() || key.contains(['\r', '\n']) { return Err("OpenRouter API 키를 확인하세요".into()); }
    // Fixed HTTPS destination; secrets are sent on stdin, never in process arguments.
    let response = crate::bearer_request("https://openrouter.ai/api/v1/key", key, "10", None)
        .await.map_err(|_| "OpenRouter 한도 서버 연결 실패")?;
    let (body, status) = response.rsplit_once('\n').ok_or("OpenRouter 응답 형식 오류")?;
    if status.trim() != "200" { return Err(format!("OpenRouter 한도 조회 실패 (HTTP {})", status.trim())); }
    let value: Value = serde_json::from_str(body).map_err(|_| "OpenRouter 응답 형식 오류")?;
    project(name, &value, crate::now() * 1000)
}

fn project(name: &str, response: &Value, now_ms: i64) -> Result<Value, String> {
    let daily = &response["data"]["free_model_daily_requests"];
    let limit = daily["limit"].as_u64().filter(|n| *n > 0).ok_or("무료 요청 잔여 횟수를 제공하지 않는 응답입니다")?;
    let left = daily["remaining"].as_u64().filter(|n| *n <= limit).ok_or("무료 요청 잔여 횟수 형식 오류")?;
    let used = daily["used"].as_u64().ok_or("무료 요청 사용 횟수 형식 오류")?;
    Ok(json!({
        "provider": name, "label": "OpenRouter", "source": "openrouter:key", "updatedAt": now_ms,
        "quota": { "customWindows": [{
            "label": "무료 요청 · 오늘 (UTC)", "percent": 100.0 * (limit - left) as f64 / limit as f64,
            "remainingRequests": left, "requestLimit": limit, "usedRequests": used,
            "resetAt": (now_ms / 86_400_000 + 1) * 86_400_000
        }] }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn free_counts_and_missing_fields() {
        let value = project("router", &json!({"data":{"free_model_daily_requests":{"limit":50,"remaining":32,"used":18}}}), 1000).unwrap();
        assert_eq!(value["quota"]["customWindows"][0]["percent"], 36.0);
        assert_eq!(value["quota"]["customWindows"][0]["resetAt"], 86400000);
        assert!(project("router", &json!({"data":{"usage_daily":0}}), 0).is_err());
        assert!(project("router", &json!({"data":{"free_model_daily_requests":{"limit":50,"remaining":51,"used":0}}}), 0).is_err());
        assert!(!is_openrouter("https://openrouter.ai.example.com/api/v1"));
    }
}
