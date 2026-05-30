use divan::Bencher;
use phantom_protocol::{Frame, TargetAddr};

#[divan::bench(args = [64, 512, 4096, 16384])]
fn frame_encode(bencher: Bencher, payload_size: usize) {
    let payload = vec![0xAAu8; payload_size];
    let stream_id = 1u32;

    bencher.bench_local(|| {
        Frame::data(stream_id, bytes::Bytes::copy_from_slice(&payload)).encode()
    });
}

#[divan::bench(args = [64, 512, 4096, 16384])]
fn frame_decode(bencher: Bencher, payload_size: usize) {
    let payload = vec![0xAAu8; payload_size];
    let stream_id = 1u32;
    let encoded = Frame::data(stream_id, bytes::Bytes::copy_from_slice(&payload)).encode();

    bencher.bench_local(|| {
        Frame::decode(encoded.clone()).unwrap()
    });
}

#[divan::bench]
fn syn_frame_encode(bencher: Bencher) {
    let target = TargetAddr::Domain("example.com".to_string(), 443);
    let payload = target.encode();

    bencher.bench_local(|| {
        Frame::syn(1, payload.clone()).encode()
    });
}

fn main() {
    divan::main();
}
