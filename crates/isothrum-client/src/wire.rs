//! One connection to a service: JSON lines over the local endpoint, both ways.

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;
use interprocess::local_socket::{GenericFilePath, ToFsName as _};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use crate::proto::MAX_LINE;

/// The reading half of a connection, by lines.
pub type Reader = LineReader<tokio::io::ReadHalf<Stream>>;

/// The writing half of a connection.
pub type Writer = tokio::io::WriteHalf<Stream>;

/// The reading half of a connection.
pub struct LineReader<R> {
    inner: BufReader<R>,
    buf: Vec<u8>,
}

impl<R: tokio::io::AsyncRead + Unpin> LineReader<R> {
    /// Lines read from `inner`.
    pub fn new(inner: R) -> LineReader<R> {
        LineReader {
            inner: BufReader::new(inner),
            buf: Vec::new(),
        }
    }

    /// The next line as a `T`; `None` at the end of the stream.
    ///
    /// # Errors
    ///
    /// The stream failed, a line is longer than [`MAX_LINE`] or isn't a `T` (`InvalidData`).
    pub async fn next<T: DeserializeOwned>(&mut self) -> std::io::Result<Option<T>> {
        loop {
            self.buf.clear();
            let limit = u64::try_from(MAX_LINE).unwrap_or(u64::MAX).saturating_add(1);
            let read = (&mut self.inner).take(limit).read_until(b'\n', &mut self.buf).await?;
            if read == 0 {
                return Ok(None);
            }
            if self.buf.last() != Some(&b'\n') && self.buf.len() > MAX_LINE {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "a line longer than the protocol allows"));
            }
            let line = self.buf.trim_ascii();
            if line.is_empty() {
                continue;
            }
            return serde_json::from_slice(line)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        }
    }
}

/// Writes `value` as one line to `writer`.
///
/// # Errors
///
/// The stream failed.
pub async fn write_line<W: tokio::io::AsyncWrite + Unpin, T: Serialize>(writer: &mut W, value: &T) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await
}

/// Connects to the endpoint `endpoint` names (a socket path, a pipe name).
///
/// # Errors
///
/// Nothing listens there.
pub async fn connect(endpoint: &str) -> std::io::Result<(Reader, Writer)> {
    let name = endpoint.to_fs_name::<GenericFilePath>()?;
    let stream = Stream::connect(name).await?;
    let (recv, send) = tokio::io::split(stream);
    Ok((LineReader::new(recv), send))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines read from `bytes`, each as JSON.
    fn reading(bytes: &[u8]) -> LineReader<std::io::Cursor<Vec<u8>>> {
        LineReader::new(std::io::Cursor::new(bytes.to_vec()))
    }

    #[tokio::test]
    async fn each_line_is_a_value_blank_ones_skipped_and_the_end_is_none() {
        let mut lines = reading(b"{\"a\":1}\n\n  \r\n{\"a\":2}");
        assert_eq!(lines.next::<serde_json::Value>().await.unwrap(), Some(serde_json::json!({"a": 1})));
        assert_eq!(
            lines.next::<serde_json::Value>().await.unwrap(),
            Some(serde_json::json!({"a": 2})),
            "a last line with no break"
        );
        assert_eq!(lines.next::<serde_json::Value>().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_malformed_line_and_one_past_the_limit_are_invalid_data() {
        let malformed = reading(b"{not json}\n").next::<serde_json::Value>().await.unwrap_err();
        assert_eq!(malformed.kind(), std::io::ErrorKind::InvalidData);
        let mut long = vec![b'"'; MAX_LINE + 2];
        long.push(b'\n');
        let past = reading(&long).next::<serde_json::Value>().await.unwrap_err();
        assert_eq!(past.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(past.to_string(), "a line longer than the protocol allows");
    }

    #[tokio::test]
    async fn a_written_value_is_one_line_read_back_as_itself() {
        let mut out = Vec::new();
        write_line(&mut out, &serde_json::json!({"cmd": "list", "id": 3})).await.unwrap();
        assert_eq!(out.last(), Some(&b'\n'));
        assert_eq!(out.iter().position(|b| *b == b'\n'), Some(out.len() - 1), "one line");
        let back = reading(&out).next::<serde_json::Value>().await.unwrap();
        assert_eq!(back, Some(serde_json::json!({"cmd": "list", "id": 3})));
    }
}
