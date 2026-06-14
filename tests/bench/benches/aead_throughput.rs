use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, Aes256Gcm, KeyInit, Nonce};
use ascon_aead::{AsconAead128, AsconAead128Nonce, Key as AsconKey, KeyInit as AsconKeyInit};
use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::KeyInit as ChaChaKeyInit;
use divan::Bencher;

#[divan::bench(args = [1024, 4096, 16384, 65536])]
fn aes256gcm_encrypt(bencher: Bencher, size: usize) {
    let key = aes_gcm::Key::<Aes256Gcm>::from_slice(&[0x42u8; 32]);
    let cipher = Aes256Gcm::new(key);
    let nonce = [0u8; 12];
    let payload = vec![0xAAu8; size];

    bencher.bench_local(|| cipher.encrypt(Nonce::from_slice(&nonce), &payload).unwrap());
}

#[divan::bench(args = [1024, 4096, 16384, 65536])]
fn aes128gcm_encrypt(bencher: Bencher, size: usize) {
    let key = aes_gcm::Key::<Aes128Gcm>::from_slice(&[0x42u8; 16]);
    let cipher = Aes128Gcm::new(key);
    let nonce = [0u8; 12];
    let payload = vec![0xAAu8; size];

    bencher.bench_local(|| cipher.encrypt(Nonce::from_slice(&nonce), &payload).unwrap());
}

#[divan::bench(args = [1024, 4096, 16384, 65536])]
fn ascon128_encrypt(bencher: Bencher, size: usize) {
    let key = AsconKey::<AsconAead128>::from_slice(&[0x42u8; 16]);
    let cipher = AsconAead128::new(&key);
    let nonce = [0u8; 16];
    let payload = vec![0xAAu8; size];

    bencher.bench_local(|| {
        cipher
            .encrypt(AsconAead128Nonce::from_slice(&nonce), &payload)
            .unwrap()
    });
}

#[divan::bench(args = [1024, 4096, 16384, 65536])]
fn chacha20poly1305_encrypt(bencher: Bencher, size: usize) {
    let key = chacha20poly1305::Key::from_slice(&[0x42u8; 32]);
    let cipher = ChaCha20Poly1305::new(key);
    let nonce = [0u8; 12];
    let payload = vec![0xAAu8; size];

    bencher.bench_local(|| {
        cipher
            .encrypt(chacha20poly1305::Nonce::from_slice(&nonce), &payload)
            .unwrap()
    });
}

fn main() {
    divan::main();
}
