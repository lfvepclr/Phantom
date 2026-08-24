#!/usr/bin/env bash
# 鸿蒙 HAP 调试签名：重建材料 + 手动签名。
#
# 背景：hvigor 的 signingConfigs 要求 storePassword/keyPassword 为加密格式
#（长度 >= 32），命令行无法直接喂明文密码；因此流程拆成两步——hvigor 产出
# unsigned HAP，再由 SDK 自带 hap-sign-tool.jar 手动签名。
#
# 材料：signing/app.p12（应用密钥对，keyAlias phantom_debug，密码 phantom123）、
# signing/app-debug.cer（应用证书链，由 SDK OpenHarmony.p12 的
# "openharmony application ca" 签发，该官方 CA 密码 123456）、
# signing/app-profile-debug.p7b（debug profile，绑定模拟器 UDID）。
#
# 材料重建（一次性，材料丢失或换设备 UDID 时）：
#   1. UDID=$(hdc shell bm get -u | tail -1)   # 写入 profile debug-info.device-ids
#   2. java -jar $SDK/lib/hap-sign-tool.jar generate-keypair -keyAlias phantom_debug \
#        -keyPwd phantom123 -keyAlg ECC -keySize NIST-P-256 -keystoreFile signing/app.p12 \
#        -keystorePwd phantom123
#   3. generate-csr → phantom.csr（subject: CN=Phantom Application Debug）
#   4. 从 SDK p12 导出 CA 证书：
#        keytool -exportcert -alias "openharmony application root ca" -keystore OpenHarmony.p12 \
#          -storepass 123456 -storetype PKCS12 -file oh-root-ca.cer
#        keytool -exportcert -alias "openharmony application ca" ... -file oh-sub-app-ca.cer
#   5. generate-app-cert -issuerKeyAlias "openharmony application ca" -issuerKeyPwd 123456 \
#        -issuerKeystoreFile $SDK/lib/OpenHarmony.p12 -issuerKeystorePwd 123456 \
#        -rootCaCertFile oh-root-ca.cer -subCaCertFile oh-sub-app-ca.cer -outForm certChain \
#        -outFile signing/app-debug.cer
#   6. 从 app-debug.cer 提取叶子证书嵌入 profile-unsigned.json
#      （bundle-info.development-certificate），bundle-name 固定 co.phantom.harmony
#   7. sign-profile -keyAlias "openharmony application profile debug" -keyPwd 123456 \
#        -profileCertFile $SDK/lib/OpenHarmonyProfileDebug.pem -signAlg SHA256withECDSA \
#        -keystoreFile $SDK/lib/OpenHarmony.p12 -keystorePwd 123456 \
#        -inFile profile-unsigned.json -outFile signing/app-profile-debug.p7b
#
# 日常签名（本脚本主体）：unsigned HAP → signed HAP。
set -euo pipefail

HARMONY_DIR="$(cd "$(dirname "$0")/../client/harmony" && pwd)"
SDK="${DEVECO_SDK_HOME:-/Applications/DevEco-Studio.app/Contents/sdk}/default/openharmony/toolchains"
UNSIGNED="${1:-$HARMONY_DIR/entry/build/default/outputs/default/entry-default-unsigned.hap}"
SIGNED="${2:-$HARMONY_DIR/entry/build/default/outputs/default/entry-default-signed.hap}"

java -jar "$SDK/lib/hap-sign-tool.jar" sign-app -mode localSign \
  -keyAlias phantom_debug -keyPwd phantom123 \
  -appCertFile "$HARMONY_DIR/signing/app-debug.cer" \
  -profileFile "$HARMONY_DIR/signing/app-profile-debug.p7b" -profileSigned 1 \
  -inFile "$UNSIGNED" -signAlg SHA256withECDSA \
  -keystoreFile "$HARMONY_DIR/signing/app.p12" -keystorePwd phantom123 \
  -outFile "$SIGNED"

echo "[sign-harmony-hap] signed: $SIGNED"
