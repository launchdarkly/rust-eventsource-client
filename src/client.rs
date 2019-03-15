use std::collections::BTreeMap as Map;
use std::str::from_utf8;

use futures::future::{self, Future};
use futures::stream::Stream;
use reqwest as r;
use reqwest::r#async as ra;

/*
 * TODO remove debug output
 * TODO reconnect
 * TODO improve error handling (less unwrap)
 * TODO consider lines split across chunks?
 */

pub type Error = String; // TODO enum

#[derive(Debug)]
// TODO can we make this require less copying?
pub struct Event {
    pub event_type: String,
    fields: Map<String, Vec<u8>>,
}

impl Event {
    fn new() -> Event {
        Event {
            event_type: "".to_string(),
            fields: Map::new(),
        }
    }

    pub fn field(&self, name: &str) -> &[u8] {
        &self.fields[name.into()]
    }

    fn set_field(&mut self, name: &str, value: &[u8]) {
        self.fields.insert(name.into(), value.to_owned());
    }
}

impl std::ops::Index<&str> for Event {
    type Output = [u8];

    fn index(&self, name: &str) -> &[u8] {
        &self.fields[name.into()]
    }
}

pub type EventStream = Box<Stream<Item = Event, Error = Error> + Send>;

pub struct ClientBuilder {
    url: r::Url,
    headers: r::header::HeaderMap,
}

impl ClientBuilder {
    pub fn header(mut self, key: &'static str, value: &str) -> ClientBuilder {
        self.headers.insert(key, value.parse().unwrap());
        self
    }

    pub fn build(self) -> Client {
        Client {
            url: self.url,
            headers: self.headers,
        }
    }
}

pub struct Client {
    url: r::Url,
    headers: r::header::HeaderMap,
}

impl Client {
    pub fn for_url<U: r::IntoUrl>(url: U) -> ClientBuilder {
        ClientBuilder {
            url: url.into_url().unwrap(),
            headers: r::header::HeaderMap::new(),
        }
    }

    pub fn stream(&mut self) -> EventStream {
        let http = ra::Client::new();
        let request = http.get(self.url.clone()).headers(self.headers.clone());
        let resp = request.send();

        let fut_stream_chunks = resp
            .and_then(|resp| {
                println!("resp: {:?}", resp);

                future::ok(resp.into_body())
            })
            .map_err(|e| {
                println!("error = {:?}", e);
                e
            });

        Box::new(
            fut_stream_chunks
                .flatten_stream()
                .map_err(|e| format!("error = {:?}", e).to_string())
                .map(|c| decode_chunk(c).expect("bad decode"))
                .filter_map(|opt| opt),
        )
    }
}

fn decode_chunk(chunk: ra::Chunk) -> Result<Option<Event>, Error> {
    println!("decoder got a chunk: {:?}", chunk);

    let mut event: Option<Event> = None;

    // TODO technically this doesn't handle newlines quite right.
    // The spec says lines are newline-terminated, rather than newline-separated
    // as this assumes, so we end up processing bogus empty strings.
    let lines = chunk.split(|b| &b'\n' == b);

    for line in lines {
        println!("splat: {:?}", from_utf8(line).unwrap());

        if line.is_empty() {
            println!("emptyline");
            return Ok(event);
        }

        match line[0] {
            b':' => {
                println!(
                    "comment: {}",
                    from_utf8(&line[1..]).unwrap_or("<bad utf-8>")
                );
                continue;
            }
            _ => match line.iter().position(|&b| b':' == b) {
                Some(colon_pos) => {
                    let key = &line[0..colon_pos];
                    let key = from_utf8(key).unwrap();
                    let value = &line[colon_pos + 1..];
                    let value = match value.iter().position(|&b| !b.is_ascii_whitespace()) {
                        Some(start) => &value[start..],
                        None => b"",
                    };

                    if event.is_none() {
                        event = Some(Event::new());
                    }
                    match key {
                        "event" => {
                            event.as_mut().unwrap().event_type =
                                from_utf8(value).unwrap().to_string()
                        }
                        _ => event.as_mut().unwrap().set_field(key, value),
                    };

                    println!("key: {}, value: {}", key, from_utf8(value).unwrap());
                }
                None => {
                    println!("some kind of weird line");
                }
            },
        }
    }

    Err("oops".to_string())
}
