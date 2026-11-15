//! The calls to the providers: HTTP/1.1, `http://` or `https://`, with
//! rustls (ring) checked against the system's certificate store.

use std::pin::Pin;
use std::time::Duration;

use axum::body::Bytes;
use futures_util::Stream;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{HeaderMap, Request, StatusCode, header};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client as Pool;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

/// How long a provider may take to accept the connection.
pub const CONNECT: Duration = Duration::from_secs(10);
/// How long it may go quiet: before its answer starts, and between two
/// pieces of a streamed answer (not for the whole).
pub const QUIET: Duration = Duration::from_secs(300);

/// The client to the providers.
#[derive(Clone)]
pub struct Client {
    pool: Pool<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

/// A provider's answer, its body still coming.
pub struct Answer {
    /// Its status.
    pub status: StatusCode,
    /// Its headers.
    pub headers: HeaderMap,
    /// Its body, piece by piece.
    pub body: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
}

impl Client {
    /// The client, with the system's roots.
    pub fn new() -> Result<Self, String> {
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(CONNECT));
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_provider_and_native_roots(rustls::crypto::ring::default_provider())
            .map_err(|error| format!("the system's certificate store: {error}"))?
            .https_or_http()
            .enable_http1()
            .wrap_connector(http);
        Ok(Self {
            pool: Pool::builder(TokioExecutor::new()).build(https),
        })
    }

    /// POST `body` as JSON to `url`, with `key` as a bearer token.
    pub async fn post_json(
        &self,
        url: &str,
        key: Option<&str>,
        body: Bytes,
    ) -> Result<Answer, String> {
        let mut request = Request::post(url).header(header::CONTENT_TYPE, "application/json");
        if let Some(key) = key {
            request = request.header(header::AUTHORIZATION, format!("Bearer {key}"));
        }
        let request = request
            .body(Full::new(body))
            .map_err(|error| format!("the request: {error}"))?;
        let response = match tokio::time::timeout(QUIET, self.pool.request(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => return Err(chain(&error)),
            Err(_) => return Err(format!("no answer within {} s", QUIET.as_secs())),
        };
        let (parts, incoming) = response.into_parts();
        Ok(Answer {
            status: parts.status,
            headers: parts.headers,
            body: Box::pin(pieces(incoming)),
        })
    }
}

/// The body's data, each piece within QUIET of the last.
fn pieces(incoming: Incoming) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    futures_util::stream::unfold(Some(incoming), |body| async move {
        let mut body = body?;
        loop {
            match tokio::time::timeout(QUIET, body.frame()).await {
                Err(_) => {
                    let quiet = format!("the provider went quiet for {} s", QUIET.as_secs());
                    return Some((Err(std::io::Error::other(quiet)), None));
                }
                Ok(None) => return None,
                Ok(Some(Err(error))) => {
                    return Some((Err(std::io::Error::other(error)), None));
                }
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        return Some((Ok(data), Some(body)));
                    }
                }
            }
        }
    })
}

/// An error with its causes, which say what went wrong (DNS, TLS, refused).
fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}
