use bytes::Bytes;
use phantom_core::constants::MAX_FRAME_PAYLOAD;
use phantom_core::protocol::Frame;
use phantom_core::protocol::codec::{FrameReader, FrameWriter, MessageRead, MessageWrite};
use phantom_core::protocol::frame::FrameFlags;
use std::time::{Duration, Instant};

/// Belt-and-braces cap for the echo helpers: well above the slowest
/// legitimate run (10 MiB AES on loopback takes ~5s; throttled weak-network
/// cases are slower), so a trip means a real stall, not slow progress.
const ECHO_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone)]
pub struct ThroughputResult {
    pub bytes_sent: usize,
    pub bytes_received: usize,
    pub elapsed: Duration,
    pub throughput_mbps: f64,
    pub latency_ms: f64,
}

impl std::fmt::Display for ThroughputResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "sent {} bytes, received {} bytes, elapsed {:.2}s, throughput {:.2} MB/s ({:.2} Mbps), latency {:.1}ms",
            self.bytes_sent,
            self.bytes_received,
            self.elapsed.as_secs_f64(),
            self.throughput_mbps / 8.0,
            self.throughput_mbps,
            self.latency_ms
        )
    }
}

/// Write every chunk of `data` as a DATA frame, then FIN + flush.
///
/// Extracted so the echo helpers can run it concurrently with the read loop:
/// write-all-then-read deadlocks once the payload exceeds the combined socket
/// buffers along the relay (the server only reads more tunnel data while its
/// writes toward the client keep draining), so the two directions must be
/// pumped at the same time.
async fn write_all_frames<W>(frame_writer: &mut FrameWriter<W>, stream_id: u32, data: &[u8])
where
    W: MessageWrite,
{
    let mut offset = 0;
    while offset < data.len() {
        let end = std::cmp::min(offset + MAX_FRAME_PAYLOAD, data.len());
        let chunk = Bytes::copy_from_slice(&data[offset..end]);
        frame_writer
            .write_frame(&Frame::data(stream_id, chunk))
            .await
            .expect("Failed to write data frame");
        offset = end;
    }
    frame_writer
        .write_frame(&Frame::fin(stream_id))
        .await
        .expect("Failed to write FIN");
    frame_writer.flush().await.expect("Failed to flush");
}

/// Read frames until FIN/RST, concatenating DATA payloads.
async fn read_until_fin<R>(frame_reader: &mut FrameReader<R>) -> Vec<u8>
where
    R: MessageRead,
{
    let mut received = Vec::new();
    loop {
        let frame = frame_reader
            .read_frame()
            .await
            .expect("Failed to read frame");
        if frame.flags.contains(FrameFlags::DATA) {
            received.extend_from_slice(&frame.payload);
        } else if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST) {
            break;
        }
    }
    received
}

pub async fn measure_echo_throughput<R, W>(
    frame_reader: &mut FrameReader<R>,
    frame_writer: &mut FrameWriter<W>,
    stream_id: u32,
    total_bytes: usize,
) -> ThroughputResult
where
    R: MessageRead,
    W: MessageWrite,
{
    let data = generate_test_data(total_bytes);
    let start = Instant::now();
    let ((), received) = tokio::time::timeout(ECHO_TIMEOUT, async {
        tokio::join!(
            write_all_frames(frame_writer, stream_id, &data),
            read_until_fin(frame_reader),
        )
    })
    .await
    .expect("echo round-trip timed out — the relay stopped making progress");
    let elapsed = start.elapsed();
    assert_eq!(
        received.len(),
        data.len(),
        "Data length mismatch: sent {}, received {}",
        data.len(),
        received.len()
    );
    assert_eq!(received, data, "Data content mismatch");
    let throughput_mbps = (total_bytes as f64 * 8.0 * 2.0) / elapsed.as_secs_f64() / 1_000_000.0;
    ThroughputResult {
        bytes_sent: total_bytes,
        bytes_received: received.len(),
        elapsed,
        throughput_mbps,
        latency_ms: elapsed.as_secs_f64() * 1000.0,
    }
}

pub async fn measure_send_throughput<R, W>(
    frame_reader: &mut FrameReader<R>,
    frame_writer: &mut FrameWriter<W>,
    stream_id: u32,
    total_bytes: usize,
) -> ThroughputResult
where
    R: MessageRead,
    W: MessageWrite,
{
    let data = generate_test_data(total_bytes);
    let start = Instant::now();
    let mut offset = 0;
    while offset < data.len() {
        let end = std::cmp::min(offset + MAX_FRAME_PAYLOAD, data.len());
        let chunk = Bytes::copy_from_slice(&data[offset..end]);
        frame_writer
            .write_frame(&Frame::data(stream_id, chunk))
            .await
            .expect("Failed to write data frame");
        offset = end;
    }
    frame_writer
        .write_frame(&Frame::fin(stream_id))
        .await
        .expect("Failed to write FIN");
    frame_writer.flush().await.expect("Failed to flush");
    loop {
        let frame = frame_reader
            .read_frame()
            .await
            .expect("Failed to read frame");
        if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST) {
            break;
        }
    }
    let elapsed = start.elapsed();
    let throughput_mbps = (total_bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0;
    ThroughputResult {
        bytes_sent: total_bytes,
        bytes_received: 0,
        elapsed,
        throughput_mbps,
        latency_ms: elapsed.as_secs_f64() * 1000.0,
    }
}

pub async fn echo_data<R, W>(
    frame_reader: &mut FrameReader<R>,
    frame_writer: &mut FrameWriter<W>,
    stream_id: u32,
    data: &[u8],
) -> Vec<u8>
where
    R: MessageRead,
    W: MessageWrite,
{
    // Full-duplex: the write and read halves are disjoint borrows, so they can
    // be driven concurrently on the same task. Sequential write-then-read
    // deadlocks for payloads larger than the in-flight buffering of the relay
    // loop (observed with the 10 MiB echo_large tests).
    let ((), received) = tokio::time::timeout(ECHO_TIMEOUT, async {
        tokio::join!(
            write_all_frames(frame_writer, stream_id, data),
            read_until_fin(frame_reader),
        )
    })
    .await
    .expect("echo round-trip timed out — the relay stopped making progress");
    received
}

/// Half-duplex request/response WITHOUT a client FIN: write the request
/// frames, flush, then read until the server closes the stream (FIN/RST).
///
/// Use this for protocols where the server initiates teardown, such as
/// HTTP/1.1 `Connection: close`. Sending a client FIN right after the request
/// races with the response on hyper-based servers: hyper treats the
/// half-close EOF as connection teardown and may drop an in-flight response
/// (reproduced against axum: ~60-70% empty replies on loopback).
/// Request sizes are expected to stay well under the relay's in-flight
/// buffering, so a sequential write-then-read cannot deadlock here.
pub async fn exchange_data<R, W>(
    frame_reader: &mut FrameReader<R>,
    frame_writer: &mut FrameWriter<W>,
    stream_id: u32,
    data: &[u8],
) -> Vec<u8>
where
    R: MessageRead,
    W: MessageWrite,
{
    let mut offset = 0;
    while offset < data.len() {
        let end = std::cmp::min(offset + MAX_FRAME_PAYLOAD, data.len());
        let chunk = Bytes::copy_from_slice(&data[offset..end]);
        frame_writer
            .write_frame(&Frame::data(stream_id, chunk))
            .await
            .expect("Failed to write data frame");
        offset = end;
    }
    frame_writer.flush().await.expect("Failed to flush");
    read_until_fin(frame_reader).await
}

fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 256) as u8).collect()
}

pub fn generate_random_data(size: usize) -> Vec<u8> {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..size).map(|_| rng.r#gen::<u8>()).collect()
}
