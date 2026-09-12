//! 跨平台 AEAD 加密微基准（macOS / linux-musl / ohos 三目标编译）。
//!
//! 使用 phantom-core 真实数据路径的 `AeadState`（含 nonce 计数管理），
//! 输出各 cipher 在不同块大小下的 encrypt/decrypt 吞吐，供选型对比。
//!
//! 用法：`cipher_bench [块大小列表]`，默认 1024 16384 65536 字节。

use phantom_core::CipherSuite;
use phantom_core::crypto::aead_state::AeadState;
use std::time::Instant;

const BUDGET_BYTES: usize = 256 * 1024 * 1024; // 每组总数据量上限
const WARMUP_BYTES: usize = 16 * 1024 * 1024;

fn bench_one(suite: CipherSuite, size: usize) -> (f64, f64) {
    let key = vec![0x42u8; suite.key_len()];
    let mut enc = AeadState::new(suite, &key, [1, 2, 3, 4]);
    let mut dec = AeadState::new(suite, &key, [1, 2, 3, 4]);
    let mut buf = vec![0xAAu8; size];

    // 预热
    let mut warmed = 0usize;
    while warmed < WARMUP_BYTES {
        let ct = enc.encrypt(&buf).expect("encrypt");
        let n = buf.len();
        buf.copy_from_slice(&ct[..n]);
        warmed += size;
    }

    // encrypt 计时
    let iterations = (BUDGET_BYTES / size).max(8);
    let start = Instant::now();
    let mut sink = 0usize;
    for _ in 0..iterations {
        let ct = enc.encrypt(&buf).expect("encrypt");
        sink = sink.wrapping_add(ct.len());
    }
    let enc_secs = start.elapsed().as_secs_f64();

    // decrypt 计时（重新加密生成密文源）
    let cts: Vec<Vec<u8>> = {
        let mut k = AeadState::new(suite, &key, [1, 2, 3, 4]);
        (0..iterations)
            .map(|_| k.encrypt(&buf).expect("encrypt"))
            .collect()
    };
    let start = Instant::now();
    let mut sink2 = 0usize;
    for ct in &cts {
        let pt = dec.decrypt(ct).expect("decrypt");
        sink2 = sink2.wrapping_add(pt.len());
    }
    let dec_secs = start.elapsed().as_secs_f64();

    std::hint::black_box((sink, sink2));
    let total = (iterations * size) as f64;
    (
        total / enc_secs / 1024.0 / 1024.0,
        total / dec_secs / 1024.0 / 1024.0,
    )
}

fn main() {
    println!("cipher-bench target={}", std::env::consts::ARCH);
    report_selection();
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let sizes = if sizes.is_empty() {
        vec![1024, 16384, 65536]
    } else {
        sizes
    };

    let suites = [
        (CipherSuite::Aes256Gcm, "AES-256-GCM"),
        (CipherSuite::Aes128Gcm, "AES-128-GCM"),
        (CipherSuite::ChaCha20Poly, "ChaCha20-Poly1305"),
        (CipherSuite::Ascon128, "ASCON-128"),
    ];

    println!(
        "{:<20} {:>8} {:>14} {:>14}",
        "cipher", "block", "enc MiB/s", "dec MiB/s"
    );
    for (suite, name) in suites {
        for &size in &sizes {
            let (e, d) = bench_one(suite, size);
            println!("{:<20} {:>8} {:>14.1} {:>14.1}", name, size, e, d);
        }
    }
}

/// Print the three facts that decide whether this device really uses hardware
/// AES, plus the cipher `cipher=auto` would pick.
///
/// The client's AES backend only exists when the crate is compiled with
/// `--cfg=aes_armv8` (see `.cargo/config.toml`), *and* it is only taken at
/// runtime when the CPU is reported as having the `aes` extension — a probe
/// that is unavailable on targets `cpufeatures` does not know (HarmonyOS is one
/// of them) unless the feature is also enabled at compile time. Printing all
/// three answers separates "the backend is missing" from "the backend is there
/// but unused", which are indistinguishable from a throughput number alone.
fn report_selection() {
    #[cfg(target_arch = "aarch64")]
    let runtime_aes = std::arch::is_aarch64_feature_detected!("aes");
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    let runtime_aes = std::arch::is_x86_feature_detected!("aes");
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64")))]
    let runtime_aes = false;

    println!(
        "detect cfg(aes_armv8)={} target_feature(aes)={} runtime_aes={} auto_suite={:?}",
        cfg!(aes_armv8),
        cfg!(target_feature = "aes"),
        runtime_aes,
        CipherSuite::auto_detect()
    );
}
