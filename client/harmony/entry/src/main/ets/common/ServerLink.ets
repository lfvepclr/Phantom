/*
 * Parsing helpers for `phantom://` quick links.
 *
 * The UI never has to show the raw string: every screen that mentions a
 * connection renders the structural fields (node name, address, transport,
 * cipher) produced here instead.
 *
 * Format:
 *   phantom://<base64 server key>@<host>:<port>?psk=<base64>&cipher=<c>&proto=<p>#<name>
 */

export interface ServerLink {
  valid: boolean;
  /** Base64 server public key (the URI authority). */
  key: string;
  /** Pre-shared key from the query string. */
  psk: string;
  host: string;
  port: number;
  /** `auto` | `aes-256-gcm` | `aes-128-gcm` | `ascon-128` | `chacha20-poly1305`. */
  cipher: string;
  /** `tcp` | `quic`. */
  proto: string;
  /** `#fragment` — the node label chosen when the server was bootstrapped. */
  name: string;
}

export function emptyLink(): ServerLink {
  return {
    valid: false, key: '', psk: '', host: '', port: 0,
    cipher: 'auto', proto: 'tcp', name: '',
  };
}

/// Parse a quick link. Never throws: malformed input comes back `valid: false`
/// so the UI can show "连接串格式不正确" instead of a crash.
export function parseServerUri(uri: string): ServerLink {
  const link = emptyLink();
  const trimmed = uri.trim();
  if (!trimmed.startsWith('phantom://')) {
    return link;
  }

  let rest = trimmed.substring('phantom://'.length);

  // #name
  const hash = rest.indexOf('#');
  if (hash >= 0) {
    link.name = rest.substring(hash + 1).trim();
    rest = rest.substring(0, hash);
  }

  // ?query
  const query = rest.indexOf('?');
  if (query >= 0) {
    const pairs = rest.substring(query + 1).split('&');
    for (const pair of pairs) {
      const eq = pair.indexOf('=');
      if (eq < 0) {
        continue;
      }
      const key = pair.substring(0, eq);
      const value = pair.substring(eq + 1);
      if (key === 'psk') {
        link.psk = value;
      } else if (key === 'cipher' && value.length > 0) {
        link.cipher = value;
      } else if (key === 'proto' && value.length > 0) {
        link.proto = value;
      }
    }
    rest = rest.substring(0, query);
  }

  // key@host:port
  const at = rest.lastIndexOf('@');
  if (at < 0) {
    return link;
  }
  link.key = rest.substring(0, at);
  const authority = rest.substring(at + 1);

  // Bracketed IPv6 (`[::1]:443`) or the usual host:port.
  if (authority.startsWith('[')) {
    const close = authority.indexOf(']');
    if (close < 0) {
      return link;
    }
    link.host = authority.substring(1, close);
    const colon = authority.indexOf(':', close);
    link.port = colon < 0 ? 443 : toPort(authority.substring(colon + 1));
  } else {
    const colon = authority.lastIndexOf(':');
    if (colon < 0) {
      link.host = authority;
      link.port = 443;
    } else {
      link.host = authority.substring(0, colon);
      link.port = toPort(authority.substring(colon + 1));
    }
  }

  link.valid = link.key.length > 0 && link.host.length > 0 && link.port > 0;
  return link;
}

function toPort(text: string): number {
  const trimmed = text.trim();
  if (trimmed.length === 0) {
    return 0;
  }
  for (let i = 0; i < trimmed.length; i++) {
    const c = trimmed.charCodeAt(i);
    if (c < 48 || c > 57) {
      return 0;
    }
  }
  const value = parseInt(trimmed, 10);
  return value > 0 && value <= 65535 ? value : 0;
}

/// Node label for the UI: the fragment when present, otherwise the address.
export function linkTitle(link: ServerLink): string {
  if (!link.valid) {
    return '未配置';
  }
  return link.name.length > 0 ? link.name : link.host + ':' + link.port;
}

/// One-line description of where traffic goes, e.g. `203.0.113.10:443 · TCP · AES-256`.
export function linkSummary(link: ServerLink): string {
  if (!link.valid) {
    return '尚未填写连接串';
  }
  return link.host + ':' + link.port + ' · ' + link.proto.toUpperCase() + ' · ' + cipherLabel(link.cipher);
}

export function cipherLabel(cipher: string): string {
  switch (cipher) {
    case 'aes-256-gcm': return 'AES-256-GCM';
    case 'aes-128-gcm': return 'AES-128-GCM';
    case 'chacha20-poly1305': return 'ChaCha20';
    case 'ascon-128': return 'Ascon-128';
    case 'auto': return '自动（AES-256 优先）';
    default: return cipher;
  }
}

/// First characters of a base64 key/PSK, enough to tell two nodes apart
/// without putting the whole secret on screen.
export function shortFingerprint(value: string): string {
  if (value.length <= 10) {
    return value;
  }
  return value.substring(0, 10) + '…';
}

// ---------------------------------------------------------------------------
// Connection history
// ---------------------------------------------------------------------------

/**
 * One remembered connection.
 *
 * `verifiedMs` is only set once a tunnel with this URI actually reached
 * `Running`, so a tick next to an entry means "this one worked on this phone",
 * not merely "it was typed once".
 */
export interface ServerHistoryEntry {
  uri: string;
  lastUsedMs: number;
  verifiedMs: number;
}

/** How many connections are remembered (most recent first). */
export const HISTORY_MAX: number = 20;

/**
 * Persisted form: one entry per line, `uri\tlastUsedMs\tverifiedMs`.
 *
 * A tab-separated line keeps the parser trivial (the URI itself never contains
 * a tab) and keeps the preference readable when dumped from `hdc shell`.
 */
export function serializeHistory(entries: ServerHistoryEntry[]): string {
  const lines: string[] = [];
  for (const entry of entries) {
    lines.push(entry.uri + '\t' + entry.lastUsedMs + '\t' + entry.verifiedMs);
  }
  return lines.join('\n');
}

export function parseHistory(text: string): ServerHistoryEntry[] {
  const entries: ServerHistoryEntry[] = [];
  if (text.length === 0) {
    return entries;
  }
  for (const line of text.split('\n')) {
    const trimmed = line.trim();
    if (trimmed.length === 0) {
      continue;
    }
    const parts = trimmed.split('\t');
    const uri = parts[0].trim();
    if (!uri.startsWith('phantom://')) {
      continue;
    }
    entries.push({
      uri: uri,
      lastUsedMs: parts.length > 1 ? toNumber(parts[1]) : 0,
      verifiedMs: parts.length > 2 ? toNumber(parts[2]) : 0,
    });
  }
  return sortAndDedupe(entries);
}

/**
 * Record a use of `uri`: move it to the front, keep the newest timestamps and
 * drop anything past [`HISTORY_MAX`].
 */
export function upsertHistory(
  entries: ServerHistoryEntry[],
  uri: string,
  usedAtMs: number,
  verifiedAtMs: number,
): ServerHistoryEntry[] {
  const next: ServerHistoryEntry[] = [];
  for (const entry of entries) {
    if (entry.uri === uri) {
      continue;
    }
    next.push(entry);
  }
  let verified = verifiedAtMs;
  for (const entry of entries) {
    if (entry.uri === uri && entry.verifiedMs > verified) {
      verified = entry.verifiedMs;
    }
  }
  next.push({ uri: uri, lastUsedMs: usedAtMs, verifiedMs: verified });
  return sortAndDedupe(next).slice(0, HISTORY_MAX);
}

export function removeFromHistory(
  entries: ServerHistoryEntry[],
  uri: string,
): ServerHistoryEntry[] {
  return entries.filter((entry: ServerHistoryEntry) => entry.uri !== uri);
}

function sortAndDedupe(entries: ServerHistoryEntry[]): ServerHistoryEntry[] {
  const sorted = entries.sort((a: ServerHistoryEntry, b: ServerHistoryEntry) =>
    b.lastUsedMs - a.lastUsedMs);
  const seen: string[] = [];
  const unique: ServerHistoryEntry[] = [];
  for (const entry of sorted) {
    if (seen.includes(entry.uri)) {
      continue;
    }
    seen.push(entry.uri);
    unique.push(entry);
  }
  return unique;
}

function toNumber(text: string): number {
  const trimmed = text.trim();
  if (trimmed.length === 0) {
    return 0;
  }
  const value = parseInt(trimmed, 10);
  return Number.isNaN(value) ? 0 : value;
}

/** `刚刚` / `5 分钟前` / `3 天前`, for the history menu. */
export function relativeTime(thenMs: number, nowMs: number): string {
  if (thenMs <= 0) {
    return '';
  }
  const seconds = Math.max(0, Math.floor((nowMs - thenMs) / 1000));
  if (seconds < 60) {
    return '刚刚';
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return minutes + ' 分钟前';
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return hours + ' 小时前';
  }
  return Math.floor(hours / 24) + ' 天前';
}
