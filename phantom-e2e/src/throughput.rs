use std::time::{Duration, Instant};
use bytes::Bytes;
use phantom_core::constants::MAX_FRAME_PAYLOAD;
use phantom_protocol::codec::{FrameReader, FrameWriter};
use phantom_protocol::frame::FrameFlags;
use phantom_protocol::Frame;
use tokio::io::{AsyncRead, AsyncWrite};

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
        write!(f, "sent {} bytes, received {} bytes, elapsed {:.2}s, throughput {:.2} MB/s ({:.2} Mbps), latency {:.1}ms",
            self.bytes_sent, self.bytes_received, self.elapsed.as_secs_f64(),
            self.throughput_mbps / 8.0, self.throughput_mbps, self.latency_ms)
    }
}

pub async fn measure_echo_throughput<R, W>(
    frame_reader: &mut FrameReader<R>,
    frame_writer: &mut FrameWriter<W>,
    stream_id: u32,
    total_bytes: usize,
) -> ThroughputResult
where R: AsyncRead + Unpin, W: AsyncWrite + Unpin,
{
    let data = generate_test_data(total_bytes);
    let start = Instant::now();
    let mut offset = 0;
    while offset < data.len() {
        let end = std::cmp::min(offset + MAX_FRAME_PAYLOAD, data.len());
        let chunk = Bytes::copy_from_slice(&data[offset..end]);
        frame_writer.write_frame(&Frame::data(stream_id, chunk)).await.expect("Failed to write data frame");
        offset = end;
    }
    frame_writer.write_frame(&Frame::fin(stream_id)).await.expect("Failed to write FIN");
    frame_writer.flush().await.expect("Failed to flush");
    let mut received = Vec::new();
    loop {
        let frame = frame_reader.read_frame().await.expect("Failed to read frame");
        if frame.flags.contains(FrameFlags::DATA) { received.extend_from_slice(&frame.payload); }
        else if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST) { break; }
    }
    let elapsed = start.elapsed();
    assert_eq!(received.len(), data.len(), "Data length mismatch: sent {}, received {}", data.len(), received.len());
    assert_eq!(received, data, "Data content mismatch");
    let throughput_mbps = (total_bytes as f64 * 8.0 * 2.0) / elapsed.as_secs_f64() / 1_000_000.0;
    ThroughputResult { bytes_sent: total_bytes, bytes_received: received.len(), elapsed, throughput_mbps, latency_ms: elapsed.as_secs_f64() * 1000.0 }
}

pub async fn measure_send_throughput<R, W>(
    frame_reader: &mut FrameReader<R>,
    frame_writer: &mut FrameWriter<W>,
    stream_id: u32,
    total_bytes: usize,
) -> ThroughputResult
where R: AsyncRead + Unpin, W: AsyncWrite + Unpin,
{
    let data = generate_test_data(total_bytes);
    let start = Instant::now();
    let mut offset = 0;
    while offset < data.len() {
        let end = std::cmp::min(offset + MAX_FRAME_PAYLOAD, data.len());
        let chunk = Bytes::copy_from_slice(&data[offset..end]);
        frame_writer.write_frame(&Frame::data(stream_id, chunk)).await.expect("Failed to write data frame");
        offset = end;
    }
    frame_writer.write_frame(&Frame::fin(stream_id)).await.expect("Failed to write FIN");
    frame_writer.flush().await.expect("Failed to flush");
    loop {
        let frame = frame_reader.read_frame().await.expect("Failed to read frame");
        if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST) { break; }
    }
    let elapsed = start.elapsed();
    let throughput_mbps = (total_bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0;
    ThroughputResult { bytes_sent: total_bytes, bytes_received: 0, elapsed, throughput_mbps, latency_ms: elapsed.as_secs_f64() * 1000.0 }
}

pub async fn echo_data<R, W>(
    frame_reader: &mut FrameReader<R>,
    frame_writer: &mut FrameWriter<W>,
    stream_id: u32,
    data: &[u8],
) -> Vec<u8>
where R: AsyncRead + Unpin, W: AsyncWrite + Unpin,
{
    let mut offset = 0;
    while offset < data.len() {
        let end = std::cmp::min(offset + MAX_FRAME_PAYLOAD, data.len());
        let chunk = Bytes::copy_from_slice(&data[offset..end]);
        frame_writer.write_frame(&Frame::data(stream_id, chunk)).await.expect("Failed to write data frame");
        offset = end;
    }
    frame_writer.write_frame(&Frame::fin(stream_id)).await.expect("Failed to write FIN");
    frame_writer.flush().await.expect("Failed to flush");
    let mut received = Vec::new();
    loop {
        let frame = frame_reader.read_frame().await.expect("Failed to read frame");
        if frame.flags.contains(FrameFlags::DATA) { received.extend_from_slice(&frame.payload); }
        else if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST) { break; }
    }
    received
}

fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 256) as u8).collect()
}

pub fn generate_random_data(size: usize) -> Vec<u8> {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..size).map(|_| rng.r#gen::<u8>()).collect()
}
