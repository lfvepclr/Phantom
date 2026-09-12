import SwiftUI
import PhantomMacKit

/// Log pane, shared by the main window's card and the dedicated log window.
///
/// Both show the same buffer with the same controls, so "the log looks
/// different in the two places" can never be a source of confusion.
struct LogPane: View {
    @ObservedObject var tunnel: PhantomTunnel
    /// Card mode adds the toolbar affordances that only make sense in a window
    /// (open the log window); the window itself shows a follow toggle instead.
    var isCard: Bool = true
    @State private var followTail = true

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            list
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var header: some View {
        HStack(spacing: 8) {
            Text("日志")
                .font(.system(size: 14, weight: .semibold))

            if isCard {
                // Full screen belongs next to the *title*: sitting beside the
                // clear button it was one slip away from wiping the log.
                Button {
                    WindowBridge.shared.showLogs()
                } label: {
                    Image(systemName: "arrow.up.left.and.arrow.down.right")
                }
                .buttonStyle(.plain)
                .help("打开独立日志窗口")
            }

            Spacer(minLength: 0)

            filterPicker

            Text("\(tunnel.visibleLogLines.count) 行")
                .font(.system(size: 11))
                .foregroundStyle(.tertiary)

            Button {
                tunnel.toggleLogPaused()
            } label: {
                Image(systemName: tunnel.logPaused ? "play.fill" : "pause.fill")
            }
            .buttonStyle(.plain)
            .help(tunnel.logPaused ? "继续刷新日志" : "暂停刷新日志")

            Button {
                tunnel.clearLogs()
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.plain)
            .help("清空当前视图")
        }
        .font(.system(size: 12))
    }

    private var filterPicker: some View {
        Picker("", selection: $tunnel.logFilter) {
            ForEach(LogFilter.allCases, id: \.self) { filter in
                Text(filter.label).tag(filter)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .frame(width: 130)
        .help("仅隧道：只看真正走了服务器代理的连接")
    }

    private var list: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 1) {
                    if tunnel.visibleLogLines.isEmpty {
                        Text(emptyText)
                            .font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 8)
                    } else {
                        ForEach(tunnel.visibleLogLines) { line in
                            Text(line.text)
                                .font(.system(size: 11, design: .monospaced))
                                .foregroundStyle(color(for: line.text))
                                // One physical line per entry: wrapping pushed
                                // the useful part of a route decision out of view.
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .id(line.id)
                        }
                    }
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
            }
            .background(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(Color(nsColor: .textBackgroundColor))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .strokeBorder(Color.secondary.opacity(0.15), lineWidth: 0.5)
            )
            .onChange(of: tunnel.visibleLogLines) {
                guard followTail, !tunnel.logPaused,
                      let last = tunnel.visibleLogLines.last else { return }
                withAnimation(.easeOut(duration: 0.12)) {
                    proxy.scrollTo(last.id, anchor: .bottom)
                }
            }
        }
    }

    private var emptyText: String {
        if tunnel.logPaused { return "已暂停（日志仍在后台记录）" }
        if tunnel.logFilter == .tunnel && !tunnel.logs.isEmpty {
            return "当前没有走隧道的连接；切到「全部」可查看直连记录"
        }
        return tunnel.isRunning ? "等待日志…" : "连接后在这里查看实时日志"
    }

    private func color(for line: String) -> Color {
        if line.contains("ERROR") || line.contains("error") || line.contains("Error") {
            return Color(nsColor: .systemRed)
        }
        if line.contains("WARN") || line.contains("warn") {
            return Color(nsColor: .systemOrange)
        }
        if line.contains("-> Direct (") {
            return .secondary
        }
        if line.contains("INFO") {
            return .primary
        }
        return .primary.opacity(0.85)
    }
}

/// The log card inside the main window: fills the height the window gives it.
struct LogCard: View {
    @ObservedObject var tunnel: PhantomTunnel
    let height: CGFloat

    var body: some View {
        Card {
            LogPane(tunnel: tunnel, isCard: true)
        }
        .frame(height: height)
        .padding(.horizontal, Theme.pagePadding)
        .padding(.bottom, 6)
    }
}

/// Dedicated, resizable log window — the "放大" target.
struct LogWindowView: View {
    @ObservedObject var tunnel: PhantomTunnel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Text("Phantom 日志")
                    .font(.system(size: 15, weight: .semibold))
                Text(tunnel.link.valid ? linkAddress(tunnel.link) : "未配置连接")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
                Text(tunnel.logPaused ? "已暂停" : tunnel.state.title)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Theme.accent(tunnel.state))
            }

            LogPane(tunnel: tunnel, isCard: false)
        }
        .padding(Theme.pagePadding)
        .frame(minWidth: 560, minHeight: 360)
    }
}
