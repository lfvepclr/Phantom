#!/usr/bin/env python3
"""Generate sections 3-10 of PERF_TOOLS_GUIDE.md"""
import pathlib

p = pathlib.Path(__file__).parent / 'PERF_TOOLS_GUIDE.md'
DQ = chr(34)  # double quote char for bash scripts

def bash_var(name):
    """Return ${name} for use in bash without double quotes"""
    return '${' + name + '}'

content = []

# ===== Section 3: HiDumper =====
content.append('''
## 3. HiDumper \u2014 \u547d\u4ee4\u884c\u7cfb\u7edf\u4fe1\u606f

> **AI \u81ea\u52a8\u5316\u53cb\u597d\u5ea6**: \u2b50\u2b50\u2b50\u2b50\u2b50
> **\u7528\u6cd5**: `hdc shell hidumper [\u9009\u9879]`

### 3.1 \u7f51\u7edc\u6d41\u91cf\u76d1\u63a7\uff08VPN \u6700\u5173\u952e\uff09

```bash
# \u67e5\u770b\u6307\u5b9a\u8fdb\u7a0b\u7684\u7f51\u7edc\u6d41\u91cf\u7edf\u8ba1
hdc shell hidumper --net <pid>

# \u8f93\u51fa\u793a\u4f8b:
# NetIfaceName: wlan0
# RxPackets: 123456  RxBytes: 98765432
# TxPackets: 234567  TxBytes: 123456789
```

### 3.2 CPU \u4f7f\u7528\u7387

```bash
# \u5168\u5c40 CPU \u4f7f\u7528\u7387
hdc shell hidumper --cpuusage

# \u6307\u5b9a\u8fdb\u7a0b CPU \u4f7f\u7528\u7387
hdc shell hidumper --cpuusage <pid>

# CPU \u9891\u7387\u4fe1\u606f
hdc shell hidumper --cpufreq
```

### 3.3 \u5185\u5b58\u4fe1\u606f

```bash
# \u6307\u5b9a\u8fdb\u7a0b\u5185\u5b58\u8be6\u60c5
hdc shell hidumper --mem <pid>

# \u5168\u7cfb\u7edf\u5185\u5b58\u6982\u89c8
hdc shell hidumper --mem
```

### 3.4 \u6240\u6709\u53ef\u7528\u9009\u9879

```bash
hdc shell hidumper -h

# \u5e38\u7528\u9879:
#   --net [pid]        \u7f51\u7edc\u6d41\u91cf
#   --cpuusage [pid]   CPU \u4f7f\u7528\u7387
#   --cpufreq          CPU \u9891\u7387
#   --mem [pid]        \u5185\u5b58\u4fe1\u606f
#   --ipc [pid]        IPC \u8c03\u7528\u7edf\u8ba1
#   --storage          \u5b58\u50a8\u4fe1\u606f
#   --power            \u7535\u6e90/\u529f\u8017\u4fe1\u606f
#   --temperature      \u6e29\u5ea6\u4fe1\u606f
```

### 3.5 AI \u81ea\u52a8\u5316\u91c7\u6837\u811a\u672c

```bash
#!/bin/bash
# perf_sample.sh
PID=''' + bash_var('1') + '''
DURATION=''' + bash_var('2:-30') + '''
OUTPUT_DIR=''' + bash_var('3:-./perf_data') + '''

mkdir -p ''' + bash_var('OUTPUT_DIR') + '''
for i in $(seq 1 ''' + bash_var('DURATION') + '''); do
  TIMESTAMP=$(date +%Y%m%d_%H%M%S)
  hdc shell hidumper --cpuusage $PID > ''' + bash_var('OUTPUT_DIR') + '''/cpu_''' + bash_var('TIMESTAMP') + '''.txt
  hdc shell hidumper --mem $PID     > ''' + bash_var('OUTPUT_DIR') + '''/mem_''' + bash_var('TIMESTAMP') + '''.txt
  hdc shell hidumper --net $PID     > ''' + bash_var('OUTPUT_DIR') + '''/net_''' + bash_var('TIMESTAMP') + '''.txt
  sleep 1
done
echo \u91c7\u6837\u5b8c\u6210\uff0c\u6570\u636e\u4fdd\u5b58\u5230 ''' + bash_var('OUTPUT_DIR') + '''
```

---
''')

with open(p, 'a') as f:
    f.write('\n'.join(content))
print(f'Appended. Total lines: {len(p.read_text().splitlines())}')
