import SwiftUI

@main
struct PhantomMacApp: App {
    @StateObject private var tunnel = PhantomTunnel()

    var body: some Scene {
        MenuBarExtra("Phantom", systemImage: tunnel.isRunning ? "bolt.fill" : "bolt") {
            ContentView(tunnel: tunnel)
        }
    }
}

struct ContentView: View {
    @ObservedObject var tunnel: PhantomTunnel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Phantom Tunnel")
                    .font(.headline)
                Spacer()
                Circle()
                    .fill(tunnel.isRunning ? Color.green : Color.red)
                    .frame(width: 10, height: 10)
            }

            Divider()

            TextField("phantom://key@host:port", text: $tunnel.serverURI)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 11, design: .monospaced))
                .disabled(tunnel.isRunning)

            Picker("Mode", selection: $tunnel.proxyMode) {
                ForEach(ProxyMode.allCases, id: \.self) { mode in
                    Text(mode.rawValue).tag(mode)
                }
            }
            .pickerStyle(.segmented)
            .disabled(tunnel.isRunning)

            Button(tunnel.isRunning ? "Stop" : "Start") {
                tunnel.isRunning ? tunnel.stop() : tunnel.start()
            }
            .frame(maxWidth: .infinity)

            Text("Status: \(tunnel.status)")
                .font(.caption)
                .foregroundStyle(.secondary)

            Divider()

            Button("Quit") {
                tunnel.stop()
                NSApplication.shared.terminate(nil)
            }
            .frame(maxWidth: .infinity)
        }
        .padding()
        .frame(width: 260)
    }
}
