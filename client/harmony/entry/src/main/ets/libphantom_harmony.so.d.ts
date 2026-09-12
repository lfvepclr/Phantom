declare namespace PhantomLib {
  function phantomHarmonyStart(fd: number, uri: string, mode: string): number;
  function phantomHarmonyStartConfig(fd: number, config: string): number;
  function phantomHarmonyStop(): number;
  function phantomHarmonyGetStatus(): number;
  function phantomHarmonyGetLastError(): string;
  function phantomHarmonyGetLogs(sinceCursor: number): Object[];
  // Live traffic counters as a flat JSON object
  // ({"up":…,"down":…,"udp_up":…,"udp_down":…,"conns":…,"route_direct":…,"route_proxy":…}).
  function phantomHarmonyGetStats(): string;
  // Network-change notification: resets in-flight flows, drops the shared DNS
  // tunnel flow and clears the QUIC pool. Returns the new epoch.
  function phantomHarmonyOnNetworkChange(): number;
  // Enable the opt-in TUN trace (empty string disables). Returns 0 on success.
  function phantomHarmonySetTrace(path: string): number;
  // Embedded server (phone-as-server). Start returns the phantom:// URI
  // (with PSK) once the listener is up; it throws on failure.
  function phantomHarmonyServerStart(workDir: string, port: number, cipher: string, proto: string): string;
  function phantomHarmonyServerStop(): number;
  function phantomHarmonyServerStatus(): number;
  function phantomHarmonyServerLastError(): string;
}

export default PhantomLib;
