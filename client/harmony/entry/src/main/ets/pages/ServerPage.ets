import { promptAction } from '@kit.ArkUI';
import { common } from '@kit.AbilityKit';
import { pasteboard } from '@kit.BasicServicesKit';
import phantomLib from 'libphantom_harmony.so';

/// Embedded-server page: run the Phantom server on this phone and share the
/// generated phantom:// URI (text + QR) with client devices on the LAN.
@Component
export struct ServerPage {
  @State port: string = '443';
  @State cipher: string = 'auto';
  @State proto: string = 'tcp';
  @State status: number = 0; // 0 idle, 1 starting, 2 running, 3 error
  @State uri: string = '';
  @State lastError: string = '';
  private timer: number = -1;

  aboutToDisappear(): void {
    this.stopPolling();
  }

  startPolling(): void {
    this.stopPolling();
    this.timer = setInterval(() => {
      try {
        this.status = phantomLib.phantomHarmonyServerStatus();
        if (this.status === 3) {
          this.lastError = phantomLib.phantomHarmonyServerLastError();
        }
      } catch (e) {
        console.error('[PhantomServer] poll error: ' + JSON.stringify(e));
      }
    }, 500);
  }

  stopPolling(): void {
    if (this.timer !== -1) {
      clearInterval(this.timer);
      this.timer = -1;
    }
  }

  statusText(): string {
    switch (this.status) {
      case 1: return 'Starting...';
      case 2: return 'Listening';
      case 3: return this.lastError.length > 0 ? 'Error: ' + this.lastError : 'Error';
      default: return 'Stopped';
    }
  }

  /// Extract `host:port` from phantom://key@host:port?...#name for display.
  hostOf(uri: string): string {
    const at = uri.indexOf('@');
    if (at < 0) {
      return '';
    }
    const q = uri.indexOf('?', at);
    return q > 0 ? uri.substring(at + 1, q) : uri.substring(at + 1);
  }

  isRunning(): boolean {
    return this.status === 2;
  }

  startServer(): void {
    const parsed = parseInt(this.port.trim() === '' ? '443' : this.port.trim());
    if (isNaN(parsed) || parsed < 0 || parsed > 65535) {
      promptAction.showToast({ message: 'Invalid port' });
      return;
    }
    const ctx = getContext(this) as common.UIAbilityContext;
    const workDir = ctx.filesDir + '/server';
    try {
      this.uri = phantomLib.phantomHarmonyServerStart(workDir, parsed, this.cipher, this.proto);
      this.lastError = '';
      this.status = 2;
      this.startPolling();
    } catch (e) {
      this.lastError = JSON.stringify(e);
      this.status = 3;
      promptAction.showToast({ message: 'Server start failed' });
    }
  }

  stopServer(): void {
    try {
      phantomLib.phantomHarmonyServerStop();
    } catch (e) {
      console.error('[PhantomServer] stop error: ' + JSON.stringify(e));
    }
    this.status = 0;
  }

  copyUri(): void {
    if (this.uri.length === 0) {
      return;
    }
    const data = pasteboard.createData(pasteboard.MIMETYPE_TEXT_PLAIN, this.uri);
    pasteboard.getSystemPasteboard().setData(data).then(() => {
      promptAction.showToast({ message: 'URI copied' });
    });
  }

  @Builder
  cipherButton(label: string, value: string) {
    Button(label, { type: ButtonType.Capsule })
      .backgroundColor(this.cipher === value ? '#007DFF' : '#999999')
      .onClick(() => { if (!this.isRunning()) this.cipher = value; })
  }

  @Builder
  protoButton(label: string, value: string) {
    Button(label, { type: ButtonType.Capsule })
      .backgroundColor(this.proto === value ? '#007DFF' : '#999999')
      .onClick(() => { if (!this.isRunning()) this.proto = value; })
  }

  build() {
    Column({ space: 16 }) {
      Text('Phantom Server')
        .fontSize(24)
        .fontWeight(FontWeight.Bold)
        .margin({ top: 24 })

      Text(this.statusText())
        .fontSize(16)
        .fontColor(this.isRunning() ? '#0F5132' : (this.status === 3 ? '#FF0000' : '#666666'))

      if (!this.isRunning()) {
        TextInput({ placeholder: 'Listen port (0 = 443)', text: this.port })
          .width('90%')
          .type(InputType.Number)
          .onChange((value: string) => { this.port = value; })

        Row({ space: 12 }) {
          this.cipherButton('Auto', 'auto')
          this.cipherButton('AES-256', 'aes-256-gcm')
          this.cipherButton('ChaCha20', 'chacha20-poly1305')
        }

        Row({ space: 12 }) {
          this.protoButton('TCP', 'tcp')
          this.protoButton('QUIC', 'quic')
        }
      }

      Button(this.isRunning() ? 'Stop Server' : 'Start Server')
        .width('90%')
        .height(48)
        .backgroundColor(this.isRunning() ? '#FF4444' : '#007DFF')
        .onClick(() => {
          if (this.isRunning()) {
            this.stopServer();
          } else {
            this.startServer();
          }
        })

      if (this.uri.length > 0 && this.isRunning()) {
        Text('Share this URI with clients on the same network:')
          .fontSize(13)
          .fontColor('#666666')

        QRCode(this.uri)
          .width(220)
          .height(220)
          .color('#000000')
          .backgroundColor('#FFFFFF')

        Text('Address: ' + this.hostOf(this.uri))
          .fontSize(14)
          .fontWeight(FontWeight.Medium)

        Text(this.uri)
          .width('90%')
          .fontSize(11)
          .fontFamily('monospace')
          .fontColor('#333333')
          .maxLines(3)
          .textOverflow({ overflow: TextOverflow.Ellipsis })

        Button('Copy URI', { type: ButtonType.Capsule })
          .backgroundColor('#007DFF')
          .onClick(() => { this.copyUri(); })
      }
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#FFFFFF')
  }
}
