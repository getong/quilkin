/*
 * Copyright 2023 Google LLC
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

mod health;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode};
type Body = Full<Bytes>;

use crate::config::Config;
use health::Health;

use super::{agent, manage, proxy, relay};

pub const PORT: u16 = 8000;

pub(crate) const IDLE_REQUEST_INTERVAL: Duration = Duration::from_secs(30);

/// The runtime mode of Quilkin, which contains various runtime configurations
/// specific to a mode.
#[derive(Clone, Debug)]
pub enum Admin {
    Proxy(proxy::Ready),
    Relay(relay::Ready),
    Manage(manage::Ready),
    Agent(agent::Ready),
}

impl Admin {
    pub fn idle_request_interval(&self) -> Duration {
        match self {
            Self::Proxy(config) => config.idle_request_interval,
            Self::Relay(config) => config.idle_request_interval,
            _ => IDLE_REQUEST_INTERVAL,
        }
    }

    pub fn server(
        &self,
        config: Arc<Config>,
        address: Option<std::net::SocketAddr>,
    ) -> std::thread::JoinHandle<eyre::Result<()>> {
        let address = address.unwrap_or_else(|| (std::net::Ipv6Addr::UNSPECIFIED, PORT).into());
        let health = Health::new();
        tracing::info!(address = %address, "Starting admin endpoint");

        let mode = self.clone();
        std::thread::Builder::new()
            .name("admin-http".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .thread_name("admin-http-worker")
                    .build()
                    .expect("couldn't create tokio runtime in thread");
                runtime.block_on(async move {
                    let accept_stream = tokio::net::TcpListener::bind(address).await?;
                    let http_task: tokio::task::JoinHandle<eyre::Result<()>> =
                        tokio::task::spawn(async move {
                            loop {
                                let (stream, _) = accept_stream.accept().await?;
                                let stream = hyper_util::rt::TokioIo::new(stream);

                                let config = config.clone();
                                let health = health.clone();
                                let mode = mode.clone();
                                tokio::spawn(async move {
                                    let svc = hyper::service::service_fn(move |req| {
                                        let config = config.clone();
                                        let health = health.clone();
                                        let mode = mode.clone();

                                        async move {
                                            Ok::<_, std::convert::Infallible>(
                                                mode.handle_request(req, config, health).await,
                                            )
                                        }
                                    });

                                    let svc = tower::ServiceBuilder::new().service(svc);
                                    if let Err(err) = hyper::server::conn::http1::Builder::new()
                                        .serve_connection(stream, svc)
                                        .await
                                    {
                                        tracing::warn!(
                                            "failed to reponse to phoenix request: {err}"
                                        );
                                    }
                                });
                            }
                        });

                    http_task.await?
                })
            })
            .expect("failed to spawn admin-http thread")
    }

    fn is_ready(&self, config: &Config) -> bool {
        match &self {
            Self::Proxy(proxy) => proxy
                .is_ready()
                .unwrap_or_else(|| config.clusters.read().has_endpoints()),
            Self::Agent(agent) => agent.is_ready(),
            Self::Manage(manage) => manage.is_ready(),
            Self::Relay(relay) => relay.is_ready(),
        }
    }

    async fn handle_request(
        &self,
        request: Request<hyper::body::Incoming>,
        config: Arc<Config>,
        health: Health,
    ) -> Response<Body> {
        match (request.method(), request.uri().path()) {
            (&Method::GET, "/metrics") => collect_metrics(),
            (&Method::GET, "/live" | "/livez") => health.check_liveness(),
            #[cfg(target_os = "linux")]
            (&Method::GET, "/debug/pprof/profile") => {
                let duration = request.uri().query().and_then(|query| {
                    form_urlencoded::parse(query.as_bytes())
                        .find(|(k, _)| k == "seconds")
                        .and_then(|(_, v)| v.parse().ok())
                        .map(std::time::Duration::from_secs)
                });

                match collect_pprof(duration).await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(%error, "admin http server error");
                        Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::new(Bytes::from("internal error")))
                            .unwrap()
                    }
                }
            }
            (&Method::GET, "/ready" | "/readyz") => check_readiness(|| self.is_ready(&config)),
            (&Method::GET, "/config") => match serde_json::to_string(&config) {
                Ok(body) => Response::builder()
                    .status(StatusCode::OK)
                    .header(
                        "Content-Type",
                        hyper::header::HeaderValue::from_static("application/json"),
                    )
                    .body(Body::new(Bytes::from(body)))
                    .unwrap(),
                Err(err) => Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::new(Bytes::from(format!(
                        "failed to create config dump: {err}"
                    ))))
                    .unwrap(),
            },
            (_, _) => {
                let mut response = Response::new(Body::new(Bytes::new()));
                *response.status_mut() = StatusCode::NOT_FOUND;
                response
            }
        }
    }
}

fn check_readiness(check: impl Fn() -> bool) -> Response<Body> {
    if (check)() {
        return Response::new("ok".into());
    }

    let mut response = Response::new(Body::new(Bytes::new()));
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    response
}

fn collect_metrics() -> Response<Body> {
    let mut response = Response::new(Body::new(Bytes::new()));
    let mut buffer = vec![];
    let encoder = prometheus::TextEncoder::new();
    let body =
        prometheus::Encoder::encode(&encoder, &crate::metrics::registry().gather(), &mut buffer)
            .map_err(|error| tracing::warn!(%error, "Failed to encode metrics"))
            .and_then(|_| {
                String::from_utf8(buffer)
                    .map(hyper::body::Bytes::from)
                    .map_err(|error| tracing::warn!(%error, "Failed to convert metrics to utf8"))
            });

    match body {
        Ok(body) => {
            *response.body_mut() = Body::new(body);
        }
        Err(_) => {
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    response
}

/// Collects profiling information using `prof` for an optional `duration` or
/// the default if `None`.
#[cfg(target_os = "linux")]
async fn collect_pprof(
    duration: Option<std::time::Duration>,
) -> Result<Response<Body>, eyre::Error> {
    let duration = duration.unwrap_or_else(|| std::time::Duration::from_secs(2));
    tracing::debug!(duration_seconds = duration.as_secs(), "profiling");

    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(1000)
        // From the pprof docs, this blocklist helps prevent deadlock with
        // libgcc's unwind.
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()?;

    tokio::time::sleep(duration).await;

    let encoded_profile = crate::codec::prost::encode(&guard.report().build()?.pprof()?)?;

    // gzip profile
    let mut encoder = libflate::gzip::Encoder::new(Vec::new())?;
    std::io::copy(&mut &encoded_profile[..], &mut encoder)?;
    let gzip_body = encoder.finish().into_result()?;
    tracing::debug!("profile encoded to gzip");

    Response::builder()
        .header(hyper::header::CONTENT_LENGTH, gzip_body.len() as u64)
        .header(hyper::header::CONTENT_TYPE, "application/octet-stream")
        .header(hyper::header::CONTENT_ENCODING, "gzip")
        .body(Body::new(Bytes::from(gzip_body)))
        .map_err(From::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::endpoint::Endpoint;

    #[tokio::test]
    async fn collect_metrics() {
        let response = super::collect_metrics();
        assert_eq!(response.status(), hyper::StatusCode::OK);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn collect_pprof() {
        // Custom time to make the test fast.
        super::collect_pprof(Some(std::time::Duration::from_millis(1)))
            .await
            .unwrap();
    }

    #[test]
    fn check_proxy_readiness() {
        let config = crate::Config::default_non_agent();
        assert_eq!(config.clusters.read().endpoints().len(), 0);

        let admin = Admin::Proxy(<_>::default());
        assert!(!admin.is_ready(&config));

        config
            .clusters
            .write()
            .insert_default([Endpoint::new((std::net::Ipv4Addr::LOCALHOST, 25999).into())].into());

        assert!(admin.is_ready(&config));
    }
}
