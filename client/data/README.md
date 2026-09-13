# 内置分流白名单数据（`client/data/`）

Phantom **默认直连**：只有命中白名单的目标才走隧道，其余全部直连。这里的三个文件就是那份白名单的编译产物。

| 文件 | 内容 | 生成方式 |
|------|------|----------|
| `proxy_domains.fst` | 被墙域名集合的 FST 索引（约 4.4k 条 / 37 KiB），`include_bytes!` 嵌入二进制，运行时零解析、零分配加载 | `cargo xtask rules update` |
| `proxy_cidrs.txt` | 只能按 IP 访问的服务网段（如 Telegram），编译期 `include_str!` 内联 | 同上 |
| `proxy_domains.meta.json` | 溯源信息：上游来源、生成时间、域名/CIDR 条数、FST 字节数与 SHA256 | 同上 |

## 使用约定

- **不要手工编辑**。全部由 `cargo xtask rules update` 从 Loyalsoldier/clash-rules 拉取重建；`--extended` 会额外并入 `proxy.txt` 扩大集合。
- 加载逻辑在 `client/src/whitelist.rs`：FST 作为零拷贝视图打开，CIDR 走最长前缀匹配。
- 白名单目标**不在本地解析 DNS**，域名原样送到服务端解析，避免本地 resolver 缓存被污染。
- 用户自定义规则走 `client/src/rules.rs`（域名 / IP / 端口 / GeoIP），优先级高于内置白名单。

## 常用命令

```bash
cargo xtask rules update            # 重新拉取并重建
cargo xtask rules update --extended # 扩大集合
cargo xtask rules verify            # 校验索引可用
```
