use bytes::Bytes;
use phantom_core::{PhantomError, Result};
use tokio::io::AsyncWriteExt;

use crate::aead_state::AeadState;

pub struct SessionWriter<W> {
    writer: W,
    state: AeadState,
}

impl<W: AsyncWriteExt + Unpin> SessionWriter<W> {
    pub fn new(writer: W, state: AeadState) -> Self {
        Self { writer, state }
    }

    /// Write a message from a byte slice, copying into a Vec for encryption.
    pub async fn write_message(&mut self, payload: &[u8]) -> Result<()> {
        let mut buf = payload.to_vec();
        self.state.encrypt_in_place(&mut buf)?;

        let len_be = (buf.len() as u16).to_be_bytes();
        self.writer
            .write_all(&len_be)
            .await
            .map_err(|e| PhantomError::Protocol(format!("Write message length failed: {}", e)))?;
        self.writer
            .write_all(&buf)
            .await
            .map_err(|e| PhantomError::Protocol(format!("Write message body failed: {}", e)))?;

        Ok(())
    }

    /// Write a message from Bytes, avoiding a copy when possible.
    ///
    /// When the `Bytes` has a single unique reference (e.g. freshly created from
    /// `BytesMut::freeze()`), `try_into_mut()` succeeds and we can convert to
    /// `Vec<u8>` without copying. Otherwise falls back to `to_vec()`.
    ///
    /// Avoids per-message flush — the caller is responsible for flushing
    /// when necessary (e.g. on FIN/RST frames or after a batch of DATA frames).
    pub async fn write_message_bytes(&mut self, payload: Bytes) -> Result<()> {
        let mut buf: Vec<u8> = match payload.try_into_mut() {
            Ok(bytes_mut) => bytes_mut.into(),
            Err(shared_payload) => shared_payload.to_vec(),
        };
        self.state.encrypt_in_place(&mut buf)?;

        let len_be = (buf.len() as u16).to_be_bytes();
        self.writer
            .write_all(&len_be)
            .await
            .map_err(|e| PhantomError::Protocol(format!("Write message length failed: {}", e)))?;
        self.writer
            .write_all(&buf)
            .await
            .map_err(|e| PhantomError::Protocol(format!("Write message body failed: {}", e)))?;

        Ok(())
    }

    /// Flush the underlying writer.
    /// Call this after sending a batch of frames or on control frames (FIN/RST).
    pub async fn flush(&mut self) -> Result<()> {
        self.writer
            .flush()
            .await
            .map_err(|e| PhantomError::Protocol(format!("Flush failed: {}", e)))
    }

    pub fn cipher(&self) -> crate::cipher::CipherSuite {
        self.state.cipher()
    }
}
