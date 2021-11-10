use actix_web::{web, App, HttpResponse, HttpServer, Responder};
use eventsource_client as es;
use futures::channel::mpsc::channel;
use futures::TryStreamExt;
use serde::{self, Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Serialize)]
struct Status {
    capabilities: Vec<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Config {
    /// The URL of the SSE endpoint created by the test harness.
    url: String,
    /// A string describing the current test, if desired for logging.
    tag: Option<String>,
    /// An optional integer specifying the initial reconnection delay parameter, in
    /// milliseconds. Not all SSE client implementations allow this to be configured, but the
    /// test harness will send a value anyway in an attempt to avoid having reconnection tests
    /// run unnecessarily slowly.
    initial_delay_ms: Option<u64>,
    /// An optional string which should be sent as the Last-Event-Id header in the initial
    /// HTTP request. The test harness will only set this property if the test service has the
    /// "last-event-id" capability.
    last_event_id: Option<String>,
    /// A JSON object containing additional HTTP header names and string values. The SSE
    /// client should be configured to add these headers to its HTTP requests; the test harness
    /// will then verify that it receives those headers. The test harness will only set this
    /// property if the test service has the "headers" capability. Header names can be assumed
    /// to all be lowercase.
    headers: Option<HashMap<String, String>>,
    /// A string specifying an HTTP method to use instead of GET. The test harness will only
    /// set this property if the test service has the "post" or "report" capability.
    method: Option<String>,
    /// A string specifying data to be sent in the HTTP request body. The test harness will
    /// only set this property if the test service has the "post" or "report" capability.
    body: Option<String>,
}

#[derive(Serialize, Debug)]
#[serde(tag = "kind")]
enum EventType {
    #[serde(rename = "hello")]
    Hello,
    #[serde(rename = "event")]
    Event { event: Event },
    #[serde(rename = "comment")]
    Comment { comment: String },
    #[serde(rename = "error")]
    Error { error: String },
}

impl From<es::Event> for EventType {
    fn from(event: es::Event) -> Self {
        Self::Event {
            event: Event {
                event_type: event.event_type,
                data: String::from_utf8(event.data.to_vec()).unwrap(),
                id: String::from_utf8(event.id.to_vec()).unwrap(),
            },
        }
    }
}

#[derive(Serialize, Debug)]
struct Event {
    #[serde(rename = "type")]
    event_type: String,
    data: String,
    id: String,
}

async fn status() -> impl Responder {
    web::Json(Status {
        capabilities: vec![],
    })
}

async fn stream(config: web::Json<Config>) -> HttpResponse {
    let client_builder = es::Client::for_url(&config.url).unwrap();
    let mut reconnect_options = es::ReconnectOptions::reconnect(false);
    if let Some(delay_ms) = config.initial_delay_ms {
        reconnect_options = reconnect_options.delay(Duration::from_millis(delay_ms));
    }
    let client = client_builder.reconnect(reconnect_options.build()).build();

    // TODO(mmk) Do we want to change this to be unbounded?
    let (mut tx, rx) = channel::<Result<web::Bytes, actix_web::Error>>(100);
    actix_web::rt::spawn(async move {
        let hello = EventType::Hello;
        let hello_json = format!("{}\n", serde_json::to_string(&hello).unwrap());
        if tx
            .try_send(Ok(web::Bytes::copy_from_slice(hello_json.as_bytes())))
            .is_err()
        {
            eprintln!("we couldn't even say hello");
            return;
        };

        let mut stream = Box::pin(client.stream());

        loop {
            match stream.try_next().await {
                Ok(Some(event)) => {
                    let event_type: EventType = event.into();
                    let json = format!("{}\n", serde_json::to_string(&event_type).unwrap());
                    if tx
                        .try_send(Ok(web::Bytes::copy_from_slice(json.as_bytes())))
                        .is_err()
                    {
                        println!("we failed to send something back");
                        return;
                    };
                }
                Ok(None) => {
                    eprintln!("Received nothing");
                    break;
                }
                Err(e) => {
                    eprintln!("We had a failure on the stream {:?}", e);
                    break;
                }
            };
        }

        tx.close_channel();
    });

    HttpResponse::Ok().streaming(rx)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    HttpServer::new(|| {
        App::new()
            .route("/", web::get().to(status))
            .route("/", web::post().to(stream))
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
