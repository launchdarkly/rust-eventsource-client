use std::{env, process};

use futures::{future::Future, lazy, stream::Stream};

use eventsource_client as es;

fn main() -> Result<(), es::Error> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 3 {
        eprintln!("Please pass args: <url> <auth_hdr>");
        process::exit(1);
    }

    let url = &args[1];
    let auth_header = &args[2];

    let client = es::Client::for_url(url)?
        .header("Authorization", auth_header)?
        .build();
    tokio::run(lazy(|| tail_events(client)));
    Ok(())
}

fn tail_events(mut client: es::Client) -> impl Future<Item = (), Error = ()> {
    client
        .stream()
        .for_each(|event| {
            println!("got an event: {}", event.event_type);
            Ok(())
        })
        .map_err(|err| eprintln!("error streaming events: {:?}", err))
}
