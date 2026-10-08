use std::io::Read;
use std::io::Result as IoResult;
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};

/// A `Reader` that reads exactly the number of bytes from a sub-reader.
///
/// If the limit is reached, it returns EOF. If the limit is not reached
/// when the destructor is called, the remaining bytes will be read and
/// thrown away.
pub struct EqualReader<R>
where
    R: Read,
{
    reader: R,
    size: usize,
    last_read_signal: Sender<IoResult<()>>,
}

impl<R> EqualReader<R>
where
    R: Read,
{
    pub fn new(reader: R, size: usize) -> (EqualReader<R>, Receiver<IoResult<()>>) {
        let (tx, rx) = channel();

        let r = EqualReader {
            reader,
            size,
            last_read_signal: tx,
        };

        (r, rx)
    }
}

impl<R> Read for EqualReader<R>
where
    R: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        if self.size == 0 {
            return Ok(0);
        }

        let buf = if buf.len() < self.size {
            buf
        } else {
            &mut buf[..self.size]
        };

        match self.reader.read(buf) {
            Ok(len) => {
                self.size -= len;
                Ok(len)
            }
            err @ Err(_) => err,
        }
    }
}

// Fairspoken patch (the only change to this vendored copy of tiny_http
// 0.12.0): upstream drained the unread body into `vec![0; remaining]`, so a
// client-chosen `Content-Length` decided the allocation. 18446744073709551615
// panicked with "capacity overflow" and large values aborted the process or
// allocated gigabytes, on any route that answers without reading its body.
// Drain through a fixed buffer instead, and only a body small enough to be
// worth keeping the connection in step for.

/// Larger unread bodies are left unread: the client claimed more than any
/// route accepts, and whatever it sends next fails to parse as a request,
/// which closes the connection.
const MAX_DRAIN_BYTES: usize = 16 * 1024 * 1024;
const DRAIN_CHUNK_BYTES: usize = 8 * 1024;

impl<R> Drop for EqualReader<R>
where
    R: Read,
{
    fn drop(&mut self) {
        if self.size > MAX_DRAIN_BYTES {
            return;
        }
        let mut buf = [0; DRAIN_CHUNK_BYTES];
        let mut remaining_to_read = self.size;

        while remaining_to_read > 0 {
            let chunk = remaining_to_read.min(buf.len());

            match self.reader.read(&mut buf[..chunk]) {
                Err(e) => {
                    self.last_read_signal.send(Err(e)).ok();
                    break;
                }
                Ok(0) => {
                    self.last_read_signal.send(Ok(())).ok();
                    break;
                }
                Ok(other) => {
                    remaining_to_read -= other;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EqualReader;
    use std::io::Read;

    #[test]
    fn test_limit() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) = EqualReader::new(org_reader.by_ref(), 5);

            let mut string = String::new();
            equal_reader.read_to_string(&mut string).unwrap();
            assert_eq!(string, "hello");
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }

    #[test]
    fn test_not_enough() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) = EqualReader::new(org_reader.by_ref(), 5);

            let mut vec = [0];
            equal_reader.read_exact(&mut vec).unwrap();
            assert_eq!(vec[0], b'h');
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }

    #[test]
    fn test_huge_claimed_length_is_not_drained() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (_equal_reader, _) = EqualReader::new(org_reader.by_ref(), usize::MAX);
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, "hello world");
    }
}
