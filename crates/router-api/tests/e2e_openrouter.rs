//! E2E: OpenAI-Request geht rein, mocked OpenRouter antwortet.

use std::sync::Arc;
use std::str::FromStr;

use router_api::state::AppState;
use router_config::Config;
use router_providers::{LatencyTracker, RegistryHandle};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn start_app(mock_url: String) -> (tokio::task::JoinHandle<()>, String) {
    let toml = format!(
        r#"
        [server]
        bind = "127.0.0.1:0"

        [backends.openrouter]
        enabled = true
        kind = "openrouter"
        base_url = "{mock_url}"
        auth = {{ type = "api_key", env = "TEST_OR_KEY" }}

        [[registry.overrides]]
        backend = "omlx"
        id_prefix = ""
        input_modalities = []
        caps = []

        [registry.privacy]
        local = []
        zdr = ["anthropic"]

        [profiles.default]
        weights = {{ cost = 0.5, latency = 0.1, context = 0.1, preference = 0.3 }}
        provider_require_parameters = true

        [profiles.cheap]
        weights = {{ cost = 0.95, latency = 0.0, context = 0.05, preference = 0.0 }}
        max_price_out_per_mtok = 10.0
        provider_sort = "price"
        provider_require_parameters = true
        "#
    );
    std::env::set_var("TEST_OR_KEY", "test-key");
    let cfg: Config = Config::from_str(&toml).unwrap();
    let tracker = LatencyTracker::new();
    let registry = RegistryHandle::new(&cfg, tracker.clone());
    let state = AppState {
        config: Arc::new(cfg),
        config_path: Arc::new(std::path::PathBuf::from("config/router.toml")),
        registry: Arc::new(registry),
        tracker,
        history: router_api::history::TransactionHistory::new(),
        store: router_api::store::Store::default(),
        rotator: Arc::new(router_api::rotate::Rotator::from_env()),
        logs: router_api::logbuf::LogBuffer::new(100),
    };
    let app = router_api::routes::build(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let jh = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (jh, format!("http://{addr}"))
}

fn mock_models_response() -> serde_json::Value {
    json!({
        "data": [
            {
                "id": "anthropic/claude-sonnet-4-6",
                "context_length": 200000,
                "architecture": {
                    "input_modalities": ["text", "image"],
                    "output_modalities": ["text"]
                },
                "pricing": { "prompt": "0.000003", "completion": "0.000015" },
                "supported_parameters": ["temperature","tools","tool_choice","response_format","structured_outputs","reasoning"],
                "top_provider": { "is_moderated": false, "max_completion_tokens": 8192, "context_length": 200000 }
            },
            {
                "id": "cheap/tiny",
                "context_length": 32000,
                "architecture": {
                    "input_modalities": ["text"],
                    "output_modalities": ["text"]
                },
                "pricing": { "prompt": "0.0000002", "completion": "0.0000006" },
                "supported_parameters": ["temperature"],
                "top_provider": { "is_moderated": false, "max_completion_tokens": 4096, "context_length": 32000 }
            }
        ]
    })
}

#[tokio::test]
async fn cheap_profile_picks_cheap_model_and_sets_provider_flags() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_models_response()))
        .mount(&mock)
        .await;

    // Wir matchen nur, dass Model und provider.sort=price gesetzt werden.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({
            "model": "cheap/tiny",
            "provider": { "sort": "price", "require_parameters": true }
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n\
                     data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                     data: [DONE]\n\n",
                ),
        )
        .mount(&mock)
        .await;

    let (_jh, base) = start_app(mock.uri()).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("x-route-profile", "cheap")
        .json(&json!({
            "model": "auto",
            "stream": false,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["model"], "cheap/tiny");
    assert_eq!(body["choices"][0]["message"]["content"], "hi");
}

#[tokio::test]
async fn vision_request_picks_vision_capable_model() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_models_response()))
        .mount(&mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({ "model": "anthropic/claude-sonnet-4-6" })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"seen\"}}]}\n\n\
                     data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                     data: [DONE]\n\n",
                ),
        )
        .mount(&mock)
        .await;

    let (_jh, base) = start_app(mock.uri()).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "auto",
            "stream": false,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "what is this?" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,xxxx" } }
                ]
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["model"], "anthropic/claude-sonnet-4-6");
    assert_eq!(body["choices"][0]["message"]["content"], "seen");
}

#[tokio::test]
async fn no_candidate_returns_503() {
    let mock = MockServer::start().await;
    // Nur ein ganz kleines Modell -> Prompt passt nicht rein
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": "tiny/model",
                "context_length": 256,
                "architecture": { "input_modalities": ["text"] },
                "pricing": { "prompt": "0", "completion": "0" },
                "supported_parameters": [],
                "top_provider": { "is_moderated": false, "max_completion_tokens": 64, "context_length": 256 }
            }]
        })))
        .mount(&mock)
        .await;

    let (_jh, base) = start_app(mock.uri()).await;

    let client = reqwest::Client::new();
    let big = "x".repeat(20_000);
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "auto",
            "stream": false,
            "max_tokens": 500,
            "messages": [{ "role": "user", "content": big }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}
