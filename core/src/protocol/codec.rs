use bytes::Bytes;
use crate::Result;
use crate::crypto::{SessionReader, SessionWriter};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::protocol::frame::Frame;

pub struct FrameReader<R> {
    reader: SessionReader<R>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(reader: SessionReader<R>) -> Self {
        Self { reader }
    }

    pub async fn read_frame(&mut self) -> Result<Frame> {
        let data: Bytes = self.reader.read_message().await?;
        Frame::decode(data)
    }
}

pub struct FrameWriter<W> {
    writer: SessionWriter<W>,
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    pub fn new(writer: SessionWriter<W>) -> Self {
        Self { writer }
    }

    /// Write a frame to the encrypted tunnel.
    ///
    /// Uses `write_message_bytes` for zero-copy: `Frame::encode()` returns a
    /// freshly-frozen `Bytes` (unique reference), so `try_into_mut()` succeeds
    /// without copying.
    ///
    /// Flushes are not performed per-frame; call `flush()` explicitly after
    /// control frames (FIN/RST) or after a batch of DATA frames.
    pub async fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        let encoded: Bytes = frame.encode();
        self.writer.write_message_bytes(encoded).await
    }

    /// Flush the underlying encrypted writer.
    pub async fn flush(&mut self) -> Result<()> {
        self.writer.flush().await
    }
}
