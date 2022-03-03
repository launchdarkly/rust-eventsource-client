use std::{collections::VecDeque, convert::TryFrom, str::from_utf8};

use hyper::body::Bytes;
use log::{debug, log_enabled, trace};
use pin_project::pin_project;

use super::error::{Error, Result};

#[derive(Default, PartialEq)]
struct EventData {
    pub event_type: String,
    pub data: Vec<u8>,
    pub id: Vec<u8>,
    pub retry: Option<u64>,
    pub comment: Vec<u8>,
}

impl EventData {
    fn new() -> Self {
        Self::default()
    }

    pub fn append_data(&mut self, value: &[u8]) {
        self.data.extend(value.to_vec());
        self.data.push(b'\n');
    }

    pub fn set_id(&mut self, value: &[u8]) {
        // TODO(mmk) If value has a null code point, ignore this assignment
        // https://github.com/whatwg/html/issues/689
        self.id = value.to_vec();
    }
}

#[derive(Debug, PartialEq)]
pub enum SSE {
    Event(Event),
    Comment(Vec<u8>),
}

impl TryFrom<EventData> for Option<SSE> {
    type Error = Error;

    fn try_from(event_data: EventData) -> std::result::Result<Self, Self::Error> {
        let mut default = EventData::default();
        if event_data == default {
            return Err(Error::InvalidEvent);
        }

        default.comment = event_data.comment.clone();
        if event_data == default {
            return Ok(Some(SSE::Comment(event_data.comment)));
        }

        if event_data.data.is_empty() {
            return Ok(None);
        }

        let event_type = match event_data.event_type.is_empty() {
            true => String::from("message"),
            false => event_data.event_type,
        };

        let mut data = event_data.data;
        data.truncate(data.len() - 1);

        Ok(Some(SSE::Event(Event {
            event_type,
            data,
            id: event_data.id,
            retry: event_data.retry,
        })))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub event_type: String,
    pub data: Vec<u8>,
    pub id: Vec<u8>,
    pub retry: Option<u64>,
}

impl Event {
    pub fn append_data(&mut self, value: &[u8]) {
        self.data.extend(value.to_vec());
        self.data.push(b'\n');
    }

    pub fn set_id(&mut self, value: &[u8]) {
        // TODO(mmk) If value has a null code point, ignore this assignment
        // https://github.com/whatwg/html/issues/689
        self.id = value.to_vec();
    }
}

const LOGIFY_MAX_CHARS: usize = 100;
fn logify(bytes: &[u8]) -> &str {
    let stringified = from_utf8(bytes).unwrap_or("<bad utf8>");
    if stringified.len() <= LOGIFY_MAX_CHARS {
        stringified
    } else {
        &stringified[..LOGIFY_MAX_CHARS - 1]
    }
}

fn parse_field(line: &[u8]) -> Result<Option<(&str, &[u8])>> {
    if line.is_empty() {
        return Err(Error::InvalidLine(
            "should never try to parse an empty line (probably a bug)".into(),
        ));
    }

    match line.iter().position(|&b| b':' == b) {
        Some(0) => {
            let value = &line[1..];
            debug!("comment: {}", from_utf8(value).unwrap_or("<bad utf-8>"));
            Ok(Some(("comment", value)))
        }
        Some(colon_pos) => {
            let key = &line[0..colon_pos];
            let key = parse_key(key)?;

            let mut value = &line[colon_pos + 1..];
            // remove the first initial space character if any (but remove no other whitespace)
            if value.starts_with(b" ") {
                value = &value[1..];
            }

            debug!("key: {}, value: {}", key, logify(value));

            Ok(Some((key, value)))
        }
        None => Ok(Some((parse_key(line)?, b""))),
    }
}

fn parse_key(key: &[u8]) -> Result<&str> {
    from_utf8(key).map_err(|e| Error::InvalidLine(format!("malformed key: {:?}", e)))
}

#[pin_project]
#[must_use = "streams do nothing unless polled"]
pub struct EventParser {
    /// buffer for lines we know are complete (terminated) but not yet parsed into event fields, in
    /// the order received
    complete_lines: VecDeque<Vec<u8>>,
    /// buffer for the most-recently received line, pending completion (by a newline terminator) or
    /// extension (by more non-newline bytes)
    incomplete_line: Option<Vec<u8>>,
    /// flagged if the last character processed as a carriage return; used to help process CRLF
    /// pairs
    last_char_was_cr: bool,
    /// the event data currently being decoded
    event_data: Option<EventData>,

    sse: VecDeque<SSE>,
}

impl EventParser {
    pub fn new() -> Self {
        Self {
            complete_lines: VecDeque::with_capacity(10),
            incomplete_line: None,
            last_char_was_cr: false,
            event_data: None,
            sse: VecDeque::with_capacity(3),
        }
    }

    pub fn was_processing(&self) -> bool {
        if self.incomplete_line.is_some() || !self.complete_lines.is_empty() {
            true
        } else {
            !self.sse.is_empty()
        }
    }

    pub fn get_event(&mut self) -> Option<SSE> {
        self.sse.pop_front()
    }

    pub fn process_bytes(&mut self, bytes: Bytes) -> Result<()> {
        trace!("Parsing bytes {:?}", bytes);
        // We get bytes from the underlying stream in chunks.  Decoding a chunk has two phases:
        // decode the chunk into lines, and decode the lines into events.
        //
        // We counterintuitively do these two phases in reverse order. Because both lines and
        // events may be split across chunks, we need to ensure we have a complete
        // (newline-terminated) line before parsing it, and a complete event
        // (empty-line-terminated) before returning it. So we buffer lines between poll()
        // invocations, and begin by processing any incomplete events from previous invocations,
        // before requesting new input from the underlying stream and processing that.

        self.decode_and_buffer_lines(bytes);
        self.parse_complete_lines_into_event()?;

        Ok(())
    }

    // Populate the event fields from the complete lines already seen, until we either encounter an
    // empty line - indicating we've decoded a complete event - or we run out of complete lines to
    // process.
    //
    // Returns the event for dispatch if it is complete.
    fn parse_complete_lines_into_event(&mut self) -> Result<()> {
        loop {
            let mut seen_empty_line = false;

            while let Some(line) = self.complete_lines.pop_front() {
                if line.is_empty() && self.event_data.is_some() {
                    seen_empty_line = true;
                    break;
                } else if line.is_empty() {
                    continue;
                }

                if let Some((key, value)) = parse_field(&line)? {
                    let event_data = self.event_data.get_or_insert_with(EventData::new);
                    if key == "comment" {
                        event_data.comment = value.to_vec();
                    } else if key == "event" {
                        event_data.event_type = from_utf8(value)
                            .map_err(Error::InvalidEventType)?
                            .to_string();
                    } else if key == "data" {
                        event_data.append_data(value);
                    } else if key == "id" {
                        event_data.set_id(value);
                    } else if key == "retry" {
                        match from_utf8(value).unwrap_or("").parse::<u64>() {
                            Ok(retry) => event_data.retry = Some(retry),
                            _ => debug!("Failed to parse {:?} into retry value", value),
                        };
                    }
                }
            }

            if seen_empty_line {
                let event_data = self.event_data.take();

                trace!(
                    "seen empty line, event_data is {:?})",
                    self.event_data
                        .as_ref()
                        .map(|event_data| &event_data.event_type)
                );

                if let Some(event_data) = event_data {
                    match Option::<SSE>::try_from(event_data) {
                        Err(e) => return Err(e),
                        Ok(None) => (),
                        Ok(Some(event)) => self.sse.push_back(event),
                    };
                }

                continue;
            } else {
                trace!("processed all complete lines but event_data not yet complete");
            }

            break;
        }

        Ok(())
    }

    // Decode a chunk into lines and buffer them for subsequent parsing, taking account of
    // incomplete lines from previous chunks.
    fn decode_and_buffer_lines(&mut self, chunk: Bytes) {
        let mut lines = chunk.split_inclusive(|&b| b == b'\n' || b == b'\r');

        // The first and last elements in this split are special. The spec requires lines to be
        // terminated. But lines may span chunks, so:
        //  * the last line, if non-empty (i.e. if chunk didn't end with a line terminator),
        //    should be buffered as an incomplete line
        //  * the first line should be appended to the incomplete line, if any

        if let Some(incomplete_line) = self.incomplete_line.as_mut() {
            let line = lines
                .next()
                // split always returns at least one item
                .unwrap();
            trace!(
                "extending line from previous chunk: {:?}+{:?}",
                logify(incomplete_line),
                logify(line)
            );

            self.last_char_was_cr = false;
            if !line.is_empty() {
                // Checking the last character handles lines where the last character is a
                // terminator, but also where the entire line is a terminator.
                match line.last().unwrap() {
                    b'\r' => {
                        incomplete_line.extend_from_slice(&line[..line.len() - 1]);
                        let il = self.incomplete_line.take();
                        self.complete_lines.push_back(il.unwrap());
                        self.last_char_was_cr = true;
                    }
                    b'\n' => {
                        incomplete_line.extend_from_slice(&line[..line.len() - 1]);
                        let il = self.incomplete_line.take();
                        self.complete_lines.push_back(il.unwrap());
                    }
                    _ => incomplete_line.extend_from_slice(line),
                };
            }
        }

        let mut lines = lines.peekable();
        while let Some(line) = lines.next() {
            if let Some(actually_complete_line) = self.incomplete_line.take() {
                // we saw the next line, so the previous one must have been complete after all
                trace!(
                    "previous line was complete: {:?}",
                    logify(&actually_complete_line)
                );
                self.complete_lines.push_back(actually_complete_line);
            }

            if self.last_char_was_cr && line == [b'\n'] {
                // This is a continuation of a \r\n pair, so we can ignore this line. We do need to
                // reset our flag though.
                self.last_char_was_cr = false;
                continue;
            }

            self.last_char_was_cr = false;
            if line.ends_with(&[b'\r']) {
                self.complete_lines
                    .push_back(line[..line.len() - 1].to_vec());
                self.last_char_was_cr = true;
            } else if line.ends_with(&[b'\n']) {
                // self isn't a continuation, but rather a line ending with a LF terminator.
                self.complete_lines
                    .push_back(line[..line.len() - 1].to_vec());
            } else if line.is_empty() {
                // this is the last line and it's empty, no need to buffer it
                trace!("chunk ended with a line terminator");
            } else if lines.peek().is_some() {
                // this line isn't the last and we know from previous checks it doesn't end in a
                // terminator, so we can consider it complete
                self.complete_lines.push_back(line.to_vec());
            } else {
                // last line needs to be buffered as it may be incomplete
                trace!("buffering incomplete line: {:?}", logify(line));
                self.incomplete_line = Some(line.to_vec());
            }
        }

        if log_enabled!(log::Level::Trace) {
            for line in &self.complete_lines {
                trace!("complete line: {:?}", logify(line));
            }
            if let Some(line) = &self.incomplete_line {
                trace!("incomplete line: {:?}", logify(line));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Error::*, *};
    use test_case::test_case;

    fn field<'a>(key: &'a str, value: &'a [u8]) -> Result<Option<(&'a str, &'a [u8])>> {
        Ok(Some((key, value)))
    }

    #[test]
    fn test_parse_field_invalid() {
        assert!(parse_field(b"").is_err());

        match parse_field(b"\x80: invalid UTF-8") {
            Err(InvalidLine(msg)) => assert!(msg.contains("Utf8Error")),
            res => panic!("expected InvalidLine error, got {:?}", res),
        }
    }

    #[test]
    fn test_parse_field_comments() {
        assert_eq!(parse_field(b":"), field("comment", b""));
        assert_eq!(
            parse_field(b":hello \0 world"),
            field("comment", b"hello \0 world")
        );
        assert_eq!(parse_field(b":event: foo"), field("comment", b"event: foo"));
    }

    #[test]
    fn test_parse_field_valid() {
        assert_eq!(parse_field(b"event:foo"), field("event", b"foo"));
        assert_eq!(parse_field(b"event: foo"), field("event", b"foo"));
        assert_eq!(parse_field(b"event:  foo"), field("event", b" foo"));
        assert_eq!(parse_field(b"event:\tfoo"), field("event", b"\tfoo"));
        assert_eq!(parse_field(b"event: foo "), field("event", b"foo "));

        assert_eq!(parse_field(b"disconnect:"), field("disconnect", b""));
        assert_eq!(parse_field(b"disconnect: "), field("disconnect", b""));
        assert_eq!(parse_field(b"disconnect:  "), field("disconnect", b" "));
        assert_eq!(parse_field(b"disconnect:\t"), field("disconnect", b"\t"));

        assert_eq!(parse_field(b"disconnect"), field("disconnect", b""));

        assert_eq!(parse_field(b" : foo"), field(" ", b"foo"));
        assert_eq!(parse_field(b"\xe2\x98\x83: foo"), field("☃", b"foo"));
    }

    fn event(typ: &str, data: &[u8], id: &[u8]) -> SSE {
        SSE::Event(Event {
            data: data.to_vec(),
            id: id.to_vec(),
            event_type: typ.to_string(),
            retry: None,
        })
    }

    #[test]
    fn test_event_without_data_yields_no_event() {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from("id: abc\n\n")).is_ok());
        assert!(parser.get_event().is_none());
    }

    #[test_case("event: add\ndata: hello\n\n", "add".into())]
    #[test_case("data: hello\n\n", "message".into())]
    fn test_event_can_parse_type_correctly(chunk: &'static str, event_type: String) {
        let mut parser = EventParser::new();

        assert!(parser.process_bytes(Bytes::from(chunk)).is_ok());
        if let Some(SSE::Event(event)) = parser.get_event() {
            assert_eq!(event_type, event.event_type);
        } else {
            panic!("Event should have been received")
        }
    }

    #[test_case("data: hello\n\n", event("message", &b"hello"[..], &b""[..]); "parses event body with LF")]
    #[test_case("data: hello\n\r", event("message", &b"hello"[..], &b""[..]); "parses event body with LF and trailing CR")]
    #[test_case("data: hello\r\n\n", event("message", &b"hello"[..], &b""[..]); "parses event body with CRLF")]
    #[test_case("data: hello\r\n\r", event("message", &b"hello"[..], &b""[..]); "parses event body with CRLF and trailing CR")]
    #[test_case("data: hello\r\r", event("message", &b"hello"[..], &b""[..]); "parses event body with CR")]
    #[test_case("data: hello\r\r\n", event("message", &b"hello"[..], &b""[..]); "parses event body with CR and trailing CRLF")]
    fn test_decode_chunks_simple(chunk: &'static str, event: SSE) {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(chunk)).is_ok());
        assert_eq!(parser.get_event().unwrap(), event);
        assert!(parser.get_event().is_none());
    }

    #[test_case(b":hello\n"; "with LF")]
    #[test_case(b":hello\r"; "with CR")]
    #[test_case(b":hello\r\n"; "with CRLF")]
    fn test_decode_chunks_comments_are_ignored(chunk: &'static [u8]) {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(chunk)).is_ok());
        assert!(parser.get_event().is_none());
    }

    #[test_case(&["data:", "hello\n\n"], event("message", &b"hello"[..], &b""[..]); "data split")]
    #[test_case(&["data:hell", "o\n\n"], event("message", &b"hello"[..], &b""[..]); "data truncated")]
    fn test_decode_message_split_across_chunks(chunks: &[&'static str], event: SSE) {
        let mut parser = EventParser::new();

        if let Some((last, chunks)) = chunks.split_last() {
            for chunk in chunks {
                assert!(parser.process_bytes(Bytes::from(*chunk)).is_ok());
                assert!(parser.get_event().is_none());
            }

            assert!(parser.process_bytes(Bytes::from(*last)).is_ok());
            assert_eq!(parser.get_event(), Some(event));
            assert!(parser.get_event().is_none());
        } else {
            panic!("Failed to split last");
        }
    }

    #[test_case(&["data:hell", "o\n\ndata:", "world\n\n"], &[event("message", &b"hello"[..], &b""[..]), event("message", &b"world"[..], &b""[..])]; "with lf")]
    #[test_case(&["data:hell", "o\r\rdata:", "world\r\r"], &[event("message", &b"hello"[..], &b""[..]), event("message", &b"world"[..], &b""[..])]; "with cr")]
    #[test_case(&["data:hell", "o\r\n\ndata:", "world\r\n\n"], &[event("message", &b"hello"[..], &b""[..]), event("message", &b"world"[..], &b""[..])]; "with crlf")]
    fn test_decode_multiple_messages_split_across_chunks(chunks: &[&'static str], events: &[SSE]) {
        let mut parser = EventParser::new();

        for chunk in chunks {
            assert!(parser.process_bytes(Bytes::from(*chunk)).is_ok());
        }

        for event in events {
            assert_eq!(parser.get_event().unwrap(), *event);
        }

        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_decode_line_split_across_chunks() {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from("data:foo")).is_ok());
        assert!(parser.process_bytes(Bytes::from("")).is_ok());
        assert!(parser.process_bytes(Bytes::from("baz\n\n")).is_ok());
        assert_eq!(
            parser.get_event(),
            Some(event("message", &b"foobaz"[..], &b""[..]))
        );
        assert!(parser.get_event().is_none());

        assert!(parser.process_bytes(Bytes::from("data:foo")).is_ok());
        assert!(parser.process_bytes(Bytes::from("bar")).is_ok());
        assert!(parser.process_bytes(Bytes::from("baz\n\n")).is_ok());
        assert_eq!(
            parser.get_event(),
            Some(event("message", &b"foobarbaz"[..], &b""[..]))
        );
        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_decode_concatenates_multiple_values_for_same_field() {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from("data:hello\n")).is_ok());
        assert!(parser.process_bytes(Bytes::from("data:world\n\n")).is_ok());
        assert_eq!(
            parser.get_event(),
            Some(event("message", &b"hello\nworld"[..], &b""[..]))
        );
        assert!(parser.get_event().is_none());
    }

    #[test_case("\n\n\n\n" ; "all LFs")]
    #[test_case("\r\r\r\r" ; "all CRs")]
    #[test_case("\r\n\r\n\r\n\r\n" ; "all CRLFs")]
    fn test_decode_repeated_terminators(chunk: &'static str) {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(chunk)).is_ok());

        // spec seems unclear on whether this should actually dispatch empty events, but that seems
        // unhelpful for all practical purposes
        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_decode_extra_terminators_between_events() {
        let mut parser = EventParser::new();
        assert!(parser
            .process_bytes(Bytes::from("data: abc\n\n\ndata: def\n\n"))
            .is_ok());

        assert_eq!(
            parser.get_event(),
            Some(event("message", &b"abc"[..], &b""[..]))
        );
        assert_eq!(
            parser.get_event(),
            Some(event("message", &b"def"[..], &b""[..]))
        );
        assert!(parser.get_event().is_none());
    }

    #[test_case("one-event.sse"; "one-event.sse")]
    #[test_case("one-event-crlf.sse"; "one-event-crlf.sse")]
    fn test_decode_one_event(file: &str) {
        let contents = read_contents_from_file(file);
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(contents)).is_ok());

        if let Some(SSE::Event(event)) = parser.get_event() {
            assert_eq!(event.event_type, "patch");
            let data = event_data(&event).expect("event data should parse");
            assert!(data.contains(r#"path":"/flags/goals.02.featureWithGoals"#));
        } else {
            panic!("Should have returned an event");
        }
    }

    #[test_case("two-events.sse"; "two-events.sse")]
    #[test_case("two-events-crlf.sse"; "two-events-crlf.sse")]
    fn test_decode_two_events(file: &str) {
        let contents = read_contents_from_file(file);
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(contents)).is_ok());

        if let Some(SSE::Event(event)) = parser.get_event() {
            assert_eq!(event.event_type, "one");
            let data = event_data(&event).expect("event data should parse");
            assert_eq!(data, "One");
        } else {
            panic!("Should have returned an event");
        }

        if let Some(SSE::Event(event)) = parser.get_event() {
            assert_eq!(event.event_type, "two");
            let data = event_data(&event).expect("event data should parse");
            assert_eq!(data, "Two");
        } else {
            panic!("Should have returned an event");
        }
    }

    #[test_case("big-event-followed-by-another.sse"; "big-event-followed-by-another.sse")]
    #[test_case("big-event-followed-by-another-crlf.sse"; "big-event-followed-by-another-crlf.sse")]
    fn test_decode_big_event_followed_by_another(file: &str) {
        let contents = read_contents_from_file(file);
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(contents)).is_ok());

        if let Some(SSE::Event(event)) = parser.get_event() {
            assert_eq!(event.event_type, "patch");
            let data = event_data(&event).expect("event data should parse");
            assert!(data.len() > 10_000);
            assert!(data.contains(r#"path":"/flags/big.00.bigFeatureKey"#));
        } else {
            panic!("Should have returned an event");
        }

        if let Some(SSE::Event(event)) = parser.get_event() {
            assert_eq!(event.event_type, "patch");
            let data = event_data(&event).expect("event data should parse");
            assert!(data.contains(r#"path":"/flags/goals.02.featureWithGoals"#));
        } else {
            panic!("Should have returned an event");
        }
    }

    fn read_contents_from_file(name: &str) -> Vec<u8> {
        std::fs::read(format!("test-data/{}", name))
            .unwrap_or_else(|_| panic!("couldn't read {}", name))
    }

    fn event_data(event: &Event) -> std::result::Result<&str, String> {
        let data = &event.data;
        from_utf8(data).map_err(|e| format!("invalid UTF-8: {}", e))
    }
}
