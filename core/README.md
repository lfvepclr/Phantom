# Phantom Core（`phantom-core`）

工作区内所有 crate 的**依赖根**：服务端、客户端、CLI、E2E 全部只依赖 `core`，不互相依赖。
这里只放**与平台无关的协议与密码学实现**，任何平台特判（`target_os`、JNI/NAPI、`libc`）都不允许出现在本目录。

## 模块职责

| 模块 | 路径 | 职责 |
|------|------|------|
| `config` | `src/config.rs` | `ClientConfig` / `ServerConfig` / `RulesConfig` 的 serde 定义与校验；`[[allowed_clients]]` 白名单、PSK 解码 |
| `constants` | `src/constants.rs` | 帧头长度、MTU、超时、版本号等全局常量 |
| `error` | `src/error.rs` | `PhantomError` / `Result`，统一错误面 |
| `uri` | `src/uri.rs` | `phantom://` URI 的构建与解析（公钥 / host / port / `cipher=` / `proto=` / `psk=` / `#name`） |
| `crypto::cipher` | `src/crypto/cipher.rs` | 密码套件枚举与**运行时 CPU 能力探测**（`auto` → AES-256-GCM / ASCON-128），AEAD 后端选择 |
| `crypto::keys` | `src/crypto/keys.rs` | X25519 密钥对生成/加载、`server.key` 三行格式（公钥 / 私钥 / PSK） |
| `crypto::noise` | `src/crypto/noise.rs` | `Noise_IKpsk1_25519_ChaChaPoly_SHA256` initiator/responder；PSK 绑在第一条消息 |
| `crypto::session` | `src/crypto/session.rs` | 握手后 `split()` → 双向 `SessionReader` / `SessionWriter` |
| `crypto::aead_state` | `src/crypto/aead_state.rs` | 每方向独立 nonce 计数器与密钥状态 |
| `protocol::frame` | `src/protocol/frame.rs` | 8 字节帧头 `[ver][stream_id:4][flags:1][len:2]` 与 SYN/FIN/RST/ACK/DATA/PING/PONG/UDP 标志 |
| `protocol::codec` | `src/protocol/codec.rs` | 帧读写（零拷贝 `BytesMut`），含长度上限与畸形帧拒绝 |
| `protocol::address` | `src/protocol/address.rs` | `TargetAddr`（域名 / IPv4 / IPv6）编码，SOCKS5 atyp 兼容 |
| `transport::traits` | `src/transport/traits.rs` | `Transport` 抽象：屏蔽 TCP 与 QUIC 差异，供上层统一调用 |
| `transport::tcp` | `src/transport/tcp.rs` | TCP 流实现 |
| `transport::quic` | `src/transport/quic.rs` | Noise-over-QUIC（quinn-hyphae），PSK 经 prologue 绑定；**不支持 ascon-128** |

## 关键不变量（改动前务必确认）

1. **PSK 必须是 `psk1`**：放在第一条握手消息，无 PSK 的探测者得不到任何回应。改 pattern 会直接破坏抗主动探测能力，回归用例见 `src/crypto/noise.rs` 的 `handshake_fails_when_psk_differs` 与 `pattern_binds_the_psk_to_the_first_message`。
2. **帧格式是跨版本兼容面**：`stream_id = 0` 是保留控制流（Hello），业务流必须 `>= 1`。
3. **新增配置项必须同步数据面**：`config.rs` 里每加一个字段，`ARCHITECTURE.md` §5 的配置契约表要有对应行，否则就是"配置声明了但没实现"。
4. **QUIC 不再叠加会话层加密**：QUIC 本身已由 Noise 加密，帧用长度前缀明文分帧，不要再加 `SessionReader/Writer`。
5. feature `geoip`（可选 `maxminddb`）是 IP 归属地规则的唯一开关，默认关闭。

## 常用命令

```bash
cargo test -p phantom-core                       # 帧协议 / URI / 密码协商单测
cargo test -p phantom-core --features geoip      # 含 GeoIP
cargo build -p phantom-core --target aarch64-unknown-linux-musl   # 交叉校验
```
