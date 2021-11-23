use actix_web::rt::task::JoinHandle;
use actix_web::{guard, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use eventsource_client as es;
use futures::TryStreamExt;
use serde::{self, Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Serialize)]
struct Status {
    capabilities: Vec<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Config {
    /// The URL of an SSE endpoint created by the test harness.
    stream_url: String,
    /// The URL of a callback endpoint created by the test harness .
    callback_url: String,
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
    #[serde(rename = "event")]
    Event { event: Event },
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
        capabilities: vec!["last-event-id".to_string()],
    })
}

async fn stream(
    req: HttpRequest,
    config: web::Json<Config>,
    app_state: web::Data<AppState>,
) -> HttpResponse {
    let mut client_builder = es::Client::for_url(&config.stream_url).unwrap();
    let mut reconnect_options = es::ReconnectOptions::reconnect(true);

    if let Some(delay_ms) = config.initial_delay_ms {
        reconnect_options = reconnect_options.delay(Duration::from_millis(delay_ms));
    }

    // TODO(mmk) This is really supposed to be part of the client interface I believe
    if let Some(last_event_id) = &config.last_event_id {
        client_builder = client_builder
            .header("last-event-id", &last_event_id)
            .unwrap();
    }

    let client = client_builder.reconnect(reconnect_options.build()).build();

    let handle = actix_web::rt::spawn(async move {
        let mut stream = Box::pin(client.stream());
        let mut callback_counter = 0;
        let callback_url = &config.callback_url;
        let client = reqwest::Client::new();

        loop {
            match stream.try_next().await {
                Ok(Some(event)) => {
                    let event_type: EventType = event.into();
                    let json = format!("{}\n", serde_json::to_string(&event_type).unwrap());
                    client
                        .post(format!("{}/{}", callback_url, callback_counter))
                        .body(json)
                        .send()
                        .await
                        .unwrap();
                    callback_counter += 1;
                }
                Ok(None) => break,
                Err(_) => break,
            };
        }
    });

    let mut counter = app_state.counter.lock().unwrap();
    let mut handles = app_state.handles.lock().unwrap();

    *counter += 1;
    handles.insert(*counter, handle);

    let stream_resource = req.url_for("close_stream", &[counter.to_string()]).unwrap();
    let mut response = HttpResponse::Ok();
    response.insert_header(("Location", stream_resource.to_string()));
    response.finish()
}

async fn close() -> HttpResponse {
    HttpResponse::NoContent().finish()
}

async fn close_stream(req: HttpRequest, app_state: web::Data<AppState>) -> HttpResponse {
    let stream_id = req.match_info().get("id").unwrap();
    let stream_id: u32 = match stream_id.parse() {
        Ok(id) => id,
        Err(_) => return HttpResponse::BadRequest().body("Unable to parse stream id"),
    };

    let mut handles = app_state.handles.lock().unwrap();
    match handles.remove(&stream_id) {
        Some(handle) => handle.abort(),
        None => (),
    }

    HttpResponse::NoContent().finish()
}

struct AppState {
    counter: Mutex<u32>,
    handles: Mutex<HashMap<u32, JoinHandle<()>>>,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    let state = web::Data::new(AppState {
        counter: Mutex::new(0),
        handles: Mutex::new(HashMap::new()),
    });

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .route("/", web::get().to(status))
            .route("/", web::post().to(stream))
            .route("/", web::delete().to(close))
            .service(
                web::resource("/stream/{id}")
                    .name("close_stream")
                    .guard(guard::Delete())
                    .to(close_stream),
            )
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
