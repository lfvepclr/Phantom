import SwiftUI
import AppKit
import PhantomMacKit

/// Proxy whitelist editor.
///
/// Previously a 72pt `TextEditor` holding raw text: typos silently became rules
/// that never matched, and there was no way to see what was actually in there.
/// Now it is a list — every entry is one visible, deletable row, and anything
/// that would not match is rejected with a reason instead of being stored.
struct WhitelistCard: View {
    @ObservedObject var tunnel: PhantomTunnel
    @State private var expanded = false
    @State private var draft = ""
    @State private var errorText = ""
    @State private var importText = ""
    @State private var showImport = false
    @State private var statusText = ""

    private var locked: Bool { tunnel.isRunning || tunnel.state == .connecting }

    var body: some View {
        Card {
            Button {
                expanded.toggle()
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: expanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.secondary)
                    Text("分流白名单")
                        .font(.system(size: 13, weight: .semibold))
                    Spacer(minLength: 0)
                    Text("\(tunnel.whitelistEntries.count) 条")
                        .font(.system(size: 11))
                        .foregroundStyle(.tertiary)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            if expanded {
                if tunnel.whitelistEntries.isEmpty {
                    Text("暂无自定义域名")
                        .font(.system(size: 11))
                        .foregroundStyle(.tertiary)
                } else {
                    VStack(spacing: 2) {
                        ForEach(tunnel.whitelistEntries, id: \.self) { domain in
                            HStack(spacing: 6) {
                                Image(systemName: "checkmark.circle.fill")
                                    .font(.system(size: 10))
                                    .foregroundStyle(Color(nsColor: .systemGreen))
                                Text(domain)
                                    .font(.system(size: 11, design: .monospaced))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                Spacer(minLength: 0)
                                Button {
                                    tunnel.removeWhitelistEntry(domain)
                                } label: {
                                    Image(systemName: "minus.circle")
                                }
                                .buttonStyle(.plain)
                                .disabled(locked)
                                .help("移除")
                            }
                            .padding(.vertical, 2)
                        }
                    }
                    .padding(6)
                    .background(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(Color(nsColor: .textBackgroundColor))
                    )
                }

                HStack(spacing: 6) {
                    TextField("example.com", text: $draft)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 11, design: .monospaced))
                        .disabled(locked)
                        .onSubmit(addDraft)
                    Button("添加", action: addDraft)
                        .disabled(locked || draft.trimmingCharacters(in: .whitespaces).isEmpty)
                }

                if !errorText.isEmpty {
                    Text(errorText)
                        .font(.system(size: 11))
                        .foregroundStyle(Color(nsColor: .systemRed))
                }
                if !statusText.isEmpty {
                    Text(statusText)
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }

                HStack(spacing: 8) {
                    Button("批量导入") { showImport.toggle() }
                    Button("复制全部") {
                        let board = NSPasteboard.general
                        board.clearContents()
                        board.setString(tunnel.whitelistExport, forType: .string)
                        statusText = "已复制 \(tunnel.whitelistEntries.count) 条到剪贴板"
                    }
                    .disabled(tunnel.whitelistEntries.isEmpty)
                    Spacer(minLength: 0)
                }
                .controlSize(.small)

                if showImport {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("粘贴域名（换行、逗号或空格分隔，支持直接粘贴 URL）")
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                        TextEditor(text: $importText)
                            .font(.system(size: 11, design: .monospaced))
                            .frame(height: 64)
                            .overlay(
                                RoundedRectangle(cornerRadius: 6, style: .continuous)
                                    .strokeBorder(Color.secondary.opacity(0.2), lineWidth: 0.5)
                            )
                        HStack(spacing: 8) {
                            Button("导入") {
                                statusText = tunnel.importWhitelist(importText)
                                errorText = ""
                                importText = ""
                                showImport = false
                            }
                            .disabled(locked || importText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                            Button("取消") {
                                importText = ""
                                showImport = false
                            }
                        }
                        .controlSize(.small)
                    }
                }

                Text("内置被墙域名清单始终生效，这里只补充额外需要走代理的域名；改动在下次连接时生效。")
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
            }
        }
    }

    private func addDraft() {
        let reason = tunnel.addWhitelistEntry(draft)
        if let reason {
            errorText = "「\(draft)」无法用作域名：\(reason)"
            statusText = ""
        } else {
            errorText = ""
            statusText = "已添加 \(draft)"
            draft = ""
        }
    }
}
