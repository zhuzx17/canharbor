mod common;
// SPDX-License-Identifier: AGPL-3.0-or-later
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use canharbor_core::runtime::Runtime;
use canharbor_server::{AppState, router};
use canharbor_virtual::VirtualBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
fn state() -> Arc<AppState> {
    AppState::new(
        Runtime::new(Arc::new(VirtualBus::default())),
        "http://127.0.0.1:32180".into(),
    )
}
async fn call(
    app: Router,
    method: &str,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:32180")
        .header("content-type", "application/json");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let res = app
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let (status, h) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !value.is_null() {
        let name = if !status.is_success() {
            "ApiErrorBody"
        } else if path == "/api/v1/version" {
            "Version"
        } else if path == "/api/v1/devices" {
            "Devices"
        } else if path.ends_with("/capabilities") {
            "Capabilities"
        } else if path.starts_with("/api/v1/auth/") {
            "Auth"
        } else if path == "/api/v1/native/pair" {
            "PairLink"
        } else if path == "/api/v1/sessions" && method == "GET" {
            "Sessions"
        } else if path.split('?').next().unwrap().ends_with("/tx") || path.contains("/tx/") {
            "TxReceipt"
        } else if path == "/api/v1/captures" && method == "GET" {
            "Captures"
        } else if path.starts_with("/api/v1/captures") {
            "Capture"
        } else if path == "/api/v1/diagnostics/devices" {
            "DeviceReport"
        } else if path == "/api/v1/tasks" {
            "PeriodicTasks"
        } else if path.contains("/tasks") {
            "PeriodicTask"
        } else if path == "/api/v1/diagnostics" {
            "Diagnostics"
        } else if path == "/api/v1/legal" {
            "Legal"
        } else {
            "Session"
        };
        common::validate(name, &value);
    }
    (status, h, value)
}
#[tokio::test]
async fn browser_pairing_origin_csrf_and_logout() {
    let s = state();
    let native = router(s.clone(), true);
    let web = router(s, false);
    let client = uuid::Uuid::new_v4().to_string();
    let (_, _, v) = call(
        native,
        "POST",
        "/api/v1/native/pair",
        json!({}),
        &[("x-canharbor-client", &client)],
    )
    .await;
    let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap();
    let (status, _, _) = call(web.clone(), "GET", "/api/v1/devices", Value::Null, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(
        web.clone(),
        "POST",
        "/api/v1/auth/pair",
        json!({"code":code}),
        &[("origin", "https://attacker.test")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, h, v) = call(
        web.clone(),
        "POST",
        "/api/v1/auth/pair",
        json!({"code":code}),
        &[("origin", "http://127.0.0.1:32180")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = h["set-cookie"].to_str().unwrap().split(';').next().unwrap();
    let csrf = v["csrf_token"].as_str().unwrap();
    let (status, _, _) = call(
        web.clone(),
        "POST",
        "/api/v1/auth/pair",
        json!({"code":code}),
        &[("origin", "http://127.0.0.1:32180")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let headers = [("cookie", cookie), ("origin", "http://127.0.0.1:32180")];
    let (status, _, _) = call(web.clone(), "GET", "/api/v1/devices", Value::Null, &headers).await;
    assert_eq!(status, StatusCode::OK);
    let body =
        json!({"channel_id":"virtual:0","config":{"mode":"listen_only","nominal_bitrate":500000}});
    let (status, _, _) = call(
        web.clone(),
        "POST",
        "/api/v1/sessions",
        body.clone(),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let good = [
        ("cookie", cookie),
        ("origin", "http://127.0.0.1:32180"),
        ("x-canharbor-csrf", csrf),
    ];
    let (status, _, _) = call(web.clone(), "POST", "/api/v1/sessions", body, &good).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, _) = call(web.clone(), "POST", "/api/v1/native/pair", json!({}), &good).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(
        web.clone(),
        "DELETE",
        "/api/v1/auth/session",
        json!({}),
        &good,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = call(web, "GET", "/api/v1/devices", Value::Null, &headers).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn native_control_and_idempotent_transmission() {
    let shared = state();
    let app = router(shared.clone(), true);
    let client = uuid::Uuid::new_v4().to_string();
    let h = [("x-canharbor-client", client.as_str())];
    let (_, _, v) = call(
        app.clone(),
        "POST",
        "/api/v1/sessions",
        json!({"channel_id":"virtual:0","config":{"mode":"active","nominal_bitrate":500000,"fd":{"enabled":true,"data_bitrate":2000000,"iso":true}}}),
        &h,
    )
    .await;
    let id = v["id"].as_str().unwrap();
    let (_, _, lease) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/lease"),
        json!({}),
        &h,
    )
    .await;
    let g = lease["generation"].as_str().unwrap();
    let control = [
        ("x-canharbor-client", client.as_str()),
        ("x-canharbor-lease-generation", g),
    ];
    let other = uuid::Uuid::new_v4().to_string();
    let (status, _, _) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/start"),
        json!({}),
        &[
            ("x-canharbor-client", &other),
            ("x-canharbor-lease-generation", g),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/start"),
        json!({}),
        &control,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tx = json!({"client_request_id":"once","frame":{"kind":"fd_data","id":291,"extended":false,"brs":true,"data_hex":"0102030405060708090A0B0C"}});
    let (status, _, first) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/tx"),
        tx.clone(),
        &control,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, _, second) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/tx"),
        tx.clone(),
        &control,
    )
    .await;
    assert_eq!(first, second);
    let receipt_path = format!(
        "/api/v1/sessions/{id}/tx/{}",
        first["receipt"].as_str().unwrap()
    );
    let (status, _, queried) = call(app.clone(), "GET", &receipt_path, Value::Null, &h).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queried, first);
    let lookup_path = format!(
        "/api/v1/sessions/{id}/tx?client_request_id=once&generation={}",
        lease["generation"].as_str().unwrap()
    );
    let (status, _, lookup) = call(app.clone(), "GET", &lookup_path, Value::Null, &h).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lookup, first);
    for query in [
        "",
        "?client_request_id=",
        "?client_request_id=once&generation=01",
        "?client_request_id=once&unexpected=1",
    ] {
        let (status, _, _) = call(
            app.clone(),
            "GET",
            &format!("/api/v1/sessions/{id}/tx{query}"),
            Value::Null,
            &h,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    let other = uuid::Uuid::new_v4().to_string();
    let (status, _, _) = call(
        app.clone(),
        "GET",
        &receipt_path,
        Value::Null,
        &[("x-canharbor-client", &other)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, pair) = call(app.clone(), "POST", "/api/v1/native/pair", json!({}), &h).await;
    let code = pair["url"]
        .as_str()
        .unwrap()
        .split("#pair=")
        .nth(1)
        .unwrap();
    let shared_for_lookup = shared.clone();
    let web = router(shared, false);
    let (_, paired, _) = call(
        web.clone(),
        "POST",
        "/api/v1/auth/pair",
        json!({"code":code}),
        &[("origin", "http://127.0.0.1:32180")],
    )
    .await;
    let cookie = paired["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let (status, _, _) = call(
        web,
        "GET",
        &receipt_path,
        Value::Null,
        &[("cookie", cookie)],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The same browser boundary applies to request-ID lookup.
    let browser = router(shared_for_lookup, false);
    let (status, _, _) = call(
        browser,
        "GET",
        &lookup_path,
        Value::Null,
        &[("cookie", cookie)],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mut normalized = tx.clone();
    normalized["frame"]["data_hex"] = json!("0102030405060708090a0b0c");
    let (_, _, normalized) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/tx"),
        normalized,
        &control,
    )
    .await;
    assert_eq!(normalized, first);

    let mut changed = tx;
    changed["frame"]["data_hex"] = json!("00");
    let (status, _, _) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/tx"),
        changed,
        &control,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let invalid = json!({"client_request_id":"bad","frame":{"kind":"fd_data","id":291,"data_hex":"000000000000000000"}});
    let (status, _, _) = call(
        app,
        "POST",
        &format!("/api/v1/sessions/{id}/tx"),
        invalid,
        &control,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
#[tokio::test]
async fn host_and_native_header_do_not_bypass_browser_boundary() {
    let app = router(state(), false);
    let req = Request::builder()
        .uri("/api/v1/version")
        .header("host", "attacker.test")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let client = uuid::Uuid::new_v4().to_string();
    let (status, _, _) = call(
        app,
        "GET",
        "/api/v1/devices",
        Value::Null,
        &[("x-canharbor-client", &client)],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(start_paused = true)]
async fn pairing_code_expires_and_headless_service_does_not_issue_links() {
    let s = state();
    let native = router(s.clone(), true);
    let web = router(s, false);
    let client = uuid::Uuid::new_v4().to_string();
    let (_, _, v) = call(
        native,
        "POST",
        "/api/v1/native/pair",
        json!({}),
        &[("x-canharbor-client", &client)],
    )
    .await;
    let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap();
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    let (status, _, _) = call(
        web,
        "POST",
        "/api/v1/auth/pair",
        json!({"code":code}),
        &[("origin", "http://127.0.0.1:32180")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let headless = AppState::with_web(
        Runtime::new(Arc::new(VirtualBus::default())),
        "http://127.0.0.1:0".into(),
        false,
    );
    let (status, _, _) = call(
        router(headless, true),
        "POST",
        "/api/v1/native/pair",
        json!({}),
        &[("x-canharbor-client", &client)],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn backend_faults_have_typed_http_errors_and_diagnostic_state() {
    use canharbor_virtual::Fault;
    for (fault, code, state) in [
        (Fault::Disconnect, "device_disconnected", "disconnected"),
        (Fault::BusOff, "bus_off", "faulted"),
        (Fault::ReceiveOverflow, "receive_overflow", "faulted"),
        (Fault::FailNextTransmit, "transmit_failed", "faulted"),
    ] {
        let bus = Arc::new(VirtualBus::default());
        let runtime = Runtime::new(bus.clone());
        let s = runtime
            .create("virtual:0", canharbor_core::runtime::Mode::Active)
            .await
            .unwrap();
        let client = uuid::Uuid::new_v4().to_string();
        let owner = format!("native:{client}");
        let lease = runtime.acquire(&s.id, &owner).await.unwrap();
        runtime
            .running(&s.id, &owner, lease.generation, true)
            .await
            .unwrap();
        let app = router(
            AppState::new(runtime.clone(), "http://127.0.0.1:32180".into()),
            true,
        );
        let generation = lease.generation.to_string();
        let headers = [
            ("x-canharbor-client", client.as_str()),
            ("x-canharbor-lease-generation", generation.as_str()),
        ];
        bus.inject(fault);
        // Force backend observation under the runtime lock before forwarding
        // tasks can process the asynchronous fault notification.
        let(status,_,body)=call(app.clone(),"POST",&format!("/api/v1/sessions/{}/tx",s.id),json!({"client_request_id":"fault","frame":{"kind":"classic_data","id":1,"data_hex":"00"}}),&headers).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], code);
        if let Some(receipt) = body["error"]["receipt"].as_str() {
            let (status, _, record) = call(
                app.clone(),
                "GET",
                &format!("/api/v1/sessions/{}/tx/{receipt}", s.id),
                Value::Null,
                &headers,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(record["state"], "failed");
            assert_eq!(record["error"]["outcome_unknown"], false);
        }

        let (status, _, body) = call(
            app,
            "GET",
            &format!("/api/v1/sessions/{}", s.id),
            Value::Null,
            &headers,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], state);
        assert_eq!(body["fault"], code);
        assert!(body["controller"].is_null());
    }
}
#[tokio::test]
async fn malformed_json_requests_use_the_generated_error_envelope() {
    let app = router(state(), true);
    let client = uuid::Uuid::new_v4().to_string();
    let (status, _, value) = call(
        app,
        "POST",
        "/api/v1/sessions",
        json!({"channel_id":"virtual:0","config":{"mode":"typo","nominal_bitrate":500000}}),
        &[("x-canharbor-client", &client)],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(value["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn implemented_read_responses_match_generated_definitions() {
    let app = router(state(), true);
    let id = uuid::Uuid::new_v4().to_string();
    for path in [
        "/api/v1/version",
        "/api/v1/devices",
        "/api/v1/sessions",
        "/api/v1/diagnostics",
        "/api/v1/legal",
    ] {
        let (status, _, _) = call(
            app.clone(),
            "GET",
            path,
            Value::Null,
            &[("x-canharbor-client", &id)],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
}

#[tokio::test]
async fn capabilities_and_revision_guarded_config_updates() {
    let state = state();
    let app = router(state.clone(), true);
    let client = uuid::Uuid::new_v4().to_string();
    let headers = [("x-canharbor-client", client.as_str())];
    let (status, _, caps) = call(
        app.clone(),
        "GET",
        "/api/v1/channels/virtual:0/capabilities",
        Value::Null,
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(caps["fixed_nominal_bitrates"], json!([500000]));
    assert_eq!(caps["fd"], true);
    let (_, _, session) = call(
        app.clone(),
        "POST",
        "/api/v1/sessions",
        json!({"channel_id":"virtual:0","config":{"mode":"listen_only","nominal_bitrate":500000}}),
        &headers,
    )
    .await;
    let id = session["id"].as_str().unwrap();
    let (_, _, lease) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/lease"),
        json!({}),
        &headers,
    )
    .await;
    let generation = lease["generation"].as_str().unwrap();
    let (_, etag_headers, current) = call(
        app.clone(),
        "GET",
        &format!("/api/v1/sessions/{id}"),
        Value::Null,
        &headers,
    )
    .await;
    let etag = etag_headers["etag"].to_str().unwrap();
    assert_eq!(
        etag,
        format!("\"{}\"", current["revision"].as_str().unwrap())
    );
    let path = format!("/api/v1/sessions/{id}/config");
    let config = json!({"mode":"active","nominal_bitrate":500000,"fd":{"enabled":true,"data_bitrate":2000000,"iso":true}});
    let mut controlled = vec![
        ("x-canharbor-client", client.as_str()),
        ("x-canharbor-lease-generation", generation),
    ];
    let (status, _, _) = call(app.clone(), "PATCH", &path, config.clone(), &controlled).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    controlled.push(("if-match", "W/\"1\""));
    let (status, _, _) = call(app.clone(), "PATCH", &path, config.clone(), &controlled).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    controlled.pop();
    controlled.push(("if-match", "\"0\""));
    let (status, _, _) = call(app.clone(), "PATCH", &path, config.clone(), &controlled).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    controlled.pop();
    controlled.push(("if-match", etag));
    let (status, updated_headers, updated) =
        call(app.clone(), "PATCH", &path, config.clone(), &controlled).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["config"]["data_bitrate"], 2000000);
    assert_ne!(updated_headers["etag"], etag);
    let (status, _, _) = call(app.clone(), "PATCH", &path, config.clone(), &controlled).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    let (status, _, _) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/start"),
        json!({}),
        &controlled,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, fresh_headers, _) = call(
        app.clone(),
        "GET",
        &format!("/api/v1/sessions/{id}"),
        Value::Null,
        &headers,
    )
    .await;
    controlled.pop();
    controlled.push(("if-match", fresh_headers["etag"].to_str().unwrap()));
    let (status, _, _) = call(app, "PATCH", &path, config, &controlled).await;
    assert_eq!(status, StatusCode::CONFLICT);
    state.runtime.shutdown().await;
}

#[tokio::test]
async fn classic_configuration_rejects_fd_transmission() {
    let state = state();
    let app = router(state.clone(), true);
    let client = uuid::Uuid::new_v4().to_string();
    let headers = [("x-canharbor-client", client.as_str())];
    let (_, _, session) = call(
        app.clone(),
        "POST",
        "/api/v1/sessions",
        json!({"channel_id":"virtual:0","config":{"mode":"active","nominal_bitrate":500000}}),
        &headers,
    )
    .await;
    let id = session["id"].as_str().unwrap();
    assert!(session["config"]["data_bitrate"].is_null());
    let (_, _, lease) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/lease"),
        json!({}),
        &headers,
    )
    .await;
    let headers = [
        ("x-canharbor-client", client.as_str()),
        (
            "x-canharbor-lease-generation",
            lease["generation"].as_str().unwrap(),
        ),
    ];
    call(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{id}/start"),
        json!({}),
        &headers,
    )
    .await;
    let (status, _, _) = call(
        app,
        "POST",
        &format!("/api/v1/sessions/{id}/tx"),
        json!({"client_request_id":"fd","frame":{"kind":"fd_data","id":1,"data_hex":""}}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    state.runtime.shutdown().await;
}

#[tokio::test]
async fn authenticated_capture_api_and_download_schema() {
    let state = state();
    let session = state
        .runtime
        .create("virtual:0", canharbor_core::runtime::Mode::ListenOnly)
        .await
        .unwrap();
    let lease = state.runtime.acquire(&session.id, "owner").await.unwrap();
    state
        .runtime
        .running(&session.id, "owner", lease.generation, true)
        .await
        .unwrap();
    let app = router(state.clone(), true);
    let client = uuid::Uuid::new_v4().to_string();
    let headers = [("x-canharbor-client", client.as_str())];
    let (status, _, _) = call(
        app.clone(),
        "POST",
        "/api/v1/captures",
        json!({"session_id":session.id,"client_request_id":"invalid","max_duration_seconds":0}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, capture) = call(
        app.clone(),
        "POST",
        "/api/v1/captures",
        json!({"session_id":session.id,"client_request_id":"capture"}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = capture["id"].as_str().unwrap();
    let path = format!("/api/v1/captures/{id}");
    let (status, _, _) = call(
        app.clone(),
        "PATCH",
        &path,
        json!({"name":"bad\nname", "expected_name":""}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, renamed) = call(
        app.clone(),
        "PATCH",
        &path,
        json!({"name":"台架测试", "expected_name":""}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["name"], "台架测试");
    let (status, _, _) = call(
        app.clone(),
        "PATCH",
        &path,
        json!({"name":"Stale", "expected_name":""}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, retried) = call(
        app.clone(),
        "POST",
        "/api/v1/captures",
        json!({"session_id":session.id,"client_request_id":"capture"}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(retried["id"], id);
    assert_eq!(retried["name"], "台架测试");
    let (_, _, list) = call(
        app.clone(),
        "GET",
        "/api/v1/captures",
        Value::Null,
        &headers,
    )
    .await;
    assert_eq!(list["captures"].as_array().unwrap().len(), 1);
    let (status, _, capture) = call(
        app.clone(),
        "POST",
        &format!("/api/v1/captures/{id}/stop"),
        json!({}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(capture["complete"], true);
    assert_eq!(capture["loss_count_known"], true);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/captures/{id}/download"))
                .header("host", "127.0.0.1:32180")
                .header("x-canharbor-client", &client)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "application/x-ndjson");
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("filename*=UTF-8''%E5%8F%B0%E6%9E%B6%E6%B5%8B%E8%AF%95")
    );
    let length = response.headers()["content-length"]
        .to_str()
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.len(), length);
    assert!(canharbor_capture::scan(&bytes[..]).unwrap().complete);
    let (status, _, _) = call(
        app,
        "DELETE",
        &format!("/api/v1/captures/{id}"),
        json!({}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    state.runtime.shutdown().await;
    assert!(state.captures.shutdown().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn device_report_is_read_only_and_does_not_claim_hardware_evidence() {
    use canharbor_core::runtime::Mode;
    let shared = state();
    let view = shared
        .runtime
        .create("virtual:0", Mode::ListenOnly)
        .await
        .unwrap();
    let view = shared
        .runtime
        .acquire(&view.id, "private-controller")
        .await
        .unwrap();
    shared
        .runtime
        .running(&view.id, "private-controller", view.generation, true)
        .await
        .unwrap();
    tokio::time::advance(std::time::Duration::from_secs(16)).await;
    let reader = uuid::Uuid::new_v4().to_string();
    let h = [("x-canharbor-client", reader.as_str())];
    let (status, _, report) = call(
        router(shared.clone(), true),
        "GET",
        "/api/v1/diagnostics/devices",
        Value::Null,
        &h,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["report_version"], "1");
    assert_eq!(report["host"]["os"], std::env::consts::OS);
    assert_eq!(report["host"]["architecture"], std::env::consts::ARCH);
    let channel = report["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["channel_id"] == "virtual:0")
        .unwrap();
    assert_eq!(channel["verification"], "simulation_only");
    assert_eq!(channel["communication_verified"], false);
    assert_eq!(channel["sessions"][0]["state"], "running");
    assert_eq!(channel["sessions"][0]["config"]["nominal_bitrate"], 500000);
    for key in ["device_model", "firmware_version", "library_version"] {
        assert!(channel[key].is_null());
    }
    assert!(!report.to_string().contains("private-controller"));
    assert!(!report.to_string().contains(&view.id));
    let after = shared.runtime.session_snapshot().await;
    assert!(after[0].running);
    assert_eq!(after[0].generation, view.generation);
    let (status, _, _) = call(
        router(shared, false),
        "GET",
        "/api/v1/diagnostics/devices",
        Value::Null,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

async fn paired_browser(shared: &Arc<AppState>) -> (String, String, String) {
    let native = router(shared.clone(), true);
    let web = router(shared.clone(), false);
    let client = uuid::Uuid::new_v4().to_string();
    let (_, _, link) = call(
        native,
        "POST",
        "/api/v1/native/pair",
        json!({}),
        &[("x-canharbor-client", &client)],
    )
    .await;
    let code = link["url"]
        .as_str()
        .unwrap()
        .split("#pair=")
        .nth(1)
        .unwrap();
    let (status, headers, auth) = call(
        web,
        "POST",
        "/api/v1/auth/pair",
        json!({"code":code}),
        &[("origin", "http://127.0.0.1:32180")],
    )
    .await;
    assert_eq!(status, 200);
    (
        headers["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned(),
        auth["csrf_token"].as_str().unwrap().to_owned(),
        auth["client_id"].as_str().unwrap().to_owned(),
    )
}
#[tokio::test]
async fn browser_tasks_require_csrf_and_owner_identity_and_logout_cancels_them() {
    let shared = state();
    let web = router(shared.clone(), false);
    let native = router(shared.clone(), true);
    let (cookie, csrf, owner) = paired_browser(&shared).await;
    let (other_cookie, other_csrf, _) = paired_browser(&shared).await;
    let session = shared
        .runtime
        .create("virtual:0", canharbor_core::runtime::Mode::Active)
        .await
        .unwrap();
    let acquired = shared.runtime.acquire(&session.id, &owner).await.unwrap();
    shared
        .runtime
        .running(&session.id, &owner, acquired.generation, true)
        .await
        .unwrap();
    let generation = acquired.generation.to_string();
    let headers = [
        ("cookie", cookie.as_str()),
        ("origin", "http://127.0.0.1:32180"),
        ("x-canharbor-csrf", csrf.as_str()),
        ("x-canharbor-lease-generation", generation.as_str()),
    ];
    let request = json!({"client_request_id":"browser-task","frame":{"kind":"classic_data","id":291,"data_hex":"00"},"count":2,"interval_ms":60000});
    let (status, _, task) = call(
        web.clone(),
        "POST",
        &format!("/api/v1/sessions/{}/tasks", session.id),
        request,
        &headers,
    )
    .await;
    assert_eq!(status, 202);
    let id = task["id"].as_str().unwrap();
    let other = [
        ("cookie", other_cookie.as_str()),
        ("origin", "http://127.0.0.1:32180"),
        ("x-canharbor-csrf", other_csrf.as_str()),
    ];
    assert_eq!(
        call(
            web.clone(),
            "GET",
            &format!("/api/v1/tasks/{id}"),
            Value::Null,
            &other
        )
        .await
        .0,
        404
    );
    assert_eq!(
        call(
            web.clone(),
            "POST",
            &format!("/api/v1/tasks/{id}/cancel"),
            json!({}),
            &other
        )
        .await
        .0,
        404
    );
    assert!(
        call(web.clone(), "GET", "/api/v1/tasks", Value::Null, &other)
            .await
            .2["tasks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        call(
            web.clone(),
            "POST",
            &format!("/api/v1/tasks/{id}/cancel"),
            json!({}),
            &[("cookie", &cookie), ("origin", "http://127.0.0.1:32180")]
        )
        .await
        .0,
        403
    );
    assert_eq!(
        call(web, "DELETE", "/api/v1/auth/session", json!({}), &headers)
            .await
            .0,
        204
    );
    let client = uuid::Uuid::new_v4().to_string();
    let until = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let (_, _, result) = call(
            native.clone(),
            "GET",
            &format!("/api/v1/tasks/{id}"),
            Value::Null,
            &[("x-canharbor-client", &client)],
        )
        .await;
        if result["state"] == "cancelled" {
            break;
        }
        assert!(tokio::time::Instant::now() < until, "{result}");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    shared.runtime.shutdown().await;
}
