use super::*;
use crate::local_router::tests::{connect_router_websocket, router_config};

const MODEL: &str = "gemini-3.8-flash-high";

#[test]
fn gemini_legacy_baseline_matches_verified_cli_01533_fingerprint() {
    let normalized = LEGACY_BASE_INSTRUCTIONS.replace("\r\n", "\n");
    assert_eq!(
        crate::fs_util::sha256_hex(normalized.trim().as_bytes()),
        "822b92294a217f46c8f9794589faee3b5ba6fbd2d61ac6c90168042829b8cf03"
    );
}

fn gemini_config(base_url: String, protocol: &str) -> (CodeyConfig, String) {
    let (mut config, provider, _) = router_config(base_url);
    config.profiles[0].upstream_protocol = protocol.into();
    config.profiles[0].supports_websockets = false;
    config.profiles[0].normalize();
    config
        .selected_models_by_provider
        .insert(provider.clone(), vec![MODEL.into()]);
    (config, provider)
}

fn payload(model: &str) -> Value {
    json!({
        "model":model, "instructions":LEGACY_BASE_INSTRUCTIONS,
        "input":[
            {"role":"developer","content":"ROLE_SENTINEL: read-only; preserve Codex and GPT-5 words"},
            {"role":"user","content":"TASK_SENTINEL: keep strings and docstrings unchanged"}
        ],
        "tools":[{"type":"function","name":"lookup","description":"TOOL_SENTINEL Codex", "parameters":{"type":"object","properties":{}}}],
        "reasoning":{"effort":"high"},"stream":false
    })
}

#[test]
fn gemini_exact_templates_and_scope() {
    for name in ["gemini", "GEMINI-3.8", "route/vendor/gemini-3.8-flash-high"] {
        let mut body = payload(name);
        let before = body.clone();
        assert_eq!(
            adapt_gemini_base_instructions(&mut body, name, false).unwrap(),
            Some(true)
        );
        assert_eq!(
            body["instructions"],
            GEMINI_BASE_INSTRUCTIONS.replace("\r\n", "\n").trim()
        );
        for key in ["input", "tools", "reasoning", "model"] {
            assert_eq!(body[key], before[key]);
        }
        let adapted = body.clone();
        assert_eq!(
            adapt_gemini_base_instructions(&mut body, name, false).unwrap(),
            Some(false)
        );
        assert_eq!(body, adapted);
    }
    for (name, official) in [
        ("gpt-5.6-terra", false),
        ("gemini_alias", false),
        ("gemini", true),
        ("gemini/opaque", false),
    ] {
        let mut body = json!({"instructions":"custom"});
        let before = body.clone();
        assert_eq!(
            adapt_gemini_base_instructions(&mut body, name, official).unwrap(),
            None
        );
        assert_eq!(body, before);
    }
}

#[test]
fn gemini_line_endings_and_whitespace_only_are_normalized() {
    for template in [LEGACY_BASE_INSTRUCTIONS, GEMINI_BASE_INSTRUCTIONS] {
        for text in [
            template.replace("\r\n", "\n"),
            template.replace("\r\n", "\n").replace('\n', "\r\n"),
        ] {
            let mut body = json!({"instructions":format!(" \n{text}\r\n\t")});
            assert!(adapt_gemini_base_instructions(&mut body, MODEL, false).is_ok());
        }
    }
}

#[test]
fn gemini_unknown_or_input_only_instructions_are_rejected_without_mutation() {
    for value in [
        Value::Null,
        json!(42),
        json!([]),
        json!(""),
        json!("You are Codex, a custom agent"),
        json!(format!("{LEGACY_BASE_INSTRUCTIONS}\nCUSTOM")),
        json!(format!("CUSTOM\n{LEGACY_BASE_INSTRUCTIONS}")),
    ] {
        let mut body = json!({"instructions":value,"input":[{"role":"developer","content":LEGACY_BASE_INSTRUCTIONS}]});
        let before = body.clone();
        assert!(adapt_gemini_base_instructions(&mut body, MODEL, false).is_err());
        assert_eq!(body, before);
    }
    assert!(adapt_gemini_base_instructions(&mut json!({"input":"hello"}), MODEL, false).is_err());
}

#[tokio::test]
async fn gemini_native_raw_rewrite_and_offload_preserve_unrelated_slices() {
    for size in [8, REQUEST_JSON_OFFLOAD_BYTES + 8] {
        let input = format!(
            r#"[ {{ "role" : "user", "content" : "{}" }} ]"#,
            "x".repeat(size)
        );
        let raw = format!(
            r#"{{"model":"gemini","instructions":{},"input":{input},"tools" : [ ],"custom" : 1.00}}"#,
            serde_json::to_string(LEGACY_BASE_INSTRUCTIONS).unwrap()
        );
        let mut body: Value = serde_json::from_str(&raw).unwrap();
        adapt_gemini_base_instructions(&mut body, MODEL, false).unwrap();
        let (encoded, _) =
            rewrite_native_responses_encoded_body_offloaded(raw.into_bytes(), &body, None)
                .await
                .unwrap();
        let encoded = String::from_utf8(encoded).unwrap();
        assert!(encoded.contains(&input));
        assert!(encoded.contains("[ ]"));
        assert!(encoded.contains("1.00"));
        assert_eq!(
            serde_json::from_str::<Value>(&encoded).unwrap()["instructions"],
            body["instructions"]
        );
    }
    let raw = br#"{"model":"gpt","instructions":"keep \u0043odex","input":[]}"#;
    let value: Value = serde_json::from_slice(raw).unwrap();
    assert_eq!(
        rewrite_native_responses_encoded_body(raw, &value).unwrap(),
        raw
    );
}

async fn send_test_request(
    endpoint: &RuntimeRouterEndpoint,
    body: &Value,
    websocket: bool,
) -> String {
    if websocket {
        let mut socket = connect_router_websocket(endpoint).await;
        let mut body = body.clone();
        body["type"] = json!("response.create");
        body["stream"] = json!(true);
        socket
            .send(WebSocketMessage::Text(body.to_string().into()))
            .await
            .unwrap();
        let mut result = String::new();
        loop {
            let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let WebSocketMessage::Text(text) = message {
                result.push_str(&text);
                let event: Value = serde_json::from_str(&text).unwrap();
                if responses_event_is_terminal(&event) {
                    break;
                }
            }
        }
        let _ = socket.close(None).await;
        result
    } else {
        reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn gemini_routes_adapt_before_all_bridges_for_http_and_websocket() {
    for protocol in [
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        for (websocket, large) in [(false, false), (true, false), (false, true)] {
            let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = upstream.local_addr().unwrap();
            let capture = tokio::spawn(async move {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                // A real upstream error also verifies it remains transparent after adaptation.
                write_json_response(
                    &mut stream,
                    429,
                    &json!({"error":{"message":"probe rate limit","code":"probe_429"}}),
                )
                .await
                .unwrap();
                body
            });
            let (config, provider) = gemini_config(format!("http://{address}/v1"), protocol);
            let router = LocalRouter::start(&config).await.unwrap();
            let alias = model_id::model_alias(&provider, MODEL);
            let mut body = payload(
                if protocol == UPSTREAM_PROTOCOL_OPENAI_RESPONSES && !websocket {
                    MODEL
                } else {
                    &alias
                },
            );
            if large {
                body["input"][1]["content"] = json!("x".repeat(REQUEST_JSON_OFFLOAD_BYTES + 8));
            }
            let response = send_test_request(&router.endpoint(), &body, websocket).await;
            assert!(
                response.contains("probe rate limit"),
                "{protocol}: {response}"
            );
            let captured = tokio::time::timeout(Duration::from_secs(5), capture)
                .await
                .unwrap()
                .unwrap();
            let mut expected = body;
            expected["model"] = json!(MODEL);
            expected["instructions"] = json!(GEMINI_BASE_INSTRUCTIONS.replace("\r\n", "\n").trim());
            if websocket {
                expected["stream"] = json!(true);
            }
            let bridge = ProtocolBridge::from_upstream_protocol(UpstreamProtocol::from_profile(
                false, protocol,
            ));
            let expected = bridge
                .convert_responses_body(&expected)
                .unwrap()
                .map_or(expected, |c| c.body);
            for key in [
                "instructions",
                "input",
                "tools",
                "messages",
                "system",
                "reasoning",
                "reasoning_effort",
                "thinking",
            ] {
                assert!(
                    captured.get(key) == expected.get(key),
                    "{protocol}, ws={websocket}, large={large}, key={key}"
                );
            }
            router.stop().await.unwrap();
        }
    }
}

#[tokio::test]
async fn gemini_unknown_instructions_never_connect_to_upstream() {
    for websocket in [false, true] {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (config, provider) = gemini_config(
            format!("http://{}/v1", upstream.local_addr().unwrap()),
            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        );
        let router = LocalRouter::start(&config).await.unwrap();
        let mut body = payload(&model_id::model_alias(&provider, MODEL));
        body["instructions"] = json!("custom instructions");
        let response = if websocket {
            send_test_request(&router.endpoint(), &body, true).await
        } else {
            let endpoint = router.endpoint();
            let response = reqwest::Client::new()
                .post(format!("{}/responses", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .json(&body)
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
            response.text().await.unwrap()
        };
        assert!(response.contains(GEMINI_INSTRUCTIONS_ERROR), "{response}");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), upstream.accept())
                .await
                .is_err()
        );
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn gemini_compaction_keeps_existing_instructions_contract() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let capture = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        assert_eq!(request.path, "/v1/responses/compact");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        write_json_response(
            &mut stream,
            429,
            &json!({"error":{"message":"compact probe"}}),
        )
        .await
        .unwrap();
        body
    });
    let (mut config, provider) = gemini_config(
        format!("http://{address}/v1"),
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
    );
    config.profiles[0].supports_remote_compaction = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let response = reqwest::Client::new().post(format!("{}/responses/compact", endpoint.base_url))
        .bearer_auth(&endpoint.token).json(&json!({"model":model_id::model_alias(&provider,MODEL), "input":[], "instructions":"custom compaction instructions"}))
        .timeout(Duration::from_secs(10)).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert!(response.text().await.unwrap().contains("compact probe"));
    let body = tokio::time::timeout(Duration::from_secs(5), capture)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body["instructions"], "custom compaction instructions");
    router.stop().await.unwrap();
}

#[tokio::test]
async fn gemini_reasoning_retry_retains_adapted_instructions() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let capture = tokio::spawn(async move {
        let mut attempts = Vec::new();
        for index in 0..2 {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            attempts.push(serde_json::from_slice::<Value>(&request.body).unwrap());
            if index == 0 {
                write_json_response(&mut stream,400,&json!({"error":{"message":"The `reasoning_text` in the thinking mode must be passed back to the API.","type":"invalid_request_error","code":"invalid_request_error"}})).await.unwrap();
            } else {
                write_json_response(&mut stream,200,&json!({"id":"resp-retry","object":"response","status":"completed","model":MODEL,"output":[]})).await.unwrap();
            }
        }
        attempts
    });
    let (config, provider) = gemini_config(
        format!("http://{address}/v1"),
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
    );
    let router = LocalRouter::start(&config).await.unwrap();
    let mut body = payload(&model_id::model_alias(&provider, MODEL));
    body["input"].as_array_mut().unwrap().insert(
        0,
        json!({"type":"reasoning","id":"rs_probe","summary":[],"encrypted_content":"opaque-state"}),
    );
    let response = send_test_request(&router.endpoint(), &body, false).await;
    assert!(response.contains("resp-retry"), "{response}");
    let attempts = tokio::time::timeout(Duration::from_secs(5), capture)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["instructions"], attempts[1]["instructions"]);
    assert_eq!(
        attempts[0]["instructions"],
        GEMINI_BASE_INSTRUCTIONS.replace("\r\n", "\n").trim()
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn gemini_upstream_websocket_fallback_retains_adaptation() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let capture = tokio::spawn(async move {
        let (mut handshake, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut handshake).await.unwrap();
        assert_eq!(request.method, "GET");
        handshake
            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        drop(handshake);
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        assert_eq!(request.method, "POST");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        write_json_response(&mut stream,200,&json!({"id":"resp-fallback","object":"response","status":"completed","model":MODEL,"output":[]})).await.unwrap();
        body
    });
    let (mut config, provider) = gemini_config(
        format!("http://{address}/v1"),
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
    );
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let response = send_test_request(
        &router.endpoint(),
        &payload(&model_id::model_alias(&provider, MODEL)),
        true,
    )
    .await;
    assert!(response.contains("response.completed"), "{response}");
    let body = tokio::time::timeout(Duration::from_secs(5), capture)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        body["instructions"],
        GEMINI_BASE_INSTRUCTIONS.replace("\r\n", "\n").trim()
    );
    router.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit CODEY_GEMINI_PROBE_CLI and CODEY_GEMINI_PROBE_CATALOG; loopback model traffic only"]
async fn gemini_native_cli_child_through_actual_router() {
    let cli = std::env::var_os("CODEY_GEMINI_PROBE_CLI").expect("set native CLI path");
    let catalog =
        std::env::var_os("CODEY_GEMINI_PROBE_CATALOG").expect("set captured catalog path");
    let root = tempfile::Builder::new()
        .prefix("gemini-router-probe-")
        .tempdir()
        .unwrap()
        .keep();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gemini-router-probe.mjs");
    let mut command = tokio::process::Command::new("node");
    command
        .arg(script)
        .env("CODEY_GEMINI_PROBE_ROOT", &root)
        .env("CODEY_GEMINI_PROBE_CLI", cli)
        .env("CODEY_GEMINI_PROBE_CATALOG", catalog)
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let upstream_path = root.join("upstream.json");
    while !upstream_path.exists() {
        assert!(
            Instant::now() < deadline,
            "probe handshake timed out: {}",
            root.display()
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "probe exited before handshake"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let upstream: Value = serde_json::from_slice(&std::fs::read(upstream_path).unwrap()).unwrap();
    let mut profiles = Vec::new();
    let mut selected = BTreeMap::new();
    for (id, model, protocol) in [
        (
            "route-parent",
            "gpt-5.6-terra",
            UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        ),
        (
            "route-child",
            MODEL,
            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        ),
    ] {
        let mut profile = ProviderProfile::new("Loopback probe");
        profile.id = id.into();
        profile.base_url = upstream["base_url"].as_str().unwrap().into();
        profile.api_key = "local-test-only".into();
        profile.upstream_protocol = protocol.into();
        profile.supports_websockets = false;
        profile.normalize();
        selected.insert(profile.provider_id().to_string(), vec![model.into()]);
        profiles.push(profile);
    }
    let config = CodeyConfig {
        active_profile_id: "route-parent".into(),
        profiles,
        selected_models_by_provider: selected,
        ..CodeyConfig::default()
    }
    .normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let handshake = root.join("router.pending.json");
    std::fs::write(
        &handshake,
        serde_json::to_vec(&json!({"base_url":endpoint.base_url,"token":endpoint.token})).unwrap(),
    )
    .unwrap();
    std::fs::rename(handshake, root.join("router.json")).unwrap();
    let status = tokio::time::timeout(Duration::from_secs(105), child.wait())
        .await
        .unwrap()
        .unwrap();
    router.stop().await.unwrap();
    eprintln!("Native Gemini router probe artifacts: {}", root.display());
    let summary: Value =
        serde_json::from_slice(&std::fs::read(root.join("summary.json")).unwrap()).unwrap();
    assert!(status.success(), "{summary}");
    for key in [
        "childPersistedLegacyBase",
        "chatPathCorrect",
        "baseMatches",
        "rolePreserved",
        "taskPreserved",
        "toolsPreserved",
        "reasoningPreserved",
    ] {
        assert_eq!(summary[key], true, "{summary}");
    }
}
