import { promptAction, router } from '@kit.ArkUI';
import { common, Want } from '@kit.AbilityKit';
import { vpnExtension } from '@kit.NetworkKit';
import { preferences, uniformTypeDescriptor as utd } from '@kit.ArkData';
import { BusinessError, pasteboard } from '@kit.BasicServicesKit';
import { systemShare } from '@kit.ShareKit';
import {
  PHANTOM_BUNDLE,
  PHANTOM_VPN_ABILITY,
  LogTail,
  TunnelStats,
  VpnStatus,
  clearLog,
  clearNetworkChange,
  clearStats,
  publishStart,
  publishStatus,
  readLogTail,
  readNetworkChangeAt,
  readStats,
  readStatus,
  traceFilePath,
} from '../common/VpnBridge';
import {
  ServerHistoryEntry,
  ServerLink,
  cipherLabel,
  emptyLink,
  linkSummary,
  parseHistory,
  parseServerUri,
  relativeTime,
  serializeHistory,
  shortFingerprint,
  upsertHistory,
} from '../common/ServerLink';
import { LatencyResult, SpeedResult, probeLatency, runSpeedTest } from '../common/TunnelProbe';
import { ServerPage } from './ServerPage';
import { Theme } from '../common/Theme';

/// One entry of the routing-mode segment control.
interface ModeOption {
  key: string;
  label: string;
  hint: string;
}

const MODES: ModeOption[] = [
  { key: 'proxy', label: '全局', hint: '所有流量走服务器' },
  { key: 'smart', label: '智能', hint: '仅白名单域名走服务器' },
  { key: 'direct', label: '直连', hint: '全部直连（相当于关闭分流）' },
];

/// Structured view of the connection, shown in the detail sheet.
///
/// This lives in its own component on purpose: a `bindSheet` builder rendered
/// by the page itself is created once when the panel opens, so the live fields
/// (uptime, throughput, probe results) would stay frozen. `@Prop` inputs are
/// re-synchronised from the page's state on every change.
@Component
struct ServerInfoPanel {
  @Prop name: string = '';
  @Prop status: string = '';
  @Prop statusColor: string = Theme.textSecondary;
  @Prop host: string = '';
  @Prop port: number = 0;
  @Prop proto: string = '';
  @Prop cipher: string = '';
  @Prop serverKey: string = '';
  @Prop psk: string = '';
  @Prop uri: string = '';
  @Prop configured: boolean = false;
  @Prop running: boolean = false;
  @Prop uptimeSeconds: number = 0;
  @Prop downRate: number = 0;
  @Prop upRate: number = 0;
  @Prop totalDown: number = 0;
  @Prop totalUp: number = 0;
  @Prop proxiedFlows: number = 0;
  @Prop directFlows: number = 0;
  @Prop latency: string = '';
  @Prop speedResult: string = '';
  @Prop latencyBusy: boolean = false;
  @Prop speedBusy: boolean = false;
  @Prop tunTrace: boolean = false;
  onProbeLatency: () => void = () => {};
  onProbeSpeed: () => void = () => {};
  onCopy: () => void = () => {};
  onShare: () => void = () => {};
  onToggleTrace: (value: boolean) => void = () => {};

  @Builder
  row(label: string, value: string) {
    Row() {
      Text(label)
        .fontSize(13)
        .fontColor(Theme.textSecondary)
        .width(96)
      Text(value)
        .fontSize(13)
        .fontColor(Theme.textPrimary)
        .layoutWeight(1)
        .maxLines(1)
        .textOverflow({ overflow: TextOverflow.Ellipsis })
    }
    .width('100%')
    .padding({ top: 5, bottom: 5 })
  }

  private rate(bytesPerSecond: number): string {
    if (bytesPerSecond < 1024) {
      return bytesPerSecond + ' B/s';
    }
    if (bytesPerSecond < 1024 * 1024) {
      return (bytesPerSecond / 1024).toFixed(1) + ' KB/s';
    }
    return (bytesPerSecond / (1024 * 1024)).toFixed(2) + ' MB/s';
  }

  private formatSize(bytes: number): string {
    if (bytes < 1024) {
      return bytes + ' B';
    }
    if (bytes < 1024 * 1024) {
      return (bytes / 1024).toFixed(1) + ' KB';
    }
    if (bytes < 1024 * 1024 * 1024) {
      return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    }
    return (bytes / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
  }

  private duration(seconds: number): string {
    const total = Math.max(0, Math.floor(seconds));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) {
      return h + ' 小时 ' + m + ' 分';
    }
    if (m > 0) {
      return m + ' 分 ' + s + ' 秒';
    }
    return s + ' 秒';
  }

  build() {
    Scroll() {
      Column({ space: 10 }) {
        Text(this.configured ? this.name : '未配置')
          .fontSize(20)
          .fontWeight(FontWeight.Bold)
          .fontColor(Theme.textPrimary)
        Text(this.status)
          .fontSize(13)
          .fontColor(this.statusColor)

        Divider().color(Theme.divider).margin({ top: 4, bottom: 4 })

        this.row('地址', this.configured ? this.host + ':' + this.port : '—')
        this.row('传输协议', this.configured ? this.proto.toUpperCase() : '—')
        this.row('加密套件', this.configured ? cipherLabel(this.cipher) : '—')
        this.row('服务器密钥', this.configured ? shortFingerprint(this.serverKey) : '—')
        this.row('预共享密钥', this.psk.length > 0 ? shortFingerprint(this.psk) : '—')
        this.row('连接时长', this.running ? this.duration(this.uptimeSeconds) : '—')
        this.row('实时速率', this.running
          ? '↓ ' + this.rate(this.downRate) + '   ↑ ' + this.rate(this.upRate)
          : '—')
        this.row('本次流量', this.running
          ? '↓ ' + this.formatSize(this.totalDown) + '   ↑ ' + this.formatSize(this.totalUp)
          : '—')
        this.row('分流统计', '隧道 ' + this.proxiedFlows + ' 条 · 直连 ' + this.directFlows + ' 条')
        if (this.latency.length > 0) {
          this.row('链路延迟', this.latency)
        }
        if (this.speedResult.length > 0) {
          this.row('测速结果', this.speedResult)
        }

        Divider().color(Theme.divider).margin({ top: 4, bottom: 4 })

        Row({ space: 8 }) {
          Button('测延迟')
            .height(38)
            .fontSize(13)
            .layoutWeight(1)
            .type(ButtonType.Capsule)
            .backgroundColor(this.running && !this.latencyBusy ? Theme.brand : Theme.surfaceAlt)
            .fontColor(this.running && !this.latencyBusy ? Theme.onBrand : Theme.textTertiary)
            .onClick(() => {
              if (this.running && !this.latencyBusy) {
                this.onProbeLatency();
              }
            })
          Button('测速')
            .height(38)
            .fontSize(13)
            .layoutWeight(1)
            .type(ButtonType.Capsule)
            .backgroundColor(this.running && !this.speedBusy ? Theme.brand : Theme.surfaceAlt)
            .fontColor(this.running && !this.speedBusy ? Theme.onBrand : Theme.textTertiary)
            .onClick(() => {
              if (this.running && !this.speedBusy) {
                this.onProbeSpeed();
              }
            })
        }
        .width('100%')
        .enabled(this.configured)

        Row() {
          Column({ space: 2 }) {
            Text('记录 TUN 追踪')
              .fontSize(14)
              .fontColor(Theme.textPrimary)
            Text('排查 Google 等应用卡住时用；重启隧道后生效')
              .fontSize(11)
              .fontColor(Theme.textTertiary)
          }
          .layoutWeight(1)
          .alignItems(HorizontalAlign.Start)

          Toggle({ type: ToggleType.Switch, isOn: this.tunTrace })
            .selectedColor(Theme.brand)
            .onChange((isOn: boolean) => {
              this.onToggleTrace(isOn);
            })
        }
        .width('100%')
        .padding({ top: 6 })

        Divider().color(Theme.divider).margin({ top: 4, bottom: 4 })

        // Sharing lives here rather than on the dashboard: the quick link is
        // something you hand to another device occasionally, not a daily
        // control, and the QR code needs the room.
        if (this.configured) {
          Text('扫码导入到另一台设备')
            .fontSize(13)
            .fontColor(Theme.textSecondary)
            .width('100%')
            .textAlign(TextAlign.Center)
          Row() {
            QRCode(this.uri)
              .width(160)
              .height(160)
              .color(Theme.textPrimary)
              .backgroundColor(Theme.surface)
          }
          .width('100%')
          .justifyContent(FlexAlign.Center)
        }

        Row({ space: 8 }) {
          Button('复制连接串')
            .height(38)
            .fontSize(13)
            .layoutWeight(1)
            .backgroundColor(Theme.surfaceAlt)
            .fontColor(Theme.textPrimary)
            .type(ButtonType.Capsule)
            .onClick(() => { this.onCopy(); })
          Button('系统分享')
            .height(38)
            .fontSize(13)
            .layoutWeight(1)
            .backgroundColor(Theme.surfaceAlt)
            .fontColor(Theme.textPrimary)
            .type(ButtonType.Capsule)
            .onClick(() => { this.onShare(); })
        }
        .width('100%')
        .enabled(this.configured)
      }
      .width('100%')
      .padding({ left: 20, right: 20, top: 12, bottom: 24 })
      .alignItems(HorizontalAlign.Start)
    }
    .width('100%')
    // Without an explicit height the Scroll grows to its content, which the
    // sheet then clips — the last rows became unreachable.
    .height('100%')
    .align(Alignment.Top)
    .scrollBar(BarState.Off)
  }
}

/// Time after which "Connecting" is assumed to mean "waiting for the system
/// VPN authorization dialog" rather than "tunnel handshake in flight".
const AUTH_HINT_AFTER_MS: number = 4000;

/// Delay before an edit to the URI box is written to preferences, so typing
/// does not hit the disk on every keystroke.
const SAVE_DEBOUNCE_MS: number = 600;

/// The VPN extension publishes its state once a second. If that heartbeat stops
/// (process killed, VPN torn down from Settings, device rebooted) the status
/// file keeps its last value — treat it as stale instead of showing a tunnel
/// that no longer exists.
const STATUS_STALE_AFTER_MS: number = 5000;

/// The extension only rewrites the counter file when a counter actually moved,
/// so a snapshot older than this means "the tunnel is idle": the live rate has
/// to fall back to zero instead of freezing at its last value.
const STATS_STALE_AFTER_MS: number = 2500;

/// Back-off for restarting an extension the system killed. Three attempts,
/// then the user is asked to reconnect by hand.
const RECONNECT_DELAYS_MS: number[] = [3000, 10000, 30000];

/// How long the "network changed" banner stays on screen.
const NET_CHANGE_BANNER_MS: number = 15000;

/// Latency probe target. Whitelisted (so it is tunnel-routed even without the
/// pre-routed marker) and TLS-free at the probe layer: only the TCP connect is
/// timed.
const LATENCY_HOST: string = 'www.gstatic.com';
const LATENCY_PORT: number = 443;
const LATENCY_TIMEOUT_MS: number = 8000;

/// Throughput probe. A large, plain-HTTP file on a whitelisted host; the test
/// samples for a fixed window and hangs up, so the download never completes.
const SPEEDTEST_HOST: string = 'dl.google.com';
const SPEEDTEST_PORT: number = 80;
const SPEEDTEST_PATH: string =
  '/dl/android/studio/install/3.0.0.18/android-studio-ide-171.4408382-windows.exe';
const SPEEDTEST_WINDOW_MS: number = 5000;
const SPEEDTEST_MAX_BYTES: number = 14000000;

/// Lines rendered in the log pane (the file keeps far more).
const LOG_VIEW_LINES: number = 200;

/// Minimum height of the log card: big enough for ~7 monospace lines, small
/// enough that the whole dashboard still fits without scrolling on a phone in
/// portrait.
///
/// On a taller window (a foldable's unfolded inner screen) the card grows into
/// the space the page has left instead of leaving a blank strip under it — see
/// `logCard()` and the dashboard column's `minHeight`.
const LOG_CARD_MIN_HEIGHT: number = 220;

/// `17:26:44` prefix written by the bridge, or '' when the line has none.
function stampOf(line: string): string {
  if (line.length > 9 && line.charAt(2) === ':' && line.charAt(5) === ':' &&
    line.charAt(8) === ' ') {
    return line.substring(0, 8);
  }
  return '';
}

/// The log message with the bridge's timestamp removed, used to detect
/// consecutive repeats.
function messageOf(line: string): string {
  const stamp = stampOf(line);
  return stamp.length === 0 ? line : line.substring(9);
}

/// `3 天前`, or '' when the entry has never been used.
///
/// Whether the last connection actually worked is shown as a status dot next to
/// the address instead of a character here, so the text stays about time.
function historyLabel(entry: ServerHistoryEntry): string {
  return relativeTime(entry.lastUsedMs, Date.now());
}

/// `203.0.113.10:443` — the connection has no meaningful name, so lists show
/// what the user can actually recognise: where it connects to.
function linkAddress(link: ServerLink): string {
  if (!link.valid) {
    return '未配置';
  }
  return link.host + ':' + link.port;
}

function collapsedConnectionLabel(count: number): string {
  return count > 0 ? '已保存 ' + count + ' 个连接' : '添加连接';
}

function modeLabel(key: string): string {
  for (const mode of MODES) {
    if (mode.key === key) {
      return mode.label;
    }
  }
  return key;
}

@Entry
@Component
struct Index {
  @State serverUri: string = '';
  @State proxyMode: string = 'smart';
  @State statusCode: number = VpnStatus.Idle;
  @State status: string = 'Idle';
  @State statusError: string = '';
  @State logs: string = '';
  @State isRunning: boolean = false;
  @State busy: boolean = false;
  @State logPaused: boolean = false;
  @State showDirectLogs: boolean = false;
  @State stale: boolean = false;
  @State requestStartedAt: number = 0;
  /// Live throughput, derived from the extension's counter snapshots.
  @State downRate: number = 0;
  @State upRate: number = 0;
  @State totalDown: number = 0;
  @State totalUp: number = 0;
  @State proxiedFlows: number = 0;
  @State directFlows: number = 0;
  @State runningSince: number = 0;
  /// Whole seconds the tunnel has been up; drives the detail sheet (a `@State`
  /// is required for the panel to re-render, `Date.now()` alone is not).
  @State uptimeSeconds: number = 0;
  @State showInfo: boolean = false;
  /// `false` = 连接 panel, `true` = 服务端 panel (was the bottom tab bar).
  @State showServer: boolean = false;
  /// Log pane blown up to the whole page; toggled by the expand icon.
  @State logFull: boolean = false;
  @State latency: string = '';
  @State latencyBusy: boolean = false;
  @State speedResult: string = '';
  @State speedBusy: boolean = false;
  /// Connection card: collapsed by default once something is configured.
  @State inputExpanded: boolean = false;
  @State history: ServerHistoryEntry[] = [];
  @State tunTrace: boolean = false;
  /// Transient "the radio changed, reconnecting" banner.
  @State showNetBanner: boolean = false;
  /// Non-zero while an automatic restart is scheduled; shown to the user.
  @State reconnectIn: number = 0;
  @State reconnectExhausted: boolean = false;
  private desiredRunning: boolean = false;
  private reconnectAttempts: number = 0;
  private reconnectTimer: number = -1;
  private netChangedAt: number = 0;
  private timer: number = -1;
  private saveTimer: number = -1;
  private filesDir: string = '';
  private logScroller: ListScroller = new ListScroller();
  /** Size of the session log at the last read; lets the poll skip the read. */
  private lastLogSize: number = 0;
  private lastStatsAt: number = 0;
  /// Viewport of the dashboard scroll and the height of everything above the log
  /// card, both in vp (`onAreaChange` reports vp). Together they size the log
  /// card so it fills the page on a tall (unfolded) screen.
  private viewportVp: number = 0;
  private aboveLogVp: number = 0;
  /// Explicit log height; `layoutWeight` alone cannot do this — it hands out the
  /// leftover space even when that is less than the readable minimum.
  @State logCardHeight: number = LOG_CARD_MIN_HEIGHT;
  private lastDown: number = 0;
  private lastUp: number = 0;
  private lastUri: string = '';
  @State logLines: string[] = [];
  @State link: ServerLink = emptyLink();

  aboutToAppear(): void {
    const ctx = getContext(this) as common.UIAbilityContext;
    this.filesDir = ctx.filesDir;
    try {
      const prefs = preferences.getPreferencesSync(ctx, { name: 'phantom_ui' });
      const savedUri = prefs.getSync('serverUri', '') as string;
      const savedMode = prefs.getSync('proxyMode', 'smart') as string;
      const savedShowDirect = prefs.getSync('showDirectLogs', false) as boolean;
      const savedTrace = prefs.getSync('tunTrace', false) as boolean;
      const savedHistory = prefs.getSync('serverHistory', '') as string;
      if (savedUri.length > 0) {
        this.serverUri = savedUri;
      }
      if (savedMode.length > 0) {
        this.proxyMode = savedMode;
      }
      this.showDirectLogs = savedShowDirect;
      this.tunTrace = savedTrace;
      this.history = parseHistory(savedHistory);
      this.link = parseServerUri(this.serverUri);
      // Nothing stored yet but a URI exists (older versions saved only the
      // URI): adopt it so the history dropdown is not empty on first launch.
      if (this.history.length === 0 && this.link.valid) {
        this.history = upsertHistory([], this.serverUri.trim(), Date.now(), 0);
        this.persistHistory();
      }
      this.inputExpanded = !this.link.valid;
    } catch (e) {
      console.warn('[Phantom] load preferences failed: ' + JSON.stringify(e));
      this.inputExpanded = true;
    }
    // The tunnel may already be up from a previous run of this page, or the
    // extension may still be publishing while we were backgrounded.
    this.refresh();
    this.startPolling();
  }

  /// Runs when returning from the scanner: pick up the URI it persisted.
  onPageShow(): void {
    this.reloadPreferences();
    this.refresh();
  }

  aboutToDisappear(): void {
    this.stopPolling();
    if (this.saveTimer !== -1) {
      clearTimeout(this.saveTimer);
      this.saveTimer = -1;
    }
    if (this.reconnectTimer !== -1) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = -1;
    }
  }

  private reloadPreferences(): void {
    try {
      const ctx = getContext(this) as common.UIAbilityContext;
      const prefs = preferences.getPreferencesSync(ctx, { name: 'phantom_ui' });
      const savedUri = prefs.getSync('serverUri', '') as string;
      const savedMode = prefs.getSync('proxyMode', 'smart') as string;
      const savedHistory = prefs.getSync('serverHistory', '') as string;
      if (savedUri.length > 0 && savedUri !== this.serverUri) {
        this.serverUri = savedUri;
        this.link = parseServerUri(savedUri);
        this.adoptIntoHistory(savedUri);
      }
      if (savedMode.length > 0) {
        this.proxyMode = savedMode;
      }
      const parsed = parseHistory(savedHistory);
      if (parsed.length > 0) {
        this.history = parsed;
      }
    } catch (e) {
      console.warn('[Phantom] reload preferences failed: ' + JSON.stringify(e));
    }
  }

  /// Persist on every edit (debounced) so the URI survives even if the user
  /// never taps Start.
  private scheduleSave(): void {
    if (this.saveTimer !== -1) {
      clearTimeout(this.saveTimer);
    }
    this.saveTimer = setTimeout(() => {
      this.saveTimer = -1;
      this.savePreferences();
    }, SAVE_DEBOUNCE_MS);
  }

  private savePreferences(): void {
    try {
      const ctx = getContext(this) as common.UIAbilityContext;
      const prefs = preferences.getPreferencesSync(ctx, { name: 'phantom_ui' });
      prefs.putSync('serverUri', this.serverUri);
      prefs.putSync('proxyMode', this.proxyMode);
      prefs.putSync('showDirectLogs', this.showDirectLogs);
      prefs.putSync('tunTrace', this.tunTrace);
      prefs.putSync('serverHistory', serializeHistory(this.history));
      prefs.flush();
    } catch (e) {
      console.warn('[Phantom] save preferences failed: ' + JSON.stringify(e));
    }
  }

  private persistHistory(): void {
    try {
      const ctx = getContext(this) as common.UIAbilityContext;
      const prefs = preferences.getPreferencesSync(ctx, { name: 'phantom_ui' });
      prefs.putSync('serverHistory', serializeHistory(this.history));
      prefs.flush();
    } catch (e) {
      console.warn('[Phantom] persist history failed: ' + JSON.stringify(e));
    }
  }

  /// Remember a connection that was just used, so it appears in the dropdown.
  private adoptIntoHistory(uri: string, verified: boolean = false): void {
    const trimmed = uri.trim();
    if (!parseServerUri(trimmed).valid) {
      return;
    }
    const now = Date.now();
    this.history = upsertHistory(this.history, trimmed, now, verified ? now : 0);
    this.persistHistory();
  }

  /// Mark the current connection as "worked at least once" for the ✓ badge.
  private markCurrentVerified(): void {
    const trimmed = this.serverUri.trim();
    if (trimmed.length === 0 || this.lastUri === trimmed) {
      return;
    }
    this.lastUri = trimmed;
    this.adoptIntoHistory(trimmed, true);
  }

  startPolling(): void {
    if (this.timer !== -1) {
      return;
    }
    this.timer = setInterval(() => {
      this.refresh();
    }, 500);
  }

  stopPolling(): void {
    if (this.timer !== -1) {
      clearInterval(this.timer);
      this.timer = -1;
    }
  }

  /// The Rust tunnel lives in the VPN extension process, so everything we show
  /// here comes from the sandbox files that process publishes.
  private refresh(): void {
    if (this.filesDir.length === 0) {
      return;
    }
    try {
      const snapshot = readStatus(this.filesDir);
      const heartbeatAge = snapshot.updatedAt > 0 ? Date.now() - snapshot.updatedAt : 0;
      const live =
        snapshot.status !== VpnStatus.Running && snapshot.status !== VpnStatus.Starting
          ? true
          : heartbeatAge < STATUS_STALE_AFTER_MS;
      this.stale = !live;
      this.statusCode = live ? snapshot.status : VpnStatus.Idle;
      this.statusError = snapshot.error;
      this.isRunning = live && snapshot.status === VpnStatus.Running;
      this.busy = live && snapshot.status === VpnStatus.Starting;
      if (this.isRunning) {
        this.requestStartedAt = 0;
        this.reconnectAttempts = 0;
        this.reconnectExhausted = false;
        if (this.reconnectTimer !== -1) {
          clearTimeout(this.reconnectTimer);
          this.reconnectTimer = -1;
          this.reconnectIn = 0;
        }
        this.markCurrentVerified();
        if (this.runningSince === 0) {
          this.runningSince = Date.now();
        }
        const seconds = Math.floor((Date.now() - this.runningSince) / 1000);
        if (seconds !== this.uptimeSeconds) {
          this.uptimeSeconds = seconds;
        }
      } else {
        this.runningSince = 0;
        this.uptimeSeconds = 0;
        this.downRate = 0;
        this.upRate = 0;
      }
      this.status = this.statusText();
      this.refreshNetBanner();
      this.refreshStats();
      this.syncLogHeight();
      this.maybeAutoReconnect();
      if (!this.logPaused) {
        // Only reads the file when it actually grew (and then only its tail).
        const tail: LogTail = readLogTail(this.filesDir, this.lastLogSize);
        this.lastLogSize = tail.size;
        const text = tail.changed ? tail.text : this.logs;
        // Assigning an identical string still re-renders, and 200 monospace
        // lines are not free — only touch the state when it changed.
        if (text !== this.logs) {
          this.logs = text;
          this.logLines = this.visibleLogLines(text);
          if (this.logLines.length > 0) {
            this.logScroller.scrollToIndex(this.logLines.length - 1, false, ScrollAlign.END);
          }
        }
      }
    } catch (e) {
      console.error('[Phantom] poll error: ' + JSON.stringify(e));
    }
  }

  /// Size the log card to the space the page has left.
  ///
  /// The card is the last block on the dashboard, so "space left" is the scroll
  /// viewport minus everything above it. On a foldable's unfolded screen that is
  /// a lot of space, on a folded phone the result falls back to
  /// [`LOG_CARD_MIN_HEIGHT`] and the page scrolls exactly as before.
  ///
  /// Measured rather than declared because `layoutWeight` cannot express
  /// "take the leftover, but never less than X": it shrinks the child below X
  /// when the parent is short.
  private syncLogHeight(): void {
    if (this.viewportVp <= 0 || this.aboveLogVp <= 0) {
      return;
    }
    // Gaps and paddings the column adds around the card.
    const chrome = Theme.gap + 12 + Theme.pagePadding;
    const leftover = this.viewportVp - this.aboveLogVp - chrome;
    const next = Math.max(LOG_CARD_MIN_HEIGHT, Math.floor(leftover));
    // Only react to real changes: this runs on every poll tick, and a state
    // write with the same value would re-render the whole pane.
    if (Math.abs(next - this.logCardHeight) > 1) {
      this.logCardHeight = next;
    }
  }

  /// Show the "network switched" banner for a few seconds after the extension
  /// reports a radio change.
  private refreshNetBanner(): void {
    const at = readNetworkChangeAt(this.filesDir);
    if (at > this.netChangedAt) {
      this.netChangedAt = at;
    }
    const visible = this.netChangedAt > 0 && Date.now() - this.netChangedAt < NET_CHANGE_BANNER_MS;
    if (visible !== this.showNetBanner) {
      this.showNetBanner = visible;
    }
    if (!visible && this.netChangedAt > 0) {
      this.netChangedAt = 0;
      clearNetworkChange(this.filesDir);
    }
  }

  /// Turn the monotonic byte counters into a rate by diffing consecutive
  /// snapshots. The extension republishes at 1 Hz, so a snapshot that has not
  /// changed yet must not be turned into a zero reading.
  private refreshStats(): void {
    const stats: TunnelStats = readStats(this.filesDir);
    this.totalDown = stats.down;
    this.totalUp = stats.up;
    this.proxiedFlows = stats.routeProxy;
    this.directFlows = stats.routeDirect;
    if (stats.at === 0 || stats.at === this.lastStatsAt) {
      // No new snapshot: either the extension has nothing to report yet or the
      // counters have been still for a while. An idle tunnel must not keep
      // showing the last rate it hit.
      if (stats.at !== 0 && Date.now() - stats.at > STATS_STALE_AFTER_MS) {
        this.downRate = 0;
        this.upRate = 0;
      }
      return;
    }
    const seconds = (stats.at - this.lastStatsAt) / 1000;
    const downDelta = stats.down - this.lastDown;
    const upDelta = stats.up - this.lastUp;
    if (this.lastStatsAt > 0 && seconds > 0.2 && downDelta >= 0 && upDelta >= 0) {
      // Smooth over one snapshot so the readout does not flicker.
      this.downRate = Math.round(this.downRate * 0.4 + (downDelta / seconds) * 0.6);
      this.upRate = Math.round(this.upRate * 0.4 + (upDelta / seconds) * 0.6);
    }
    this.lastStatsAt = stats.at;
    this.lastDown = stats.down;
    this.lastUp = stats.up;
  }

  /// The extension is a separate process: when the system kills it (low
  /// memory, VPN torn down from Settings) the tunnel simply stops. Restart it
  /// with a short back-off instead of leaving the user staring at "已中断".
  private maybeAutoReconnect(): void {
    if (!this.desiredRunning || this.reconnectTimer !== -1) {
      return;
    }
    if (this.reconnectAttempts >= RECONNECT_DELAYS_MS.length) {
      if (!this.reconnectExhausted) {
        this.reconnectExhausted = true;
      }
      return;
    }
    if (!this.stale) {
      return;
    }
    const delay = RECONNECT_DELAYS_MS[Math.min(this.reconnectAttempts, RECONNECT_DELAYS_MS.length - 1)];
    this.reconnectAttempts++;
    this.reconnectIn = Math.round(delay / 1000);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = -1;
      this.reconnectIn = 0;
      if (this.desiredRunning && this.stale) {
        console.info('[Phantom] auto-reconnect attempt ' + this.reconnectAttempts);
        this.startTunnel(true);
      }
    }, delay);
  }

  /// Direct-routed traffic is the noisy majority (every background app talking
  /// to a domestic server) and is rarely what you are debugging, so it is
  /// hidden unless explicitly enabled. The full log stays on disk meanwhile.
  ///
  /// Consecutive repeats of the same message collapse into one line with a
  /// `×N` suffix: a phone that keeps re-resolving the same domestic domain (or
  /// retrying the same host) would otherwise push everything useful out of the
  /// 200-line window.
  private visibleLogLines(text: string): string[] {
    const raw = text.length === 0 ? [] : text.split('\n');
    const out: string[] = [];
    let prevMessage = '';
    let prevStamp = '';
    let repeat = 0;
    for (const line of raw) {
      if (line.length === 0) {
        continue;
      }
      const stamp = stampOf(line);
      const message = messageOf(line);
      if (!this.showDirectLogs && message.includes('-> Direct (')) {
        continue;
      }
      if (message === prevMessage && out.length > 0) {
        repeat++;
        out[out.length - 1] = stamp + ' ' + message + ' ×' + (repeat + 1);
        prevStamp = stamp;
        continue;
      }
      prevMessage = message;
      prevStamp = stamp;
      repeat = 0;
      out.push(line);
    }
    if (out.length <= LOG_VIEW_LINES) {
      return out;
    }
    return out.slice(out.length - LOG_VIEW_LINES);
  }

  private statusText(): string {
    switch (this.statusCode) {
      case VpnStatus.Starting: {
        if (this.requestStartedAt > 0 && Date.now() - this.requestStartedAt > AUTH_HINT_AFTER_MS) {
          return '等待系统 VPN 授权…';
        }
        return '正在连接…';
      }
      case VpnStatus.Running: return '已连接';
      case VpnStatus.Error:
        return this.statusError.length > 0 ? '错误：' + this.statusError : '错误';
      default: return this.stale ? '连接已中断' : '未连接';
    }
  }

  private statusColor(): string {
    switch (this.statusCode) {
      case VpnStatus.Running: return Theme.success;
      case VpnStatus.Error: return Theme.danger;
      case VpnStatus.Starting: return Theme.warning;
      default: return this.stale ? Theme.warning : Theme.textSecondary;
    }
  }

  private statusBackground(): string {
    switch (this.statusCode) {
      case VpnStatus.Running: return Theme.successSoft;
      case VpnStatus.Error: return Theme.dangerSoft;
      case VpnStatus.Starting: return Theme.warningSoft;
      default: return this.stale ? Theme.warningSoft : Theme.neutralSoft;
    }
  }

  /// `1.2 MB/s` style, so a busy tunnel stays readable.
  private formatRate(bytesPerSecond: number): string {
    if (bytesPerSecond < 1024) {
      return bytesPerSecond + ' B/s';
    }
    if (bytesPerSecond < 1024 * 1024) {
      return (bytesPerSecond / 1024).toFixed(1) + ' KB/s';
    }
    return (bytesPerSecond / (1024 * 1024)).toFixed(2) + ' MB/s';
  }

  private formatBytes(bytes: number): string {
    if (bytes < 1024) {
      return bytes + ' B';
    }
    if (bytes < 1024 * 1024) {
      return (bytes / 1024).toFixed(1) + ' KB';
    }
    if (bytes < 1024 * 1024 * 1024) {
      return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    }
    return (bytes / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
  }

  // -------------------------------------------------------------------------
  // Actions
  // -------------------------------------------------------------------------

  /// Ask the system to start PhantomVpnExtensionAbility. This is the call that
  /// raises the VPN authorization dialog and, once granted, the VPN key icon.
  private startTunnel(reconnect: boolean = false): void {
    if (this.serverUri.trim().length === 0) {
      promptAction.showToast({ message: '请先填写连接串' });
      this.inputExpanded = true;
      return;
    }
    if (!parseServerUri(this.serverUri).valid) {
      promptAction.showToast({ message: '连接串格式不正确（应以 phantom:// 开头）' });
      this.inputExpanded = true;
      return;
    }
    this.savePreferences();
    this.adoptIntoHistory(this.serverUri);
    if (!reconnect) {
      clearLog(this.filesDir);
      this.lastLogSize = 0;
      clearStats(this.filesDir);
      this.lastStatsAt = 0;
      this.lastDown = 0;
      this.lastUp = 0;
      this.downRate = 0;
      this.upRate = 0;
    }
    this.desiredRunning = true;
    this.reconnectExhausted = false;
    publishStart(this.filesDir, this.serverUri.trim(), this.proxyMode, this.tunTrace);
    if (!reconnect) {
      // Optimistic "connecting" until the extension process reports back.
      publishStatus(this.filesDir, VpnStatus.Starting, '');
      this.statusCode = VpnStatus.Starting;
      this.status = '正在连接…';
      this.requestStartedAt = Date.now();
    }

    const want: Want = {
      bundleName: PHANTOM_BUNDLE,
      abilityName: PHANTOM_VPN_ABILITY,
    };
    vpnExtension.startVpnExtensionAbility(want).then(() => {
      console.info('[Phantom] startVpnExtensionAbility resolved');
    }).catch((err: BusinessError) => {
      console.error('[Phantom] startVpnExtensionAbility failed: ' + JSON.stringify(err));
      publishStatus(this.filesDir, VpnStatus.Error,
        'startVpnExtensionAbility failed: ' + err.code + ' ' + err.message);
      promptAction.showToast({ message: 'VPN 启动失败: ' + err.code });
    });
  }

  private stopTunnel(): void {
    this.desiredRunning = false;
    this.reconnectAttempts = 0;
    this.reconnectExhausted = false;
    if (this.reconnectTimer !== -1) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = -1;
    }
    this.reconnectIn = 0;
    const want: Want = {
      bundleName: PHANTOM_BUNDLE,
      abilityName: PHANTOM_VPN_ABILITY,
    };
    vpnExtension.stopVpnExtensionAbility(want).then(() => {
      console.info('[Phantom] stopVpnExtensionAbility resolved');
      publishStatus(this.filesDir, VpnStatus.Idle, '');
    }).catch((err: BusinessError) => {
      // The extension may already be gone (killed by the system, VPN torn down
      // from Settings). Treat that as success and just reset the local view.
      console.warn('[Phantom] stopVpnExtensionAbility failed: ' + JSON.stringify(err));
      publishStatus(this.filesDir, VpnStatus.Idle, '');
    });
    this.requestStartedAt = 0;
    this.runningSince = 0;
    this.downRate = 0;
    this.upRate = 0;
    this.latency = '';
    this.speedResult = '';
  }

  private copyUri(): void {
    if (this.serverUri.length === 0) {
      return;
    }
    const data = pasteboard.createData(pasteboard.MIMETYPE_TEXT_PLAIN, this.serverUri);
    pasteboard.getSystemPasteboard().setData(data).then(() => {
      promptAction.showToast({ message: '连接串已复制' });
    }).catch((e: BusinessError) => {
      promptAction.showToast({ message: '复制失败: ' + e.code });
    });
  }

  /// Hand the connection string to any other app on the phone (messaging,
  /// mail, notes) through the system share panel.
  private shareUri(): void {
    if (this.serverUri.length === 0) {
      return;
    }
    try {
      const data = new systemShare.SharedData({
        utd: utd.UniformDataType.PLAIN_TEXT,
        content: this.serverUri,
        title: 'Phantom 连接串',
        description: '在 Phantom 客户端中粘贴即可连接',
      });
      const controller = new systemShare.ShareController(data);
      const ctx = getContext(this) as common.UIAbilityContext;
      controller.show(ctx, {
        selectionMode: systemShare.SelectionMode.SINGLE,
        previewMode: systemShare.SharePreviewMode.DEFAULT,
      }).catch((e: BusinessError) => {
        console.warn('[Phantom] share panel failed: ' + JSON.stringify(e));
        this.copyUri();
      });
    } catch (e) {
      console.warn('[Phantom] share failed: ' + JSON.stringify(e));
      this.copyUri();
    }
  }

  private async measureLatency(): Promise<void> {
    if (this.latencyBusy) {
      return;
    }
    if (!this.isRunning) {
      promptAction.showToast({ message: '请先连接隧道' });
      return;
    }
    this.latencyBusy = true;
    this.latency = '测量中…';
    const result: LatencyResult = await probeLatency(LATENCY_HOST, LATENCY_PORT, LATENCY_TIMEOUT_MS);
    this.latency = result.ok ? result.milliseconds + ' ms（含服务端出口）' : '失败：' + result.error;
    this.latencyBusy = false;
    promptAction.showToast({ message: '链路延迟：' + this.latency });
  }

  private async measureSpeed(): Promise<void> {
    if (this.speedBusy) {
      return;
    }
    if (!this.isRunning) {
      promptAction.showToast({ message: '请先连接隧道' });
      return;
    }
    this.speedBusy = true;
    this.speedResult = '测速中（约 5 秒）…';
    const result: SpeedResult = await runSpeedTest(
      SPEEDTEST_HOST,
      SPEEDTEST_PORT,
      SPEEDTEST_PATH,
      SPEEDTEST_WINDOW_MS,
      SPEEDTEST_MAX_BYTES,
    );
    if (result.ok) {
      this.speedResult = '↓ ' + this.formatRate(result.bytesPerSecond) +
        '（' + this.formatBytes(result.bytes) + ' / ' +
        (result.elapsedMs / 1000).toFixed(1) + ' 秒，源站 ' + SPEEDTEST_HOST + '）';
    } else {
      this.speedResult = '失败：' + result.error;
    }
    this.speedBusy = false;
    promptAction.showToast({ message: '测速：' + this.speedResult });
  }

  private useHistoryEntry(entry: ServerHistoryEntry): void {
    // Fill only — connecting stays an explicit user action.
    this.serverUri = entry.uri;
    this.link = parseServerUri(entry.uri);
    this.adoptIntoHistory(entry.uri);
    this.inputExpanded = true;
  }

  private clearHistory(): void {
    this.history = [];
    this.persistHistory();
    promptAction.showToast({ message: '历史记录已清空' });
  }

  private toggleTrace(isOn: boolean): void {
    this.tunTrace = isOn;
    this.savePreferences();
    promptAction.showToast({
      message: isOn ? '已开启追踪，重启隧道后写入 ' + traceFilePath(this.filesDir) : '已关闭追踪',
    });
  }

  // -------------------------------------------------------------------------
  // Views
  // -------------------------------------------------------------------------

  @Builder
  modeSegment() {
    Row() {
      ForEach(MODES, (mode: ModeOption) => {
        Text(mode.label)
          .layoutWeight(1)
          .height(34)
          .textAlign(TextAlign.Center)
          .fontSize(14)
          .fontWeight(this.proxyMode === mode.key ? FontWeight.Medium : FontWeight.Normal)
          .fontColor(this.proxyMode === mode.key ? Theme.onBrand : Theme.textSecondary)
          .backgroundColor(this.proxyMode === mode.key ? Theme.brand : Color.Transparent)
          .borderRadius(Theme.radiusPill)
          .enabled(!this.isRunning && !this.busy)
          .onClick(() => {
            if (!this.isRunning && !this.busy) {
              this.proxyMode = mode.key;
              this.savePreferences();
            }
          })
      }, (mode: ModeOption) => mode.key)
    }
    .width('100%')
    .padding(3)
    .borderRadius(Theme.radiusPill)
    .backgroundColor(Theme.surfaceAlt)
  }

  @Builder
  logFilterSegment() {
    Row() {
      Text('仅隧道')
        .layoutWeight(1)
        .height(28)
        .textAlign(TextAlign.Center)
        .fontSize(12)
        .fontColor(this.showDirectLogs ? Theme.textSecondary : Theme.brand)
        .backgroundColor(this.showDirectLogs ? Color.Transparent : Theme.brandSoft)
        .borderRadius(Theme.radiusChip)
        .onClick(() => { this.setShowDirectLogs(false); })
      Text('全部')
        .layoutWeight(1)
        .height(28)
        .textAlign(TextAlign.Center)
        .fontSize(12)
        .fontColor(this.showDirectLogs ? Theme.brand : Theme.textSecondary)
        .backgroundColor(this.showDirectLogs ? Theme.brandSoft : Color.Transparent)
        .borderRadius(Theme.radiusChip)
        .onClick(() => { this.setShowDirectLogs(true); })
    }
    .width(140)
    .padding(2)
    .borderRadius(14)
    .backgroundColor(Theme.surfaceAlt)
  }

  private setShowDirectLogs(value: boolean): void {
    if (this.showDirectLogs === value) {
      return;
    }
    this.showDirectLogs = value;
    this.savePreferences();
    this.logLines = this.visibleLogLines(this.logs);
    if (this.logLines.length > 0) {
      this.logScroller.scrollToIndex(this.logLines.length - 1, false, ScrollAlign.END);
    }
  }

  @Builder
  historyRow(entry: ServerHistoryEntry) {
    Row({ space: 8 }) {
      // Green once this connection has completed a real session; grey while it
      // is only a saved string. Same signal as the status chip on the dashboard.
      Circle()
        .width(8)
        .height(8)
        .fill(entry.verifiedMs > 0 ? Theme.success : Theme.textTertiary)
      Column({ space: 2 }) {
        Text(linkAddress(parseServerUri(entry.uri)))
          .fontSize(15)
          .fontColor(Theme.textPrimary)
          .maxLines(1)
          .textOverflow({ overflow: TextOverflow.Ellipsis })
        Text(historyLabel(entry))
          .fontSize(11)
          .fontColor(Theme.textTertiary)
          .maxLines(1)
      }
      .alignItems(HorizontalAlign.Start)
      .layoutWeight(1)
    }
    .width('100%')
    .padding({ top: 6, bottom: 6 })
  }

  @Builder
  historyMenu() {
    Menu() {
      if (this.history.length === 0) {
        MenuItem({ content: '暂无历史连接' })
          .enabled(false)
      }
      ForEach(this.history, (entry: ServerHistoryEntry) => {
        MenuItem(() => { this.historyRow(entry) })
          .onClick(() => { this.useHistoryEntry(entry); })
      }, (entry: ServerHistoryEntry) => entry.uri)
      if (this.history.length > 0) {
        MenuItem({ content: '清空历史' })
          .onClick(() => { this.clearHistory(); })
      }
    }
  }

  @Builder
  serverCard() {
    Column({ space: 8 }) {
      Row({ space: 8 }) {
        Text(this.link.valid ? linkAddress(this.link) : '添加连接')
          .fontSize(17)
          .fontWeight(FontWeight.Medium)
          .fontColor(this.link.valid ? Theme.textPrimary : Theme.brand)
          .layoutWeight(1)
          .maxLines(1)
          .textOverflow({ overflow: TextOverflow.Ellipsis })

        Text(this.status)
          .fontSize(12)
          .fontColor(this.statusColor())
          .backgroundColor(this.statusBackground())
          .padding({ left: 10, right: 10, top: 4, bottom: 4 })
          .borderRadius(Theme.radiusChip)
      }
      .width('100%')

      if (this.link.valid) {
        Text(linkSummary(this.link))
          .fontSize(12)
          .fontColor(Theme.textSecondary)
          .width('100%')
          .maxLines(1)
          .textOverflow({ overflow: TextOverflow.Ellipsis })
      }

      if (this.isRunning) {
        Row({ space: 14 }) {
          Text('↓ ' + this.formatRate(this.downRate))
            .fontSize(12)
            .fontColor(Theme.success)
          Text('↑ ' + this.formatRate(this.upRate))
            .fontSize(12)
            .fontColor(Theme.success)
          Text('隧道 ' + this.proxiedFlows + ' · 直连 ' + this.directFlows)
            .fontSize(12)
            .fontColor(Theme.textSecondary)
        }
        .width('100%')
      }
    }
    .width('100%')
    .padding(Theme.cardPadding)
    .backgroundColor(Theme.surface)
    .borderRadius(Theme.radiusCard)
    .onClick(() => {
      if (this.link.valid) {
        this.showInfo = true;
      } else {
        this.inputExpanded = true;
      }
    })
  }

  @Builder
  connectionCard() {
    Column({ space: 10 }) {
      Row({ space: 8 }) {
        Column({ space: 2 }) {
          Text(this.inputExpanded ? '连接串' : collapsedConnectionLabel(this.history.length))
            .fontSize(15)
            .fontWeight(FontWeight.Medium)
            .fontColor(Theme.textPrimary)
            .maxLines(1)
            .textOverflow({ overflow: TextOverflow.Ellipsis })
          Text(this.inputExpanded ? '支持粘贴 / 扫码 / 历史记录' : '手动输入或扫码添加')
            .fontSize(11)
            .fontColor(Theme.textTertiary)
        }
        .layoutWeight(1)
        .alignItems(HorizontalAlign.Start)
        .onClick(() => { this.inputExpanded = !this.inputExpanded; })

        Image($r('app.media.ic_scan'))
          .width(Theme.iconSize)
          .height(Theme.iconSize)
          .fillColor(Theme.brand)
          .onClick(() => {
            router.pushUrl({ url: 'pages/ScanPage' }).catch((e: BusinessError) => {
              promptAction.showToast({ message: '打开扫码失败: ' + e.code });
            });
          })

        Image($r('app.media.ic_chevron'))
          .width(Theme.iconSize)
          .height(Theme.iconSize)
          .fillColor(this.history.length > 0 ? Theme.textSecondary : Theme.textTertiary)
          .bindMenu(this.historyMenu())
      }
      .width('100%')

      if (this.inputExpanded) {
        TextInput({ placeholder: 'phantom://key@host:port', text: this.serverUri })
          .width('100%')
          .enabled(!this.isRunning)
          .backgroundColor(Theme.surfaceAlt)
          .onChange((value: string) => {
            this.serverUri = value;
            this.link = parseServerUri(value);
            this.scheduleSave();
          })

        if (this.history.length > 0) {
          Row({ space: 6 }) {
            Text('历史 ' + this.history.length + ' 条')
              .fontSize(12)
              .fontColor(Theme.textTertiary)
            Blank()
            Text('最近：' +
              linkAddress(parseServerUri(this.history[0].uri)) + ' ' +
              historyLabel(this.history[0]))
              .fontSize(12)
              .fontColor(Theme.textTertiary)
              .maxLines(1)
              .textOverflow({ overflow: TextOverflow.Ellipsis })
          }
          .width('100%')
        }
      }
    }
    .width('100%')
    .padding(Theme.cardPadding)
    .backgroundColor(Theme.surface)
    .borderRadius(Theme.radiusCard)
  }

  @Builder
  logHeader() {
    Row({ space: 8 }) {
      Text('日志')
        .fontSize(15)
        .fontWeight(FontWeight.Medium)
        .fontColor(Theme.textPrimary)

      // Full screen belongs to the *title*, not to the toolbar: sitting next to
      // the trash icon it was one fat finger away from wiping the log.
      Image(this.logFull ? $r('app.media.ic_collapse') : $r('app.media.ic_expand'))
        .width(Theme.iconSize)
        .height(Theme.iconSize)
        .fillColor(Theme.textSecondary)
        .onClick(() => { this.logFull = !this.logFull; })

      Blank()

      this.logFilterSegment()

      Image(this.logPaused ? $r('app.media.ic_play') : $r('app.media.ic_pause'))
        .width(Theme.iconSize)
        .height(Theme.iconSize)
        .fillColor(Theme.textSecondary)
        .onClick(() => { this.logPaused = !this.logPaused; })

      Image($r('app.media.ic_clear'))
        .width(Theme.iconSize)
        .height(Theme.iconSize)
        .fillColor(Theme.textSecondary)
        .onClick(() => {
          clearLog(this.filesDir);
          this.lastLogSize = 0;
          this.logs = '';
          this.logLines = [];
        })
    }
    .width('100%')
  }

  @Builder
  logBody() {
    List({ space: 2, scroller: this.logScroller }) {
      ForEach(this.logLines, (line: string, index: number) => {
        ListItem() {
          // One physical line per entry: the pane is narrow, so wrapping
          // pushed every useful field out of view.
          Text(line)
            .width('100%')
            .fontSize(11)
            .fontFamily('monospace')
            .fontColor(Theme.textSecondary)
            .maxLines(1)
            .textOverflow({ overflow: TextOverflow.Ellipsis })
            .textAlign(TextAlign.Start)
        }
      }, (line: string, index: number) => index + '#' + line)
    }
    .width('100%')
    .layoutWeight(1)
    .backgroundColor(Theme.codeSurface)
    .borderRadius(12)
    .padding({ left: 8, right: 8, top: 6, bottom: 6 })
  }

  @Builder
  logCard() {
    Column({ space: 8 }) {
      this.logHeader()
      this.logBody()
    }
    .width('100%')
    // Height is computed by `syncLogHeight()`: everything the page has left,
    // never below `LOG_CARD_MIN_HEIGHT` (then the page scrolls instead, so
    // enlarging the connection card can no longer squeeze the log to a strip).
    .height(this.logCardHeight)
    .padding(Theme.cardPadding)
    .backgroundColor(Theme.surface)
    .borderRadius(Theme.radiusCard)
  }

  /// Full-screen log: same header and list, sized to the page instead of to the
  /// card. Useful when a line is long or a failure needs scrolling back.
  @Builder
  logFullPage() {
    Column({ space: 8 }) {
      this.logHeader()
      this.logBody()
    }
    .width('100%')
    .height('100%')
    .padding({ left: Theme.cardPadding, right: Theme.cardPadding, top: 12, bottom: 12 })
    .backgroundColor(Theme.canvas)
  }

  @Builder
  infoSheet() {
    ServerInfoPanel({
      name: linkAddress(this.link),
      status: this.status,
      statusColor: this.statusColor(),
      host: this.link.host,
      port: this.link.port,
      proto: this.link.proto,
      cipher: this.link.cipher,
      serverKey: this.link.key,
      psk: this.link.psk,
      uri: this.serverUri.trim(),
      configured: this.link.valid,
      running: this.isRunning,
      uptimeSeconds: this.uptimeSeconds,
      downRate: this.downRate,
      upRate: this.upRate,
      totalDown: this.totalDown,
      totalUp: this.totalUp,
      proxiedFlows: this.proxiedFlows,
      directFlows: this.directFlows,
      latency: this.latency,
      speedResult: this.speedResult,
      latencyBusy: this.latencyBusy,
      speedBusy: this.speedBusy,
      tunTrace: this.tunTrace,
      onProbeLatency: () => { this.measureLatency(); },
      onProbeSpeed: () => { this.measureSpeed(); },
      onCopy: () => { this.copyUri(); },
      onShare: () => { this.shareUri(); },
      onToggleTrace: (value: boolean) => { this.toggleTrace(value); },
    })
  }

  @Builder
  topBar() {
    Row({ space: 8 }) {
      Text('Phantom')
        .fontSize(24)
        .fontWeight(FontWeight.Bold)
        .fontColor(Theme.textPrimary)
        .layoutWeight(1)

      // Page switch. Replaces the bottom `Tabs` bar, which cost ~56 vp of
      // height plus the system navigation inset for two items — and the日志
      // pane is exactly what that space is worth to the user.
      Row() {
        Text('连接')
          .layoutWeight(1)
          .height(30)
          .textAlign(TextAlign.Center)
          .fontSize(13)
          .fontWeight(this.showServer ? FontWeight.Normal : FontWeight.Medium)
          .fontColor(this.showServer ? Theme.textSecondary : Theme.onBrand)
          .backgroundColor(this.showServer ? Color.Transparent : Theme.brand)
          .borderRadius(Theme.radiusPill)
          .onClick(() => {
            this.showServer = false;
            this.startPolling();
          })
        Text('服务端')
          .layoutWeight(1)
          .height(30)
          .textAlign(TextAlign.Center)
          .fontSize(13)
          .fontWeight(this.showServer ? FontWeight.Medium : FontWeight.Normal)
          .fontColor(this.showServer ? Theme.onBrand : Theme.textSecondary)
          .backgroundColor(this.showServer ? Theme.brand : Color.Transparent)
          .borderRadius(Theme.radiusPill)
          .onClick(() => {
            this.showServer = true;
            // Leaving the dashboard stops its polling; the server page owns its
            // own timer and starts in aboutToAppear.
            this.stopPolling();
          })
      }
      .width(150)
      .padding(3)
      .borderRadius(Theme.radiusPill)
      .backgroundColor(Theme.surfaceAlt)
    }
    .width('100%')
    .padding({ left: Theme.pagePadding, right: Theme.pagePadding, top: 6, bottom: 6 })
  }

  @Builder
  dashboard() {
    Scroll() {
      Column({ space: Theme.gap }) {
      // Everything above the log card, in its own column so its height can be
      // measured — the log card below takes whatever the viewport has left.
      Column({ space: Theme.gap }) {
      this.serverCard()

      if (this.showNetBanner) {
        Row({ space: 8 }) {
          Text('网络已切换')
            .fontSize(13)
            .fontColor(Theme.warning)
            .fontWeight(FontWeight.Medium)
          Text('正在自动重建隧道，稍后重试即可')
            .fontSize(12)
            .fontColor(Theme.textSecondary)
            .layoutWeight(1)
        }
        .width('100%')
        .padding({ left: 12, right: 12, top: 8, bottom: 8 })
        .backgroundColor(Theme.warningSoft)
        .borderRadius(Theme.radiusChip)
      }

      Button(this.isRunning ? '停止连接' : (this.busy ? '取消连接' : '启动连接'), { type: ButtonType.Capsule })
        .width('100%')
        .height(52)
        .fontSize(17)
        .fontWeight(FontWeight.Medium)
        .backgroundColor(this.isRunning || this.busy ? Theme.danger : Theme.brand)
        .fontColor(Theme.onBrand)
        .onClick(() => {
          if (this.isRunning || this.busy) {
            this.stopTunnel();
          } else {
            this.startTunnel();
          }
        })

      Text(this.primaryHint())
        .fontSize(12)
        .fontColor(Theme.textSecondary)
        .width('100%')
        .textAlign(TextAlign.Center)

      this.modeSegment()

      this.connectionCard()
      }
      .width('100%')
      .onAreaChange((_old: Area, now: Area) => {
        this.aboveLogVp = now.height as number;
        this.syncLogHeight();
      })

        this.logCard()
      }
      .width('100%')
      // At least as tall as the viewport so the log card has space to grow into
      // (`layoutWeight` needs a definite parent height to distribute). Taller
      // content still overflows and scrolls exactly as before.
      .constraintSize({ minHeight: '100%' })
      .padding({
        left: Theme.pagePadding,
        right: Theme.pagePadding,
        top: 12,
        bottom: Theme.pagePadding,
      })
      .backgroundColor(Theme.canvas)
    }
    .width('100%')
    .height('100%')
    .backgroundColor(Theme.canvas)
    // `Scroll` centres its child when the content is shorter than the viewport.
    // On a foldable's unfolded screen (tall inner display) that floated the whole
    // dashboard in the middle with equal gaps above and below; the page must
    // always start at the top and stack downwards.
    .align(Alignment.Top)
    .onAreaChange((_old: Area, now: Area) => {
      this.viewportVp = now.height as number;
      this.syncLogHeight();
    })
    .scrollBar(BarState.Off)
    .bindSheet(this.showInfo, this.infoSheet(), {
      height: 640,
      showClose: true,
      onDisappear: () => { this.showInfo = false; },
    })
  }

  /// One line under the main button: what the user should do (or know) now.
  private primaryHint(): string {
    if (this.busy) {
      return '请在系统弹窗中点击“允许”，状态栏会出现钥匙图标';
    }
    if (this.isRunning) {
      return '分流：' + modeLabel(this.proxyMode) + ' · 点开服务器卡片可看详情与二维码';
    }
    if (this.reconnectIn > 0) {
      return this.reconnectIn + ' 秒后自动重连（' + this.reconnectAttempts + '/' +
        RECONNECT_DELAYS_MS.length + '）';
    }
    if (this.reconnectExhausted) {
      return '自动重连已停止，请手动点击“启动连接”';
    }
    if (this.stale) {
      return '连接已中断，点击“启动连接”恢复';
    }
    return '点击启动后，系统会请求 VPN 授权';
  }

  build() {
    Stack() {
      Column() {
        this.topBar()

        if (this.showServer) {
          ServerPage()
        } else {
          this.dashboard()
        }
      }
      .width('100%')
      .height('100%')

      // Full-screen log sits above the tabs: it is a reading mode, not a page,
      // so nothing else should compete with it while it is open.
      if (this.logFull) {
        this.logFullPage()
      }
    }
    .width('100%')
    .height('100%')
    .backgroundColor(Theme.canvas)
  }
}
