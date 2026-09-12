import SwiftUI
import PhantomMacKit

/// Everything about the current connection, in the same order the HarmonyOS
/// info sheet uses: address → transport → crypto → keys → live numbers →
/// probes → share.
struct InfoCard: View {
    @ObservedObject var tunnel: PhantomTunnel
    @State private var expanded = true
    @State private var showShare = false

    private var link: ServerLink { tunnel.link }

    var body: some View {
        Card {
            Button {
                expanded.toggle()
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: expanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.secondary)
                    Text("连接信息")
                        .font(.system(size: 13, weight: .semibold))
                    Spacer(minLength: 0)
                    if tunnel.isRunning {
                        Text("↓ \(formatRate(tunnel.rates.downPerSecond))  ↑ \(formatRate(tunnel.rates.upPerSecond))")
                            .font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(Color(nsColor: .systemGreen))
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            if expanded {
                InfoRow(label: "地址", value: link.valid ? "\(link.host):\(link.port)" : "—", monospaced: true)
                InfoRow(label: "传输协议", value: link.valid ? link.proto.uppercased() : "—")
                InfoRow(label: "加密套件", value: link.valid ? cipherLabel(link.cipher) : "—")
                InfoRow(label: "服务器密钥", value: link.valid ? shortFingerprint(link.key) : "—", monospaced: true)
                InfoRow(label: "预共享密钥", value: link.psk.isEmpty ? "—" : shortFingerprint(link.psk), monospaced: true)
                InfoRow(label: "连接时长", value: tunnel.isRunning ? formatDuration(seconds: tunnel.uptimeSeconds) : "—")
                InfoRow(
                    label: "实时速率",
                    value: tunnel.isRunning
                        ? "↓ \(formatRate(tunnel.rates.downPerSecond))  ↑ \(formatRate(tunnel.rates.upPerSecond))"
                        : "—"
                )
                InfoRow(
                    label: "本次流量",
                    value: tunnel.isRunning
                        ? "↓ \(formatBytes(tunnel.rates.totalDown))  ↑ \(formatBytes(tunnel.rates.totalUp))"
                        : "—"
                )
                InfoRow(
                    label: "分流统计",
                    value: "隧道 \(tunnel.rates.proxiedFlows) 条 · 直连 \(tunnel.rates.directFlows) 条"
                )
                if !tunnel.latencyText.isEmpty {
                    InfoRow(label: "链路延迟", value: tunnel.latencyText)
                }
                if !tunnel.speedText.isEmpty {
                    InfoRow(label: "测速结果", value: tunnel.speedText)
                }

                HStack(spacing: 8) {
                    Button {
                        tunnel.measureLatency()
                    } label: {
                        Label("测延迟", systemImage: "stopwatch")
                    }
                    .disabled(!tunnel.isRunning || tunnel.latencyBusy)

                    Button {
                        tunnel.measureSpeed()
                    } label: {
                        Label("测速", systemImage: "speedometer")
                    }
                    .disabled(!tunnel.isRunning || tunnel.speedBusy)

                    Spacer(minLength: 0)

                    Button {
                        showShare = true
                    } label: {
                        Label("分享", systemImage: "qrcode")
                    }
                    .disabled(!link.valid)
                }
                .controlSize(.small)
                .padding(.top, 2)

                Text("测延迟与测速都经过当前隧道（本机 SOCKS5），未连接时不可用。")
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
            }
        }
        .sheet(isPresented: $showShare) {
            ShareSheet(uri: tunnel.serverURI, link: link)
        }
    }
}
