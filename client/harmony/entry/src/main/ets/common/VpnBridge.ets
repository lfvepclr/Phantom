/*
 * Cross-process bridge between the ArkUI page (control plane) and
 * PhantomVpnExtensionAbility (data plane).
 *
 * VpnExtensionAbility runs in its own process, so neither Rust globals nor the
 * in-memory `preferences` cache can be shared between them. Plain files inside
 * the app sandbox are used instead: they are re-read from disk on every poll,
 * so the UI never observes a stale value.
 *
 * Layout (all inside `<filesDir>`):
 *   phantom_vpn_start.txt     — "mode\nuri\ntrace" written by UI, read by extension
 *   phantom_vpn_status.txt    — "status\nts\nerr" written by extension, read by UI
 *   phantom_vpn.log           — appended by extension, rendered by UI
 *   phantom_vpn_stats.txt     — traffic counters, extension -> UI
 *   phantom_vpn_netchange.txt — timestamp of the last network switch
 *   phantom_tun_trace.log     — opt-in packet-level trace of the TUN stack
 */
import { fileIo as fs } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';

/** Bundle that hosts the VPN extension ability. */
export const PHANTOM_BUNDLE: string = 'co.phantom.harmony';

/** `name` field of the `extensionAbilities` entry in module.json5. */
export const PHANTOM_VPN_ABILITY: string = 'PhantomVpnExtensionAbility';

const START_FILE: string = 'phantom_vpn_start.txt';
const STATUS_FILE: string = 'phantom_vpn_status.txt';
const LOG_FILE: string = 'phantom_vpn.log';
const STATS_FILE: string = 'phantom_vpn_stats.txt';
const NETCHANGE_FILE: string = 'phantom_vpn_netchange.txt';
const TRACE_FILE: string = 'phantom_tun_trace.log';

/**
 * Lines a compacted log keeps. Rolling, never unbounded: troubleshooting needs
 * history, a 1 GB VM phone does not need a growing log file.
 */
const LOG_MAX_LINES: number = 2000;
/** Lines handed to the UI: rendering more than this makes the popup crawl. */
export const LOG_UI_LINES: number = 200;

/**
 * Bytes of log tail handed to the UI.
 *
 * The pane renders at most `LOG_UI_LINES` lines, so reading further back only
 * costs UI-thread time: the 20 KB window below always covers well over 200
 * lines, however long the file has grown.
 */
const LOG_TAIL_BYTES: number = 20480;

/**
 * Compact the log once it passes this size.
 *
 * Compaction is a full rewrite (read + trim + write), so it must stay rare. It
 * used to happen implicitly on *every* append because each append rewrote the
 * whole file; appending directly and compacting once per few thousand lines
 * turns the 1 Hz bridge from "rewrite ~2000 lines per second" into "append a
 * few hundred bytes per second, rewrite every few minutes".
 */
const LOG_COMPACT_BYTES: number = 512 * 1024;

/** Reused UTF-8 decoder for the log tail (stateless for whole-buffer reads). */
let logDecoder: util.TextDecoder | null = null;

/**
 * Resolvers the client dials directly (over the physical network) for
 * destinations that are routed outside the tunnel. The VPN extension excludes
 * these addresses from the tunnel so those queries never loop back into it.
 *
 * Keep in sync with `ClientSettings::dns_direct`'s default in core/src/config.rs.
 */
export const DIRECT_DNS_SERVERS: string[] = ['223.5.5.5', '119.29.29.29'];

/** Resolver the tunnelled path uses; only ever reachable through the tunnel. */
export const TUNNEL_DNS_SERVER: string = '8.8.8.8';

/** Tunnel lifecycle, mirrored from the Rust side (`android_get_status`). */
export enum VpnStatus {
  Idle = 0,
  Starting = 1,
  Running = 2,
  Error = 3,
}

export interface VpnStartRequest {
  uri: string;
  mode: string;
  /** Record the packet-level TUN trace for this session. */
  trace: boolean;
}

export interface VpnStatusSnapshot {
  status: number;
  /** `Date.now()` when the extension last published; 0 when unknown. */
  updatedAt: number;
  error: string;
}

/**
 * Live counters of the running tunnel, published by the extension process.
 *
 * Throughput is derived from successive snapshots (`down`/`up` are monotonic
 * byte counters), which keeps the data plane free of any timing logic.
 */
export interface TunnelStats {
  /** TCP payload bytes sent upstream since the tunnel started. */
  up: number;
  /** TCP payload bytes received downstream since the tunnel started. */
  down: number;
  udpUp: number;
  udpDown: number;
  /** TCP flows opened through the tunnel. */
  conns: number;
  /** Flows the whitelist sent straight out (no tunnel). */
  routeDirect: number;
  /** Flows the whitelist sent through the tunnel. */
  routeProxy: number;
  /** `Date.now()` of the snapshot; 0 when no snapshot has been read yet. */
  at: number;
}

export function emptyStats(): TunnelStats {
  return {
    up: 0, down: 0, udpUp: 0, udpDown: 0,
    conns: 0, routeDirect: 0, routeProxy: 0, at: 0,
  };
}

function pathOf(dir: string, name: string): string {
  return dir + '/' + name;
}

function writeText(dir: string, name: string, text: string): void {
  const file = fs.openSync(
    pathOf(dir, name),
    fs.OpenMode.READ_WRITE | fs.OpenMode.CREATE | fs.OpenMode.TRUNC,
  );
  try {
    fs.writeSync(file.fd, text);
  } finally {
    fs.closeSync(file);
  }
}

function readText(dir: string, name: string): string {
  const path = pathOf(dir, name);
  try {
    if (!fs.accessSync(path)) {
      return '';
    }
    return fs.readTextSync(path);
  } catch (e) {
    return '';
  }
}

/// Parse a leading integer without relying on `JSON.parse` casts.
function parseLeadingInt(text: string, fallback: number): number {
  let end = 0;
  while (end < text.length) {
    const c = text.charCodeAt(end);
    if (c < 48 || c > 57) {
      break;
    }
    end++;
  }
  if (end === 0) {
    return fallback;
  }
  return parseInt(text.substring(0, end), 10);
}

// ---------------------------------------------------------------------------
// Start request (UI -> extension)
// ---------------------------------------------------------------------------

/// Hand the connection parameters to the extension process.
/// Must be called *before* `startVpnExtensionAbility`, which reads it in onCreate.
export function publishStart(dir: string, uri: string, mode: string, trace: boolean): void {
  writeText(dir, START_FILE, mode + '\n' + uri + '\n' + (trace ? '1' : '0'));
}

export function readStart(dir: string): VpnStartRequest {
  const text = readText(dir, START_FILE);
  if (text.length === 0) {
    return { uri: '', mode: 'smart', trace: false };
  }
  const nl = text.indexOf('\n');
  if (nl < 0) {
    return { uri: text.trim(), mode: 'smart', trace: false };
  }
  const mode = text.substring(0, nl).trim();
  const rest = text.substring(nl + 1);
  const second = rest.indexOf('\n');
  const uri = (second < 0 ? rest : rest.substring(0, second)).trim();
  const trace = second >= 0 && rest.substring(second + 1).trim() === '1';
  return { uri: uri, mode: mode.length === 0 ? 'smart' : mode, trace: trace };
}

// ---------------------------------------------------------------------------
// Network changes (extension -> UI)
// ---------------------------------------------------------------------------

/// Absolute path of the opt-in TUN trace file.
export function traceFilePath(dir: string): string {
  return pathOf(dir, TRACE_FILE);
}

/// Stamp "the radio just changed". The UI shows a short "reconnecting" banner
/// for a few seconds afterwards; the extension writes it before it starts
/// rebuilding its sockets.
export function publishNetworkChange(dir: string): void {
  writeText(dir, NETCHANGE_FILE, '' + Date.now());
}

/// `Date.now()` of the last network change, or 0 when none was recorded.
export function readNetworkChangeAt(dir: string): number {
  return parseLeadingInt(readText(dir, NETCHANGE_FILE), 0);
}

export function clearNetworkChange(dir: string): void {
  writeText(dir, NETCHANGE_FILE, '');
}

// ---------------------------------------------------------------------------
// Status (extension -> UI)
// ---------------------------------------------------------------------------

export function publishStatus(dir: string, status: number, error: string): void {
  const flatError = error.replace(/\r?\n/g, ' ');
  writeText(dir, STATUS_FILE, status + '\n' + Date.now() + '\n' + flatError);
}

export function readStatus(dir: string): VpnStatusSnapshot {
  const text = readText(dir, STATUS_FILE);
  if (text.length === 0) {
    return { status: VpnStatus.Idle, updatedAt: 0, error: '' };
  }
  const first = text.indexOf('\n');
  if (first < 0) {
    return { status: parseLeadingInt(text, VpnStatus.Idle), updatedAt: 0, error: '' };
  }
  const second = text.indexOf('\n', first + 1);
  const status = parseLeadingInt(text.substring(0, first), VpnStatus.Idle);
  const updatedAt = parseLeadingInt(
    second < 0 ? text.substring(first + 1) : text.substring(first + 1, second),
    0,
  );
  const error = second < 0 ? '' : text.substring(second + 1);
  return { status: status, updatedAt: updatedAt, error: error };
}

// ---------------------------------------------------------------------------
// Logs (extension -> UI)
// ---------------------------------------------------------------------------

export function clearLog(dir: string): void {
  writeText(dir, LOG_FILE, '');
}

function pad2(value: number): string {
  return value < 10 ? '0' + value : '' + value;
}

/// Short local wall-clock stamp ("17:26:44"). The Rust side logs without a
/// timestamp on this platform because the phone's log pane is narrow.
function clockStamp(): string {
  const now = new Date();
  return pad2(now.getHours()) + ':' + pad2(now.getMinutes()) + ':' + pad2(now.getSeconds());
}

/// Append one bridge tick's worth of lines to the session log.
///
/// Write-only: the file is opened in append mode and the whole thing is
/// rewritten only when it outgrows `LOG_COMPACT_BYTES`, so steady-state cost
/// per tick is the bytes of the new lines and nothing else.
export function appendLog(dir: string, lines: string[]): void {
  if (lines.length === 0) {
    return;
  }
  const stamp = clockStamp();
  let chunk = '';
  for (const line of lines) {
    chunk += stamp + ' ' + line + '\n';
  }
  const path = pathOf(dir, LOG_FILE);
  try {
    const file = fs.openSync(
      path,
      fs.OpenMode.READ_WRITE | fs.OpenMode.CREATE | fs.OpenMode.APPEND,
    );
    try {
      fs.writeSync(file.fd, chunk);
    } finally {
      fs.closeSync(file);
    }
    if (fs.statSync(path).size > LOG_COMPACT_BYTES) {
      compactLog(dir);
    }
  } catch (e) {
    // Logging must never take the tunnel down.
    console.warn('[Phantom] append log failed: ' + JSON.stringify(e));
  }
}

/// Trim an oversized log back to its last `LOG_MAX_LINES` lines.
function compactLog(dir: string): void {
  const lines = readText(dir, LOG_FILE).split('\n').filter((line: string) => line.length > 0);
  if (lines.length <= LOG_MAX_LINES) {
    return;
  }
  writeText(dir, LOG_FILE, lines.slice(lines.length - LOG_MAX_LINES).join('\n') + '\n');
  console.info('[Phantom] log compacted to ' + LOG_MAX_LINES + ' lines');
}

/// One read of the session log tail.
export interface LogTail {
  /// New text (the last `LOG_UI_LINES` lines), or `''` when nothing changed.
  text: string;
  /// File size to pass back as `knownSize` on the next call.
  size: number;
  /// True when the file changed and `text` holds its current tail.
  changed: boolean;
}

/// Read the tail of the session log, skipping the read when the file has not
/// grown since `knownSize`.
///
/// The UI polls this from its 500 ms refresh loop, so the common case has to be
/// free: a `stat` that reports an unchanged size returns without touching the
/// file. When the log did grow, only the last `LOG_TAIL_BYTES` are read — the
/// pane cannot show more than `LOG_UI_LINES` lines anyway.
export function readLogTail(dir: string, knownSize: number): LogTail {
  const path = pathOf(dir, LOG_FILE);
  let size = 0;
  try {
    if (!fs.accessSync(path)) {
      return { text: '', size: 0, changed: false };
    }
    size = fs.statSync(path).size;
  } catch (e) {
    return { text: '', size: knownSize, changed: false };
  }
  if (size === knownSize) {
    return { text: '', size: size, changed: false };
  }

  let raw = '';
  try {
    if (size <= LOG_TAIL_BYTES) {
      raw = fs.readTextSync(path);
    } else {
      const file = fs.openSync(path, fs.OpenMode.READ_ONLY);
      try {
        const buffer = new ArrayBuffer(LOG_TAIL_BYTES);
        const read = fs.readSync(file.fd, buffer, {
          offset: size - LOG_TAIL_BYTES,
          length: LOG_TAIL_BYTES,
        });
        raw = decodeUtf8(buffer, read);
      } finally {
        fs.closeSync(file);
      }
    }
  } catch (e) {
    console.warn('[Phantom] read log failed: ' + JSON.stringify(e));
    return { text: '', size: knownSize, changed: false };
  }

  // The window starts mid-line (and may cut a multi-byte character in half):
  // drop everything up to and including the first newline.
  const firstBreak = raw.indexOf('\n');
  const body = firstBreak >= 0 ? raw.substring(firstBreak + 1) : raw;
  const lines = body.split('\n').filter((line: string) => line.length > 0);
  const tail = lines.length <= LOG_UI_LINES ? lines : lines.slice(lines.length - LOG_UI_LINES);
  return { text: tail.join('\n'), size: size, changed: true };
}

/// Decode at most `length` bytes of `buffer` as UTF-8.
function decodeUtf8(buffer: ArrayBuffer, length: number): string {
  if (length <= 0) {
    return '';
  }
  if (logDecoder === null) {
    logDecoder = util.TextDecoder.create('utf-8');
  }
  return logDecoder.decodeToString(new Uint8Array(buffer, 0, length));
}

// ---------------------------------------------------------------------------
// Traffic counters (extension -> UI)
// ---------------------------------------------------------------------------

/// Last payload written to `phantom_vpn_stats.txt`.
///
/// The extension republishes at 1 Hz, but the counters only move when traffic
/// flows; skipping the repeated write keeps an idle (or tunnelled-but-silent)
/// tunnel from dirtying a file every second. The timestamp in the file is *not*
/// refreshed by design — the UI treats a snapshot older than a couple of
/// seconds as "idle" and drops its extrapolated rate to zero.
let lastStatsPayload: string = '';

/// Persist one snapshot of the Rust counters. Called at 1 Hz by the extension;
/// the UI diffs consecutive snapshots to show live throughput.
export function publishStats(dir: string, json: string): void {
  if (json === lastStatsPayload) {
    return;
  }
  lastStatsPayload = json;
  writeText(dir, STATS_FILE, Date.now() + '\n' + json);
}

/// Read the last published counter snapshot (all zeroes when absent).
export function readStats(dir: string): TunnelStats {
  const text = readText(dir, STATS_FILE);
  const stats = emptyStats();
  if (text.length === 0) {
    return stats;
  }
  const nl = text.indexOf('\n');
  if (nl < 0) {
    return stats;
  }
  stats.at = parseLeadingInt(text.substring(0, nl), 0);
  // The payload is a flat object of non-negative integers produced by the Rust
  // side; reading the numbers directly avoids depending on `JSON.parse`'s
  // (untyped) result in ArkTS.
  const body = text.substring(nl + 1);
  stats.up = jsonCounter(body, 'up');
  stats.down = jsonCounter(body, 'down');
  stats.udpUp = jsonCounter(body, 'udp_up');
  stats.udpDown = jsonCounter(body, 'udp_down');
  stats.conns = jsonCounter(body, 'conns');
  stats.routeDirect = jsonCounter(body, 'route_direct');
  stats.routeProxy = jsonCounter(body, 'route_proxy');
  return stats;
}

/// Read `"key":<integer>` out of a flat JSON object.
function jsonCounter(body: string, key: string): number {
  const marker = '"' + key + '":';
  const at = body.indexOf(marker);
  if (at < 0) {
    return 0;
  }
  return parseLeadingInt(body.substring(at + marker.length), 0);
}

/// Clear the counter snapshot (called when a session stops, so a stale
/// throughput reading does not survive into the next connection).
export function clearStats(dir: string): void {
  lastStatsPayload = '';
  writeText(dir, STATS_FILE, '');
}
