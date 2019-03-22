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

#[derive(Clone, Debug)]
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
        self.field(name)
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

        //Box::new(
        //fut_stream_chunks
        //.flatten_stream()
        //.map_err(|e| format!("error = {:?}", e).to_string())
        //.map(|c| decode_chunk(c).expect("bad decode"))
        //.filter_map(|opt| opt),
        //)
        Box::new(Decoded::new(fut_stream_chunks.flatten_stream()))
    }
}

// Decode a chunk into an event, assuming that events are not split between
// chunks (i.e. that each chunk contains either 0 or 1 event).
// TODO that is FALSE
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

// TODO is all of the following unnecessary?
use futures::stream::Fuse;
use futures::{Async, Poll};

#[must_use = "streams do nothing unless polled"]
struct Decoded<S> {
    chunk_stream: Fuse<S>,
    incomplete_line: Option<Vec<u8>>,
    event: Option<Event>,
}

impl<S: Stream> Decoded<S> {
    fn new(s: S) -> Decoded<S> {
        return Decoded {
            chunk_stream: s.fuse(),
            incomplete_line: None,
            event: None,
        };
    }
}

impl<S> Stream for Decoded<S>
where
    S: Stream<Item = ra::Chunk>,
    S::Error: std::fmt::Display,
{
    type Item = Event;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Event>, Error> {
        println!("decoder poll!");

        let chunk = match self
            .chunk_stream
            .poll()
            .map_err(|e| format!("stream error: {}", e).to_string())?
        {
            Async::Ready(Some(c)) => c,
            Async::Ready(None) | Async::NotReady => {
                if self.chunk_stream.is_done() {
                    println!("decoder is done");
                    return Ok(Async::Ready(None));
                } else {
                    println!("decoder is not ready");
                    return Ok(Async::NotReady);
                }
            }
        };

        println!("decoder got a chunk: {:?}", chunk);

        let chunk = if chunk[chunk.len() - 1] == b'\n' {
            // strip off final newline so that .split below doesn't yield a
            // bogus empty string as the last "line"
            &chunk[..chunk.len() - 1]
        } else {
            println!("Chunk does not end with newline!");
            // TODO
            &chunk
        };

        let lines = chunk.split(|&b| b'\n' == b);
        let mut seen_empty_line = false;

        for line in lines {
            println!("Line: {}", from_utf8(line).unwrap());

            if line.is_empty() {
                println!("emptyline");
                seen_empty_line = true;
                continue;
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

                        if self.event.is_none() {
                            self.event = Some(Event::new());
                        }
                        match key {
                            "event" => {
                                self.event.as_mut().unwrap().event_type =
                                    from_utf8(value).unwrap().to_string()
                            }
                            _ => self.event.as_mut().unwrap().set_field(key, value),
                        };

                        println!("key: {}, value: {}", key, from_utf8(value).unwrap());
                    }
                    None => {
                        println!("some kind of weird line");
                    }
                },
            }
        }

        println!(
            "seen empty line: {} (event is {:?})",
            seen_empty_line,
            self.event.as_ref().map(|_| "<event>")
        );

        /*TODO wrong! need to poll in a loop*/

        match (seen_empty_line, &self.event) {
            (_, None) => Ok(Async::NotReady), // TODO wrong
            (true, Some(event)) => {
                let event = event.clone();
                self.event = None;
                Ok(Async::Ready(Some(event)))
            }
            (false, Some(_)) => {
                println!("Haven't seen an empty line in this whole chunk, weird");
                Ok(Async::NotReady) // TODO wrong
            }
        }
    }
}
