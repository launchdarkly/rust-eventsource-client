//! Example SSE client that tails an event stream
//!
//! This example uses the built-in HyperTransport for HTTP/HTTPS support.
//!
//! To run this example with HTTP support:
//! ```bash
//! cargo run --example tail --features hyper -- http://example.com/events "Bearer token"
//! ```
//!
//! To run this example with HTTPS support:
//! ```bash
//! cargo run --example tail --features hyper,hyper-rustls -- https://example.com/events "Bearer token"
//! ```

use futures::{Stream, TryStreamExt};
use std::{env, process, time::Duration};

use eventsource_client as es;
use launchdarkly_sdk_transport::HyperTransport;

#[tokio::main]
#[allow(clippy::result_large_err)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = env::args().collect();

    if args.len() != 3 {
        eprintln!("Please pass args: <url> <auth_hdr>");
        eprintln!("Example: cargo run --example tail --features hyper https://sse.dev/test 'Bearer token'");
        process::exit(1);
    }

    let url = &args[1];
    let auth_header = &args[2];

    // Run the appropriate version based on URL scheme and features
    if url.starts_with("https://") {
        #[cfg(feature = "hyper-rustls")]
        {
            run_with_https(url, auth_header).await?;
        }
        #[cfg(not(feature = "hyper-rustls"))]
        {
            eprintln!("Error: HTTPS URL requires the 'hyper-rustls' feature");
            eprintln!(
                "Run with: cargo run --example tail --features hyper,hyper-rustls -- {} '{}'",
                url, auth_header
            );
            process::exit(1);
        }
    } else {
        run_with_http(url, auth_header).await?;
    }

    Ok(())
}

async fn run_with_http(url: &str, auth_header: &str) -> Result<(), Box<dyn std::error::Error>> {
    let transport = HyperTransport::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .build_http()?;

    let client = es::ClientBuilder::for_url(url)?
        .header("Authorization", auth_header)?
        .reconnect(
            es::ReconnectOptions::reconnect(true)
                .retry_initial(false)
                .delay(Duration::from_secs(1))
                .backoff_factor(2)
                .delay_max(Duration::from_secs(60))
                .build(),
        )
        .build_with_transport(transport);

    let mut stream = tail_events(client);

    while let Ok(Some(_)) = stream.try_next().await {}

    Ok(())
}

#[cfg(feature = "hyper-rustls")]
async fn run_with_https(url: &str, auth_header: &str) -> Result<(), Box<dyn std::error::Error>> {
    let transport = HyperTransport::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .build_https()?;

    let client = es::ClientBuilder::for_url(url)?
        .header("Authorization", auth_header)?
        .reconnect(
            es::ReconnectOptions::reconnect(true)
                .retry_initial(false)
                .delay(Duration::from_secs(1))
                .backoff_factor(2)
                .delay_max(Duration::from_secs(60))
                .build(),
        )
        .build_with_transport(transport);

    let mut stream = tail_events(client);

    while let Ok(Some(_)) = stream.try_next().await {}

    Ok(())
}

fn tail_events(client: impl es::Client) -> impl Stream<Item = Result<(), ()>> {
    client
        .stream()
        .map_ok(|event| match event {
            es::SSE::Connected(connection) => {
                println!("got connected: \nstatus={}", connection.response().status())
            }
            es::SSE::Event(ev) => {
                println!("got an event: {}\n{}", ev.event_type, ev.data)
            }
            es::SSE::Comment(comment) => {
                println!("got a comment: \n{comment}")
            }
        })
        .map_err(|err| eprintln!("error streaming events: {err:?}"))
}
