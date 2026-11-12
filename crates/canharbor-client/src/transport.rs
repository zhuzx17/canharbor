// SPDX-License-Identifier: AGPL-3.0-or-later
//! One bounded HTTP exchange over an authenticated local byte stream; no retry.
use http_body_util::{BodyExt, Full};
use hyper::{
    Request as HttpRequest,
    body::{Bytes, Incoming},
};
use hyper_util::rt::TokioIo;
use std::{path::PathBuf, time::Duration};
pub type Error = Box<dyn std::error::Error + Send + Sync>;
#[derive(Clone)]
pub struct Client {
    dir: PathBuf,
}
pub struct Request {
    dir: PathBuf,
    method: reqwest::Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    query: Vec<(String, String)>,
}
struct Driver(tokio::task::JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub struct Response {
    response: hyper::Response<Incoming>,
    deadline: tokio::time::Instant,
    _driver: Driver,
}
impl Client {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
    pub fn request(&self, method: reqwest::Method, url: String) -> Request {
        Request {
            dir: self.dir.clone(),
            method,
            url,
            headers: vec![],
            body: vec![],
            query: vec![],
        }
    }
    pub fn get(&self, url: String) -> Request {
        self.request(reqwest::Method::GET, url)
    }
}
impl Request {
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
    pub fn json(mut self, value: &serde_json::Value) -> Self {
        self.body = value.to_string().into_bytes();
        self.headers
            .push(("content-type".into(), "application/json".into()));
        self
    }
    pub fn query(mut self, query: &[(&str, &str)]) -> Self {
        self.query = query
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        self
    }
    pub async fn send(self) -> Result<Response, Error> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        tokio::time::timeout_at(deadline, async move {
            let mut url = reqwest::Url::parse(&self.url)?;
            if !self.query.is_empty() {
                url.query_pairs_mut().extend_pairs(self.query);
            }
            let path = match url.query() {
                Some(q) => format!("{}?{q}", url.path()),
                None => url.path().to_owned(),
            };
            let mut request = HttpRequest::builder()
                .method(self.method)
                .uri(path)
                .header("host", "localhost");
            for (k, v) in self.headers {
                request = request.header(k, v);
            }
            let request = request.body(Full::new(Bytes::from(self.body)))?;
            let stream = canharbor_ipc::connect(&self.dir).await?;
            let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
                .max_buf_size(64 * 1024)
                .handshake(TokioIo::new(stream))
                .await?;
            let driver = Driver(tokio::spawn(async move {
                let _ = connection.with_upgrades().await;
            }));
            let response = sender.send_request(request).await?;
            Ok(Response {
                response,
                deadline,
                _driver: driver,
            })
        })
        .await
        .map_err(|e| Box::new(e) as Error)?
    }
}
impl Response {
    pub fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }
    pub fn content_length(&self) -> Option<u64> {
        self.response
            .headers()
            .get("content-length")?
            .to_str()
            .ok()?
            .parse()
            .ok()
    }
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        loop {
            let frame =
                tokio::time::timeout_at(self.deadline, self.response.body_mut().frame()).await?;
            match frame {
                Some(Ok(f)) => {
                    if let Ok(data) = f.into_data() {
                        return Ok(Some(data));
                    }
                }
                Some(Err(e)) => return Err(Box::new(e)),
                None => return Ok(None),
            }
        }
    }
}
