# Phantom 部署指南

`deploy/` 目录提供两件部署资产：

| 文件 | 用途 |
|------|------|
| `install.sh` | 一键构建 + 安装二进制 + 准备 systemd 工作目录 + 安装 service 单元 |
| `phantom.service` | systemd 单元；默认使用自举（auto）模式启动 |

部署后唯一的"配置文件"是服务端在启动目录下生成的 `server.toml`：头部带 `phantom://` URI 快速链接注释，下方有 `bind` / `cipher` / `protocol` 配置以及内联 `[[allowed_clients]]` 白名单数组。

---

## 1. 零配置自举（推荐）

### 1.1 工作目录

服务端会把以下文件写到**当前工作目录（CWD）**：

| 文件 | 用途 | 权限 |
|------|------|------|
| `server.key` | X25519 私钥（第 1 行公钥，第 2 行私钥） | 600 |
| `server.toml` | bind / cipher / protocol + 顶部 URI 注释 + `[[allowed_clients]]` 白名单 | 644 |

`install.sh` 把工作目录固定为 `/var/lib/phantom`，并以专用系统用户 `phantom` 运行。
首次启动时，server 自动生成密钥和 URI，并把它们写到该目录。

### 1.2 启动：phantom server

```bash
sudo systemctl start phantom
# 查看启动日志（应包含 "Phantom server bootstrapped" 摘要）：
sudo journalctl -u phantom -f
```

如果是手动运行：

```bash
cd /var/lib/phantom
sudo -u phantom phantom-server
# 或在普通用户目录下直接：
cd ~/phantom && phantom server
```

### 1.3 获取 URI

```bash
sudo grep '^#   phantom://' /var/lib/phantom/server.toml | sed 's/^#   //'
# phantom://cNykRDdCVWVLBaQJrluFaML54JpdiTeT5T9RasbJw2Q=@192.168.1.10:443?cipher=auto&proto=tcp#default
```

把这个 URI 分发给客户端：

```bash
URI=$(sudo grep '^#   phantom://' /var/lib/phantom/server.toml | sed 's/^#   //')
phantom client --server "$URI"
```

### 1.4 白名单（可选）

```bash
# 1. 让客户端先生成自己的公钥：
phantom client --print-key   # 输出 32 字节 base64 公钥

# 2. 编辑服务端 server.toml，向 `[[allowed_clients]]` 数组追加公钥：
sudo -u phantom tee -a /var/lib/phantom/server.toml >/dev/null <<'EOF'

[[allowed_clients]]
public_key = "<客户端公钥>"
name = "client-laptop"
EOF

# 3. 重启服务端使白名单生效：
sudo systemctl restart phantom
```

> 客户端在 open 模式下可直连；一旦 `[[allowed_clients]]` 段有非空条目，则只接受白名单中的客户端。

### 1.5 自举支持的运行时参数

| 标志 | 默认值 | 说明 |
|------|--------|------|
| `--port <p>` | 443 | 起始端口；占用时自动 +1，最多重试 10 次 |
| `--public-host <h>` | 自动探测出口 IP | 写入 `server.toml` URI 注释的 host 部分 |
| `--cipher <c>` | auto | auto / aes-256-gcm / aes-128-gcm / ascon-128 / chacha20-poly1305 |
| `--proto <p>` | tcp | tcp / quic |
| `-i` / `--interactive` | 关闭 | 启用交互式向导（需要 TTY） |

---

## 2. 高级 TOML 部署（load 模式）

需要 io_uring、精细的拥塞控制、ACME、ACL 等高级特性时，回退到 TOML 配置：

### 2.1 准备 config

```bash
sudo mkdir -p /etc/phantom
sudo cp config/server.toml /etc/phantom/server.toml
# 复用自举模式生成的密钥（也可以用 auto 模式跑一次拿 server.key）：
sudo cp /var/lib/phantom/server.key /etc/phantom/server_private
# 编辑 /etc/phantom/server.toml，填入正确的 bind / private_key；如需白名单，向 [[allowed_clients]] 追加条目
```

### 2.2 切换 service 到 load 模式

编辑 `/etc/systemd/system/phantom.service`：

```ini
[Service]
# 注释自举模式，启用 load 模式：
# ExecStart=/usr/local/bin/phantom-server
ExecStart=/usr/local/bin/phantom-server /etc/phantom/server.toml
WorkingDirectory=/etc/phantom
ReadWritePaths=/etc/phantom /var/lib/phantom
```

然后：

```bash
sudo systemctl daemon-reload
sudo systemctl restart phantom
```

> **不推荐混用两种模式**：如果同时存在 `/var/lib/phantom/server.key` 和 `/etc/phantom/server.toml` 中的 `private_key`，以 `ExecStart` 指定的配置为准。

---

## 3. systemd 集成详解

### 3.1 WorkingDirectory 的必要性

`phantom-server` 在 auto 模式下依赖 CWD 解析 `./server.key` 与 `./server.toml`（whitelist 也在 toml 内的 `[[allowed_clients]]`）。
systemd 默认 CWD 是 `/`，所以单元文件**必须**显式设置 `WorkingDirectory` 到可写目录，
并把该目录加入 `ReadWritePaths`（`ProtectSystem=strict` 强制只读大部分路径）。

### 3.2 自定义端口 / 用户

如果需要把端口从默认 443 改成其他值：

```bash
sudo systemctl edit phantom
# 写入：
[Service]
ExecStart=
ExecStart=/usr/local/bin/phantom-server --port 8443
```

如果要让 service 以 root 身份运行（仅用于监听 1024 以下端口）：

```bash
sudo systemctl edit phantom
# 写入：
[Service]
User=root
Group=root
```

### 3.3 日志与重启

```bash
# 实时日志
sudo journalctl -u phantom -f

# 最近 100 行
sudo journalctl -u phantom -n 100 --no-pager

# 单元已配置 Restart=always / RestartSec=5：进程崩溃 5 秒后自动拉起
```

### 3.4 防火墙

自举默认端口 443（可改）。防火墙放行：

```bash
# ufw
sudo ufw allow 443/tcp

# firewalld
sudo firewall-cmd --permanent --add-port=443/tcp
sudo firewall-cmd --reload
```

---

## 4. 密钥与配置管理

### 4.1 server.key 位置

自举模式下：`./server.key`（CWD 相对路径）。systemd 默认在 `/var/lib/phantom/server.key`。
权限 600，owner 为 `phantom` 用户。

### 4.2 server.toml 位置

自举模式下：`./server.toml`（CWD 相对路径）。systemd 默认在 `/var/lib/phantom/server.toml`。
每次启动时覆盖写入；其顶部带 URI 注释，下方有内联白名单数组。

### 4.3 从 auto 模式迁移到 TOML

1. 备份自举文件：
   ```bash
   sudo cp /var/lib/phantom/server.key /etc/phantom/server_private
   sudo cp /var/lib/phantom/server.toml /etc/phantom/server.toml
   ```
2. 编辑 `/etc/phantom/server.toml`，向 `[[allowed_clients]]` 追加条目（如需白名单）。`private_key` 必须指向 600 权限的私钥文件。
3. 改 service 单元的 `ExecStart` 和 `WorkingDirectory`（见 §2.2）。

### 4.4 备份与恢复

需要备份的只有两个文件：

```bash
# 备份
sudo tar czf phantom-backup.tgz /var/lib/phantom/server.key /var/lib/phantom/server.toml

# 在新机器上恢复
sudo mkdir -p /var/lib/phantom
sudo tar xzf phantom-backup.tgz -C /
sudo systemctl restart phantom
```

恢复后新机器的 `server.toml` 会写入新的 host:port（基于新机器的网卡），客户端需要更新 URI 中的 host。

### 4.5 客户端 URI 中 host 不正确？

`server.toml` URI 注释中的 host 部分由服务端自动探测（UDP socket trick 拿到主出接口 IP）。
如果探测到错误的网卡 IP，可以：

- 用 `--public-host your.domain.com` 覆盖
- 或用 TOML 模式手动指定 `bind`

---

## 5. 端口冲突与防火墙

### 5.1 auto 模式自动递增

默认从 443 开始；占用则尝试 444、445、…、452（共 10 次）。
10 次都失败则报错列出尝试范围：

```
Error: No free TCP port in 0.0.0.0:443..452 (10 attempt(s) all busy): Address already in use
```

### 5.2 服务器实际端口与 URI

`server.toml` URI 注释中写入的端口是**服务端实际监听的端口**（不是 `--port` 传入的）。
如果看到 `:8443` 而你传入的是 443，说明 443 被占用，自动递增到 8443 了。
客户端按 URI 中的端口连接即可。

---

## 6. 故障排查

| 问题 | 可能原因 | 解决方法 |
|------|----------|----------|
| `journalctl` 报 "Permission denied" 写 server.key | `WorkingDirectory` 不可写 | 确认 `ReadWritePaths` 包含该目录；目录 owner 是 `phantom` |
| 客户端连不上 | URI 中的 host 不可达 | 用 `phantom client --server phantom://...@<正确的IP或域名>:443` 覆盖；或加 `--public-host` |
| 客户端握手被拒 | 白名单不含客户端公钥 | 编辑 `server.toml` 的 `[[allowed_clients]]` 段追加客户端公钥后 `systemctl restart phantom` |
| 端口连续占用 10 次 | 端口段被其他服务占满 | 用 `--port` 改成其他段（如 `--port 8443`） |
| auto 模式下重新生成密钥（导致客户端失效） | 删除了 `server.key` | 不要删；备份迁移见 §4.4 |
| 切换到 load 模式后启动失败 | TOML 中 `private_key` 路径错误 | 用 `sudo -u phantom cat <path>` 验证文件可读 |

调试模式（更详细日志）：

```bash
sudo systemctl edit phantom
# 写入：
[Service]
Environment=RUST_LOG=phantom_server=debug,phantom_core=debug
sudo systemctl daemon-reload
sudo systemctl restart phantom
sudo journalctl -u phantom -f
```

---

## 7. 卸载

```bash
sudo systemctl disable --now phantom
sudo rm /etc/systemd/system/phantom.service
sudo rm /usr/local/bin/phantom-server
sudo rm -rf /var/lib/phantom /etc/phantom
# 可选：删除 phantom 系统用户
sudo userdel phantom
sudo groupdel phantom
sudo systemctl daemon-reload
```
