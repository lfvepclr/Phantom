use crate::{PhantomError, Result};
use bytes::Bytes;
use std::io::IoSlice;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::crypto::aead_state::AeadState;

pub struct SessionWriter<W> {
    writer: W,
    state: AeadState,
}

impl<W: AsyncWrite + Unpin> SessionWriter<W> {
    pub fn new(writer: W, state: AeadState) -> Self {
        Self { writer, state }
    }

    /// Write a message from a byte slice, copying into a Vec for encryption.
    pub async fn write_message(&mut self, payload: &[u8]) -> Result<()> {
        let mut buf = payload.to_vec();
        self.state.encrypt_in_place(&mut buf)?;

        let len_be = (buf.len() as u16).to_be_bytes();
        write_vectored_all(
            &mut self.writer,
            [IoSlice::new(&len_be), IoSlice::new(&buf)],
        )
        .await
        .map_err(|e| PhantomError::Protocol(format!("Write message failed: {}", e)))?;

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
        // Reserve room for the AEAD tag so `encrypt_in_place`'s
        // `extend_from_slice(&tag)` does not reallocate + copy 64 KiB.
        buf.reserve(crate::constants::NOISE_TAG_LEN);
        self.state.encrypt_in_place(&mut buf)?;

        let len_be = (buf.len() as u16).to_be_bytes();
        write_vectored_all(
            &mut self.writer,
            [IoSlice::new(&len_be), IoSlice::new(&buf)],
        )
        .await
        .map_err(|e| PhantomError::Protocol(format!("Write message failed: {}", e)))?;

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

    pub fn cipher(&self) -> crate::crypto::cipher::CipherSuite {
        self.state.cipher()
    }
}

/// Write all slices with as few syscalls as possible (single `writev` in the
/// common case), advancing past partially-written bytes on short writes.
/// Shared by the AEAD (TCP) and plain (QUIC) message writers.
pub(crate) async fn write_vectored_all<W: AsyncWrite + Unpin>(
    writer: &mut W,
    mut iovs: [IoSlice<'_>; 2],
) -> std::io::Result<()> {
    let total: usize = iovs.iter().map(|io| io.len()).sum();
    let mut written = 0usize;
    while written < total {
        let n = writer.write_vectored(&iovs).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "failed to write whole message",
            ));
        }
        written += n;
        // Advance past fully-written slices.
        let mut skip = n;
        for io in iovs.iter_mut() {
            if skip == 0 {
                break;
            }
            let len = io.len();
            if skip >= len {
                skip -= len;
                *io = IoSlice::new(&[]);
            } else {
                io.advance(skip);
                skip = 0;
            }
        }
    }
    Ok(())
}
