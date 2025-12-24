use actix_web::rt::task::JoinHandle;
use futures::{StreamExt, TryStreamExt};
use log::error;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use eventsource_client as es;
use eventsource_client::{ByteStream, HttpTransport, TransportError};

use crate::{Config, EventType};

// Simple reqwest-based transport implementation
#[derive(Clone)]
struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    fn new(timeout: Option<Duration>) -> Result<Self, reqwest::Error> {
        let mut builder = reqwest::Client::builder();

        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }

        let client = builder.build()?;
        Ok(Self { client })
    }
}

impl HttpTransport for ReqwestTransport {
    fn request(
        &self,
        request: http::Request<Option<String>>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<http::Response<ByteStream>, TransportError>>
                + Send
                + Sync
                + 'static,
        >,
    > {
        let (parts, body_opt) = request.into_parts();

        let mut req_builder = self
            .client
            .request(parts.method.clone(), parts.uri.to_string());

        for (name, value) in parts.headers.iter() {
            req_builder = req_builder.header(name, value);
        }

        if let Some(body) = body_opt {
            req_builder = req_builder.body(body);
        }

        let req = match req_builder.build() {
            Ok(r) => r,
            Err(e) => return Box::pin(async move { Err(TransportError::new(e)) }),
        };

        let client = self.client.clone();

        Box::pin(async move {
            let resp = client
                .execute(req)
                .await
                .map_err(|e| TransportError::new(e))?;

            let status = resp.status();
            let headers = resp.headers().clone();

            let byte_stream: ByteStream = Box::pin(
                resp.bytes_stream()
                    .map(|result| result.map_err(|e| TransportError::new(e))),
            );

            let mut response_builder = http::Response::builder().status(status);

            for (name, value) in headers.iter() {
                response_builder = response_builder.header(name, value);
            }

            let response = response_builder
                .body(byte_stream)
                .map_err(|e| TransportError::new(e))?;

            Ok(response)
        })
    }
}

pub(crate) struct Inner {
    callback_counter: Mutex<i32>,
    callback_url: String,
    client: Box<dyn es::Client>,
}

impl Inner {
    pub(crate) fn new(config: Config) -> Result<Self, String> {
        let client = Inner::build_client(&config)?;

        Ok(Self {
            callback_counter: Mutex::new(0),
            callback_url: config.callback_url,
            client,
        })
    }

    pub(crate) async fn start(&self) {
        let mut stream = self.client.stream();

        let client = reqwest::Client::new();

        loop {
            match stream.try_next().await {
                Ok(Some(event)) => {
                    let event_type: EventType = event.into();
                    if matches!(event_type, EventType::Connected { .. }) {
                        continue;
                    }

                    if !self.send_message(event_type, &client).await {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let failure = EventType::Error {
                        error: format!("Error: {:?}", e),
                    };

                    if !self.send_message(failure, &client).await {
                        break;
                    }
                }
            };
        }
    }

    async fn send_message(&self, event_type: EventType, client: &reqwest::Client) -> bool {
        let json = match serde_json::to_string(&event_type) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to json encode event type {:?}", e);
                return false;
            }
        };

        // send_message is only invoked via the event loop, so this access and following
        // update will be serialized. The usage of a mutex is for the interior mutability.
        let counter_val = *self.callback_counter.lock().unwrap();

        match client
            .post(format!("{}/{}", self.callback_url, counter_val))
            .body(format!("{}\n", json))
            .send()
            .await
        {
            Ok(_) => {
                let mut counter = self.callback_counter.lock().unwrap();
                *counter = counter_val + 1
            }
            Err(e) => {
                error!("Failed to send post back to test harness {:?}", e);
                return false;
            }
        };

        true
    }

    fn build_client(config: &Config) -> Result<Box<dyn es::Client>, String> {
        let mut client_builder = match es::ClientBuilder::for_url(&config.stream_url) {
            Ok(cb) => cb,
            Err(e) => return Err(format!("Failed to create client builder {:?}", e)),
        };

        let mut reconnect_options = es::ReconnectOptions::reconnect(true);

        if let Some(delay_ms) = config.initial_delay_ms {
            reconnect_options = reconnect_options.delay(Duration::from_millis(delay_ms));
        }

        // Create transport with timeout configuration
        let timeout = config.read_timeout_ms.map(Duration::from_millis);
        let transport = match ReqwestTransport::new(timeout) {
            Ok(t) => t,
            Err(e) => return Err(format!("Failed to create transport {:?}", e)),
        };

        if let Some(last_event_id) = &config.last_event_id {
            client_builder = client_builder.last_event_id(last_event_id.clone());
        }

        if let Some(method) = &config.method {
            client_builder = client_builder.method(method.to_string());
        }

        if let Some(body) = &config.body {
            client_builder = client_builder.body(body.to_string());
        }

        if let Some(headers) = &config.headers {
            for (name, value) in headers {
                client_builder = match client_builder.header(name, value) {
                    Ok(cb) => cb,
                    Err(e) => return Err(format!("Unable to set header {:?}", e)),
                };
            }
        }

        Ok(Box::new(
            client_builder
                .reconnect(reconnect_options.build())
                .build_with_transport(transport),
        ))
    }
}

pub(crate) struct StreamEntity {
    inner: Arc<Inner>,
    handle: Option<JoinHandle<()>>,
}

impl StreamEntity {
    pub(crate) fn new(config: Config) -> Result<Self, String> {
        let inner = Inner::new(config)?;

        Ok(Self {
            inner: Arc::new(inner),
            handle: None,
        })
    }

    pub(crate) fn start(&mut self) {
        let inner = self.inner.clone();

        self.handle = Some(actix_web::rt::spawn(async move {
            inner.start().await;
        }));
    }

    pub(crate) fn stop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}
