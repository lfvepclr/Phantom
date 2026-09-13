import SwiftUI
import PhantomMacKit

/// Log pane, shared by the dashboard card and the expanded (full-popover) view.
///
/// Both show the same buffer with the same controls, so "the log looks
/// different in the two places" can never be a source of confusion.
struct LogPane: View {
    @ObservedObject var tunnel: PhantomTunnel
    /// Card mode offers "放大"; the expanded view offers "收起" instead.
    var onExpand: (() -> Void)?
    var onClose: (() -> Void)?
    @State private var followTail = true

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            header
            list
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var header: some View {
        HStack(spacing: 8) {
            Text("日志")
                .font(.system(size: 14, weight: .semibold))

            if let onExpand {
                // Full screen belongs next to the *title*: sitting beside the
                // clear button it was one slip away from wiping the log.
                Button(action: onExpand) {
                    Image(systemName: "arrow.up.left.and.arrow.down.right")
                }
                .buttonStyle(.plain)
                .help("放大到整个面板")
            }

            if let onClose {
                Button(action: onClose) {
                    Image(systemName: "arrow.down.right.and.arrow.up.left")
                }
                .buttonStyle(.plain)
                .help("收起")
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
                // No inter-row spacing: monospace lines already have their own
                // leading, and every point saved here is another route line the
                // operator can see without scrolling.
                LazyVStack(alignment: .leading, spacing: 0) {
                    if tunnel.visibleLogLines.isEmpty {
                        Text(emptyText)
                            .font(.system(size: 10.5, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 8)
                    } else {
                        ForEach(tunnel.visibleLogLines) { line in
                            Text(line.text)
                                // 10.5pt monospace fits a whole route line
                                // (`17:26:44 route www.google.com:443 -> Proxy (whitelist)`)
                                // in the ~340pt of log width the popover leaves;
                                // at 11pt the routing verdict fell off the end.
                                .font(.system(size: 10.5, design: .monospaced))
                                .foregroundStyle(color(for: line))
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
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
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

    /// Severity comes from the parsed level rather than a substring search:
    /// the token is stripped from the text so the route itself fits on one line.
    private func color(for line: LogLine) -> Color {
        switch line.level {
        case .error: return Color(nsColor: .systemRed)
        case .warn: return Color(nsColor: .systemOrange)
        case .info, .other:
            // Direct flows are the boring majority; keep them visually quieter
            // than proxied ones so a scan finds the tunnel traffic first.
            return LogRoute.isDirect(line.text) ? .secondary : .primary
        }
    }
}

/// The log card inside the popover: fills the height the layout gives it.
struct LogCard: View {
    @ObservedObject var tunnel: PhantomTunnel
    let height: CGFloat
    var onExpand: (() -> Void)?

    var body: some View {
        // Tighter than the default card: a monospace block does not need 14pt
        // of padding on every side, and the frame is fixed here anyway.
        Card(padding: 10) {
            LogPane(tunnel: tunnel, onExpand: onExpand)
        }
        .frame(height: height)
        // Narrower than the cards' page padding: the log is text, not a card
        // you read across, and those 4pt buy a character of line width.
        .padding(.horizontal, 12)
        .padding(.bottom, 4)
    }
}
