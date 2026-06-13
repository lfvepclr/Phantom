use bytes::Bytes;
use crate::{PhantomError, Result};
use tokio::io::AsyncReadExt;

use crate::crypto::aead_state::AeadState;

pub struct SessionReader<R> {
    reader: R,
    state: AeadState,
}

impl<R: AsyncReadExt + Unpin> SessionReader<R> {
    pub fn new(reader: R, state: AeadState) -> Self {
        Self { reader, state }
    }

    pub async fn read_message(&mut self) -> Result<Bytes> {
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

        // Read ciphertext directly into a buffer, then decrypt in-place
        let mut buf = vec![0u8; len];
        self.reader
            .read_exact(&mut buf)
            .await
            .map_err(|e| PhantomError::Protocol(format!("Read message body failed: {}", e)))?;

        self.state.decrypt_in_place(&mut buf)?;
        Ok(Bytes::from(buf))
    }

    pub fn cipher(&self) -> crate::crypto::cipher::CipherSuite {
        self.state.cipher()
    }
}
