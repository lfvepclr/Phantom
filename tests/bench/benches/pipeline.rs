use bytes::Bytes;
use divan::Bencher;
use phantom_core::crypto::aead_state::AeadState;
use phantom_core::crypto::cipher::CipherSuite;
use phantom_core::protocol::Frame;

#[divan::bench(args = [1024, 4096, 16384])]
fn pipeline_aes256gcm(bencher: Bencher, payload_size: usize) {
    pipeline_bench(bencher, CipherSuite::Aes256Gcm, payload_size);
}

#[divan::bench(args = [1024, 4096, 16384])]
fn pipeline_ascon128(bencher: Bencher, payload_size: usize) {
    pipeline_bench(bencher, CipherSuite::Ascon128, payload_size);
}

#[divan::bench(args = [1024, 4096, 16384])]
fn pipeline_chacha20(bencher: Bencher, payload_size: usize) {
    pipeline_bench(bencher, CipherSuite::ChaCha20Poly, payload_size);
}

fn pipeline_bench(bencher: Bencher, cipher: CipherSuite, payload_size: usize) {
    let key = vec![0x42u8; cipher.key_len()];
    let nonce_prefix = [0xAA, 0xBB, 0xCC, 0xDD];
    let payload = vec![0xAAu8; payload_size];
    let frame = Frame::data(1, Bytes::copy_from_slice(&payload));

    bencher.bench_local(|| {
        let mut enc_state = AeadState::new(cipher, &key, nonce_prefix);
        let mut dec_state = AeadState::new(cipher, &key, nonce_prefix);

        // Encode frame to Bytes, then convert to Vec for encrypt
        let encoded: Bytes = frame.encode();
        let mut buf: Vec<u8> = encoded
            .try_into_mut()
            .map(|bm| bm.into())
            .unwrap_or_else(|b| b.to_vec());
        enc_state.encrypt_in_place(&mut buf).unwrap();

        // In-place decrypt
        dec_state.decrypt_in_place(&mut buf).unwrap();
        let _decoded = Frame::decode(Bytes::from(buf)).unwrap();
    });
}

fn main() {
    divan::main();
}
