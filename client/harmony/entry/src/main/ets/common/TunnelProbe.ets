/*
 * Latency / throughput probes that run *through the live tunnel*.
 *
 * The VPN extension process exposes a SOCKS5 ingress on loopback; probing it
 * exercises the exact same datapath the browser uses (TUN-less clients reach
 * it too), so the numbers describe the real connection rather than a
 * side-channel speed test.
 *
 * The handshake is written by hand instead of using the socket library's
 * `proxy` option because Phantom's ingress accepts an internal address type
 * that pins a request to the tunnel regardless of the routing whitelist. Doing
 * it here also makes the probe work on every API level, not just API 18+.
 */
import { socket } from '@kit.NetworkKit';
import { BusinessError } from '@kit.BasicServicesKit';

/** SOCKS5 ingress of the running client (`client.listen`). */
const PROXY_HOST: string = '127.0.0.1';
const PROXY_PORT: number = 11080;

/** Phantom-internal ATYP: "routing already decided, use the tunnel". */
const ATYP_PREROUTED: number = 0x80;
const ATYP_DOMAIN: number = 0x03;

/**
 * Socket buffers requested on the probe connection.
 *
 * 256 KiB is four times the default on most builds and roughly matches the
 * tunnel's own send buffer, so a burst the server delivers cannot be dropped
 * on the floor here and reported as "tunnel throughput".
 */
const PROBE_BUFFER_BYTES: number = 262144;

const PHASE_GREETING: number = 0;
const PHASE_REQUEST: number = 1;
const PHASE_STREAM: number = 2;
const PHASE_DEAD: number = 3;

export interface ProbeResult {
  ok: boolean;
  error: string;
}

export interface SpeedResult extends ProbeResult {
  /** Payload bytes received during the sampling window. */
  bytes: number;
  /** Sampling window in milliseconds. */
  elapsedMs: number;
  bytesPerSecond: number;
  /** HTTP status line as returned by the origin, for sanity checking. */
  status: string;
}

export interface LatencyResult extends ProbeResult {
  milliseconds: number;
}

/** A TCP stream to 127.0.0.1:11080 that has completed the SOCKS5 handshake. */
class TunnelStream {
  private tcp: socket.TCPSocket | null = null;
  private buffer: Uint8Array = new Uint8Array(0);
  private phase: number = PHASE_GREETING;
  private closed: boolean = false;
  private onChunk: ((chunk: Uint8Array) => void) | null = null;
  private resolveOpen: (() => void) | null = null;
  private rejectOpen: ((error: Error) => void) | null = null;

  /// Connect, negotiate, and issue a CONNECT for `host:port`.
  /// Resolves once the tunnel reports success.
  async open(
    host: string,
    port: number,
    timeoutMs: number,
    onChunk: (chunk: Uint8Array) => void,
  ): Promise<void> {
    this.onChunk = onChunk;
    const tcp = socket.constructTCPSocketInstance();
    this.tcp = tcp;
    tcp.on('message', (info: socket.SocketMessageInfo) => {
      this.consume(new Uint8Array(info.message), host, port, timeoutMs);
    });
    tcp.on('error', (err: BusinessError) => {
      this.fail('socket error ' + err.code);
    });
    // The handshake reply can land while `connect`/`send` are still being
    // awaited, so the deferred is armed *before* anything is written — arming
    // it afterwards left the promise pending forever.
    const established = new Promise<void>((resolve, reject) => {
      this.resolveOpen = resolve;
      this.rejectOpen = reject;
    });
    // A probe must never leave the UI stuck on "measuring…".
    setTimeout(() => {
      this.fail('探测超时（' + timeoutMs + ' ms）');
    }, timeoutMs);
    try {
      await tcp.connect({
        address: { address: PROXY_HOST, port: PROXY_PORT, family: 1 },
        timeout: timeoutMs,
      });
    } catch (e) {
      this.close();
      throw new Error('本地代理未就绪（请先 Start Tunnel）');
    }
    // The probe is supposed to report what the *tunnel* costs, so the local
    // socket must not add anything of its own: Nagle would put a round trip in
    // front of the small handshake writes, and a small receive buffer would cap
    // the throughput reading. Both are best-effort.
    try {
      await tcp.setExtraOptions({
        TCPNoDelay: true,
        keepAlive: true,
        receiveBufferSize: PROBE_BUFFER_BYTES,
        sendBufferSize: PROBE_BUFFER_BYTES,
      });
    } catch (e) {
      console.warn('[PhantomProbe] setExtraOptions failed: ' + JSON.stringify(e));
    }
    await tcp.send({ data: new Uint8Array([0x05, 0x01, 0x00]).buffer });
    return established;
  }

  send(text: string): void {
    const tcp = this.tcp;
    if (tcp === null || this.closed) {
      return;
    }
    tcp.send({ data: text }).catch((e: BusinessError) => {
      this.fail('send failed ' + e.code);
    });
  }

  close(): void {
    if (this.closed) {
      return;
    }
    this.teardown();
    this.resolveOpen = null;
    this.rejectOpen = null;
  }

  private fail(message: string): void {
    if (this.closed) {
      return;
    }
    this.teardown();
    const reject = this.rejectOpen;
    this.rejectOpen = null;
    this.resolveOpen = null;
    if (reject !== null) {
      reject(new Error(message));
    }
  }

  /// Drop the socket exactly once. Both `close()` (finished normally) and
  /// `fail()` (error path) funnel through here so a probe can never leak a
  /// half-open connection into the tunnel.
  private teardown(): void {
    this.closed = true;
    this.phase = PHASE_DEAD;
    const tcp = this.tcp;
    this.tcp = null;
    if (tcp !== null) {
      try {
        tcp.off('message');
        tcp.close().catch(() => {});
      } catch (e) {
        console.debug('[PhantomProbe] close failed: ' + JSON.stringify(e));
      }
    }
  }

  private consume(raw: Uint8Array, host: string, port: number, timeoutMs: number): void {
    if (this.closed || raw.length === 0) {
      return;
    }
    this.buffer = concat(this.buffer, raw);
    while (true) {
      if (this.phase === PHASE_GREETING) {
        if (this.buffer.length < 2) {
          return;
        }
        const method = this.buffer[1];
        this.buffer = sliceFrom(this.buffer, 2);
        if (method !== 0x00) {
          this.fail('代理不接受免认证方式');
          return;
        }
        this.phase = PHASE_REQUEST;
        const request = buildConnectRequest(host, port, timeoutMs);
        const tcp = this.tcp;
        if (tcp !== null) {
          tcp.send({ data: request.buffer }).catch((e: BusinessError) => {
            this.fail('request failed ' + e.code);
          });
        }
        continue;
      }
      if (this.phase === PHASE_REQUEST) {
        // Fixed-size reply: VER REP RSV ATYP BND.ADDR(4) BND.PORT(2).
        if (this.buffer.length < 10) {
          return;
        }
        const reply = this.buffer[1];
        this.buffer = sliceFrom(this.buffer, 10);
        if (reply !== 0x00) {
          this.fail('隧道建立失败 (SOCKS5 错误码 ' + reply + ')');
          return;
        }
        this.phase = PHASE_STREAM;
        console.info('[PhantomProbe] tunnel to ' + host + ':' + port + ' established');
        const resolve = this.resolveOpen;
        this.resolveOpen = null;
        this.rejectOpen = null;
        if (resolve !== null) {
          resolve();
        }
        continue;
      }
      if (this.phase === PHASE_STREAM) {
        if (this.buffer.length === 0) {
          return;
        }
        const chunk = this.buffer;
        this.buffer = new Uint8Array(0);
        const handler = this.onChunk;
        if (handler !== null) {
          handler(chunk);
        }
        continue;
      }
      return;
    }
  }
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

function sliceFrom(a: Uint8Array, start: number): Uint8Array {
  if (start >= a.length) {
    return new Uint8Array(0);
  }
  return a.slice(start);
}

/// `[VER=5, CMD=CONNECT, RSV, ATYP_PREROUTED, ATYP_DOMAIN, len, host…, port]`
function buildConnectRequest(host: string, port: number, _timeoutMs: number): Uint8Array {
  const hostBytes = new Uint8Array(host.length);
  for (let i = 0; i < host.length; i++) {
    hostBytes[i] = host.charCodeAt(i) & 0xff;
  }
  const out = new Uint8Array(4 + 1 + 1 + hostBytes.length + 2);
  let at = 0;
  out[at++] = 0x05;
  out[at++] = 0x01;
  out[at++] = 0x00;
  out[at++] = ATYP_PREROUTED;
  out[at++] = ATYP_DOMAIN;
  out[at++] = hostBytes.length;
  out.set(hostBytes, at);
  at += hostBytes.length;
  out[at++] = (port >> 8) & 0xff;
  out[at++] = port & 0xff;
  return out;
}

function describeError(e: Object): string {
  const err = e as BusinessError;
  if (err !== undefined && err.message !== undefined) {
    return err.message;
  }
  return JSON.stringify(e);
}

/**
 * Round-trip cost of reaching a destination through the tunnel.
 *
 * Covers the local ingress, the encrypted tunnel handshake towards the server,
 * and the server's own outbound connect — i.e. what a browser pays before the
 * first byte of a page arrives.
 */
export async function probeLatency(
  host: string,
  port: number,
  timeoutMs: number,
): Promise<LatencyResult> {
  const stream = new TunnelStream();
  const started = Date.now();
  console.info('[PhantomProbe] latency probe → ' + host + ':' + port);
  try {
    await stream.open(host, port, timeoutMs, () => {});
  } catch (e) {
    console.warn('[PhantomProbe] latency probe failed: ' + describeError(e));
    stream.close();
    return { ok: false, error: describeError(e), milliseconds: 0 };
  }
  const milliseconds = Date.now() - started;
  stream.close();
  console.info('[PhantomProbe] latency ' + milliseconds + ' ms');
  return { ok: true, error: '', milliseconds: milliseconds };
}

/**
 * Download `path` from `host` through the tunnel for a fixed sampling window
 * and report the achieved throughput.
 *
 * Bounded by time (not just size) so the result is a rate rather than a total,
 * and cancellable at any point: once the window closes the socket is dropped,
 * which also makes the test safe on a metered connection.
 */
export async function runSpeedTest(
  host: string,
  port: number,
  path: string,
  windowMs: number,
  maxBytes: number,
): Promise<SpeedResult> {
  const stream = new TunnelStream();
  let bytes = 0;
  let header = '';
  let settled = false;
  let resolveWindow: (() => void) | null = null;

  try {
    await stream.open(host, port, 10000, (chunk: Uint8Array) => {
      bytes += chunk.length;
      if (header.length < 48) {
        let text = '';
        for (let i = 0; i < chunk.length && header.length + text.length < 48; i++) {
          text += String.fromCharCode(chunk[i]);
        }
        header += text;
      }
      if (bytes >= maxBytes && !settled) {
        settled = true;
        const resolve = resolveWindow;
        if (resolve !== null) {
          resolve();
        }
      }
    });
  } catch (e) {
    stream.close();
    return {
      ok: false, error: describeError(e), bytes: 0, elapsedMs: 0,
      bytesPerSecond: 0, status: '',
    };
  }

  const request =
    'GET ' + path + ' HTTP/1.1\r\n' +
    'Host: ' + host + '\r\n' +
    'User-Agent: Phantom-HarmonyOS\r\n' +
    'Accept: */*\r\n' +
    'Connection: close\r\n\r\n';
  const startedAt = Date.now();
  stream.send(request);

  await new Promise<void>((resolve) => {
    resolveWindow = resolve;
    setTimeout(() => {
      if (!settled) {
        settled = true;
        resolve();
      }
    }, windowMs);
  });

  const elapsedMs = Date.now() - startedAt;
  stream.close();

  const statusLine = header.length > 0 ? header.split('\r\n')[0] : '';
  const ok = statusLine.indexOf(' 200') > 0 || statusLine.indexOf(' 206') > 0;
  console.info('[PhantomProbe] speed test: bytes=' + bytes + ' ms=' + elapsedMs +
    ' status="' + statusLine + '"');
  return {
    ok: ok,
    error: ok ? '' : (statusLine.length > 0 ? '源站返回: ' + statusLine : '未收到响应'),
    bytes: bytes,
    elapsedMs: elapsedMs,
    bytesPerSecond: elapsedMs > 0 ? Math.round(bytes * 1000 / elapsedMs) : 0,
    status: statusLine,
  };
}
