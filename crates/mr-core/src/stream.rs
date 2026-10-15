//! Streaming reader for the `Saved Media` array of `memories_history.json`.
//!
//! The file is consumed through a small fixed buffer. Only one array element
//! is ever materialised at a time (as bytes, then as a decoded object), so a
//! file of any size parses in constant memory (FR-10, NFR-PERF-02). A UTF-8
//! byte-order mark, CRLF line endings, unknown top-level keys before or after
//! the array, and trailing commas are all tolerated.
//!
//! Design: a hand-written scanner finds element boundaries by tracking string
//! and bracket state; each element's bytes are then handed to `serde_json`.
//! `serde_json`'s own `StreamDeserializer` was not used because it reads
//! whitespace-separated values, not comma-separated array elements, and a
//! reader-backed `Deserializer` may hold a peeked byte that is lost between
//! elements.

use std::io::{self, Read};

use serde_json::Value;
use thiserror::Error;

use crate::entry::{ParsedEntry, RawEntry, parse_entry};

/// Default cap on the byte size of a single array element.
pub const DEFAULT_MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;

const BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum StreamError {
    #[error("I/O error while reading memories_history.json: {0}")]
    Io(#[from] io::Error),
    #[error("memories_history.json does not start with a JSON object (byte {offset})")]
    NotAnObject { offset: u64 },
    #[error("memories_history.json has no \"Saved Media\" array")]
    SavedMediaMissing,
    #[error("\"Saved Media\" is not an array (byte {offset})")]
    SavedMediaNotArray { offset: u64 },
    #[error("malformed JSON at byte {offset}: {detail}")]
    Syntax { offset: u64, detail: &'static str },
    #[error("unexpected end of file at byte {offset}")]
    UnexpectedEof { offset: u64 },
    #[error("JSON value at byte {offset} is larger than the {limit}-byte limit")]
    ValueTooLarge { offset: u64, limit: usize },
    /// Recoverable: the iterator continues with the next element.
    #[error("Saved Media[{index}] is not a JSON object (byte {offset})")]
    EntryNotObject { index: usize, offset: u64 },
    /// Recoverable: the iterator continues with the next element.
    #[error("Saved Media[{index}] is malformed: {source}")]
    EntryJson {
        index: usize,
        #[source]
        source: serde_json::Error,
    },
}

impl StreamError {
    /// `true` when iteration can continue after this error.
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            StreamError::EntryNotObject { .. } | StreamError::EntryJson { .. }
        )
    }
}

/// Byte source with one-byte lookahead over a fixed buffer.
struct Bytes<R: Read> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    offset: u64,
    eof: bool,
}

impl<R: Read> Bytes<R> {
    fn new(inner: R) -> Self {
        Bytes {
            inner,
            buf: vec![0u8; BUFFER_BYTES].into_boxed_slice(),
            pos: 0,
            len: 0,
            offset: 0,
            eof: false,
        }
    }

    fn fill(&mut self) -> io::Result<()> {
        if self.pos < self.len || self.eof {
            return Ok(());
        }
        loop {
            match self.inner.read(&mut self.buf) {
                Ok(0) => {
                    self.eof = true;
                    self.pos = 0;
                    self.len = 0;
                    return Ok(());
                }
                Ok(n) => {
                    self.pos = 0;
                    self.len = n;
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn peek(&mut self) -> io::Result<Option<u8>> {
        self.fill()?;
        Ok((self.pos < self.len).then(|| self.buf[self.pos]))
    }

    fn next(&mut self) -> io::Result<Option<u8>> {
        self.fill()?;
        if self.pos < self.len {
            let b = self.buf[self.pos];
            self.pos += 1;
            self.offset += 1;
            Ok(Some(b))
        } else {
            Ok(None)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Just after `[`: the next token is `]` or the first element.
    First,
    /// After an element: the next token is `,` or `]`.
    AfterElement,
    Done,
}

/// Iterator over the elements of `Saved Media`, yielding one [`RawEntry`] per
/// element without ever buffering the whole file.
pub struct SavedMediaReader<R: Read> {
    src: Bytes<R>,
    state: State,
    index: usize,
    scratch: Vec<u8>,
    skipped_keys: Vec<String>,
    max_entry_bytes: usize,
}

impl<R: Read> SavedMediaReader<R> {
    /// Open the stream and position it at the first element of `Saved Media`.
    /// Fails if the document is not an object or has no such array.
    pub fn new(reader: R) -> Result<Self, StreamError> {
        Self::with_limit(reader, DEFAULT_MAX_ENTRY_BYTES)
    }

    /// Like [`new`](Self::new) with a custom per-element size cap.
    pub fn with_limit(reader: R, max_entry_bytes: usize) -> Result<Self, StreamError> {
        let mut r = SavedMediaReader {
            src: Bytes::new(reader),
            state: State::Done,
            index: 0,
            scratch: Vec::with_capacity(1024),
            skipped_keys: Vec::new(),
            max_entry_bytes,
        };
        r.skip_bom()?;
        r.skip_ws()?;
        match r.src.next()? {
            Some(b'{') => {}
            Some(_) => {
                return Err(StreamError::NotAnObject {
                    offset: r.src.offset - 1,
                });
            }
            None => {
                return Err(StreamError::UnexpectedEof {
                    offset: r.src.offset,
                });
            }
        }
        loop {
            r.skip_ws()?;
            match r.src.peek()? {
                Some(b'"') => {}
                Some(b'}') => return Err(StreamError::SavedMediaMissing),
                Some(b',') => {
                    r.src.next()?;
                    continue;
                }
                Some(_) => return Err(r.syntax("expected a key")),
                None => return Err(r.eof()),
            }
            let key = r.read_key()?;
            r.skip_ws()?;
            if r.src.next()? != Some(b':') {
                return Err(r.syntax("expected ':' after key"));
            }
            r.skip_ws()?;
            if key.trim().eq_ignore_ascii_case("saved media") {
                return match r.src.next()? {
                    Some(b'[') => {
                        r.state = State::First;
                        Ok(r)
                    }
                    Some(_) => Err(StreamError::SavedMediaNotArray {
                        offset: r.src.offset - 1,
                    }),
                    None => Err(r.eof()),
                };
            }
            r.skipped_keys.push(key);
            r.scan_value(false)?;
            r.skip_ws()?;
            match r.src.peek()? {
                Some(b',') => {
                    r.src.next()?;
                }
                Some(b'}') => return Err(StreamError::SavedMediaMissing),
                Some(_) => return Err(r.syntax("expected ',' or '}' after value")),
                None => return Err(r.eof()),
            }
        }
    }

    /// Top-level keys that were skipped before `Saved Media` was found.
    pub fn skipped_keys(&self) -> &[String] {
        &self.skipped_keys
    }

    /// Number of elements yielded so far (including recoverable errors).
    pub fn elements_seen(&self) -> usize {
        self.index
    }

    /// Bytes consumed from the underlying reader so far.
    pub fn bytes_read(&self) -> u64 {
        self.src.offset
    }

    /// Adapter yielding fully parsed entries.
    pub fn parsed(self) -> impl Iterator<Item = Result<ParsedEntry, StreamError>> {
        self.map(|r| r.map(|raw| parse_entry(&raw)))
    }

    fn syntax(&self, detail: &'static str) -> StreamError {
        StreamError::Syntax {
            offset: self.src.offset,
            detail,
        }
    }

    fn eof(&self) -> StreamError {
        StreamError::UnexpectedEof {
            offset: self.src.offset,
        }
    }

    fn skip_bom(&mut self) -> Result<(), StreamError> {
        if self.src.peek()? == Some(0xEF) {
            self.src.next()?;
            if self.src.next()? != Some(0xBB) || self.src.next()? != Some(0xBF) {
                return Err(self.syntax("bad byte-order mark"));
            }
        }
        Ok(())
    }

    fn skip_ws(&mut self) -> Result<(), StreamError> {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.src.peek()? {
            self.src.next()?;
        }
        Ok(())
    }

    fn read_key(&mut self) -> Result<String, StreamError> {
        self.scan_value(true)?;
        let bytes = std::mem::take(&mut self.scratch);
        let key = serde_json::from_slice::<String>(&bytes)
            .map_err(|_| self.syntax("invalid key string"))?;
        self.scratch = bytes;
        Ok(key)
    }

    /// Consume exactly one JSON value. With `keep`, its bytes are left in
    /// `self.scratch`; otherwise they are discarded as they stream past.
    fn scan_value(&mut self, keep: bool) -> Result<(), StreamError> {
        self.scratch.clear();
        let start = self.src.offset;
        let limit = self.max_entry_bytes;
        let mut kept = 0usize;
        macro_rules! push {
            ($b:expr) => {{
                // Bind first: the argument must be evaluated whether or not
                // the byte is kept.
                let b: u8 = $b;
                kept += 1;
                if kept > limit {
                    return Err(StreamError::ValueTooLarge {
                        offset: start,
                        limit,
                    });
                }
                if keep {
                    self.scratch.push(b);
                }
            }};
        }
        match self.src.peek()? {
            None => Err(self.eof()),
            Some(b'{' | b'[') => {
                let mut depth = 0usize;
                let mut in_string = false;
                let mut escaped = false;
                loop {
                    let Some(b) = self.src.next()? else {
                        return Err(self.eof());
                    };
                    push!(b);
                    if in_string {
                        if escaped {
                            escaped = false;
                        } else if b == b'\\' {
                            escaped = true;
                        } else if b == b'"' {
                            in_string = false;
                        }
                        continue;
                    }
                    match b {
                        b'"' => in_string = true,
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return Ok(());
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(b'"') => {
                push!(self.src.next()?.expect("peeked"));
                let mut escaped = false;
                loop {
                    let Some(b) = self.src.next()? else {
                        return Err(self.eof());
                    };
                    push!(b);
                    if escaped {
                        escaped = false;
                    } else if b == b'\\' {
                        escaped = true;
                    } else if b == b'"' {
                        return Ok(());
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => {
                while let Some(b @ (b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')) =
                    self.src.peek()?
                {
                    self.src.next()?;
                    push!(b);
                }
                Ok(())
            }
            Some(b't' | b'f' | b'n') => {
                while let Some(b) = self.src.peek()? {
                    if !b.is_ascii_alphabetic() {
                        break;
                    }
                    self.src.next()?;
                    push!(b);
                }
                Ok(())
            }
            Some(_) => Err(self.syntax("unexpected byte where a value should start")),
        }
    }

    fn next_element(&mut self) -> Result<Option<RawEntry>, StreamError> {
        match self.state {
            State::Done => return Ok(None),
            State::First => {
                self.skip_ws()?;
                if self.src.peek()? == Some(b']') {
                    self.src.next()?;
                    self.state = State::Done;
                    return Ok(None);
                }
            }
            State::AfterElement => {
                self.skip_ws()?;
                match self.src.next()? {
                    Some(b',') => {
                        self.skip_ws()?;
                        // Trailing comma before the closing bracket.
                        if self.src.peek()? == Some(b']') {
                            self.src.next()?;
                            self.state = State::Done;
                            return Ok(None);
                        }
                    }
                    Some(b']') => {
                        self.state = State::Done;
                        return Ok(None);
                    }
                    Some(_) => return Err(self.syntax("expected ',' or ']' after element")),
                    None => return Err(self.eof()),
                }
            }
        }
        let offset = self.src.offset;
        let index = self.index;
        self.index += 1;
        self.state = State::AfterElement;
        self.scan_value(true)?;
        let value: Value = serde_json::from_slice(&self.scratch)
            .map_err(|source| StreamError::EntryJson { index, source })?;
        match value {
            Value::Object(map) => Ok(Some(RawEntry::from_json_object(index, map))),
            _ => Err(StreamError::EntryNotObject { index, offset }),
        }
    }
}

impl<R: Read> std::fmt::Debug for SavedMediaReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SavedMediaReader")
            .field("state", &self.state)
            .field("index", &self.index)
            .field("bytes_read", &self.src.offset)
            .field("skipped_keys", &self.skipped_keys)
            .finish_non_exhaustive()
    }
}

impl<R: Read> Iterator for SavedMediaReader<R> {
    type Item = Result<RawEntry, StreamError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_element() {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => None,
            Err(e) => {
                if !e.is_recoverable() {
                    self.state = State::Done;
                }
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn read_all(text: &[u8]) -> Vec<RawEntry> {
        SavedMediaReader::new(Cursor::new(text))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    const SAMPLE: &str = r#"{
  "Saved Media": [
    {
      "Date": "2021-12-25 18:42:07 UTC",
      "Media Type": "Image",
      "Location": "Latitude, Longitude: 51.50735, -0.12776",
      "Download Link": "",
      "Media Download Url": "N/A"
    },
    {
      "Date": "2019-07-04 21:03:55 UTC",
      "Media Type": "Video",
      "Location": "Latitude, Longitude: 0.0, 0.0",
      "Media Download Url": ""
    }
  ]
}"#;

    #[test]
    fn reads_the_illustrative_2026_sample() {
        let entries = read_all(SAMPLE.as_bytes());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].index, 0);
        assert_eq!(entries[0].date.as_deref(), Some("2021-12-25 18:42:07 UTC"));
        assert_eq!(entries[0].media_type.as_deref(), Some("Image"));
        assert_eq!(entries[0].download_link.as_deref(), Some(""));
        assert_eq!(entries[1].index, 1);
        assert_eq!(entries[1].download_link, None);
        assert_eq!(entries[1].media_download_url.as_deref(), Some(""));
    }

    #[test]
    fn tolerates_bom_crlf_tabs_and_other_top_level_keys() {
        let mut text = Vec::new();
        text.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        text.extend_from_slice(
            b"{\r\n\t\"Export Info\": {\"Version\": 2, \"Tags\": [\"a\", \"]}\"]},\r\n\t\"Count\": 1,\r\n\t\"Note\": \"with \\\"quotes\\\" and ]\",\r\n\t\"Saved Media\": [\r\n\t\t{\"Date\": \"2021-12-25 18:42:07 UTC\"}\r\n\t],\r\n\t\"Trailer\": [1, 2, 3]\r\n}\r\n",
        );
        let reader = SavedMediaReader::new(Cursor::new(text)).unwrap();
        assert_eq!(reader.skipped_keys(), ["Export Info", "Count", "Note"]);
        let entries: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].date.as_deref(), Some("2021-12-25 18:42:07 UTC"));
    }

    #[test]
    fn key_matching_is_case_and_space_insensitive() {
        let entries = read_all(br#"{" saved MEDIA ": [{"date": "x"}]}"#);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].date.as_deref(), Some("x"));
    }

    #[test]
    fn empty_array_and_trailing_comma() {
        assert!(read_all(br#"{"Saved Media": []}"#).is_empty());
        assert!(read_all(br#"{"Saved Media": [ ]}"#).is_empty());
        assert_eq!(read_all(br#"{"Saved Media": [{}, {},]}"#).len(), 2);
    }

    #[test]
    fn strings_with_brackets_and_escapes_inside_entries() {
        // Second entry carries JSON escapes for an accented letter and a
        // surrogate-pair emoji (`\x5C` is a backslash, so the stream holds the
        // six-character escape sequences, not the characters).
        let text = concat!(
            r#"{"Saved Media": [{"Caption": "a ] } \" \\ ", "Date": "2021-12-25"}, "#,
            "{\"Date\": \"x\", \"Emoji\": \"caf\x5Cu00e9 \x5Cud83d\x5Cude00\"}]}"
        );
        let entries = read_all(text.as_bytes());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].unknown_fields, ["Caption"]);
        assert_eq!(entries[0].date.as_deref(), Some("2021-12-25"));
        assert_eq!(entries[1].unknown_fields, ["Emoji"]);
    }

    #[test]
    fn non_string_values_are_recorded_not_fatal() {
        let entries = read_all(
            br#"{"Saved Media": [{"Date": 12345, "Location": null, "Media Type": ["Image"]}]}"#,
        );
        assert_eq!(entries[0].date, None);
        assert_eq!(entries[0].location, None);
        assert_eq!(entries[0].non_string_fields, ["Date", "Media Type"]);
    }

    #[test]
    fn structural_errors() {
        let err = SavedMediaReader::new(Cursor::new(b"[]")).unwrap_err();
        assert!(
            matches!(err, StreamError::NotAnObject { offset: 0 }),
            "{err}"
        );
        let err = SavedMediaReader::new(Cursor::new(br#"{"Other": 1}"#)).unwrap_err();
        assert!(matches!(err, StreamError::SavedMediaMissing), "{err}");
        let err = SavedMediaReader::new(Cursor::new(br#"{"Saved Media": {}}"#)).unwrap_err();
        assert!(
            matches!(err, StreamError::SavedMediaNotArray { .. }),
            "{err}"
        );
        let err = SavedMediaReader::new(Cursor::new(b"")).unwrap_err();
        assert!(matches!(err, StreamError::UnexpectedEof { .. }), "{err}");
        let err = SavedMediaReader::new(Cursor::new(br#"{"Saved Media": "#)).unwrap_err();
        assert!(matches!(err, StreamError::UnexpectedEof { .. }), "{err}");
    }

    #[test]
    fn truncated_array_reports_eof_after_the_good_entries() {
        let text = br#"{"Saved Media": [{"Date": "a"}, {"Date": "b"}, {"Date": "c"#;
        let items: Vec<_> = SavedMediaReader::new(Cursor::new(text)).unwrap().collect();
        assert_eq!(items.len(), 3);
        assert!(items[0].is_ok() && items[1].is_ok());
        assert!(matches!(items[2], Err(StreamError::UnexpectedEof { .. })));
    }

    #[test]
    fn non_object_element_is_skipped_and_iteration_continues() {
        let text = br#"{"Saved Media": [{"Date": "a"}, 42, "str", {"Date": "b"}]}"#;
        let items: Vec<_> = SavedMediaReader::new(Cursor::new(text)).unwrap().collect();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].as_ref().unwrap().date.as_deref(), Some("a"));
        assert!(matches!(
            items[1],
            Err(StreamError::EntryNotObject { index: 1, .. })
        ));
        assert!(matches!(
            items[2],
            Err(StreamError::EntryNotObject { index: 2, .. })
        ));
        assert_eq!(items[3].as_ref().unwrap().date.as_deref(), Some("b"));
        assert_eq!(items[3].as_ref().unwrap().index, 3);
    }

    #[test]
    fn malformed_element_is_reported_and_iteration_continues() {
        let text = br#"{"Saved Media": [{"Date": "a",}, {"Date": "b"}]}"#;
        let items: Vec<_> = SavedMediaReader::new(Cursor::new(text)).unwrap().collect();
        assert_eq!(items.len(), 2);
        assert!(matches!(
            items[0],
            Err(StreamError::EntryJson { index: 0, .. })
        ));
        assert_eq!(items[1].as_ref().unwrap().date.as_deref(), Some("b"));
    }

    #[test]
    fn oversized_element_is_refused() {
        let big = format!(r#"{{"Saved Media": [{{"Blob": "{}"}}]}}"#, "x".repeat(2000));
        let items: Vec<_> = SavedMediaReader::with_limit(Cursor::new(big), 1024)
            .unwrap()
            .collect();
        assert!(matches!(
            items[0],
            Err(StreamError::ValueTooLarge { limit: 1024, .. })
        ));
        assert_eq!(items.len(), 1, "fatal: iteration stops");
    }

    /// A reader that manufactures the JSON on demand, so the document never
    /// exists in memory. Proves the parser works on a stream, not a slice.
    struct Synth {
        remaining: usize,
        total: usize,
        pending: Vec<u8>,
        pos: usize,
        started: bool,
        finished: bool,
    }

    impl Synth {
        fn new(total: usize) -> Self {
            Synth {
                remaining: total,
                total,
                pending: Vec::new(),
                pos: 0,
                started: false,
                finished: false,
            }
        }

        fn refill(&mut self) {
            self.pending.clear();
            self.pos = 0;
            if !self.started {
                self.started = true;
                self.pending.extend_from_slice(
                    b"\xEF\xBB\xBF{\"Export\": {\"v\": 1},\r\n \"Saved Media\": [\r\n",
                );
                return;
            }
            if self.remaining > 0 {
                let i = self.total - self.remaining;
                self.remaining -= 1;
                let sep = if i == 0 { "" } else { ",\r\n" };
                let entry = format!(
                    "{sep}  {{\"Date\": \"2021-01-{:02} 10:{:02}:{:02} UTC\", \"Media Type\": \"{}\", \"Location\": \"Latitude, Longitude: {}.{}, -0.{}\", \"Download Link\": \"\", \"Media Download Url\": \"N/A\"}}",
                    (i % 28) + 1,
                    i % 60,
                    (i / 60) % 60,
                    if i.is_multiple_of(3) {
                        "Video"
                    } else {
                        "Image"
                    },
                    40 + i % 10,
                    i % 1000,
                    i % 777,
                );
                self.pending.extend_from_slice(entry.as_bytes());
                return;
            }
            if !self.finished {
                self.finished = true;
                self.pending
                    .extend_from_slice(b"\r\n], \"Trailer\": null}\r\n");
            }
        }
    }

    impl Read for Synth {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.pending.len() {
                self.refill();
                if self.pending.is_empty() {
                    return Ok(0);
                }
            }
            // Deliberately small reads to exercise buffer boundaries.
            let n = out.len().min(self.pending.len() - self.pos).min(7);
            out[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn streams_a_synthetic_document_of_100k_entries() {
        let total = 100_000;
        let reader = SavedMediaReader::new(Synth::new(total)).unwrap();
        assert_eq!(reader.skipped_keys(), ["Export"]);
        let mut summary = crate::entry::Summary::default();
        let mut count = 0;
        for entry in reader.parsed() {
            let entry = entry.unwrap();
            assert!(entry.warnings.is_empty(), "{:?}", entry.warnings);
            summary.record(&entry);
            count += 1;
        }
        assert_eq!(count, total);
        assert_eq!(summary.entries, total);
        assert_eq!(summary.videos, total.div_ceil(3));
        assert_eq!(summary.with_location, total);
        assert_eq!(summary.with_url, 0);
    }
}
