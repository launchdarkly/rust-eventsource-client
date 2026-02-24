mod stream_entity;

use actix_web::{guard, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use eventsource_client as es;
use futures::executor;
use serde::{self, Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{mpsc, Mutex};
use std::thread;
use stream_entity::StreamEntity;

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
    /// An optional integer specifying the initial reconnection delay parameter, in
    /// milliseconds. Not all SSE client implementations allow this to be configured, but the
    /// test harness will send a value anyway in an attempt to avoid having reconnection tests
    /// run unnecessarily slowly.
    initial_delay_ms: Option<u64>,
    /// A JSON object containing additional HTTP header names and string values. The SSE
    /// client should be configured to add these headers to its HTTP requests; the test harness
    /// will then verify that it receives those headers. The test harness will only set this
    /// property if the test service has the "headers" capability. Header names can be assumed
    /// to all be lowercase.
    headers: Option<HashMap<String, String>>,
    /// An optional integer specifying the read timeout for the connection, in
    /// milliseconds.
    read_timeout_ms: Option<u64>,
    /// An optional string which should be sent as the Last-Event-Id header in the initial
    /// HTTP request. The test harness will only set this property if the test service has the
    /// "last-event-id" capability.
    last_event_id: Option<String>,
    /// A string specifying an HTTP method to use instead of GET. The test harness will only
    /// set this property if the test service has the "post" or "report" capability.
    method: Option<String>,
    /// A string specifying data to be sent in the HTTP request body. The test harness will
    /// only set this property if the test service has the "post" or "report" capability.
    body: Option<String>,
}

#[derive(Serialize, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum EventType {
    Connected {},
    Event { event: Event },
    Comment { comment: String },
    Error { error: String },
}

impl From<es::SSE> for EventType {
    fn from(event: es::SSE) -> Self {
        match event {
            es::SSE::Connected(_) => Self::Connected {},
            es::SSE::Event(evt) => Self::Event {
                event: Event {
                    event_type: evt.event_type,
                    data: evt.data,
                    id: evt.id,
                },
            },
            es::SSE::Comment(comment) => Self::Comment { comment },
        }
    }
}

#[derive(Serialize, Debug)]
struct Event {
    #[serde(rename = "type")]
    event_type: String,
    data: String,
    id: Option<String>,
}

async fn status() -> impl Responder {
    web::Json(Status {
        capabilities: vec![
            "bom".to_string(),
            "comments".to_string(),
            "headers".to_string(),
            "last-event-id".to_string(),
            "read-timeout".to_string(),
        ],
    })
}

async fn stream(
    req: HttpRequest,
    config: web::Json<Config>,
    app_state: web::Data<AppState>,
) -> HttpResponse {
    let mut stream_entity = match StreamEntity::new(config.into_inner()) {
        Ok(se) => se,
        Err(e) => return HttpResponse::InternalServerError().body(e),
    };

    let mut counter = match app_state.counter.lock() {
        Ok(c) => c,
        Err(_) => return HttpResponse::InternalServerError().body("Unable to retrieve counter"),
    };

    let mut entities = match app_state.stream_entities.lock() {
        Ok(h) => h,
        Err(_) => return HttpResponse::InternalServerError().body("Unable to retrieve handles"),
    };

    let stream_resource = match req.url_for("stop_stream", [counter.to_string()]) {
        Ok(sr) => sr,
        Err(_) => {
            return HttpResponse::InternalServerError()
                .body("Unable to generate stream response URL")
        }
    };

    *counter += 1;
    stream_entity.start();
    entities.insert(*counter, stream_entity);

    let mut response = HttpResponse::Ok();
    response.insert_header(("Location", stream_resource.to_string()));
    response.finish()
}

async fn shutdown(stopper: web::Data<mpsc::Sender<()>>) -> HttpResponse {
    match stopper.send(()) {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(_) => HttpResponse::InternalServerError().body("Unable to send shutdown signal"),
    }
}

async fn stop_stream(req: HttpRequest, app_state: web::Data<AppState>) -> HttpResponse {
    if let Some(stream_id) = req.match_info().get("id") {
        let stream_id: u32 = match stream_id.parse() {
            Ok(id) => id,
            Err(_) => return HttpResponse::BadRequest().body("Unable to parse stream id"),
        };

        match app_state.stream_entities.lock() {
            Ok(mut entities) => {
                if let Some(mut entity) = entities.remove(&stream_id) {
                    entity.stop();
                }
            }
            Err(_) => {
                return HttpResponse::InternalServerError().body("Unable to retrieve handles")
            }
        };

        HttpResponse::NoContent().finish()
    } else {
        HttpResponse::BadRequest().body("No stream id was provided in the URL")
    }
}

struct AppState {
    counter: Mutex<u32>,
    stream_entities: Mutex<HashMap<u32, StreamEntity>>,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    let (tx, rx) = mpsc::channel::<()>();

    let state = web::Data::new(AppState {
        counter: Mutex::new(0),
        stream_entities: Mutex::new(HashMap::new()),
    });

    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(tx.clone()))
            .app_data(state.clone())
            .route("/", web::get().to(status))
            .route("/", web::post().to(stream))
            .route("/", web::delete().to(shutdown))
            .service(
                web::resource("/stream/{id}")
                    .name("stop_stream")
                    .guard(guard::Delete())
                    .to(stop_stream),
            )
    })
    .bind("127.0.0.1:8080")?
    .run();

    let handle = server.handle();

    thread::spawn(move || {
        // wait for shutdown signal
        if let Ok(()) = rx.recv() {
            executor::block_on(handle.stop(true))
        }
    });

    // run server
    server.await
}
