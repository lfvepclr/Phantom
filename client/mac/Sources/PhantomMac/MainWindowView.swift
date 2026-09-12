import SwiftUI
import AppKit
import PhantomMacKit

/// The app's main UI, laid out in the same order as the HarmonyOS dashboard:
/// header → server card → connect button → mode → connection → info →
/// whitelist → log → footer.
///
/// The log card takes whatever height the window has left (never below
/// `Theme.logMinHeight`), which is why this is a resizable window rather than
/// the old 380pt popover: the log is the part operators actually watch.
struct MainWindowView: View {
    @ObservedObject var tunnel: PhantomTunnel

    @State private var headerHeight: CGFloat = 0
    @State private var contentHeight: CGFloat = 0
    @State private var footerHeight: CGFloat = 0

    /// Split the window's height between "everything above the log" and the log.
    ///
    /// The cards get their natural height when there is room; the log gets the
    /// rest, never below `Theme.logMinHeight` — when the window is short the
    /// cards area scrolls instead of squeezing the log into a strip.
    private func layout(total: CGFloat) -> (content: CGFloat, log: CGFloat) {
        let chrome = headerHeight + footerHeight + 4
        let available = max(Theme.logMinHeight + 120, total - chrome)
        let content = min(contentHeight, max(120, available - Theme.logMinHeight))
        return (content, max(Theme.logMinHeight, available - content))
    }

    var body: some View {
        GeometryReader { proxy in
            let sizes = layout(total: proxy.size.height)
            VStack(spacing: 0) {
                HeaderBar(tunnel: tunnel)
                    .padding(.horizontal, Theme.pagePadding)
                    .padding(.vertical, 10)
                    .measureHeight { headerHeight = $0 }
                Divider()

                ScrollView {
                    VStack(spacing: Theme.gap) {
                        ServerCard(tunnel: tunnel)
                        ConnectButton(tunnel: tunnel)
                        ModeSegment(tunnel: tunnel)
                        ConnectionCard(tunnel: tunnel)
                        InfoCard(tunnel: tunnel)
                        WhitelistCard(tunnel: tunnel)
                    }
                    .padding(.horizontal, Theme.pagePadding)
                    .padding(.vertical, Theme.gap)
                    .measureHeight { contentHeight = $0 }
                }
                .frame(height: sizes.content)

                Divider()
                LogCard(tunnel: tunnel, height: sizes.log)

                Divider()
                FooterBar(tunnel: tunnel)
                    .padding(.horizontal, Theme.pagePadding)
                    .padding(.vertical, 8)
                    .measureHeight { footerHeight = $0 }
            }
        }
        .frame(minWidth: 420, minHeight: 620)
        .background(Theme.canvas)
    }
}

// MARK: - Height measurement

private struct HeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}

private extension View {
    /// Report this view's laid-out height so the log card can size itself from
    /// what is left (SwiftUI has no "remaining space" primitive).
    func measureHeight(_ onChange: @escaping (CGFloat) -> Void) -> some View {
        background(
            GeometryReader { proxy in
                Color.clear.preference(key: HeightKey.self, value: proxy.size.height)
            }
        )
        .onPreferenceChange(HeightKey.self, perform: onChange)
    }
}

// MARK: - Header

private struct HeaderBar: View {
    @ObservedObject var tunnel: PhantomTunnel

    var body: some View {
        HStack(spacing: 10) {
            // The real bundle icon: previously this slot hard-coded the old
            // full-colour MenuBarIcon.png illustration, so the app looked
            // unchanged no matter how often the app icon was regenerated.
            Image(nsImage: NSApplication.shared.applicationIconImage)
                .resizable()
                .interpolation(.high)
                .frame(width: 28, height: 28)

            VStack(alignment: .leading, spacing: 1) {
                Text("Phantom")
                    .font(.system(size: 15, weight: .semibold))
                Text(tunnel.link.valid ? linkAddress(tunnel.link) : "未配置连接")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }

            Spacer(minLength: 0)

            StatusPill(state: tunnel.state)

            Menu {
                Button("关于 Phantom") {
                    NSApp.orderFrontStandardAboutPanel(nil)
                    NSApp.activate()
                }
                Button("打开日志窗口") {
                    WindowBridge.shared.showLogs()
                }
                Divider()
                Button("退出 Phantom") {
                    NSApp.terminate(nil)
                }
                .keyboardShortcut("q", modifiers: .command)
            } label: {
                Image(systemName: "ellipsis.circle")
                    .font(.system(size: 14))
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .frame(width: 24)
            .help("更多")
        }
    }
}

private struct StatusPill: View {
    let state: PhantomState

    var body: some View {
        HStack(spacing: 5) {
            if state == .connecting {
                ProgressView()
                    .scaleEffect(0.5)
                    .frame(width: 8, height: 8)
            } else {
                Circle()
                    .fill(Theme.accent(state))
                    .frame(width: 8, height: 8)
            }
            Text(state.title)
                .font(.system(size: 11, weight: .medium))
        }
        .foregroundStyle(Theme.accent(state))
        .padding(.horizontal, 9)
        .padding(.vertical, 4)
        .background(Capsule().fill(Theme.accentSoft(state)))
    }
}

// MARK: - Server card

private struct ServerCard: View {
    @ObservedObject var tunnel: PhantomTunnel

    var body: some View {
        Card {
            HStack(spacing: 8) {
                Text(tunnel.link.valid ? linkAddress(tunnel.link) : "添加连接")
                    .font(.system(size: 16, weight: .semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 0)
                if tunnel.isRunning {
                    Text("隧道 \(tunnel.rates.proxiedFlows) · 直连 \(tunnel.rates.directFlows)")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
            }

            if tunnel.link.valid {
                Caption(text: linkSummary(tunnel.link))
            }

            if tunnel.isRunning {
                HStack(spacing: 14) {
                    Text("↓ \(formatRate(tunnel.rates.downPerSecond))")
                    Text("↑ \(formatRate(tunnel.rates.upPerSecond))")
                    Text("累计 ↓ \(formatBytes(tunnel.rates.totalDown)) · ↑ \(formatBytes(tunnel.rates.totalUp))")
                }
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(Color(nsColor: .systemGreen))
            }
        }
    }
}

// MARK: - Connect button

private struct ConnectButton: View {
    @ObservedObject var tunnel: PhantomTunnel

    var body: some View {
        Button {
            tunnel.toggle()
        } label: {
            HStack(spacing: 6) {
                Image(systemName: tunnel.isRunning ? "stop.fill" : "play.fill")
                    .font(.system(size: 12, weight: .semibold))
                Text(tunnel.isRunning ? "断开" : "启动")
                    .font(.system(size: 14, weight: .semibold))
            }
            .frame(maxWidth: .infinity, minHeight: 36)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(.white)
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(gradient)
        )
        .shadow(color: shadow.opacity(0.3), radius: 4, y: 1)
        .keyboardShortcut(.return, modifiers: [])
        .disabled(!tunnel.isRunning && !tunnel.link.valid)
        .help(tunnel.isRunning ? "断开隧道并还原系统代理" : "启动隧道并设置系统 SOCKS5 代理")
    }

    private var gradient: LinearGradient {
        tunnel.isRunning
            ? LinearGradient(
                colors: [Color(red: 0.93, green: 0.31, blue: 0.31), Color(red: 0.79, green: 0.19, blue: 0.19)],
                startPoint: .top, endPoint: .bottom)
            : LinearGradient(
                colors: [Color(red: 0.16, green: 0.62, blue: 0.36), Color(red: 0.09, green: 0.49, blue: 0.27)],
                startPoint: .top, endPoint: .bottom)
    }

    private var shadow: Color { tunnel.isRunning ? .red : .green }
}

// MARK: - Mode segment

private struct ModeSegment: View {
    @ObservedObject var tunnel: PhantomTunnel

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text("模式")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
                Text(modeHint)
                    .font(.system(size: 11))
                    .foregroundStyle(.tertiary)
            }
            Picker("", selection: $tunnel.proxyMode) {
                ForEach(ProxyMode.allCases, id: \.self) { mode in
                    Text(mode.label).tag(mode)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .disabled(tunnel.isRunning || tunnel.state == .connecting)
        }
    }

    private var modeHint: String {
        switch tunnel.proxyMode {
        case .global: return "全部流量走服务器"
        case .smart: return "白名单走服务器，其余直连"
        case .direct: return "全部直连（不代理）"
        }
    }
}

// MARK: - Footer

private struct FooterBar: View {
    @ObservedObject var tunnel: PhantomTunnel
    @State private var quitHovered = false

    var body: some View {
        HStack(spacing: 8) {
            Text("Phantom v1.0")
                .font(.system(size: 10))
                .foregroundStyle(.tertiary)
            if tunnel.logPaused {
                Text("· 日志已暂停")
                    .font(.system(size: 10))
                    .foregroundStyle(Color(nsColor: .systemOrange))
            }
            Spacer(minLength: 0)

            // Quitting is a real action, not a footnote: it disconnects and
            // restores the system proxy, so it gets a visible button.
            Button {
                NSApp.terminate(nil)
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "power")
                        .font(.system(size: 11, weight: .semibold))
                    Text("退出 Phantom")
                        .font(.system(size: 12, weight: .medium))
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 5)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(quitHovered ? Color.white : Color.primary)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(quitHovered ? Color(nsColor: .systemRed) : Color.secondary.opacity(0.12))
            )
            .onHover { quitHovered = $0 }
            .keyboardShortcut("q", modifiers: .command)
            .help("断开隧道、还原系统代理并退出")
        }
    }
}
