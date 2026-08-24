import { VpnExtensionAbility, vpnExtension } from '@kit.NetworkKit';
import { preferences } from '@kit.ArkData';
import phantomLib from 'libphantom_harmony.so';

// HarmonyOS VPN extension entry point.
//
// Flow: the UI (pages/Index.ets) writes serverUri/proxyMode into the shared
// `phantom_vpn` preferences and starts this ability via
// `vpnExtension.startVpnExtensionAbility`. We then create the TUN interface,
// protect this process's own sockets (so the tunnel connection itself never
// loops back into the TUN), and hand the real TUN fd to the Rust core.
//
// Status/logs flow back to the UI through the same preferences file (the two
// live in separate processes, so the Rust-side global state cannot be read
// directly from the UI process).
export default class PhantomVpnExtensionAbility extends VpnExtensionAbility {
  private vpnConn: vpnExtension.VpnConnection | null = null;
  private bridgeTimer: number = -1;
  private logCursor: number = 0;

  onCreate(): void {
    console.info('[Phantom] VpnExtensionAbility onCreate');
    const prefs = preferences.getPreferencesSync(this.context, { name: 'phantom_vpn' });
    const uri = prefs.getSync('serverUri', '') as string;
    const mode = prefs.getSync('proxyMode', 'smart') as string;
    if (uri.length === 0) {
      this.publishError(prefs, 'missing serverUri in phantom_vpn preferences');
      return;
    }

    const serverHost = PhantomVpnExtensionAbility.parseHost(uri);
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
    if (serverHost.length > 0) {
      routes.push({
        interface: 'tun0',
        destination: { address: { address: serverHost, family: 1 }, prefixLength: 32 },
        gateway: { address: '10.8.0.1', family: 1 },
        hasGateway: false,
        isDefaultRoute: false,
        isExcludedRoute: true,
      });
    }

    const config: vpnExtension.VpnConfig = {
      addresses: [{ address: { address: '10.8.0.2', family: 1 }, prefixLength: 32 }],
      routes: routes,
      dnsAddresses: ['223.5.5.5', '8.8.8.8'],
      mtu: 1500,
      isIPv4Accepted: true,
    };

    this.vpnConn.create(config).then((tunFd: number) => {
      console.info(`[Phantom] TUN established, fd=${tunFd}`);
      const rc: number = phantomLib.phantomHarmonyStart(tunFd, uri, mode);
      console.info(`[Phantom] phantomHarmonyStart rc=${rc}`);
      if (rc !== 0) {
        this.publishError(prefs, `phantomHarmonyStart failed rc=${rc}`);
        return;
      }
      this.startBridge(prefs);
    }).catch((e: Object) => {
      console.error('[Phantom] TUN create failed: ' + JSON.stringify(e));
      this.publishError(prefs, 'TUN create failed: ' + JSON.stringify(e));
    });
  }

  onDestroy(): void {
    console.info('[Phantom] VpnExtensionAbility onDestroy');
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
  }

  /// Bridge Rust tunnel state into the shared preferences so the UI process
  /// can poll it (1 Hz; cheap and robust enough for a status page).
  private startBridge(prefs: preferences.Preferences): void {
    this.bridgeTimer = setInterval(() => {
      try {
        const status: number = phantomLib.phantomHarmonyGetStatus();
        prefs.putSync('vpnStatus', status);
        if (status === 3) {
          const err: string = phantomLib.phantomHarmonyGetLastError();
          prefs.putSync('vpnError', err);
        }
        const result: Object[] = phantomLib.phantomHarmonyGetLogs(this.logCursor);
        const lines = result[0] as string[];
        this.logCursor = result[1] as number;
        if (lines.length > 0) {
          const prev = prefs.getSync('vpnLog', '') as string;
          let merged = prev + '\n' + lines.join('\n');
          if (merged.length > 16000) {
            merged = merged.substring(merged.length - 16000);
          }
          prefs.putSync('vpnLog', merged);
        }
        prefs.flush();
      } catch (e) {
        console.error('[Phantom] bridge error: ' + JSON.stringify(e));
      }
    }, 1000);
  }

  private publishError(prefs: preferences.Preferences, msg: string): void {
    prefs.putSync('vpnStatus', 3);
    prefs.putSync('vpnError', msg);
    prefs.flush();
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
