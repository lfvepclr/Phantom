#!/usr/bin/env python3
import pathlib

Q = chr(39)  # single quote
DQ = chr(34)  # double quote
NL = chr(10)  # newline

lines = []
a = lines.append

# Section 3: HiDumper
a(NL)
a('## 3. HiDumper -- ' + chr(21629) + chr(20196) + chr(34892) + chr(31995) + chr(32479) + chr(20449) + chr(24687))
a(NL)
a('> **AI ' + chr(33258) + chr(21160) + chr(21270) + chr(21451) + chr(22909) + chr(24230)**: ' + chr(9733) + chr(9733) + chr(9733) + chr(9733) + chr(9733))
a('> **' + chr(29992) + chr(27861)**: `hdc shell hidumper [' + chr(36873) + chr(39033) + ']`')
a(NL)
a('### 3.1 ' + chr(32593) + chr(32476) + chr(27969) + chr(37327) + chr(30417) + chr(25511) + chr(65288) + 'VPN ' + chr(26368) + chr(20851) + chr(38190) + chr(65289))
a(NL)
a('```bash')
a('# ' + chr(26597) + chr(30475) + chr(25351) + chr(23450) + chr(36827) + chr(31243) + chr(30340) + chr(32593) + chr(32476) + chr(27969) + chr(37327) + chr(32479) + chr(35745))
a('hdc shell hidumper --net <pid>')
a(NL)
a('# ' + chr(36755) + chr(20986) + chr(31034) + chr(20363) + ':')
a('# NetIfaceName: wlan0')
a('# RxPackets: 123456  RxBytes: 98765432')
a('# TxPackets: 234567  TxBytes: 123456789')
a('```')

p = pathlib.Path(__file__).parent / 'PERF_TOOLS_GUIDE.md'
with open(p, 'a') as f:
    f.write(NL.join(lines))
print(f'Done. File size: {p.stat().st_size}')
