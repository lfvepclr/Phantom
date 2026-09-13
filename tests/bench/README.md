# Phantom 性能基准（`phantom-bench`）

基于 [divan](https://github.com/nvzqz/divan) 的微基准，用于量化数据面关键路径的开销，防止"优化"退化。
与 `tests/` 的 E2E 用例互补：这里测**单点函数/流水线**，那里测**端到端链路**。

## 基准项

| Bench | 测量对象 |
|-------|----------|
| `aead_throughput` | AES-256/128-GCM、ChaCha20-Poly1305、ASCON-128 各块大小加解密吞吐 |
| `handshake` | Noise IKpsk1 握手耗时（含 PSK） |
| `key_derivation` | HKDF 会话密钥派生开销 |
| `frame_codec` | 帧编解码（8 字节头 + payload）吞吐 |
| `pipeline` | 加密 → 分帧 → 写入 的整条发送流水线 |
| `rule_engine` | 规则匹配（域名 Trie / AC 自动机 / CIDR）查询延迟 |

## 常用命令

```bash
cargo bench -p phantom-bench                 # 全量
cargo bench -p phantom-bench --bench frame_codec
cargo bench -p phantom-bench -- aead         # 按名称过滤
```

## 约定

- 基准结果不写死断言，只做回归对比；改动 `core/src/crypto` 或 `core/src/protocol` 后应本地跑一遍对比基线。
- 需要宏观吞吐/解锁验证时改用 `cargo xtask speedtest`，不是这个 crate 的职责。
