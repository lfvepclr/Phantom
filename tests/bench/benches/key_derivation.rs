use divan::Bencher;
use hkdf::Hkdf;
use sha2::Sha256;

#[divan::bench]
fn hkdf_sha256_derive_32bytes(bencher: Bencher) {
    let ikm = [0x11u8; 32];
    let info = b"phantom-v2-AES-256-GCM-write";

    bencher.bench_local(|| {
        let hk = Hkdf::<Sha256>::new(None, &ikm);
        let mut out = [0u8; 32];
        hk.expand(info, &mut out).unwrap();
        out
    });
}

#[divan::bench]
fn hkdf_sha256_derive_16bytes(bencher: Bencher) {
    let ikm = [0x11u8; 32];
    let info = b"phantom-v2-ASCON-128-write";

    bencher.bench_local(|| {
        let hk = Hkdf::<Sha256>::new(None, &ikm);
        let mut out = [0u8; 16];
        hk.expand(info, &mut out).unwrap();
        out
    });
}

#[divan::bench]
fn hkdf_full_session_key_derivation(bencher: Bencher) {
    let k1 = [0x11u8; 32];
    let k2 = [0x22u8; 32];

    bencher.bench_local(|| {
        let prefix = "phantom-v2-AES-256-GCM";
        let hk1 = Hkdf::<Sha256>::new(None, &k1);
        let mut write_key = [0u8; 32];
        hk1.expand(format!("{}-write", prefix).as_bytes(), &mut write_key)
            .unwrap();

        let hk2 = Hkdf::<Sha256>::new(None, &k2);
        let mut read_key = [0u8; 32];
        hk2.expand(format!("{}-read", prefix).as_bytes(), &mut read_key)
            .unwrap();

        let mut write_nonce_prefix = [0u8; 4];
        hk1.expand(
            format!("{}-write-nonce", prefix).as_bytes(),
            &mut write_nonce_prefix,
        )
        .unwrap();

        let mut read_nonce_prefix = [0u8; 4];
        hk2.expand(
            format!("{}-read-nonce", prefix).as_bytes(),
            &mut read_nonce_prefix,
        )
        .unwrap();

        (write_key, read_key, write_nonce_prefix, read_nonce_prefix)
    });
}

fn main() {
    divan::main();
}
