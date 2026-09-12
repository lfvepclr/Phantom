import { camera } from '@kit.CameraKit';
import { image } from '@kit.ImageKit';
import { fileIo as fs } from '@kit.CoreFileKit';
import { detectBarcode, scanBarcode, scanCore } from '@kit.ScanKit';
import { photoAccessHelper } from '@kit.MediaLibraryKit';
import { abilityAccessCtrl, common, Permissions } from '@kit.AbilityKit';
import { preferences } from '@kit.ArkData';
import { display, promptAction, router, window } from '@kit.ArkUI';
import { BusinessError } from '@kit.BasicServicesKit';

/// How often a frame is handed to the barcode decoder. Fast enough to feel
/// instant, slow enough that the conversion does not heat up the phone.
const DECODE_INTERVAL_MS: number = 300;
/// Fallback display refresh cadence (only used when no surface preview is up).
const DISPLAY_INTERVAL_MS: number = 150;
/// Decoder input is capped: Scan Kit does not need more, and smaller frames
/// keep the per-frame conversion cheap.
const SCAN_MAX_WIDTH: number = 960;

/// In-app QR scanner.
///
/// Two camera outputs share one session:
///
/// * `previewOutput` → the `XComponent` surface. This is the hardware preview
///   path: the camera writes frames straight to the surface, ArkUI composites
///   them, and the CPU never touches a pixel. This is what makes the viewfinder
///   visible at all — an earlier version fed the same frames through a JPEG
///   decoder, but the camera publishes YUV (`CAMERA_FORMAT_YUV_420_SP`), so
///   every decode failed and the page stayed black.
/// * `scanOutput` → an `ImageReceiver`, created with the profile's own format.
///   Those frames are converted to NV21 (the only format Scan Kit accepts) at
///   ~3 fps and decoded by HMS Scan Kit.
///
/// If the device refuses the second output, the page degrades to receiver-only:
/// the frames are converted to a `PixelMap` and drawn by ArkUI, which costs CPU
/// but keeps scanning functional.
@Entry
@Component
struct ScanPage {
  @State status: string = '正在启动相机…';
  @State hint: string = '将二维码放入框内';
  /// Fallback viewfinder image; `undefined` while the surface preview is live.
  @State preview: image.PixelMap | undefined = undefined;
  private xController: XComponentController = new XComponentController();
  private surfaceId: string = '';
  private started: boolean = false;
  private cameraManager: camera.CameraManager | null = null;
  private cameraInput: camera.CameraInput | null = null;
  private previewOutput: camera.PreviewOutput | null = null;
  private scanOutput: camera.PreviewOutput | null = null;
  private session: camera.Session | null = null;
  private imageReceiver: image.ImageReceiver | null = null;
  /// True when frames arrive as JPEG (only if such a profile exists).
  private framesAreJpeg: boolean = false;
  /// True while the `XComponent` surface receives frames directly.
  private surfacePreviewLive: boolean = false;
  private scanWidth: number = 0;
  private scanHeight: number = 0;
  private lastDisplayAt: number = 0;
  private lastDecodeAt: number = 0;
  private decoding: boolean = false;
  /// One-shot dump of the first frame's plane layout, for on-device diagnosis.
  private loggedFirstFrame: boolean = false;
  private finished: boolean = false;
  private cacheFile: string = '';
  /// Preview rotation currently applied (see `applyPreviewRotation`).
  private previewRotation: camera.ImageRotation = camera.ImageRotation.ROTATION_0;
  /// `display` reports the changed display id; the rotation is read from the
  /// default display at that moment.
  private displayListener: ((id: number) => void) | null = null;
  private pageWindow: window.Window | null = null;

  aboutToAppear(): void {
    const ctx = getContext(this) as common.UIAbilityContext;
    this.cacheFile = ctx.cacheDir + '/phantom_scan.jpg';
  }

  /// Immersive full screen: a scanner that keeps the status bar pushes its
  /// preview into a band with black bars above and below.
  onPageShow(): void {
    this.setImmersive(true);
    this.startRotationWatch();
  }

  onPageHide(): void {
    this.setImmersive(false);
    this.stopRotationWatch();
  }

  aboutToDisappear(): void {
    this.finished = true;
    this.stopRotationWatch();
    this.releaseCamera();
  }

  build() {
    Stack({ alignContent: Alignment.Center }) {
      // Hardware preview surface. `onLoad` is the earliest point where a valid
      // surface id exists, so the camera is started from there.
      XComponent({
        id: 'phantom_scan_preview',
        type: XComponentType.SURFACE,
        controller: this.xController,
      })
        .width('100%')
        .height('100%')
        .onLoad(() => {
          this.surfaceId = this.xController.getXComponentSurfaceId();
          this.start();
        })

      // Only painted in the receiver-only fallback.
      if (this.preview !== undefined) {
        Image(this.preview)
          .width('100%')
          .height('100%')
          .objectFit(ImageFit.Cover)
      }

      // Overlay: laid out as a column so it survives rotation and full screen
      // without the hard-coded offsets the first version used.
      Column() {
        Text(this.status)
          .fontSize(12)
          .fontColor('#DDDDDD')
          .backgroundColor('#66000000')
          .padding({ left: 10, right: 10, top: 4, bottom: 4 })
          .borderRadius(10)
          .margin({ top: 12 })

        Column({ space: 14 }) {
          // Self-drawn viewfinder.
          Column()
            .width(240)
            .height(240)
            .borderWidth(3)
            .borderColor('#00E5A0')
            .borderRadius(16)
            .backgroundColor(Color.Transparent)

          Text(this.hint)
            .fontSize(14)
            .fontColor(Color.White)
            .backgroundColor('#80000000')
            .padding({ left: 12, right: 12, top: 6, bottom: 6 })
            .borderRadius(14)
        }
        .layoutWeight(1)
        .justifyContent(FlexAlign.Center)

        Row({ space: 16 }) {
          Button('相册')
            .backgroundColor('#80000000')
            .onClick(() => { this.pickFromAlbum(); })

          Button('取消')
            .backgroundColor('#80000000')
            .onClick(() => { router.back(); })
        }
        .margin({ bottom: 28 })
      }
      .width('100%')
      .height('100%')
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#000000')
    .expandSafeArea([SafeAreaType.SYSTEM], [SafeAreaEdge.TOP, SafeAreaEdge.BOTTOM])
  }

  // -------------------------------------------------------------------------
  // Camera lifecycle
  // -------------------------------------------------------------------------

  /// `onLoad` can fire again after a configuration change; start once.
  private start(): void {
    if (this.started) {
      return;
    }
    this.started = true;
    this.ensurePermissionAndStart();
  }

  private async ensurePermissionAndStart(): Promise<void> {
    const ctx = getContext(this) as common.UIAbilityContext;
    try {
      const manager = abilityAccessCtrl.createAtManager();
      const permissions: Permissions[] = ['ohos.permission.CAMERA'];
      const result = await manager.requestPermissionsFromUser(ctx, permissions);
      if (result.authResults.length === 0 || result.authResults[0] !== 0) {
        this.status = '未授予相机权限，请到设置里开启后重试';
        return;
      }
    } catch (e) {
      this.status = '相机权限申请失败: ' + JSON.stringify(e);
      return;
    }
    await this.startCamera();
  }

  private async startCamera(): Promise<void> {
    const ctx = getContext(this) as common.UIAbilityContext;
    try {
      this.cameraManager = camera.getCameraManager(ctx);
      const cameras = this.cameraManager.getSupportedCameras();
      if (cameras.length === 0) {
        this.status = '未找到可用相机';
        return;
      }
      let chosen = cameras[0];
      for (const candidate of cameras) {
        if (candidate.cameraPosition === camera.CameraPosition.CAMERA_POSITION_BACK) {
          chosen = candidate;
          break;
        }
      }

      const capability =
        this.cameraManager.getSupportedOutputCapability(chosen, camera.SceneMode.NORMAL_PHOTO);
      const profiles = capability.previewProfiles;
      if (profiles.length === 0) {
        this.status = '相机不支持预览输出';
        return;
      }
      // The preview profile carries the pixel format the HAL will publish —
      // asking for anything else is why the previous version decoded nothing.
      console.info('[PhantomScan] preview profiles: ' + profiles.map((p: camera.Profile) =>
        p.size.width + 'x' + p.size.height + '@' + p.format).join(', '));

      const displayProfile = pickDisplayProfile(profiles);
      const scanProfile = pickScanProfile(profiles);
      this.scanWidth = scanProfile.size.width;
      this.scanHeight = scanProfile.size.height;
      this.framesAreJpeg = scanProfile.format === camera.CameraFormat.CAMERA_FORMAT_JPEG;

      this.cameraInput = this.cameraManager.createCameraInput(chosen);
      await this.cameraInput.open();

      // The receiver's declared format never changes what the producer sends
      // ("the configuration of image properties should be done on the sending
      // side"), but it must still be one the image framework accepts: the
      // camera's own YUV_420_SP (1003) is rejected with 401, while
      // YCBCR_422_SP/JPEG are the only enumerators available. Try them in
      // order so a device that supports JPEG receivers is used verbatim.
      const receiver = this.createReceiver(scanProfile.format as number);
      if (receiver === undefined) {
        this.status = '创建取景缓冲失败';
        return;
      }
      this.imageReceiver = receiver;
      receiver.on('imageArrival', () => {
        this.onFrame();
      });
      const scanSurfaceId = await receiver.getReceivingSurfaceId();
      this.scanOutput = this.cameraManager.createPreviewOutput(scanProfile, scanSurfaceId);

      const session = this.cameraManager.createSession(camera.SceneMode.NORMAL_PHOTO);
      session.beginConfig();
      session.addInput(this.cameraInput);

      let surfacePreview = false;
      if (this.surfaceId.length > 0) {
        try {
          this.previewOutput = this.cameraManager.createPreviewOutput(displayProfile, this.surfaceId);
          if (session.canAddOutput(this.previewOutput)) {
            session.addOutput(this.previewOutput);
            surfacePreview = true;
          } else {
            console.warn('[PhantomScan] surface preview not addable, receiver-only mode');
            this.previewOutput = null;
          }
        } catch (e) {
          console.warn('[PhantomScan] surface preview unavailable: ' + JSON.stringify(e));
          this.previewOutput = null;
        }
      }

      session.addOutput(this.scanOutput);
      await session.commitConfig();
      await session.start();
      this.session = session;
      this.surfacePreviewLive = surfacePreview;
      // The output only exists from here on, so the rotation the page already
      // knows about has to be applied to it now.
      this.applyPreviewRotation(display.getDefaultDisplaySync().rotation);
      this.status = '扫码中…（也可从相册选择）';
      console.info('[PhantomScan] camera started (surfacePreview=' + surfacePreview +
        ', jpegFrames=' + this.framesAreJpeg + ')');
    } catch (e) {
      const err = e as BusinessError;
      this.status = '相机启动失败: ' + err.code;
      console.error('[PhantomScan] startCamera failed: ' + JSON.stringify(e));
    }
  }

  /// Build the frame receiver. `ImageReceiver` only accepts the two enumerators
  /// the image framework knows (`YCBCR_422_SP`, `JPEG`); everything else — the
  /// camera's `YUV_420_SP` (1003) included — fails parameter validation.
  private createReceiver(profileFormat: number): image.ImageReceiver | undefined {
    const candidates: image.ImageFormat[] = this.framesAreJpeg
      ? [image.ImageFormat.JPEG, image.ImageFormat.YCBCR_422_SP]
      : [image.ImageFormat.YCBCR_422_SP, image.ImageFormat.JPEG];
    for (const format of candidates) {
      try {
        const receiver = image.createImageReceiver(
          { width: this.scanWidth, height: this.scanHeight },
          format,
          4,
        );
        if (receiver !== undefined) {
          console.info('[PhantomScan] receiver format=' + format +
            ' (profile format=' + profileFormat + ')');
          return receiver;
        }
      } catch (e) {
        console.warn('[PhantomScan] receiver format ' + format + ' rejected: ' + JSON.stringify(e));
      }
    }
    return undefined;
  }

  private releaseCamera(): void {
    try {
      if (this.session !== null) {
        this.session.stop();
        this.session.release();
        this.session = null;
      }
    } catch (e) {
      console.warn('[PhantomScan] session release: ' + JSON.stringify(e));
    }
    try {
      this.previewOutput?.release();
      this.previewOutput = null;
    } catch (e) {
      console.warn('[PhantomScan] preview release: ' + JSON.stringify(e));
    }
    try {
      this.scanOutput?.release();
      this.scanOutput = null;
    } catch (e) {
      console.warn('[PhantomScan] scan output release: ' + JSON.stringify(e));
    }
    try {
      this.cameraInput?.close();
      this.cameraInput = null;
    } catch (e) {
      console.warn('[PhantomScan] input close: ' + JSON.stringify(e));
    }
    try {
      this.imageReceiver?.release();
      this.imageReceiver = null;
    } catch (e) {
      console.warn('[PhantomScan] receiver release: ' + JSON.stringify(e));
    }
    this.cameraManager = null;
  }

  // -------------------------------------------------------------------------
  // Window / orientation
  // -------------------------------------------------------------------------

  /// Hide the system bars while the scanner is on screen (and put them back on
  /// the way out, so the rest of the app keeps its normal insets).
  private async setImmersive(enabled: boolean): Promise<void> {
    try {
      if (this.pageWindow === null) {
        this.pageWindow = await window.getLastWindow(getContext(this) as common.UIAbilityContext);
      }
      await this.pageWindow.setWindowLayoutFullScreen(enabled);
      const statusBar: Array<'status' | 'navigation'> = ['status', 'navigation'];
      await this.pageWindow.setWindowSystemBarEnable(enabled ? [] : statusBar);
    } catch (e) {
      console.warn('[PhantomScan] immersive mode unavailable: ' + JSON.stringify(e));
    }
  }

  /// The camera sensor does not follow the device: rotating the phone rotates
  /// the *overlay* while the preview keeps its sensor orientation. Listening to
  /// the display rotation and asking the output for the matching preview
  /// rotation is what keeps the viewfinder upright in all four poses.
  private startRotationWatch(): void {
    if (this.displayListener !== null) {
      return;
    }
    this.displayListener = (id: number) => {
      this.applyPreviewRotation(display.getDefaultDisplaySync().rotation);
    };
    try {
      display.on('change', this.displayListener);
      this.applyPreviewRotation(display.getDefaultDisplaySync().rotation);
    } catch (e) {
      console.warn('[PhantomScan] rotation watch failed: ' + JSON.stringify(e));
    }
  }

  private stopRotationWatch(): void {
    if (this.displayListener === null) {
      return;
    }
    try {
      display.off('change', this.displayListener);
    } catch (e) {
      console.warn('[PhantomScan] rotation unwatch failed: ' + JSON.stringify(e));
    }
    this.displayListener = null;
  }

  /// `displayRotation` is 0/1/2/3 (0°/90°/180°/270°); the camera converts that
  /// to the rotation the *preview* needs, which is not always the same value.
  private applyPreviewRotation(displayRotation: number): void {
    const output = this.previewOutput;
    if (output === null) {
      return;
    }
    try {
      const rotation = output.getPreviewRotation(displayRotation);
      if (rotation === this.previewRotation) {
        return;
      }
      output.setPreviewRotation(rotation, false);
      this.previewRotation = rotation;
      console.info('[PhantomScan] preview rotation -> ' + rotation);
    } catch (e) {
      console.warn('[PhantomScan] preview rotation failed: ' + JSON.stringify(e));
    }
  }

  // -------------------------------------------------------------------------
  // Frame handling
  // -------------------------------------------------------------------------

  private async onFrame(): Promise<void> {
    const receiver = this.imageReceiver;
    if (receiver === null || this.finished) {
      return;
    }
    let frame: image.Image | null = null;
    try {
      frame = await receiver.readNextImage();
    } catch (e) {
      return;
    }
    if (frame === null) {
      return;
    }

    const now = Date.now();
    const wantDecode = !this.decoding && now - this.lastDecodeAt >= DECODE_INTERVAL_MS;
    const wantDisplay = !this.surfacePreviewLive && now - this.lastDisplayAt >= DISPLAY_INTERVAL_MS;
    if (!wantDecode && !wantDisplay) {
      frame.release();
      return;
    }

    try {
      if (this.framesAreJpeg) {
        const component = await frame.getComponent(image.ComponentType.JPEG);
        const jpeg = component.byteBuffer;
        if (wantDisplay) {
          this.lastDisplayAt = now;
          await this.updatePreviewFromJpeg(jpeg);
        }
        if (wantDecode) {
          this.lastDecodeAt = now;
          this.decoding = true;
          await this.decodeJpeg(jpeg);
          this.decoding = false;
        }
      } else {
        const nv21 = await this.frameToNv21(frame, this.scanWidth, this.scanHeight);
        if (nv21 === null) {
          return;
        }
        if (wantDisplay) {
          this.lastDisplayAt = now;
          await this.updatePreviewFromNv21(nv21, this.scanWidth, this.scanHeight);
        }
        if (wantDecode) {
          this.lastDecodeAt = now;
          this.decoding = true;
          await this.decodeNv21(nv21, this.scanWidth, this.scanHeight);
          this.decoding = false;
        }
      }
    } catch (e) {
      console.warn('[PhantomScan] frame handling failed: ' + JSON.stringify(e));
    } finally {
      frame.release();
    }
  }

  /// Repack the camera's YUV planes into a tightly packed NV21 buffer.
  ///
  /// The HAL pads each row (`rowStride`), while NV21 is defined without
  /// padding, so the rows are copied one by one. V comes before U in NV21 —
  /// `CAMERA_FORMAT_YUV_420_SP` is documented as YVU, so the plane order is
  /// already correct and only the padding has to go.
  ///
  /// `ImageBufferData` (API 23+) is what carries raw preview frames: the older
  /// `getComponent` accessor only ever exposes encoded JPEG data, which is why
  /// reading a YUV preview through it returns nothing.
  private async frameToNv21(
    frame: image.Image,
    width: number,
    height: number,
  ): Promise<ArrayBuffer | null> {
    const bufferData = this.bufferDataOf(frame);
    if (bufferData !== null) {
      return this.bufferDataToNv21(bufferData, width, height);
    }
    return this.componentsToNv21(frame, width, height);
  }

  private bufferDataOf(frame: image.Image): image.ImageBufferData | null {
    try {
      return frame.getBufferData();
    } catch (e) {
      console.warn('[PhantomScan] getBufferData failed: ' + JSON.stringify(e));
      return null;
    }
  }

  private bufferDataToNv21(
    data: image.ImageBufferData,
    width: number,
    height: number,
  ): ArrayBuffer | null {
    const strides = data.rowStride;
    const source = new Uint8Array(data.byteBuffer);
    if (!this.loggedFirstFrame) {
      this.loggedFirstFrame = true;
      console.info('[PhantomScan] frame buffer strides=' + strides.join(',') +
        ' pixelStrides=' + data.pixelStride.join(',') +
        ' bytes=' + source.length + ' size=' + width + 'x' + height);
    }
    if (strides.length === 0 || source.length < width * height) {
      return null;
    }

    const out = new Uint8Array(width * height * 3 / 2);
    const yStride = strides[0] > 0 ? strides[0] : width;
    ScanPage.copyRows(source, 0, yStride, out, 0, width, height, width);

    const uvRows = height >> 1;
    const uvOffset = width * height;
    if (strides.length >= 3) {
      // Separate U and V planes: interleave them into VU pairs.
      const uStride = strides[1] > 0 ? strides[1] : (width >> 1);
      const vStride = strides[2] > 0 ? strides[2] : (width >> 1);
      const uOffset = yStride * height;
      const vOffset = uOffset + uStride * uvRows;
      for (let row = 0; row < uvRows; row++) {
        const dst = uvOffset + row * width;
        for (let col = 0; col < (width >> 1); col++) {
          const vAt = vOffset + row * vStride + col;
          const uAt = uOffset + row * uStride + col;
          if (vAt >= source.length || uAt >= source.length || dst + 2 * col + 1 >= out.length) {
            break;
          }
          out[dst + 2 * col] = source[vAt];
          out[dst + 2 * col + 1] = source[uAt];
        }
      }
    } else {
      // Single interleaved VU plane (the usual camera preview layout).
      const uvStride = strides[1] > 0 ? strides[1] : width;
      ScanPage.copyRows(source, yStride * height, uvStride, out, uvOffset, width, uvRows, width);
    }
    return out.buffer;
  }

  /// Copy `rows` rows of `width` bytes, skipping each row's padding.
  private static copyRows(
    source: Uint8Array,
    sourceOffset: number,
    sourceStride: number,
    target: Uint8Array,
    targetOffset: number,
    width: number,
    rows: number,
    targetStride: number,
  ): void {
    for (let row = 0; row < rows; row++) {
      const from = sourceOffset + row * sourceStride;
      const to = targetOffset + row * targetStride;
      if (from + width > source.length || to + width > target.length) {
        return;
      }
      target.set(source.subarray(from, from + width), to);
    }
  }

  private async componentsToNv21(
    frame: image.Image,
    width: number,
    height: number,
  ): Promise<ArrayBuffer | null> {
    const y = await this.componentOrNull(frame, image.ComponentType.YUV_Y);
    if (y === null) {
      console.warn('[PhantomScan] frame has no readable plane');
      return null;
    }
    const u = await this.componentOrNull(frame, image.ComponentType.YUV_U);
    const v = await this.componentOrNull(frame, image.ComponentType.YUV_V);
    const out = new Uint8Array(width * height * 3 / 2);
    const yView = new Uint8Array(y.byteBuffer);
    const yStride = y.rowStride > 0 ? y.rowStride : width;
    ScanPage.copyRows(yView, 0, yStride, out, 0, width, height, width);

    const uvRows = height >> 1;
    const uvOffset = width * height;
    if (u !== null && v !== null) {
      const uView = new Uint8Array(u.byteBuffer);
      const vView = new Uint8Array(v.byteBuffer);
      const uStride = u.rowStride > 0 ? u.rowStride : (width >> 1);
      const vStride = v.rowStride > 0 ? v.rowStride : (width >> 1);
      for (let row = 0; row < uvRows; row++) {
        const dst = uvOffset + row * width;
        for (let col = 0; col < (width >> 1); col++) {
          const uAt = row * uStride + col;
          const vAt = row * vStride + col;
          if (vAt >= vView.length || uAt >= uView.length || dst + 2 * col + 1 >= out.length) {
            break;
          }
          out[dst + 2 * col] = vView[vAt];
          out[dst + 2 * col + 1] = uView[uAt];
        }
      }
    } else if (u !== null) {
      // Interleaved VU plane already.
      const uView = new Uint8Array(u.byteBuffer);
      const uStride = u.rowStride > 0 ? u.rowStride : width;
      for (let row = 0; row < uvRows; row++) {
        const src = row * uStride;
        const dst = uvOffset + row * width;
        if (src + width > uView.length || dst + width > out.length) {
          break;
        }
        out.set(uView.subarray(src, src + width), dst);
      }
    } else {
      return null;
    }
    return out.buffer;
  }

  private async componentOrNull(
    frame: image.Image,
    type: image.ComponentType,
  ): Promise<image.Component | null> {
    try {
      return await frame.getComponent(type);
    } catch (e) {
      return null;
    }
  }

  private async updatePreviewFromJpeg(jpeg: ArrayBuffer): Promise<void> {
    try {
      const source = image.createImageSource(jpeg);
      const pixelMap = await source.createPixelMap();
      await this.swapPreview(pixelMap);
    } catch (e) {
      console.warn('[PhantomScan] preview decode failed: ' + JSON.stringify(e));
    }
  }

  private async updatePreviewFromNv21(
    nv21: ArrayBuffer,
    width: number,
    height: number,
  ): Promise<void> {
    try {
      // No decoder is involved here: the buffer is already uncompressed, so
      // the pixel map is built straight from it (NV21 is a supported input
      // format). `srcPixelFormat` and `pixelFormat` both stay NV21, which makes
      // the graphics layer do the YUV→RGB conversion when it samples.
      const pixelMap = await image.createPixelMap(nv21, {
        size: { width: width, height: height },
        srcPixelFormat: image.PixelMapFormat.NV21,
        pixelFormat: image.PixelMapFormat.NV21,
        editable: false,
        alphaType: image.AlphaType.OPAQUE,
      });
      await this.swapPreview(pixelMap);
    } catch (e) {
      console.warn('[PhantomScan] nv21 preview decode failed: ' + JSON.stringify(e));
    }
  }

  /// ArkUI keeps the previous PixelMap alive until the state is replaced, and
  /// these buffers are large — release the old one explicitly.
  ///
  /// The software fallback draws the camera buffer itself, so it has to apply
  /// the same rotation the hardware preview would have applied; otherwise the
  /// image stays in sensor orientation while the overlay follows the phone.
  private async swapPreview(pixelMap: image.PixelMap): Promise<void> {
    const shown = pixelMap;
    if (!this.surfacePreviewLive && this.previewRotation !== camera.ImageRotation.ROTATION_0) {
      try {
        // `rotate` turns the map in place.
        await pixelMap.rotate(this.previewRotation as number);
      } catch (e) {
        console.warn('[PhantomScan] pixel map rotation failed: ' + JSON.stringify(e));
      }
    }
    const previous = this.preview;
    this.preview = shown;
    if (previous !== undefined && previous !== shown) {
      previous.release();
    }
  }

  // -------------------------------------------------------------------------
  // Decoding (HMS Scan Kit)
  // -------------------------------------------------------------------------

  private scanOptions(): scanBarcode.ScanOptions {
    return {
      scanTypes: [scanCore.ScanType.QR_CODE],
      enableMultiMode: false,
      enableAlbum: false,
    };
  }

  private async decodeJpeg(jpeg: ArrayBuffer): Promise<void> {
    const options = this.scanOptions();
    const fromFile = await this.decodeFromFile(jpeg, options);
    if (fromFile.length > 0) {
      this.onScanned(fromFile);
      return;
    }
    const inMemory = await this.decodeInMemory(jpeg, options);
    if (inMemory.length > 0) {
      this.onScanned(inMemory);
    }
  }

  private async decodeNv21(nv21: ArrayBuffer, width: number, height: number): Promise<void> {
    try {
      const result = await detectBarcode.decodeImage({
        byteBuffer: nv21,
        width: width,
        height: height,
        format: detectBarcode.ImageFormat.NV21,
      }, this.scanOptions());
      if (result.scanResults.length > 0) {
        this.onScanned(result.scanResults[0].originalValue);
      }
    } catch (e) {
      console.debug('[PhantomScan] nv21 decode unavailable: ' + JSON.stringify(e));
    }
  }

  private async decodeFromFile(
    jpeg: ArrayBuffer,
    options: scanBarcode.ScanOptions,
  ): Promise<string> {
    try {
      const file = fs.openSync(
        this.cacheFile,
        fs.OpenMode.READ_WRITE | fs.OpenMode.CREATE | fs.OpenMode.TRUNC,
      );
      try {
        fs.writeSync(file.fd, jpeg);
      } finally {
        fs.closeSync(file);
      }
      const results = await detectBarcode.decode({ uri: 'file://' + this.cacheFile }, options);
      if (results.length > 0) {
        return results[0].originalValue;
      }
    } catch (e) {
      console.debug('[PhantomScan] file decode unavailable: ' + JSON.stringify(e));
    }
    return '';
  }

  private async decodeInMemory(
    jpeg: ArrayBuffer,
    options: scanBarcode.ScanOptions,
  ): Promise<string> {
    try {
      const source = image.createImageSource(jpeg);
      const pixelMap = await source.createPixelMap({
        desiredPixelFormat: image.PixelMapFormat.NV21,
      });
      try {
        const info = await pixelMap.getImageInfo();
        const buffer = new ArrayBuffer(info.size.width * info.size.height * 3 / 2);
        pixelMap.readPixelsToBufferSync(buffer);
        const result = await detectBarcode.decodeImage({
          byteBuffer: buffer,
          width: info.size.width,
          height: info.size.height,
          format: detectBarcode.ImageFormat.NV21,
        }, options);
        if (result.scanResults.length > 0) {
          return result.scanResults[0].originalValue;
        }
      } finally {
        pixelMap.release();
      }
    } catch (e) {
      console.debug('[PhantomScan] in-memory decode unavailable: ' + JSON.stringify(e));
    }
    return '';
  }

  // -------------------------------------------------------------------------
  // Result handling
  // -------------------------------------------------------------------------

  private async pickFromAlbum(): Promise<void> {
    try {
      const picker = new photoAccessHelper.PhotoViewPicker();
      const selection = await picker.select({
        MIMEType: photoAccessHelper.PhotoViewMIMETypes.IMAGE_TYPE,
        maxSelectNumber: 1,
      });
      if (selection.photoUris.length === 0) {
        return;
      }
      const results = await detectBarcode.decode(
        { uri: selection.photoUris[0] },
        this.scanOptions(),
      );
      if (results.length === 0) {
        this.hint = '未识别到二维码';
        return;
      }
      this.onScanned(results[0].originalValue);
    } catch (e) {
      const err = e as BusinessError;
      // 1000500002 == user cancelled.
      if (err.code !== 1000500002) {
        promptAction.showToast({ message: '相册识别失败: ' + err.code });
      }
    }
  }

  private onScanned(value: string): void {
    if (this.finished) {
      return;
    }
    if (!value.startsWith('phantom://')) {
      this.hint = '不是 Phantom 连接串';
      promptAction.showToast({ message: '二维码内容不是 phantom:// 连接串' });
      return;
    }
    this.finished = true;
    this.releaseCamera();
    this.persistUri(value);
    router.back();
  }

  private persistUri(uri: string): void {
    try {
      const ctx = getContext(this) as common.UIAbilityContext;
      const prefs = preferences.getPreferencesSync(ctx, { name: 'phantom_ui' });
      prefs.putSync('serverUri', uri);
      prefs.flush();
      promptAction.showToast({ message: '连接串已保存，点 Start Tunnel 连接' });
    } catch (e) {
      console.error('[PhantomScan] persist failed: ' + JSON.stringify(e));
      promptAction.showToast({ message: '保存失败，请手动粘贴' });
    }
  }
}

/// Largest 16:9-ish profile that still keeps the ISP cheap.
function pickDisplayProfile(profiles: camera.Profile[]): camera.Profile {
  let best = profiles[0];
  for (const profile of profiles) {
    if (profile.format !== camera.CameraFormat.CAMERA_FORMAT_YUV_420_SP) {
      continue;
    }
    const wider = profile.size.width > best.size.width;
    const withinBudget = profile.size.width <= 1920;
    if (wider && withinBudget) {
      best = profile;
    }
  }
  return best;
}

/// Compromise between decoding a QR code and converting every frame: ~720p is
/// plenty for Scan Kit and costs a fraction of a 4K preview.
function pickScanProfile(profiles: camera.Profile[]): camera.Profile {
  let best: camera.Profile | null = null;
  for (const profile of profiles) {
    if (profile.format !== camera.CameraFormat.CAMERA_FORMAT_YUV_420_SP &&
      profile.format !== camera.CameraFormat.CAMERA_FORMAT_JPEG) {
      continue;
    }
    if (profile.size.width > SCAN_MAX_WIDTH) {
      continue;
    }
    if (best === null || profile.size.width > best.size.width) {
      best = profile;
    }
  }
  return best === null ? profiles[0] : best;
}
