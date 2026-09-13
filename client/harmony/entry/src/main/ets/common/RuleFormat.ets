/*
 * Parsing, normalising and serialising user whitelist rules.
 *
 * Must stay behaviour-identical to the Android client's `RuleFormat.kt`: both
 * phones feed the same Rust parser, and a rule that one accepts and the other
 * rejects would make the same whitelist mean different things per device.
 *
 * The wire format is one `kind:value` per line:
 *   domain:  exact host          suffix:  host and every *.host
 *   keyword: substring           regex:   ECMAScript-flavoured pattern
 *   cidr:    IPv4 network
 */

/** The five rule shapes the core understands. */
export enum RuleKind {
  DOMAIN = 'domain',
  SUFFIX = 'suffix',
  KEYWORD = 'keyword',
  REGEX = 'regex',
  CIDR = 'cidr',
}

/// Human-readable label per kind, shown next to each entry in the editor.
export function ruleKindLabel(kind: RuleKind): string {
  switch (kind) {
    case RuleKind.DOMAIN: return '精确域名';
    case RuleKind.SUFFIX: return '通配域名';
    case RuleKind.KEYWORD: return '关键字';
    case RuleKind.REGEX: return '正则';
    case RuleKind.CIDR: return 'IP 网段';
    default: return '';
  }
}

export function ruleKindFrom(prefix: string): RuleKind | null {
  for (const kind of [RuleKind.DOMAIN, RuleKind.SUFFIX, RuleKind.KEYWORD,
    RuleKind.REGEX, RuleKind.CIDR]) {
    if (kind === prefix) {
      return kind;
    }
  }
  return null;
}

/** One entry as the editor renders it. */
export interface RuleRow {
  kind: RuleKind;
  value: string;
}

/** Outcome of parsing one line the user typed. */
export class RuleParse {
  readonly ok: boolean;
  readonly kind: RuleKind | null;
  readonly value: string;
  readonly wire: string;
  readonly reason: string;

  private constructor(ok: boolean, kind: RuleKind | null, value: string,
    wire: string, reason: string) {
    this.ok = ok;
    this.kind = kind;
    this.value = value;
    this.wire = wire;
    this.reason = reason;
  }

  static accepted(kind: RuleKind, value: string, wire: string): RuleParse {
    return new RuleParse(true, kind, value, wire, '');
  }

  static rejected(reason: string): RuleParse {
    return new RuleParse(false, null, '', '', reason);
  }
}

/** Clash-style heads, so a rule pasted from another tool still works. */
const CLASH_KINDS: Map<string, RuleKind> = new Map([
  ['DOMAIN', RuleKind.DOMAIN],
  ['DOMAIN-SUFFIX', RuleKind.SUFFIX],
  ['DOMAIN-KEYWORD', RuleKind.KEYWORD],
  ['DOMAIN-REGEX', RuleKind.REGEX],
  ['IP-CIDR', RuleKind.CIDR],
]);

export class RuleFormat {
  /**
   * Parse one line of user input.
   *
   * Forgiving about formatting, strict about meaning: a pasted URL or a
   * Clash-style line is normalised away, while a bare IP is refused with a
   * pointer to the CIDR kind — an IP in a domain rule would never match.
   */
  static parse(raw: string): RuleParse {
    let text = raw.trim();
    if (text.length === 0) {
      return RuleParse.rejected('空行');
    }
    if (text.startsWith('#')) {
      return RuleParse.rejected('注释');
    }
    const beforeHash = text.substring(0, text.indexOf('#') < 0 ? text.length : text.indexOf('#')).trim();
    if (beforeHash.length > 0) {
      text = beforeHash;
    }

    // An explicit kind, either our own `suffix:host` or a Clash-style head.
    // Both have to be stripped here, or the value handed down would still
    // carry the prefix it was recognised by.
    let declared: RuleKind | null = null;
    const colon = text.indexOf(':');
    if (colon > 0) {
      const kind = ruleKindFrom(text.substring(0, colon).trim());
      if (kind !== null) {
        declared = kind;
        text = text.substring(colon + 1).trim();
      }
    }
    const head = text.substring(0, text.indexOf(',') < 0 ? text.length : text.indexOf(','))
      .substring(0, text.indexOf(' ') < 0 ? text.length : text.indexOf(' ')).toUpperCase();
    const clashKind = CLASH_KINDS.get(head);
    if (clashKind !== undefined) {
      declared = clashKind;
      text = RuleFormat.afterAny(text, [',', ' ']);
    }

    // Only a real URL carries a scheme, and only then may path, query and
    // fragment be stripped: doing it unconditionally would eat the `/22` out
    // of a CIDR and the `?` out of a regex.
    const scheme = text.indexOf('://');
    if (scheme >= 0) {
      text = text.substring(scheme + 3);
      text = RuleFormat.beforeAny(text, ['/', '?', '&']);
    }

    return RuleFormat.parsePlain(text, declared);
  }

  /** Serialise a validated entry into the wire format. */
  static toWire(kind: RuleKind, value: string): string {
    return kind + ':' + value;
  }

  /** Split wire text back into entries, dropping anything unusable. */
  static fromWire(text: string): RuleRow[] {
    const rows: RuleRow[] = [];
    for (const line of text.split('\n')) {
      const colon = line.indexOf(':');
      if (colon <= 0) {
        continue;
      }
      const kind = ruleKindFrom(line.substring(0, colon).trim());
      const value = line.substring(colon + 1).trim();
      if (kind !== null && value.length > 0) {
        rows.push({ kind: kind, value: value });
      }
    }
    return rows;
  }

  /**
   * Fold an inclusive IPv4 range into the smallest set of CIDR blocks.
   *
   * The rule engine only speaks CIDR, so an `a.b.c.d - e.f.g.h` entry has to be
   * expressed as a cover. Returns `null` for a malformed or inverted range.
   */
  static ipRangeToCidrs(from: string, to: string): string[] | null {
    const start = RuleFormat.parseIpv4(from);
    const end = RuleFormat.parseIpv4(to);
    if (start === null || end === null || start > end) {
      return null;
    }
    const out: string[] = [];
    let lo = start;
    const hi = end;
    while (lo <= hi) {
      // A block must start on its own boundary, so its size is capped by how
      // many low bits of `lo` are zero.
      let aligned = 0;
      if (lo === 0) {
        aligned = 32;
      } else {
        let probe = lo >>> 0;
        while (aligned < 32 && (probe & 1) === 0) {
          probe = probe >>> 1;
          aligned++;
        }
      }
      let size = 0;
      while (size < aligned && lo + Math.pow(2, size) - 1 < hi) {
        size++;
      }
      out.push(RuleFormat.longToIp(lo) + '/' + (32 - size));
      lo += Math.pow(2, size);
    }
    return out;
  }

  // -------------------------------------------------------------------

  /// Text after the first of `delims`; the input itself when none is present.
  private static afterAny(text: string, delims: string[]): string {
    let best = -1;
    for (const d of delims) {
      const at = text.indexOf(d);
      if (at >= 0 && (best < 0 || at < best)) {
        best = at;
      }
    }
    return best < 0 ? text.trim() : text.substring(best + 1).trim();
  }

  /// Text before the first of `delims`; the input itself when none is present.
  private static beforeAny(text: string, delims: string[]): string {
    let best = -1;
    for (const d of delims) {
      const at = text.indexOf(d);
      if (at >= 0 && (best < 0 || at < best)) {
        best = at;
      }
    }
    return best < 0 ? text.trim() : text.substring(0, best).trim();
  }

  private static parsePlain(raw: string, kind: RuleKind | null): RuleParse {
    let text = raw.trim().toLowerCase();
    if (text.length === 0) {
      return RuleParse.rejected('内容为空');
    }
    // `*.example.com` and `.example.com` are both the suffix kind.
    if (kind === null && (text.startsWith('*.') || text.startsWith('.'))) {
      text = text.startsWith('*.') ? text.substring(2) : text.substring(1);
      return RuleFormat.finish(RuleKind.SUFFIX, text);
    }
    const resolved = kind !== null ? kind : (text.indexOf('/') >= 0 ? RuleKind.CIDR : RuleKind.DOMAIN);
    switch (resolved) {
      case RuleKind.CIDR:
        return RuleFormat.parseIpv4Cidr(text) === null
          ? RuleParse.rejected('网段要形如 91.108.4.0/22')
          : RuleFormat.finish(RuleKind.CIDR, text);
      case RuleKind.REGEX:
        try {
          // ArkTS has no `RegExp` compile check that throws on construction in
          // every mode, so a syntactically broken pattern is caught here.
          const probe = new RegExp(text);
          if (probe === null) {
            return RuleParse.rejected('正则无法编译');
          }
        } catch (e) {
          return RuleParse.rejected('正则无法编译');
        }
        return RuleFormat.finish(RuleKind.REGEX, text);
      case RuleKind.KEYWORD:
        if (text.indexOf('.') >= 0) {
          return RuleParse.rejected('关键字不要带点；按后缀匹配请用通配域名');
        }
        return RuleFormat.finish(RuleKind.KEYWORD, text);
      default:
        if (RuleFormat.parseIpv4(text) !== null) {
          return RuleParse.rejected('这是 IP，请改用「IP 网段」');
        }
        if (!RuleFormat.isHostname(text)) {
          return RuleParse.rejected('不是合法的域名');
        }
        return RuleFormat.finish(resolved, text);
    }
  }

  private static finish(kind: RuleKind, value: string): RuleParse {
    return RuleParse.accepted(kind, value, RuleFormat.toWire(kind, value));
  }

  private static isHostname(text: string): boolean {
    if (text.length > 253 || text.indexOf('.') < 0) {
      return false;
    }
    for (const label of text.split('.')) {
      if (label.length === 0 || label.length > 63) {
        return false;
      }
      for (let i = 0; i < label.length; i++) {
        const c = label.charAt(i);
        const alnum = (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9');
        if (!alnum && c !== '-') {
          return false;
        }
      }
      if (label.startsWith('-') || label.endsWith('-')) {
        return false;
      }
    }
    return true;
  }

  /** Dotted quad → unsigned value, or `null`. */
  private static parseIpv4(text: string): number | null {
    const parts = text.split('.');
    if (parts.length !== 4) {
      return null;
    }
    let value = 0;
    for (const part of parts) {
      if (part.length === 0 || part.length > 3) {
        return null;
      }
      for (let i = 0; i < part.length; i++) {
        const c = part.charAt(i);
        if (c < '0' || c > '9') {
          return null;
        }
      }
      const octet = parseInt(part, 10);
      if (octet > 255) {
        return null;
      }
      value = value * 256 + octet;
    }
    return value;
  }

  private static parseIpv4Cidr(text: string): number | null {
    const slash = text.indexOf('/');
    if (slash <= 0) {
      return null;
    }
    const addr = RuleFormat.parseIpv4(text.substring(0, slash));
    if (addr === null) {
      return null;
    }
    const prefixText = text.substring(slash + 1);
    for (let i = 0; i < prefixText.length; i++) {
      const c = prefixText.charAt(i);
      if (c < '0' || c > '9') {
        return null;
      }
    }
    const prefix = parseInt(prefixText, 10);
    if (prefix < 0 || prefix > 32) {
      return null;
    }
    return addr;
  }

  private static longToIp(value: number): string {
    return `${(value >>> 24) & 0xFF}.${(value >>> 16) & 0xFF}.${(value >>> 8) & 0xFF}.${value & 0xFF}`;
  }
}
