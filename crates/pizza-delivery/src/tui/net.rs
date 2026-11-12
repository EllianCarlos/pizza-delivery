use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use pizza_delivery_core::view::{EventView, EventsView};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const STREAM_IDLE: Duration = Duration::from_secs(60);

fn connect(port: u16) -> Result<TcpStream, String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| e.to_string())
}

fn send_get(stream: &mut TcpStream, path: &str, accept: &str) -> Result<(), String> {
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: {accept}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| e.to_string())
}

fn read_status(reader: &mut impl BufRead) -> Result<u16, String> {
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "malformed http status".to_owned())
}

fn skip_headers(reader: &mut impl BufRead) -> Result<(), String> {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 || line == "\r\n" || line == "\n" {
            return Ok(());
        }
    }
}

pub fn get_events(port: u16, since: u64, limit: usize) -> Result<EventsView, String> {
    let mut stream = connect(port)?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| e.to_string())?;
    send_get(
        &mut stream,
        &format!("/api/events?since={since}&limit={limit}"),
        "application/json",
    )?;
    let mut reader = BufReader::new(stream);
    let status = read_status(&mut reader)?;
    skip_headers(&mut reader)?;
    let mut body = String::new();
    reader
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("daemon answered http {status}"));
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

pub struct Stream {
    reader: BufReader<TcpStream>,
    parser: SseParser,
}

pub fn open_stream(port: u16, since: u64) -> Result<Stream, String> {
    let mut stream = connect(port)?;
    stream
        .set_read_timeout(Some(STREAM_IDLE))
        .map_err(|e| e.to_string())?;
    send_get(
        &mut stream,
        &format!("/api/stream?since={since}"),
        "text/event-stream",
    )?;
    let mut reader = BufReader::new(stream);
    let status = read_status(&mut reader)?;
    if status != 200 {
        return Err(format!("daemon answered http {status}"));
    }
    skip_headers(&mut reader)?;
    Ok(Stream {
        reader,
        parser: SseParser::default(),
    })
}

impl Stream {
    pub fn next_event(&mut self) -> Result<EventView, String> {
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("stream closed".to_owned());
            }
            if let Some(event) = self.parser.push(line.trim_end_matches(['\r', '\n'])) {
                return Ok(event);
            }
        }
    }
}

#[derive(Default)]
pub struct SseParser {
    data: String,
}

impl SseParser {
    pub fn push(&mut self, line: &str) -> Option<EventView> {
        if line.is_empty() {
            let data = std::mem::take(&mut self.data);
            return serde_json::from_str(&data).ok();
        }
        if let Some(rest) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(rest.trim_start());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(parser: &mut SseParser, text: &str) -> Vec<EventView> {
        text.lines().filter_map(|l| parser.push(l)).collect()
    }

    #[test]
    fn parses_a_server_sent_event_and_skips_comments() {
        let data = r#"{"cursor":7,"id":"e7","job_id":"J","stage_id":null,"seq":1,"ts_ms":5,"actor":"daemon","kind":"job-completed","payload":{}}"#;
        let text =
            format!(": connected\n\nid: 7\nevent: job-completed\ndata: {data}\n\n: keep-alive\n\n");
        let mut parser = SseParser::default();
        let events = feed(&mut parser, &text);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cursor, 7);
        assert_eq!(events[0].kind, "job-completed");
    }

    #[test]
    fn malformed_data_is_dropped_without_poisoning_the_next_event() {
        let good = r#"{"cursor":1,"id":"e1","job_id":"J","stage_id":null,"seq":1,"ts_ms":5,"actor":"human","kind":"job-cancelled","payload":{}}"#;
        let text = format!("data: not json\n\ndata: {good}\n\n");
        let mut parser = SseParser::default();
        let events = feed(&mut parser, &text);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "e1");
    }
}
