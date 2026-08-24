// 用户态弱网代理（E2E 阶段 C）：模拟客户端→服务端 WAN 段的延迟与丢包。
//
// 用法: lossy_proxy <listen_port> <target_host:port> <delay_ms> <loss_pct> <tcp|udp>
//
// 语义说明:
// - 延迟: 每个包/chunk 的发送时刻 = 到达时刻 + delay（并行延迟，保持 RTT 语义，
//   不把延迟变成串行吞吐上限——发送端按计划时刻追赶发送）。
// - UDP（QUIC）: 每包按概率丢弃（双向独立）——包级丢包语义真实，
//   客户端 QUIC 丢包恢复机制被真实触发。
// - TCP: 应用层无法丢字节（破坏流完整性），仅做延迟；TCP 丢包语义需内核
//   netem（本 VM 缺 sch_netem 模块），由 tbf 限速场景补充。
//
// 编译: rustc --edition 2021 -O -o lossy_proxy tests/e2e/lossy_proxy.rs
use std::env;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct Cfg {
    delay: Duration,
    loss_pct: f64,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 6 {
        eprintln!("usage: {} <listen_port> <target_host:port> <delay_ms> <loss_pct> <tcp|udp>", args[0]);
        std::process::exit(2);
    }
    let listen_port: u16 = args[1].parse().unwrap();
    let target: SocketAddr = args[2].parse().unwrap_or_else(|_| {
        eprintln!("bad target address: {}", args[2]);
        std::process::exit(2);
    });
    let delay_ms: u64 = args[3].parse().unwrap();
    let loss_pct: f64 = args[4].parse().unwrap();
    let proto = args[5].clone();
    let cfg = Cfg { delay: Duration::from_millis(delay_ms), loss_pct };

    eprintln!("lossy_proxy :{listen_port} -> {target} delay={delay_ms}ms loss={loss_pct}% proto={proto}");
    match proto.as_str() {
        "tcp" => run_tcp(listen_port, target, cfg),
        "udp" => run_udp(listen_port, target, cfg),
        other => { eprintln!("unknown proto: {other}"); std::process::exit(2); }
    }
}

fn roll(loss_pct: f64) -> bool {
    if loss_pct <= 0.0 { return false; }
    use std::cell::Cell;
    thread_local! { static SEED: Cell<u64> = Cell::new(0x9E3779B97F4A7C15); }
    SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        s.set(x);
        ((x as f64) / (u64::MAX as f64)) * 100.0 < loss_pct
    })
}

/// 按计划时刻发送：scheduled = arrival + delay；积压时追赶（并行延迟语义）。
fn send_scheduled<T: FnMut(&[u8])>(rx: mpsc::Receiver<(Instant, Vec<u8>)>, delay: Duration, mut send: T) {
    while let Ok((arrival, pkt)) = rx.recv() {
        let scheduled = arrival + delay;
        let now = Instant::now();
        if scheduled > now {
            thread::sleep(scheduled - now);
        }
        send(&pkt);
    }
}

// ---------- TCP: 双向泵，每 chunk 计划时刻转发 ----------
fn run_tcp(port: u16, target: SocketAddr, cfg: Cfg) -> ! {
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind tcp");
    eprintln!("tcp listening on 127.0.0.1:{port}");
    for stream in listener.incoming().flatten() {
        thread::spawn(move || {
            let Ok(upstream) = TcpStream::connect(target) else { return };
            let _ = upstream.set_nodelay(true);
            let _ = stream.set_nodelay(true);

            // client → server
            let (tx_up, rx_up) = mpsc::channel::<(Instant, Vec<u8>)>();
            let mut up_w = match upstream.try_clone() { Ok(s) => s, Err(_) => return };
            thread::spawn(move || send_scheduled(rx_up, cfg.delay, move |b| {
                let _ = up_w.write_all(b);
                let _ = up_w.flush();
            }));
            // server → client
            let (tx_dn, rx_dn) = mpsc::channel::<(Instant, Vec<u8>)>();
            let mut cl_w = match stream.try_clone() { Ok(s) => s, Err(_) => return };
            thread::spawn(move || send_scheduled(rx_dn, cfg.delay, move |b| {
                let _ = cl_w.write_all(b);
                let _ = cl_w.flush();
            }));

            // 两个读泵
            let cl_r = stream;
            let up_r = upstream;
            let t1 = thread::spawn(move || tcp_reader(cl_r, tx_up));
            let t2 = thread::spawn(move || tcp_reader(up_r, tx_dn));
            let _ = (t1.join(), t2.join());
        });
    }
    unreachable!()
}

fn tcp_reader(mut sock: TcpStream, tx: mpsc::Sender<(Instant, Vec<u8>)>) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match sock.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send((Instant::now(), buf[..n].to_vec())).is_err() { break; }
            }
        }
    }
}

// ---------- UDP: 包级丢包（双向独立）+ 计划时刻转发 ----------
fn run_udp(port: u16, target: SocketAddr, cfg: Cfg) -> ! {
    let sock = UdpSocket::bind(("127.0.0.1", port)).expect("bind udp");
    eprintln!("udp listening on 127.0.0.1:{port}");

    let mut map: std::collections::HashMap<SocketAddr, mpsc::Sender<(Instant, Vec<u8>)>> =
        std::collections::HashMap::new();
    let mut buf = [0u8; 65536];
    loop {
        let (n, from) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let pkt = buf[..n].to_vec();
        if roll(cfg.loss_pct) {
            continue; // 客户端→服务端方向丢包
        }
        let tx = map.entry(from).or_insert_with(|| {
            let up_sock = UdpSocket::bind(("127.0.0.1", 0)).expect("bind upstream");
            up_sock.connect(target).expect("connect upstream");

            // 上行泵（计划时刻发送）
            let (tx, rx) = mpsc::channel::<(Instant, Vec<u8>)>();
            let up_w = up_sock.try_clone().expect("clone up");
            let d = cfg.delay;
            thread::spawn(move || send_scheduled(rx, d, move |b| {
                let _ = up_w.send(b);
            }));

            // 下行泵: 独立丢包 + 计划时刻发送
            let reply_sock = sock.try_clone().expect("clone sock");
            let client = from;
            let d = cfg.delay;
            let loss = cfg.loss_pct;
            thread::spawn(move || {
                let mut ubuf = [0u8; 65536];
                let mut scheduled: Vec<(Instant, Vec<u8>)> = Vec::new();
                loop {
                    // 非阻塞收 + 到点发送的简单事件循环（轮询 5ms）
                    let _ = up_sock.set_read_timeout(Some(Duration::from_millis(5)));
                    match up_sock.recv_from(&mut ubuf) {
                        Ok((n, _)) => {
                            if roll(loss) { continue; }
                            scheduled.push((Instant::now() + d, ubuf[..n].to_vec()));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => break,
                    }
                    let now = Instant::now();
                    let mut i = 0;
                    while i < scheduled.len() {
                        if scheduled[i].0 <= now {
                            let pkt = scheduled.swap_remove(i);
                            let _ = reply_sock.send_to(&pkt.1, client);
                        } else {
                            i += 1;
                        }
                    }
                    if !scheduled.is_empty() {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            });
            tx
        });
        let _ = tx.send((Instant::now(), pkt));
    }
}
