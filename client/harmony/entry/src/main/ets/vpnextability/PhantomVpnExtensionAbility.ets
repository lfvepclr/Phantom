import { VpnExtensionAbility } from '@kit.NetworkKit';
import phantomLib from 'libphantom_harmony.so';

// HarmonyOS VPN extension entry point.
// This ability receives a protected fd from the system and passes it to the
// Rust core through NAPI.  The UI in pages/Index.ets triggers the connection.
export default class PhantomVpnExtensionAbility extends VpnExtensionAbility {
  onCreate(): void {
    console.info('[Phantom] VpnExtensionAbility onCreate');
  }

  onDestroy(): void {
    console.info('[Phantom] VpnExtensionAbility onDestroy');
    try {
      phantomLib.phantomHarmonyStop();
    } catch (e) {
      console.error('[Phantom] stop on destroy: ' + JSON.stringify(e));
    }
  }
}
