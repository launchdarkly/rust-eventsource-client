use std::collections::BTreeMap as Map;
use std::str::from_utf8;

use futures::future::{self, Future};
use futures::stream::Stream;
use reqwest as r;
use reqwest::r#async as ra;

/*
 * TODO remove debug output
 * TODO reconnect
 */

#[derive(Debug)]
pub enum Error {
    HttpRequest(Box<std::error::Error + Send + 'static>),
    HttpStream(Box<std::error::Error + Send + 'static>),
    InvalidLine(String),
}

#[derive(Clone, Debug)]
// TODO can we make this require less copying?
pub struct Event {
    pub event_type: String,
    fields: Map<String, Vec<u8>>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Event {
    fn new() -> Event {
        Event {
            event_type: "".to_string(),
            fields: Map::new(),
        }
    }

    pub fn field(&self, name: &str) -> Option<&[u8]> {
        self.fields.get(name.into()).map(|buf| buf.as_slice())
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
    pub fn header(mut self, key: &'static str, value: &str) -> Result<ClientBuilder> {
        let value = value.parse().map_err(|e| Error::HttpRequest(Box::new(e)))?;
        self.headers.insert(key, value);
        Ok(self)
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
    pub fn for_url<U: r::IntoUrl>(url: U) -> Result<ClientBuilder> {
        let url = url
            .into_url()
            .map_err(|e| Error::HttpRequest(Box::new(e)))?;
        Ok(ClientBuilder {
            url: url,
            headers: r::header::HeaderMap::new(),
        })
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

        Box::new(Decoded::new(fut_stream_chunks.flatten_stream()))
    }
}

fn parse_field(line: &[u8]) -> Result<Option<(&str, &[u8])>> {
    match line.iter().position(|&b| b':' == b) {
        Some(0) => {
            println!(
                "comment: {}",
                from_utf8(&line[1..]).unwrap_or("<bad utf-8>")
            );
            Ok(None)
        }
        Some(colon_pos) => {
            let key = &line[0..colon_pos];
            let key = from_utf8(key)
                .map_err(|e| Error::InvalidLine(format!("malformed key: {:?}", e)))?;
            let value = &line[colon_pos + 1..];
            let value = match value.iter().position(|&b| !b.is_ascii_whitespace()) {
                Some(start) => &value[start..],
                None => b"",
            };

            let mut val_for_printing = from_utf8(value).unwrap().to_string();
            val_for_printing.truncate(100);
            println!("key: {}, value: {}", key, val_for_printing);

            Ok(Some((key, value)))
        }
        None => Err(Error::InvalidLine("line missing ':' byte".to_string())),
    }
}

use futures::stream::Fuse;
use futures::{Async, Poll};

#[must_use = "streams do nothing unless polled"]
struct Decoded<S> {
    chunk_stream: Fuse<S>,
    incomplete_chunk: Option<Vec<u8>>,
    event: Option<Event>,
}

impl<S: Stream> Decoded<S> {
    fn new(s: S) -> Decoded<S> {
        return Decoded {
            chunk_stream: s.fuse(),
            incomplete_chunk: None,
            event: None,
        };
    }
}

impl<S> Stream for Decoded<S>
where
    S: Stream<Item = ra::Chunk>,
    S::Error: std::error::Error + Send + 'static,
{
    type Item = Event;
    type Error = Error;

    // TODO can this be simplified using tokio's Framed?
    fn poll(&mut self) -> Poll<Option<Event>, Error> {
        println!("decoder poll!");

        loop {
            let chunk = match try_ready!(self
                .chunk_stream
                .poll()
                .map_err(|e| Error::HttpStream(Box::new(e))))
            {
                Some(c) => c,
                None => {
                    return Ok(Async::Ready(None));
                }
            };

            let mut chunk_for_printing = from_utf8(&chunk).unwrap().to_string();
            chunk_for_printing.truncate(100);
            println!("decoder got a chunk: {:?}", chunk_for_printing);

            if self.incomplete_chunk.is_none() {
                self.incomplete_chunk = Some(chunk.to_vec());
            } else {
                self.incomplete_chunk
                    .as_mut()
                    .unwrap()
                    .extend(chunk.into_iter());
            }

            let incomplete_chunk = self.incomplete_chunk.as_ref().unwrap();
            let chunk = if incomplete_chunk.ends_with(b"\n") {
                // strip off final newline so that .split below doesn't yield a
                // bogus empty string as the last "line"
                &incomplete_chunk[..incomplete_chunk.len() - 1]
            } else {
                println!("Chunk does not end with newline!");
                continue;
            };

            let lines = chunk.split(|&b| b'\n' == b);
            let mut seen_empty_line = false;

            for line in lines {
                let mut line_for_printing = from_utf8(line).unwrap().to_string();
                line_for_printing.truncate(100);
                println!("Line: {}", line_for_printing);

                if line.is_empty() {
                    println!("emptyline");
                    seen_empty_line = true;
                    continue;
                }

                match parse_field(line) {
                    Ok(Some((key, value))) => {
                        if self.event.is_none() {
                            self.event = Some(Event::new());
                        }

                        let mut event = self.event.as_mut().unwrap();

                        if key == "event" {
                            match from_utf8(value) {
                                Ok(value) => event.event_type = value.to_string(),
                                Err(e) => {
                                    println!("Malformed event type: {:?}", e);
                                    continue;
                                }
                            }
                        } else {
                            event.set_field(key, value);
                        }
                    }
                    Ok(None) => (),
                    Err(e) => println!("couldn't parse line: {:?}", e),
                }
            }

            println!(
                "seen empty line: {} (event is {:?})",
                seen_empty_line,
                self.event.as_ref().map(|_| "<event>")
            );

            match (seen_empty_line, &self.event) {
                (_, None) => (),
                (true, Some(event)) => {
                    let event = event.clone();
                    self.event = None;
                    return Ok(Async::Ready(Some(event)));
                }
                (false, Some(_)) => {
                    println!("Haven't seen an empty line in this whole chunk, weird")
                }
            }
        }
    }
}
