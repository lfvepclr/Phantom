import SwiftUI
import AppKit
import CoreImage
import CoreImage.CIFilterBuiltins
import PhantomMacKit

/// Server URI entry + remembered connections.
///
/// Collapsed by default once something is configured: the URI itself is not
/// something an operator reads, it is something they paste once.
struct ConnectionCard: View {
    @ObservedObject var tunnel: PhantomTunnel
    @State private var expanded: Bool
    @State private var showShare = false

    init(tunnel: PhantomTunnel) {
        self.tunnel = tunnel
        // Start open when there is nothing to connect to yet.
        _expanded = State(initialValue: tunnel.serverURI.isEmpty)
    }

    private var locked: Bool { tunnel.isRunning || tunnel.state == .connecting }

    var body: some View {
        Card {
            HStack(spacing: 8) {
                Button {
                    expanded.toggle()
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(expanded ? "连接串" : collapsedLabel)
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(.primary)
                        Text(expanded ? "支持粘贴 / 历史记录" : "手动输入或从历史记录选择")
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)

                Button {
                    showShare = true
                } label: {
                    Image(systemName: "square.and.arrow.up")
                }
                .buttonStyle(.plain)
                .disabled(!tunnel.link.valid)
                .help("分享当前连接串（二维码）")

                historyMenu
            }

            if expanded {
                HStack(spacing: 6) {
                    Image(systemName: "link")
                        .foregroundStyle(.tertiary)
                        .font(.system(size: 11))
                    TextField("phantom://key@host:port?cipher=auto#name", text: $tunnel.serverURI)
                        .textFieldStyle(.plain)
                        .font(.system(size: 11, design: .monospaced))
                        .disabled(locked)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Color(nsColor: .textBackgroundColor))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .strokeBorder(Color.secondary.opacity(0.18), lineWidth: 0.5)
                )

                if !tunnel.serverURI.isEmpty && !tunnel.link.valid {
                    Text("连接串格式不正确：需要 phantom://<密钥>@<主机>:<端口>")
                        .font(.system(size: 11))
                        .foregroundStyle(Color(nsColor: .systemRed))
                }

                if let recent = tunnel.history.first {
                    Text("历史 \(tunnel.history.count) 条 · 最近：\(linkAddress(parseServerURI(recent.uri))) \(relativeTime(recent.lastUsedAt))")
                        .font(.system(size: 11))
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .sheet(isPresented: $showShare) {
            ShareSheet(uri: tunnel.serverURI, link: tunnel.link)
        }
    }

    private var collapsedLabel: String {
        tunnel.history.isEmpty ? "添加连接" : "已保存 \(tunnel.history.count) 个连接"
    }

    private var historyMenu: some View {
        Menu {
            if tunnel.history.isEmpty {
                Text("暂无历史记录")
            } else {
                ForEach(tunnel.history, id: \.uri) { entry in
                    Button {
                        tunnel.useHistoryEntry(entry)
                        expanded = true
                    } label: {
                        // Green tick = this URI actually reached "connected"
                        // on this Mac, not merely "was typed once".
                        Text(historyLabel(entry))
                    }
                }
            }
        } label: {
            Image(systemName: "chevron.down")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .frame(width: 22)
        .help("历史连接")
    }

    private func historyLabel(_ entry: ServerHistoryEntry) -> String {
        let link = parseServerURI(entry.uri)
        let tick = entry.verifiedAt == nil ? "○" : "✓"
        let when = relativeTime(entry.lastUsedAt)
        return "\(tick) \(linkAddress(link))\(when.isEmpty ? "" : " · \(when)")"
    }
}

/// QR code + copy + system share, mirroring the HarmonyOS info sheet.
struct ShareSheet: View {
    let uri: String
    let link: ServerLink
    @Environment(\.dismiss) private var dismiss
    @State private var copied = false

    var body: some View {
        VStack(spacing: 14) {
            Text("分享连接")
                .font(.system(size: 15, weight: .semibold))
            Text("用另一台设备上的 Phantom 扫码导入")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)

            if let image = QRCode.image(from: uri) {
                Image(nsImage: image)
                    .interpolation(.none)
                    .resizable()
                    .frame(width: 220, height: 220)
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: 10).fill(.white))
            } else {
                Text("二维码生成失败")
                    .foregroundStyle(Color(nsColor: .systemRed))
            }

            Text(linkSummary(link))
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(.secondary)

            HStack(spacing: 10) {
                Button(copied ? "已复制" : "复制连接串") {
                    let board = NSPasteboard.general
                    board.clearContents()
                    board.setString(uri, forType: .string)
                    copied = true
                }
                ShareLink(item: uri) {
                    Text("系统分享")
                }
                Button("关闭") { dismiss() }
                    .keyboardShortcut(.escape, modifiers: [])
            }
        }
        .padding(20)
        .frame(width: 340)
    }
}

/// QR rendering (CoreImage), kept here so no third-party dependency is needed.
enum QRCode {
    static func image(from text: String, scale: CGFloat = 10) -> NSImage? {
        guard !text.isEmpty, let data = text.data(using: .utf8) else { return nil }
        let filter = CIFilter.qrCodeGenerator()
        filter.message = data
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let rep = NSCIImageRep(ciImage: scaled)
        let image = NSImage(size: rep.size)
        image.addRepresentation(rep)
        return image
    }
}
