declare namespace PhantomLib {
  function phantomHarmonyStart(fd: number, uri: string, mode: string): number;
  function phantomHarmonyStartConfig(fd: number, config: string): number;
  function phantomHarmonyStop(): number;
  function phantomHarmonyGetStatus(): number;
  function phantomHarmonyGetLastError(): string;
  function phantomHarmonyGetLogs(sinceCursor: number): Object[];
  // Embedded server (phone-as-server). Start returns the phantom:// URI
  // (with PSK) once the listener is up; it throws on failure.
  function phantomHarmonyServerStart(workDir: string, port: number, cipher: string, proto: string): string;
  function phantomHarmonyServerStop(): number;
  function phantomHarmonyServerStatus(): number;
  function phantomHarmonyServerLastError(): string;
}

export default PhantomLib;
