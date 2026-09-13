# Scripts

Operational helpers. `AGENTS.md`'s rule applies: anything added here gets a row in
the table below and, if it has prerequisites, a section.

| Script | Purpose |
|---|---|
| `build-android.sh` | Build `phantom-android` + the Android APK (see `client/android/README.md`). |
| `build-harmony.sh` | Build the HarmonyOS HAP. |
| `build-mac.sh` | Build the macOS client. |
| `sign-harmony-hap.sh` | Sign a HarmonyOS HAP for a real device. |
| `deploy-server.sh` | Deploy the server container. |
| `harmony-bench.sh` | Throughput benchmark against the HarmonyOS client. |
| `mac-sysproxy.sh` | Turn the macOS system proxy on and off. |
| `measure-android.sh` | Four-dimension measurement (CPU / memory / network / power) of the Android client over `adb`. |
| `measure-harmony.sh` | Same four dimensions for the HarmonyOS client over `hdc` (HiDumper, optionally HiPerf / HiTrace). |
| `measurements/` | JSON output of the two scripts above; one file per run, git-ignored. |

The two measurement scripts are invoked through `bash` (`bash
scripts/measure-android.sh …`) so they do not depend on the executable bit.

## Why the measurement scripts exist

"Is this change cheaper?" used to be answered by feel. A power or idle-CPU
regression is invisible in the UI and obvious in the numbers, so every change to
the wake-up cadence or the idle-reclaim thresholds is measured with these scripts
and the numbers go into the README of the client that changed.

## Scenarios (identical on both platforms)

| Scenario | Definition | What it must show |
|---|---|---|
| `A_idle_screen_off` | App not connected, screen off, 30 min | **zero** periodic wake-ups from the app |
| `B_connected_screen_off` | Connected, screen off, no traffic | CPU ≈ 0, network ≈ keepalive only, wake-ups ≪ baseline |
| `C_foreground` | Connected, foreground, screen on, 10 min | no new dropped frames, no runaway memory |

```bash
scripts/measure-android.sh  B_connected_screen_off 300
scripts/measure-harmony.sh  B_connected_screen_off 300
```

| Env var | Default | Meaning |
|---|---|---|
| `INTERVAL` | `10` | Seconds between samples. `duration / interval` samples are taken. |
| `PACKAGE` | `co.phantom.android` | Android app id. |
| `BUNDLE` | `co.phantom.harmony` | HarmonyOS bundle name. |
| `PHANTOM_IDLE_JSON` | unset | Path to a `{"flows":…,"pool_idle":…,"udp_flows":…}` snapshot to fill `idle_objects`. |

## The unified output shape

Both scripts write one JSON document per run and print a copy to stdout, so a
before/after or Android/HarmonyOS comparison is a diff, not a rewrite. Both
platforms emit **exactly** these keys:

```
{ "scenario", "platform", "duration_s", "interval_s", "pids": [ … ],
  "probe": { "available": [ … ], "missing": [ … ] },
  "samples": [ {
      "t", "cpu_percent", "cpu_breakdown", "pss_kb", "mem_breakdown",
      "net_bytes": { "rx", "tx" }, "net_conns",
      "power": { "wakeups", "mah", "current_ma", "temp_c" },
      "render": { "fps", "jank_frames" },
      "idle_objects": { "flows", "pool_idle", "udp_flows" } } ] }
```

Which source fills which field:

| Field | Android | HarmonyOS |
|---|---|---|
| `cpu_percent` / `cpu_breakdown` | `dumpsys cpuinfo`; per-**thread** from `top -H` | `hidumper --cpu`; per-**process** (both PIDs) |
| `pss_kb` / `mem_breakdown` | `dumpsys meminfo`; no breakdown | `hidumper --mem` per PID, plus the total |
| `net_bytes` | `dumpsys netstats detail` for the app uid | not available — HiDumper reports connections only |
| `net_conns` | not available without root | `hidumper --net` |
| `power.current_ma` / `temp_c` | `dumpsys battery` | `PowerManagerService "current"` / `ThermalService "temp"` |
| `power.wakeups` / `mah` | `dumpsys batterystats --charged` | not per-app on HarmonyOS |
| `render.jank_frames` | `dumpsys gfxinfo` | `RenderService "jank"` |
| `idle_objects` | `PHANTOM_IDLE_JSON` only (counters are in-process) | same |

`null` means **not measurable here**, and an absent measurement is never written
as `0`: the two must not be confused when the numbers are compared. `probe.available`
lists what the run actually had, so any `null` can be explained.

## Probing before measuring

Neither tool set is guaranteed on every machine. Each script probes its tools
first; whatever is missing is recorded in `probe.missing` and the fields it feeds
degrade to `null` while the rest of the run continues. A partial measurement is
worth having; a script that aborts is not. A *device* is the one hard
prerequisite — no `adb` target or `hdc` target means no measurement at all — and
a package that disappears mid-run also aborts, because the numbers after that
point describe nothing.

The scripts deliberately do **not** shell out to `perfetto` (Android) or
`hiperf` / `hitrace` (HarmonyOS). Those produce a trace file, not a table row, so
they stay manual escalations for when the counters say "there is a wake-up" and
the question becomes "where is it coming from"; `hiperf` and `hitrace` are still
probed so the output records whether they exist. See
`client/harmony/docs/PERF_TOOLS_GUIDE.md` for the trace recipes.

## Verification status

Both scripts were exercised end-to-end against fake `adb` / `hdc` shims: JSON
validity, both degradation paths (a missing tool, a vanished package), the
`null`-not-zero rule, and key-for-key schema equality between the two platforms.
Everything that needs a real device — the actual numbers, the thresholds below,
and the emulator-vs-phone distinction — is still unverified and must be filled in
on hardware.

## What each platform can and cannot tell you

* **Emulator (Android)**: `top -H`, `dumpsys meminfo`, `dumpsys netstats` and
  `gfxinfo` are valid and fine for a same-environment A/B. `batterystats` mAh is
  synthesised and there is no Doze — **power conclusions come from a real phone**.
* **Emulator ABI**: the client is built for `arm64-v8a` only; use an arm64 API 34
  image or the APK will not install.
* **HarmonyOS**: `hidumper` is the primary source for every dimension; see
  `client/harmony/docs/PERF_TOOLS_GUIDE.md` for the tool notes, including which
  sections of that guide are actually present on disk.

## Hard thresholds used for acceptance

| Check | Threshold |
|---|---|
| Scenario A periodic wake-ups | 0 |
| Scenario B process CPU | < 0.1 % of one core |
| Scenario B wake-ups vs. baseline | ≥ 80 % lower |
| Idle object growth (flows, pool, caches) | flat over the window |
| Scenario C dropped frames | no regression |
