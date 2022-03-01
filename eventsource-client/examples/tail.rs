use es::{BoxStream, Event};
use futures::{Stream, TryStreamExt};
use std::{env, process, str::from_utf8, time::Duration};

use eventsource_client as es;

#[tokio::main]
async fn main() -> Result<(), es::Error> {
    env_logger::init();

    let args: Vec<String> = env::args().collect();

    if args.len() != 3 {
        eprintln!("Please pass args: <url> <auth_hdr>");
        process::exit(1);
    }

    let url = &args[1];
    let auth_header = &args[2];

    let client = es::for_url(url)?
        .header("Authorization", auth_header)?
        .reconnect(
            es::ReconnectOptions::reconnect(true)
                .retry_initial(false)
                .delay(Duration::from_secs(1))
                .backoff_factor(2)
                .delay_max(Duration::from_secs(60))
                .build(),
        )
        .build();

    let mut stream = tail_events(client.stream());

    while let Ok(Some(_)) = stream.try_next().await {}

    Ok(())
}

fn tail_events(stream: BoxStream<es::Result<Event>>) -> impl Stream<Item = Result<(), ()>> + '_ {
    stream
        .map_ok(|event| {
            println!(
                "got an event: {}\n{}",
                event.event_type,
                from_utf8(event.field("data").unwrap_or_default()).unwrap_or_default()
            )
        })
        .map_err(|err| eprintln!("error streaming events: {:?}", err))
}
