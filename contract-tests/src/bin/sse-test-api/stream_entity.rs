use actix_web::rt::task::JoinHandle;
use futures::TryStreamExt;
use log::error;
use std::pin::Pin;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use eventsource_client as es;

use crate::{Config, EventType};

type Connector = es::HttpsConnector;

pub(crate) struct Inner {
    callback_counter: Mutex<i32>,
    callback_url: String,
    client: Pin<Box<dyn es::Client>>,
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
                    if !self.send_message(event_type, &client).await {
                        break;
                    }
                }
                Ok(None) => continue,
                Err(e) => {
                    let failure = EventType::Error {
                        error: format!("Error: {:?}", e),
                    };

                    if !self.send_message(failure, &client).await {
                        break;
                    }

                    match e {
                        es::Error::StreamClosed => break,
                        _ => continue,
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

        let mut counter = self.callback_counter.lock().unwrap();

        match client
            .post(format!("{}/{}", self.callback_url, counter))
            .body(format!("{}\n", json))
            .send()
            .await
        {
            Ok(_) => *counter += 1,
            Err(e) => {
                error!("Failed to send post back to test harness {:?}", e);
                return false;
            }
        };

        true
    }

    fn build_client(config: &Config) -> Result<Pin<Box<dyn es::Client>>, String> {
        let mut client_builder = match es::for_url(&config.stream_url) {
            Ok(cb) => cb,
            Err(e) => return Err(format!("Failed to create client builder {:?}", e)),
        };

        let mut reconnect_options = es::ReconnectOptions::reconnect(true);

        if let Some(delay_ms) = config.initial_delay_ms {
            reconnect_options = reconnect_options.delay(Duration::from_millis(delay_ms));
        }

        if let Some(headers) = &config.headers {
            for (name, value) in headers {
                client_builder = match client_builder.header(name, value) {
                    Ok(cb) => cb,
                    Err(e) => return Err(format!("Unable to set header {:?}", e)),
                };
            }
        }

        Ok(client_builder.reconnect(reconnect_options.build()).build())
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
