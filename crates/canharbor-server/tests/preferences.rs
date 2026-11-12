// SPDX-License-Identifier: AGPL-3.0-or-later
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use canharbor_core::runtime::Runtime;
use canharbor_server::{
    AppState,
    preferences::{Locale, Preferences, Store},
    router,
};
use canharbor_virtual::VirtualBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tower::ServiceExt;
fn temporary() -> PathBuf {
    std::path::PathBuf::from(if cfg!(windows) {
        std::env::var_os("TEMP").expect("TEMP")
    } else {
        std::ffi::OsString::from("/tmp")
    })
    .join(format!("ch-locale-{}", uuid::Uuid::new_v4()))
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:32180")
        .header("content-type", "application/json");
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    )
}
#[test]
fn preferences_survive_reopen_and_invalid_storage_does_not_change_memory() {
    let root = temporary();
    let path = root.join("preferences.json");
    let store = Store::new(Some(path.clone()));
    assert_eq!(store.get().locale, Locale::Auto);
    store
        .set(Preferences {
            locale: Locale::ZhCn,
        })
        .unwrap();
    assert_eq!(Store::new(Some(path.clone())).get().locale, Locale::ZhCn);
    std::fs::write(&path, b"{\"locale\":\"../../en\"}").unwrap();
    assert_eq!(Store::new(Some(path)).get().locale, Locale::Auto);
    let blocked = root.join("not-a-directory");
    std::fs::write(&blocked, b"file").unwrap();
    let bad = Store::new(Some(blocked.join("preferences.json")));
    assert!(bad.set(Preferences { locale: Locale::En }).is_err());
    assert_eq!(bad.get().locale, Locale::Auto);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn browser_preference_writes_require_pairing_origin_and_csrf() {
    let root = temporary();
    let state = AppState::with_preferences_file(
        Runtime::new(Arc::new(VirtualBus::default())),
        "http://127.0.0.1:32180".into(),
        true,
        None,
        Some(root.join("preferences.json")),
    );
    let native = router(state.clone(), true);
    let browser = router(state.clone(), false);
    let origin = ("origin", "http://127.0.0.1:32180");
    assert_eq!(
        call(
            &browser,
            "PUT",
            "/api/v1/preferences",
            json!({"locale":"zh-CN"}),
            &[origin]
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, _, pair) = call(
        &native,
        "POST",
        "/api/v1/native/pair",
        json!({}),
        &[("x-canharbor-client", "11111111-1111-4111-8111-111111111111")],
    )
    .await;
    let code = pair["url"]
        .as_str()
        .unwrap()
        .split("#pair=")
        .nth(1)
        .unwrap();
    let (_, headers, auth) = call(
        &browser,
        "POST",
        "/api/v1/auth/pair",
        json!({"code":code}),
        &[origin],
    )
    .await;
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let headers = [
        origin,
        ("cookie", cookie),
        ("x-canharbor-csrf", auth["csrf_token"].as_str().unwrap()),
    ];
    assert_eq!(
        call(
            &browser,
            "PUT",
            "/api/v1/preferences",
            json!({"locale":"zh-CN"}),
            &headers[..2]
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &browser,
            "PUT",
            "/api/v1/preferences",
            json!({"locale":"../bad"}),
            &headers
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, _, value) = call(
        &browser,
        "PUT",
        "/api/v1/preferences",
        json!({"locale":"zh-CN"}),
        &headers,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["locale"], "zh-CN");
    assert_eq!(state.runtime.list().await.len(), 0);
    assert_eq!(
        Store::new(Some(root.join("preferences.json"))).get().locale,
        Locale::ZhCn
    );
    assert_eq!(
        call(
            &browser,
            "GET",
            "/api/v1/preferences",
            Value::Null,
            &headers[..2]
        )
        .await
        .2["locale"],
        "zh-CN"
    );
    std::fs::remove_dir_all(root).unwrap();
}
