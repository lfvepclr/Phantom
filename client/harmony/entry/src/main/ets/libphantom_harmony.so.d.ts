declare namespace PhantomLib {
  function phantomHarmonyStart(fd: number, uri: string, mode: string): number;
  function phantomHarmonyStartConfig(fd: number, config: string): number;
  function phantomHarmonyStop(): number;
  function phantomHarmonyGetStatus(): number;
  function phantomHarmonyGetLastError(): string;
  function phantomHarmonyGetLogs(sinceCursor: number): Object[];
}

export default PhantomLib;
