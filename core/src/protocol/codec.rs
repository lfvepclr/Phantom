use crate::crypto::writer::write_vectored_all;
use crate::crypto::{SessionReader, SessionWriter};
use crate::{PhantomError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use std::io::IoSlice;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::frame::Frame;

/// A message-oriented reader: one call yields exactly one length-prefixed
/// message.
///
/// Two implementations exist:
/// - [`SessionReader`] — AEAD-encrypted, used on the TCP transport.
/// - [`PlainMessageReader`] — framing only, used on QUIC streams where the
///   connection itself is already encrypted by the Noise handshake.
#[async_trait]
pub trait MessageRead {
    async fn read_message(&mut self) -> Result<Bytes>;
}

/// Message-oriented writer counterpart of [`MessageRead`].
#[async_trait]
pub trait MessageWrite {
    async fn write_message_bytes(&mut self, data: Bytes) -> Result<()>;
    async fn flush(&mut self) -> Result<()>;
}

#[async_trait]
impl<R: AsyncReadExt + Unpin + Send> MessageRead for SessionReader<R> {
    async fn read_message(&mut self) -> Result<Bytes> {
        SessionReader::read_message(self).await
    }
}

#[async_trait]
impl<W: AsyncWriteExt + Unpin + Send> MessageWrite for SessionWriter<W> {
    async fn write_message_bytes(&mut self, data: Bytes) -> Result<()> {
        SessionWriter::write_message_bytes(self, data).await
    }

    async fn flush(&mut self) -> Result<()> {
        SessionWriter::flush(self).await
    }
}

/// Length-prefixed message framing without AEAD.
///
/// QUIC streams use this: the Noise handshake inside QUIC already provides
/// confidentiality and integrity, so only the message-boundary framing the
/// frame protocol relies on remains. The wire format is identical to the TCP
/// path minus the AEAD layer: `[u16 len][payload]`.
pub struct PlainMessageReader<R> {
    reader: R,
}

impl<R: AsyncReadExt + Unpin> PlainMessageReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }
}

#[async_trait]
impl<R: AsyncReadExt + Unpin + Send> MessageRead for PlainMessageReader<R> {
    async fn read_message(&mut self) -> Result<Bytes> {
        let mut len_buf = [0u8; 2];
        self.reader
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| PhantomError::Protocol(format!("Read message length failed: {}", e)))?;
        let len = u16::from_be_bytes(len_buf) as usize;

        if len > crate::constants::NOISE_MAX_MSG_LEN {
            return Err(PhantomError::Protocol(format!(
                "Message too large: {}",
                len
            )));
        }

        let mut buf = vec![0u8; len];
        self.reader
            .read_exact(&mut buf)
            .await
            .map_err(|e| PhantomError::Protocol(format!("Read message body failed: {}", e)))?;
        Ok(Bytes::from(buf))
    }
}

/// Length-prefixed message writer without AEAD; see [`PlainMessageReader`].
pub struct PlainMessageWriter<W> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> PlainMessageWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }
}

#[async_trait]
impl<W: AsyncWrite + Unpin + Send> MessageWrite for PlainMessageWriter<W> {
    async fn write_message_bytes(&mut self, data: Bytes) -> Result<()> {
        let len_be = (data.len() as u16).to_be_bytes();
        write_vectored_all(&mut self.writer, [
            IoSlice::new(&len_be),
            IoSlice::new(&data),
        ])
        .await
        .map_err(|e| PhantomError::Protocol(format!("Write message failed: {}", e)))?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        self.writer
            .flush()
            .await
            .map_err(|e| PhantomError::Protocol(format!("Flush failed: {}", e)))
    }
}

pub struct FrameReader<M> {
    reader: M,
}

impl<M: MessageRead> FrameReader<M> {
    pub fn new(reader: M) -> Self {
        Self { reader }
    }

    pub async fn read_frame(&mut self) -> Result<Frame> {
        let data: Bytes = self.reader.read_message().await?;
        Frame::decode(data)
    }
}

pub struct FrameWriter<M> {
    writer: M,
}

impl<M: MessageWrite> FrameWriter<M> {
    pub fn new(writer: M) -> Self {
        Self { writer }
    }

    /// Write a frame to the tunnel.
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

    /// Flush the underlying writer.
    pub async fn flush(&mut self) -> Result<()> {
        self.writer.flush().await
    }
}
