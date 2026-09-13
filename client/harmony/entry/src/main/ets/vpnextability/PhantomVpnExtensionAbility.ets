import { VpnExtensionAbility, connection, vpnExtension } from '@kit.NetworkKit';
import phantomLib from 'libphantom_harmony.so';
import {
  DIRECT_DNS_SERVERS,
  TUNNEL_DNS_SERVER,
  VpnStatus,
  appendLog,
  clearStats,
  publishNetworkChange,
  publishStats,
  publishStatus,
  readStart,
  readUserRules,
} from '../common/VpnBridge';

// HarmonyOS VPN extension entry point.
//
// Flow: the UI (pages/Index.ets) writes `mode\nuri` into a sandbox file and
// starts this ability via `vpnExtension.startVpnExtensionAbility`. The system
// then shows the VPN authorization dialog and — once the user agrees — runs
// this ability in its own process. Creating the vNIC here is what puts the VPN
// key in the status bar; without it the app is just a local SOCKS5 proxy.
//
// We create the vNIC (TUN) through `createVpnConnection().create()`, protect
// this process's own sockets (so the tunnel connection itself never loops back
// into the TUN), and hand the real TUN fd to the Rust core.
//
// Status/logs flow back to the UI through sandbox files, because the two run
// in separate processes and the Rust-side global state cannot be read from the
// UI process.
export default class PhantomVpnExtensionAbility extends VpnExtensionAbility {
  private vpnConn: vpnExtension.VpnConnection | null = null;
  private bridgeTimer: number = -1;
  private logCursor: number = 0;
  private filesDir: string = '';
  /// Guards the network listeners once the ability is going away.
  private destroyed: boolean = false;
  /// De-bounce: one radio switch fires several events within a few hundred ms.
  private netChangeTimer: number = -1;

  onCreate(): void {
    console.info('[Phantom] VpnExtensionAbility onCreate');
    this.filesDir = this.context.filesDir;
    const request = readStart(this.filesDir);
    if (request.uri.length === 0) {
      this.publishError('missing server URI (phantom_vpn_start.txt is empty)');
      return;
    }
    publishStatus(this.filesDir, VpnStatus.Starting, '');

    const serverHost = PhantomVpnExtensionAbility.parseHost(request.uri);
    this.vpnConn = vpnExtension.createVpnConnection(this.context);

    // Protect every socket this process creates afterwards: the tunnel
    // connection to the server must travel the physical network, not the TUN
    // it feeds. (API 22+; on older systems the excluded /32 route below
    // carries the same load.)
    try {
      this.vpnConn.protectProcessNet().then(() => {
        console.info('[Phantom] protectProcessNet ok');
      }).catch((e: Object) => {
        console.warn('[Phantom] protectProcessNet unsupported: ' + JSON.stringify(e));
      });
    } catch (e) {
      console.warn('[Phantom] protectProcessNet call failed: ' + JSON.stringify(e));
    }

    const routes: vpnExtension.RouteInfo[] = [{
      interface: 'tun0',
      destination: { address: { address: '0.0.0.0', family: 1 }, prefixLength: 0 },
      gateway: { address: '10.8.0.1', family: 1 },
      hasGateway: true,
      isDefaultRoute: true,
    }];
    // Belt and braces: exclude the tunnel server itself from VPN routing.
    // Only dotted-quad literals are accepted here — a hostname would make the
    // whole config invalid.
    if (PhantomVpnExtensionAbility.isIPv4(serverHost)) {
      routes.push({
        interface: 'tun0',
        destination: { address: { address: serverHost, family: 1 }, prefixLength: 32 },
        gateway: { address: '10.8.0.1', family: 1 },
        hasGateway: false,
        isDefaultRoute: false,
        isExcludedRoute: true,
      });
    }

    // Exclude the direct resolvers as well: queries they answer must travel the
    // physical network, and on API < 22 (no protectProcessNet) they would
    // otherwise re-enter the TUN and loop.
    for (const resolver of DIRECT_DNS_SERVERS) {
      if (PhantomVpnExtensionAbility.isIPv4(resolver)) {
        routes.push({
          interface: 'tun0',
          destination: { address: { address: resolver, family: 1 }, prefixLength: 32 },
          gateway: { address: '10.8.0.1', family: 1 },
          hasGateway: false,
          isDefaultRoute: false,
          isExcludedRoute: true,
        });
      }
    }

    const config: vpnExtension.VpnConfig = {
      addresses: [{ address: { address: '10.8.0.2', family: 1 }, prefixLength: 24 }],
      routes: routes,
      // Every query apps make lands in the TUN, where the client decides
      // tunnel-vs-direct per domain; advertising one resolver keeps that path
      // deterministic.
      dnsAddresses: [TUNNEL_DNS_SERVER],
      mtu: 1500,
      isIPv4Accepted: true,
    };

    this.vpnConn.create(config).then((tunFd: number) => {
      console.info(`[Phantom] TUN established, fd=${tunFd}`);
      // Routing state is built once per start, so the rules the user edited in
      // the settings sheet have to be in before `phantomHarmonyStart` — after
      // it they would silently wait for the next connection.
      const userRules: string = readUserRules(this.filesDir);
      if (userRules.length > 0) {
        const rulesRc: number = phantomLib.phantomHarmonySetUserRules(userRules);
        console.info(`[Phantom] user rules rc=${rulesRc}`);
      }
      const rc: number = phantomLib.phantomHarmonyStart(tunFd, request.uri, request.mode);
      console.info(`[Phantom] phantomHarmonyStart rc=${rc}`);
      if (rc !== 0) {
        this.publishError(`phantomHarmonyStart failed rc=${rc}`);
        // Drop the vNIC again so the system VPN icon does not linger.
        this.vpnConn?.destroy().catch((e: Object) => {
          console.error('[Phantom] destroy after failed start: ' + JSON.stringify(e));
        });
        return;
      }
      // Packet-level tracing is opt-in from the detail sheet; it has to be
      // armed here because the Rust statics live in *this* process.
      if (request.trace) {
        const tracePath = this.filesDir + '/phantom_tun_trace.log';
        const traceRc: number = phantomLib.phantomHarmonySetTrace(tracePath);
        console.info(`[Phantom] TUN trace rc=${traceRc} -> ${tracePath}`);
      }
      this.watchNetworks();
      publishStatus(this.filesDir, VpnStatus.Running, '');
      this.startBridge();
    }).catch((e: Object) => {
      console.error('[Phantom] TUN create failed: ' + JSON.stringify(e));
      publishStatus(this.filesDir, VpnStatus.Error, 'TUN create failed: ' + JSON.stringify(e));
    });
  }

  onDestroy(): void {
    console.info('[Phantom] VpnExtensionAbility onDestroy');
    this.destroyed = true;
    if (this.netChangeTimer !== -1) {
      clearTimeout(this.netChangeTimer);
      this.netChangeTimer = -1;
    }
    if (this.bridgeTimer !== -1) {
      clearInterval(this.bridgeTimer);
      this.bridgeTimer = -1;
    }
    try {
      phantomLib.phantomHarmonyStop();
    } catch (e) {
      console.error('[Phantom] stop on destroy: ' + JSON.stringify(e));
    }
    try {
      this.vpnConn?.destroy();
    } catch (e) {
      console.error('[Phantom] destroy on destroy: ' + JSON.stringify(e));
    }
    this.vpnConn = null;
    if (this.filesDir.length > 0) {
      publishStatus(this.filesDir, VpnStatus.Idle, '');
      clearStats(this.filesDir);
    }
  }

  /// Bridge Rust tunnel state into the sandbox status/log files so the UI
  /// process can poll them (1 Hz; cheap and robust enough for a status page).
  private startBridge(): void {
    this.bridgeTimer = setInterval(() => {
      try {
        const status: number = phantomLib.phantomHarmonyGetStatus();
        const err: string = status === VpnStatus.Error
          ? phantomLib.phantomHarmonyGetLastError()
          : '';
        publishStatus(this.filesDir, status, err);

        const result: Object[] = phantomLib.phantomHarmonyGetLogs(this.logCursor);
        const lines = result[0] as string[];
        this.logCursor = result[1] as number;
        appendLog(this.filesDir, lines);

        // Live throughput counters for the status page. They ride the same
        // file bridge as the status, so the UI process never needs to load the
        // native library (which would give it an empty copy of the statics).
        publishStats(this.filesDir, phantomLib.phantomHarmonyGetStats());
      } catch (e) {
        console.error('[Phantom] bridge error: ' + JSON.stringify(e));
      }
    }, 1000);
  }

  private publishError(msg: string): void {
    publishStatus(this.filesDir, VpnStatus.Error, msg);
  }

  /// Follow the phone's connectivity and rebuild the datapath when it changes.
  ///
  /// A Wi-Fi ⇄ cellular switch invalidates every socket the tunnel owns: TCP
  /// flows were opened from an address that no longer exists, and the DNS and
  /// QUIC connections along with them. Without this the tunnel looked connected
  /// but nothing moved until the app was restarted by hand.
  private watchNetworks(): void {
    const onChange = () => {
      if (this.destroyed) {
        return;
      }
      // Several events land within a few hundred milliseconds of each other
      // (netLost → netAvailable → capabilities); act once per burst.
      if (this.netChangeTimer !== -1) {
        clearTimeout(this.netChangeTimer);
      }
      this.netChangeTimer = setTimeout(() => {
        this.netChangeTimer = -1;
        this.onNetworkChanged();
      }, 1000);
    };

    try {
      // `on` lives on a NetConnection handle, not the module: an unqualified
      // handle (no NetSpecifier) reports every network change on the device.
      const watcher: connection.NetConnection = connection.createNetConnection();
      watcher.on('netAvailable', (handle: connection.NetHandle) => {
        console.info(`[Phantom] netAvailable: ${handle.netId}`);
        onChange();
      });
      watcher.on('netLost', (handle: connection.NetHandle) => {
        console.info(`[Phantom] netLost: ${handle.netId}`);
        onChange();
      });
      watcher.on('netCapabilitiesChange', (info: connection.NetCapabilityInfo) => {
        console.info(`[Phantom] netCapabilitiesChange: ${info.netHandle.netId}`);
        onChange();
      });
    } catch (e) {
      console.warn('[Phantom] network watch unavailable: ' + JSON.stringify(e));
    }
  }

  private onNetworkChanged(): void {
    console.info('[Phantom] network changed: rebuilding datapath');
    publishNetworkChange(this.filesDir);
    // Re-protect after the switch: the exemption is per process and a new
    // default network can otherwise capture the tunnel's own sockets.
    try {
      this.vpnConn?.protectProcessNet().then(() => {
        console.info('[Phantom] protectProcessNet re-applied after network change');
      }).catch((e: Object) => {
        console.warn('[Phantom] protectProcessNet re-apply failed: ' + JSON.stringify(e));
      });
    } catch (e) {
      console.warn('[Phantom] protectProcessNet re-apply call failed: ' + JSON.stringify(e));
    }
    try {
      const epoch: number = phantomLib.phantomHarmonyOnNetworkChange();
      appendLog(this.filesDir, [
        `network changed: tunnel flows reset (epoch ${epoch})`,
      ]);
    } catch (e) {
      console.error('[Phantom] network change notify failed: ' + JSON.stringify(e));
    }
  }

  /// `true` for dotted-quad IPv4 literals (the only form accepted by the
  /// excluded route above).
  private static isIPv4(host: string): boolean {
    const parts = host.split('.');
    if (parts.length !== 4) {
      return false;
    }
    for (const part of parts) {
      if (part.length === 0 || part.length > 3) {
        return false;
      }
      for (let i = 0; i < part.length; i++) {
        const c = part.charCodeAt(i);
        if (c < 48 || c > 57) {
          return false;
        }
      }
    }
    return true;
  }

  /// Extract the host part from `phantom://KEY@HOST:PORT?...` (IPv4 or DNS).
  private static parseHost(uri: string): string {
    const at = uri.indexOf('@');
    if (at < 0) {
      return '';
    }
    let rest = uri.substring(at + 1);
    const slash = rest.indexOf('?');
    if (slash >= 0) {
      rest = rest.substring(0, slash);
    }
    const frag = rest.indexOf('#');
    if (frag >= 0) {
      rest = rest.substring(0, frag);
    }
    const colon = rest.lastIndexOf(':');
    if (colon > 0) {
      rest = rest.substring(0, colon);
    }
    return rest;
  }
}
