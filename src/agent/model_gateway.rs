//! 文件职责：封装 OpenAI 兼容模型的最小能力探测协议。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：按端点支持能力启用最高推理强度，并验证认证、Chat Completions 路径和结构化工具调用响应。

use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use url::Url;

use crate::config::{AiConfig, AiModelProfile};

/// 能力探测响应体上限，防止错误端点返回超大 HTML 或代理页面。
const CAPABILITY_RESPONSE_MAX_BYTES: usize = 256 * 1024;
/// 探测工具的固定名称，服务端必须通过结构化 tool call 返回。
const CAPABILITY_TOOL_NAME: &str = "argus_capability_probe";

/// 验证模型端点是否支持首期 Agent 所需的 Chat Completions 工具调用子集。
///
/// 探测请求只包含固定占位文本和一个无副作用工具 Schema，不携带日志、用户问题、日志说明或本地路径。
pub(crate) async fn probe_model_capabilities(
    config: &AiConfig,
    model: &AiModelProfile,
    api_key: &SecretString,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            config.request_timeout_seconds.min(60),
        ))
        .build()
        .map_err(|_| "创建 AI 能力探测客户端失败".to_string())?;
    let endpoint = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let mut response = client
        .post(endpoint)
        .bearer_auth(api_key.expose_secret())
        .json(&capability_probe_payload(model))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "AI 能力探测超时，请检查服务地址和网络".to_string()
            } else {
                "无法连接 AI 服务，请检查服务地址、网络和证书".to_string()
            }
        })?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > CAPABILITY_RESPONSE_MAX_BYTES as u64)
    {
        return Err("AI 能力探测响应超过 256 KiB 上限".to_string());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "读取 AI 能力探测响应失败".to_string())?
    {
        if body.len().saturating_add(chunk.len()) > CAPABILITY_RESPONSE_MAX_BYTES {
            return Err("AI 能力探测响应超过 256 KiB 上限".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err("AI 服务认证失败，请检查 API Key".to_string());
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("AI 服务未找到 Chat Completions 端点或模型".to_string());
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err("AI 服务当前限流，请稍后重试能力探测".to_string());
    }
    if !status.is_success() {
        let detail = service_error_detail(&body, api_key);
        if status == reqwest::StatusCode::BAD_REQUEST
            && official_deepseek_model_hint(model).is_some()
        {
            return Err(format!(
                "DeepSeek V4 模型 ID 应填写 deepseek-v4-pro 或 deepseek-v4-flash{}",
                detail
                    .map(|message| format!("；服务返回：{message}"))
                    .unwrap_or_default()
            ));
        }
        return Err(match detail {
            Some(message) => format!("AI 能力探测失败，HTTP {}：{message}", status.as_u16()),
            None => format!("AI 能力探测失败，服务返回 HTTP {}", status.as_u16()),
        });
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| "AI 服务响应不是兼容的 Chat Completions JSON".to_string())?;
    let tool_calls = value
        .pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "当前模型未返回结构化工具调用，请更换支持 tool calling 的模型".to_string()
        })?;
    let arguments = tool_calls
        .iter()
        .find(|tool_call| {
            tool_call.pointer("/function/name").and_then(Value::as_str)
                == Some(CAPABILITY_TOOL_NAME)
        })
        .and_then(|tool_call| tool_call.pointer("/function/arguments"))
        .and_then(Value::as_str)
        .ok_or_else(|| "当前模型的工具调用格式不兼容".to_string())?;
    let arguments: Value = serde_json::from_str(arguments)
        .map_err(|_| "当前模型返回的工具参数不是合法 JSON".to_string())?;
    if arguments.get("value").and_then(Value::as_str) != Some("ok") {
        return Err("当前模型未按 Schema 返回能力探测结果".to_string());
    }
    Ok("连接成功，模型支持结构化工具调用".to_string())
}

/// 构造兼容主流 Chat Completions 服务的最小工具探测请求。
///
/// DeepSeek V4 thinking 模式支持工具调用，但不接受强制命名的 `tool_choice`，因此探测依靠
/// 固定提示词触发工具。官方 DeepSeek 端点显式开启 thinking 和最高推理强度；其它兼容
/// 端点只发送标准 Chat Completions 字段，避免不支持私有推理参数的服务返回 400。
fn capability_probe_payload(model: &AiModelProfile) -> Value {
    let mut payload = serde_json::json!({
        "model": model.model,
        "messages": [{
            "role": "user",
            "content": "You must call argus_capability_probe exactly once with value ok. Do not answer with text."
        }],
        "tools": [{
            "type": "function",
            "function": {
                "name": CAPABILITY_TOOL_NAME,
                "description": "Return the fixed capability probe value.",
                "parameters": {
                    "type": "object",
                    "properties": { "value": { "type": "string", "enum": ["ok"] } },
                    "required": ["value"],
                    "additionalProperties": false
                }
            }
        }],
        "max_tokens": 512
    });
    if is_official_deepseek_endpoint(&model.base_url) {
        // 只有协议能力已经明确的官方端点才接收推理扩展字段；未知兼容端点保持标准请求。
        payload["thinking"] = serde_json::json!({ "type": "enabled" });
        payload["reasoning_effort"] = serde_json::json!("max");
    }
    payload
}

/// 判断端点是否为 DeepSeek 官方 OpenAI 兼容服务，避免向其它服务发送私有参数。
pub(crate) fn is_official_deepseek_endpoint(base_url: &str) -> bool {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "api.deepseek.com")
}

/// 当官方 DeepSeek 端点使用了非公开模型 ID 时返回提示标记。
fn official_deepseek_model_hint(model: &AiModelProfile) -> Option<()> {
    if !is_official_deepseek_endpoint(&model.base_url) {
        return None;
    }
    (!matches!(
        model.model.as_str(),
        "deepseek-v4-pro" | "deepseek-v4-flash" | "deepseek-chat" | "deepseek-reasoner"
    ))
    .then_some(())
}

/// 从兼容服务的标准错误信封提取有界信息，并删除可能被服务端回显的当前 API Key。
fn service_error_detail(body: &[u8], api_key: &SecretString) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let message = value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)?;
    let secret = api_key.expose_secret();
    let redacted = if secret.is_empty() {
        message.to_string()
    } else {
        message.replace(secret, "[REDACTED]")
    };
    let bounded = redacted
        .chars()
        .map(|character| {
            if character.is_control() || character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .take(240)
        .collect::<String>();
    let sanitized = bounded.split_whitespace().collect::<Vec<_>>().join(" ");
    (!sanitized.trim().is_empty()).then(|| sanitized.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// 构造指向 Wiremock 的最小能力探测配置。
    fn test_config(base_url: String) -> (AiConfig, AiModelProfile) {
        let mut model = AiModelProfile {
            profile_id: uuid::Uuid::new_v4().to_string(),
            enabled: true,
            name: "测试模型".to_string(),
            base_url,
            model: "test-tool-model".to_string(),
            context_window_tokens: crate::config::ai_config::DEFAULT_AI_CONTEXT_WINDOW_TOKENS,
        };
        model.normalize();
        (AiConfig::default(), model)
    }

    /// 验证通用探测请求只使用标准兼容字段且不强制 `tool_choice`。
    #[test]
    fn capability_payload_omits_forced_tool_choice() {
        let (_, model) = test_config("https://example.com/v1".to_string());
        let payload = capability_probe_payload(&model);
        assert!(payload.get("tool_choice").is_none());
        assert!(payload.get("thinking").is_none());
        assert!(payload.get("reasoning_effort").is_none());
        assert!(payload.get("temperature").is_none());
    }

    /// 验证 DeepSeek 官方端点在 thinking 模式探测工具能力，且不发送强制 `tool_choice`。
    #[test]
    fn capability_payload_uses_deepseek_thinking_mode() {
        let (_, mut model) = test_config("https://api.deepseek.com/v1".to_string());
        model.model = "deepseek-v4-pro".to_string();
        let payload = capability_probe_payload(&model);
        assert_eq!(
            payload.pointer("/thinking/type").and_then(Value::as_str),
            Some("enabled")
        );
        assert_eq!(
            payload.get("reasoning_effort").and_then(Value::as_str),
            Some("max")
        );
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("tool_choice").is_none());
    }

    /// 验证服务端错误详情有长度上限，并且不会回显当前测试密钥。
    #[test]
    fn service_error_detail_redacts_api_key() {
        let api_key = SecretString::from("do-not-leak");
        let detail = service_error_detail(
            br#"{"error":{"message":"invalid token do-not-leak"}}"#,
            &api_key,
        )
        .expect("应提取标准错误消息");
        assert_eq!(detail, "invalid token [REDACTED]");
    }

    /// 验证兼容端点返回固定工具调用时能力探测成功。
    #[test]
    fn capability_probe_accepts_structured_tool_call() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应创建测试运行时");
        runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .and(header("authorization", "Bearer test-key"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "tool_calls": [{
                                "id": "call-1",
                                "type": "function",
                                "function": {
                                    "name": CAPABILITY_TOOL_NAME,
                                    "arguments": "{\"value\":\"ok\"}"
                                }
                            }]
                        }
                    }]
                })))
                .mount(&server)
                .await;
            let (config, model) = test_config(format!("{}/v1", server.uri()));
            let result =
                probe_model_capabilities(&config, &model, &SecretString::from("test-key")).await;
            assert!(result.is_ok());
        });
    }

    /// 验证认证错误被归一化且不会回显测试密钥。
    #[test]
    fn capability_probe_redacts_authentication_failure() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应创建测试运行时");
        runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(ResponseTemplate::new(401))
                .mount(&server)
                .await;
            let (config, model) = test_config(format!("{}/v1", server.uri()));
            let error =
                probe_model_capabilities(&config, &model, &SecretString::from("do-not-leak"))
                    .await
                    .expect_err("401 应失败");
            assert!(error.contains("认证失败"));
            assert!(!error.contains("do-not-leak"));
        });
    }

    /// 验证 HTTP 400 会展示经过脱敏和裁剪的标准错误详情，便于定位兼容字段或模型 ID。
    #[test]
    fn capability_probe_surfaces_safe_bad_request_detail() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应创建测试运行时");
        runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": { "message": "unsupported request field" }
                })))
                .mount(&server)
                .await;
            let (config, model) = test_config(format!("{}/v1", server.uri()));
            let error = probe_model_capabilities(&config, &model, &SecretString::from("test-key"))
                .await
                .expect_err("400 应失败");
            assert!(error.contains("HTTP 400"));
            assert!(error.contains("unsupported request field"));
        });
    }

    /// 验证服务端声明超大响应时在读取正文前拒绝，避免错误代理页面耗尽内存。
    #[test]
    fn capability_probe_rejects_oversized_response() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应创建测试运行时");
        runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                    b'x';
                    CAPABILITY_RESPONSE_MAX_BYTES
                        + 1
                ]))
                .mount(&server)
                .await;
            let (config, model) = test_config(format!("{}/v1", server.uri()));
            let error = probe_model_capabilities(&config, &model, &SecretString::from("test-key"))
                .await
                .expect_err("超大响应必须失败");
            assert!(error.contains("超过 256 KiB"));
        });
    }
}
