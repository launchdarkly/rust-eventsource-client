use std::collections::BTreeMap as Map;

use futures::future::{self, Future};
use futures::stream::Stream;
use reqwest as r;
use reqwest::r#async as ra;

/*
 * TODO decode stream into events
 * TODO remove debug output
 * TODO retry
 */

pub type Error = String; // TODO enum

#[derive(Debug)]
// TODO can we make this require less copying?
pub struct Event {
    pub event_type: String,
    fields: Map<String, Vec<u8>>,
}

impl Event {
    fn new(event_type: &str) -> Event {
        Event {
            event_type: event_type.to_owned(),
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
    //http: ra::Client,
    request_builder: ra::RequestBuilder,
}

impl ClientBuilder {
    pub fn header(mut self, key: &str, value: &str) -> ClientBuilder {
        self.request_builder = self.request_builder.header(key, value);
        self
    }

    pub fn build(self) -> Client {
        Client {
            request_builder: self.request_builder,
        }
    }
}

pub struct Client {
    request_builder: ra::RequestBuilder,
}

impl Client {
    pub fn for_url<U: r::IntoUrl>(url: U) -> ClientBuilder {
        let http = ra::Client::new();
        let builder = http.get(url);
        ClientBuilder {
            //http,
            request_builder: builder,
        }
    }

    pub fn stream(self) -> EventStream {
        let resp = self.request_builder.send();

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
                .map(|chunk| {
                    let nonsense = format!("{:?}", chunk);
                    let mut event = Event::new(&nonsense);
                    event.set_field("foo", b"bar");
                    event
                })
                .map_err(|e| e.to_string()),
        )
    }
}
