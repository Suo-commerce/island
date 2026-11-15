//! HTTP/1.1 over a Unix socket, the little the agent needs: one request
//! per connection, the answer's body by length, chunked, or to the end.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// An answer.
#[derive(Debug)]
pub struct Answer {
    /// Its status.
    pub status: u16,
    /// Its body, whole.
    pub body: Vec<u8>,
}

/// Send one request and read the whole answer. `quiet`: how long the
/// other side may go quiet while it answers.
pub fn ask(
    socket: &Path,
    method: &str,
    target: &str,
    body: &[u8],
    quiet: Duration,
) -> std::io::Result<Answer> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(quiet))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    write!(
        stream,
        "{method} {target} HTTP/1.1\r\nHost: island\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    read_answer(BufReader::new(stream))
}

fn bad(what: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.into())
}

/// Read an answer: status line, headers, body.
pub fn read_answer(mut reader: impl BufRead) -> std::io::Result<Answer> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| bad(format!("not an HTTP answer: {line:?}")))?;
    let mut length = None;
    let mut chunked = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(bad("the answer ended in its headers"));
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
            if name == "content-length" {
                length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| bad("a bad content-length"))?,
                );
            } else if name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
                chunked = true;
            }
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            line.clear();
            reader.read_line(&mut line)?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| bad(format!("a bad chunk size: {line:?}")))?;
            if size == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..])?;
            line.clear();
            reader.read_line(&mut line)?;
        }
    } else if let Some(length) = length {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    } else if status != 204 && status != 304 {
        reader.read_to_end(&mut body)?;
    }
    Ok(Answer { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_are_read_by_length_chunks_or_to_the_end() {
        let answer =
            read_answer(&b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"[..]).unwrap();
        assert_eq!(
            (answer.status, answer.body.as_slice()),
            (200, &b"hello"[..])
        );
        let chunked = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n";
        assert_eq!(read_answer(&chunked[..]).unwrap().body, b"abcde");
        let to_end = read_answer(&b"HTTP/1.1 403 Forbidden\r\n\r\nno"[..]).unwrap();
        assert_eq!((to_end.status, to_end.body.as_slice()), (403, &b"no"[..]));
        let empty = read_answer(&b"HTTP/1.1 204 No Content\r\n\r\n"[..]).unwrap();
        assert_eq!((empty.status, empty.body.len()), (204, 0));
        assert!(read_answer(&b"hello"[..]).is_err());
    }
}
