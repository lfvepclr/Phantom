import SwiftUI
import AppKit
import PhantomMacKit

/// The menu-bar popover: the app's main UI, in the same order as the HarmonyOS
/// dashboard — header → server card → connect → mode → log → footer.
///
/// Everything else (connection string, details, whitelist, settings) is a
/// **page inside the same popover**, not a sheet. A menu-bar popover closes
/// itself the moment it loses focus, which strands a presented sheet in a
/// window that no longer exists — that is how the whitelist sheet ended up
/// filling the screen with nothing and refusing to close. Pushing a page keeps
/// one window, one dismiss path (the back button), and no dependence on how
/// macOS decides to host a sheet over an `NSStatusItem`.
///
/// The popover is 400pt wide and cannot be resized, so laying all seven cards
/// out flat would mean scrolling past five of them to reach the log — the one
/// thing that is worth watching continuously.
struct DashboardPopover: View {
    @ObservedObject var tunnel: PhantomTunnel

    @State private var headerHeight: CGFloat = 0
    @State private var contentHeight: CGFloat = 0
    @State private var footerHeight: CGFloat = 0
    @State private var page: PopoverPage?
    @State private var logExpanded = false

    /// Split the window's height between "everything above the log" and the log.
    ///
    /// The cards get their natural height when there is room; the log gets the
    /// rest, never below `Theme.popoverLogMinHeight` — when the popover is short the
    /// cards area scrolls instead of squeezing the log into a strip.
    private func layout(total: CGFloat) -> (content: CGFloat, log: CGFloat) {
        let chrome = headerHeight + footerHeight + 4
        let available = max(Theme.popoverLogMinHeight + 120, total - chrome)
        let content = min(contentHeight, max(120, available - Theme.popoverLogMinHeight))
        return (content, max(Theme.popoverLogMinHeight, available - content))
    }

    var body: some View {
        Group {
            if logExpanded {
                ExpandedLogPage(tunnel: tunnel) { logExpanded = false }
            } else if let page {
                PopoverPageView(
                    page: page,
                    tunnel: tunnel,
                    onBack: { self.page = nil },
                    onOpenPage: { self.page = $0 },
                    onExpandLogs: {
                        self.page = nil
                        logExpanded = true
                    },
                )
            } else {
                dashboard
            }
        }
        .frame(width: Theme.popoverWidth, height: Theme.popoverHeight)
        .background(Theme.canvas)
    }

    private var dashboard: some View {
        GeometryReader { proxy in
            let sizes = layout(total: proxy.size.height)
            VStack(spacing: 0) {
                PopoverHeader(tunnel: tunnel) { page = $0 }
                    .padding(.horizontal, Theme.pagePadding)
                    .padding(.vertical, 8)
                    .measureHeight { headerHeight = $0 }
                Divider()

                ScrollView {
                    VStack(spacing: Theme.gap) {
                        ServerCard(tunnel: tunnel)
                        ConnectButton(tunnel: tunnel)
                        ModeSegment(tunnel: tunnel)
                        ConnectionSummary(tunnel: tunnel) { page = $0 }
                    }
                    .padding(.horizontal, Theme.pagePadding)
                    .padding(.vertical, 10)
                    .measureHeight { contentHeight = $0 }
                }
                .frame(height: sizes.content)

                Divider()
                LogCard(tunnel: tunnel, height: sizes.log) { logExpanded = true }

                Divider()
                FooterBar(tunnel: tunnel)
                    .padding(.horizontal, Theme.pagePadding)
                    .padding(.vertical, 8)
                    .measureHeight { footerHeight = $0 }
            }
        }
    }
}

/// Which secondary page is pushed on top of the dashboard.
enum PopoverPage: String, Identifiable, CaseIterable {
    case connection, info, whitelist, settings
    var id: String { rawValue }

    var title: String {
        switch self {
        case .connection: return "连接串"
        case .info: return "连接信息"
        case .whitelist: return "分流白名单"
        case .settings: return "设置"
        }
    }
}

/// A pushed page: back button, title, content.
///
/// The back button is the only dismiss path, and it is always in the same
/// place — unlike a sheet's close affordance, which the popover's own chrome can
/// cover, or which, as happened, never rendered at all.
private struct PopoverPageView: View {
    let page: PopoverPage
    @ObservedObject var tunnel: PhantomTunnel
    let onBack: () -> Void
    let onOpenPage: (PopoverPage) -> Void
    let onExpandLogs: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                BackButton(action: onBack)
                Text(page.title)
                    .font(.system(size: 14, weight: .semibold))
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Theme.pagePadding)
            .padding(.vertical, 10)

            Divider()

            ScrollView {
                VStack(spacing: Theme.gap) {
                    switch page {
                    case .connection:
                        ConnectionCard(tunnel: tunnel)
                    case .info:
                        InfoCard(tunnel: tunnel)
                    case .whitelist:
                        WhitelistCard(tunnel: tunnel)
                    case .settings:
                        SettingsSheet(
                            tunnel: tunnel,
                            onOpenWhitelist: { onOpenPage(.whitelist) },
                            onExpandLogs: onExpandLogs,
                        )
                    }
                }
                .padding(Theme.pagePadding)
            }
        }
    }
}

/// The log filling the whole popover — the "放大" target.
///
/// Replaces the standalone log window: one window to manage, one place to look,
/// and no way for it to end up stuck behind another app.
private struct ExpandedLogPage: View {
    @ObservedObject var tunnel: PhantomTunnel
    let onBack: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                BackButton(action: onBack)
                Spacer(minLength: 0)
                Text(tunnel.link.valid ? linkAddress(tunnel.link) : "未配置连接")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                Text(tunnel.logPaused ? "已暂停" : tunnel.state.title)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Theme.accent(tunnel.state))
            }
            .padding(.horizontal, Theme.pagePadding)
            .padding(.vertical, 10)

            Divider()

            LogPane(tunnel: tunnel, onClose: onBack)
                .padding(Theme.pagePadding)
        }
    }
}

/// Shared "返回" control: text + chevron, and Esc as the keyboard equivalent.
private struct BackButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Image(systemName: "chevron.left")
                Text("返回")
            }
            .font(.system(size: 12, weight: .medium))
        }
        .buttonStyle(.plain)
        .keyboardShortcut(.escape, modifiers: [])
        .help("返回主面板（Esc）")
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

/// Title, status and the two doors out of the popover: settings, and the
/// overflow menu that holds about/logs/quit.
private struct PopoverHeader: View {
    @ObservedObject var tunnel: PhantomTunnel
    let onSheet: (PopoverPage) -> Void

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

            Button {
                onSheet(.settings)
            } label: {
                Image(systemName: "gearshape")
                    .font(.system(size: 14))
            }
            .buttonStyle(.borderless)
            .help("设置：外观、诊断、白名单说明")

            Menu {
                Button("连接串…") { onSheet(.connection) }
                Button("连接信息…") { onSheet(.info) }
                Button("分流白名单…") { onSheet(.whitelist) }
                Divider()
                Button("关于 Phantom") {
                    NSApp.orderFrontStandardAboutPanel(nil)
                    NSApp.activate()
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

/// One card that stands in for the three detail cards the old window stacked:
/// it says what is configured, and opens the sheet that shows the rest.
private struct ConnectionSummary: View {
    @ObservedObject var tunnel: PhantomTunnel
    let onSheet: (PopoverPage) -> Void

    var body: some View {
        Card {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(tunnel.link.valid ? "连接串" : "添加连接串")
                        .font(.system(size: 14, weight: .medium))
                    Caption(
                        text: tunnel.link.valid
                            ? "\(linkAddress(tunnel.link)) · 历史 \(tunnel.history.count) 条"
                            : "粘贴 phantom:// 连接串，或用手机扫码分享"
                    )
                }
                Spacer(minLength: 0)
                Button("编辑") { onSheet(.connection) }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                Button("详情") { onSheet(.info) }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
    }
}

/// Settings for the macOS client.
///
/// There is no appearance switch here, deliberately: the app paints itself with
/// semantic NSColors, so it already follows the system's light/dark setting and
/// a second preference would only let the two disagree.
private struct SettingsSheet: View {
    @ObservedObject var tunnel: PhantomTunnel
    let onOpenWhitelist: () -> Void
    let onExpandLogs: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Card {
                Text("外观")
                    .font(.system(size: 13, weight: .medium))
                Caption(text: "跟随 macOS 的浅色/深色设置，无需在此切换。")
            }

            Card {
                Text("诊断")
                    .font(.system(size: 13, weight: .medium))
                HStack(spacing: 8) {
                    Button("全屏查看日志") { onExpandLogs() }
                    Button(tunnel.logPaused ? "继续刷新日志" : "暂停刷新日志") {
                        tunnel.toggleLogPaused()
                    }
                    Button("清空日志") { tunnel.clearLogs() }
                }
                .controlSize(.small)
                Caption(text: "界面保留最近 \(tunnel.visibleLogLines.count) 行，磁盘历史由隧道进程维护。")
            }

            Card {
                Text("探针")
                    .font(.system(size: 13, weight: .medium))
                HStack(spacing: 8) {
                    Button(tunnel.latencyBusy ? "测延迟…" : "测延迟") { tunnel.measureLatency() }
                        .disabled(!tunnel.isRunning || tunnel.latencyBusy)
                    Button(tunnel.speedBusy ? "测速中…" : "测速") { tunnel.measureSpeed() }
                        .disabled(!tunnel.isRunning || tunnel.speedBusy)
                }
                .controlSize(.small)
                Caption(
                    text: "走本机 SOCKS5（127.0.0.1:\(tunnel.socksPort)），"
                        + "测的是隧道真实可用带宽。"
                )
                if !tunnel.latencyText.isEmpty {
                    Text("链路延迟：\(tunnel.latencyText)").font(.system(size: 11))
                }
                if !tunnel.speedText.isEmpty {
                    Text("测速结果：\(tunnel.speedText)").font(.system(size: 11))
                }
            }

            Card {
                Text("分流白名单")
                    .font(.system(size: 13, weight: .medium))
                Caption(
                    text: "内置被墙域名清单随版本更新；这里配置的域名是额外走代理的补充，"
                        + "其余流量一律直连。"
                )
                Button("编辑白名单") { onOpenWhitelist() }
                    .controlSize(.small)
            }

            Card {
                Text("关于")
                    .font(.system(size: 13, weight: .medium))
                Caption(text: "Phantom · Noise IK · AES-GCM / ChaCha20 / Ascon")
                Caption(text: "系统代理：SOCKS5 127.0.0.1:\(tunnel.socksPort)，退出时自动还原。")
            }
        }
    }
}

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
