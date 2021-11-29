use actix_web::rt::task::JoinHandle;
use actix_web::{guard, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use eventsource_client as es;
use futures::{executor, TryStreamExt};
use log::error;
use serde::{self, Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{mpsc, Mutex};
use std::thread;
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
    let mut client_builder = match es::Client::for_url(&config.stream_url) {
        Ok(cb) => cb,
        Err(e) => {
            error!("Failed to create client builder {:?}", e);
            return HttpResponse::InternalServerError()
                .body("Unable to build client for processing");
        }
    };
    let mut reconnect_options = es::ReconnectOptions::reconnect(true);

    if let Some(delay_ms) = config.initial_delay_ms {
        reconnect_options = reconnect_options.delay(Duration::from_millis(delay_ms));
    }

    if let Some(last_event_id) = &config.last_event_id {
        client_builder = client_builder.last_event_id(last_event_id.clone());
    }

    let ld_client = client_builder.reconnect(reconnect_options.build()).build();

    let handle = actix_web::rt::spawn(async move {
        let mut stream = Box::pin(ld_client.stream());
        let mut callback_counter = 0;
        let callback_url = &config.callback_url;
        let client = reqwest::Client::new();

        loop {
            match stream.try_next().await {
                Ok(Some(event)) => {
                    let event_type: EventType = event.into();
                    let json = match serde_json::to_string(&event_type) {
                        Ok(s) => s,
                        Err(e) => {
                            error!("Failed to json encode event type {:?}", e);
                            break;
                        }
                    };

                    match client
                        .post(format!("{}/{}", callback_url, callback_counter))
                        .body(format!("{}\n", json))
                        .send()
                        .await
                    {
                        Ok(_) => callback_counter += 1,
                        Err(e) => {
                            error!("Failed to send post back to test harness {:?}", e);
                            break;
                        }
                    };
                }
                Ok(None) => break,
                Err(_) => break,
            };
        }
    });

    let mut counter = match app_state.counter.lock() {
        Ok(c) => c,
        Err(_) => return HttpResponse::InternalServerError().body("Unable to retrieve counter"),
    };

    let mut handles = match app_state.handles.lock() {
        Ok(h) => h,
        Err(_) => return HttpResponse::InternalServerError().body("Unable to retrieve handles"),
    };

    *counter += 1;
    handles.insert(*counter, handle);

    let stream_resource = match req.url_for("stop_stream", &[counter.to_string()]) {
        Ok(sr) => sr,
        Err(_) => {
            return HttpResponse::InternalServerError()
                .body("Unable to generate stream response URL")
        }
    };

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

        match app_state.handles.lock() {
            Ok(mut handles) => match handles.remove(&stream_id) {
                Some(handle) => handle.abort(),
                None => (), // If the provided stream wasn't in the map, then it's shutdown.
            },
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
    handles: Mutex<HashMap<u32, JoinHandle<()>>>,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    let (tx, rx) = mpsc::channel::<()>();

    let state = web::Data::new(AppState {
        counter: Mutex::new(0),
        handles: Mutex::new(HashMap::new()),
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
