/*
 * Reads the app's own signing certificate expiry.
 *
 * Why: when the HarmonyOS signing Profile expires, the system refuses to start
 * the app with `aa start 10106105 / The target application is under control`
 * and AppGallery only offers "Uninstall". The app cannot show anything once it
 * is blocked, so the only useful moment to warn the user is *before* expiry.
 *
 * `bundleManager` exposes the app's signing certificate
 * (`GET_BUNDLE_INFO_WITH_SIGNATURE_INFO`, API 14+), and the DeviceCertificateKit
 * parses the X.509 `notAfter` field for us.
 */
import { bundleManager } from '@kit.AbilityKit';
import { cert } from '@kit.DeviceCertificateKit';
import { util } from '@kit.ArkTS';

export interface SigningExpiry {
  notAfterMs: number;
  /// Local date, e.g. "2026-10-18".
  notAfterText: string;
  /// Whole days remaining; negative after expiry.
  daysLeft: number;
}

function pad2(value: number): string {
  return value < 10 ? '0' + value.toString() : value.toString();
}

/// ASN.1 UTCTime ("YYMMDDHHMMSSZ") or GeneralizedTime ("YYYYMMDDHHMMSSZ")
/// to Unix epoch milliseconds. Returns NaN when the input is not understood.
function parseAsn1Time(raw: string): number {
  const value = raw.trim();
  const digits = value.endsWith('Z') ? value.substring(0, value.length - 1) : value;
  if (!/^[0-9]+$/.test(digits)) {
    return NaN;
  }
  let year = 0;
  let rest = '';
  if (digits.length === 12) {
    const yy = parseInt(digits.substring(0, 2), 10);
    year = yy >= 50 ? 1900 + yy : 2000 + yy;
    rest = digits.substring(2);
  } else if (digits.length === 14) {
    year = parseInt(digits.substring(0, 4), 10);
    rest = digits.substring(4);
  } else {
    return NaN;
  }
  const month = parseInt(rest.substring(0, 2), 10);
  const day = parseInt(rest.substring(2, 4), 10);
  const hour = parseInt(rest.substring(4, 6), 10);
  const minute = parseInt(rest.substring(6, 8), 10);
  const second = parseInt(rest.substring(8, 10), 10);
  return Date.UTC(year, month - 1, day, hour, minute, second);
}

/// Returns the app signing certificate expiry, or null when it cannot be read
/// (for example on an older device without the signature-info API).
export async function readSigningExpiry(): Promise<SigningExpiry | null> {
  try {
    const flags = bundleManager.BundleFlag.GET_BUNDLE_INFO_WITH_SIGNATURE_INFO;
    const info = bundleManager.getBundleInfoForSelfSync(flags);
    const encoded = info.signatureInfo.certificate;
    if (encoded === undefined || encoded.length === 0) {
      return null;
    }

    let blob: cert.EncodingBlob;
    if (encoded.indexOf('BEGIN CERTIFICATE') >= 0) {
      blob = {
        data: new util.TextEncoder().encodeInto(encoded),
        encodingFormat: cert.EncodingFormat.FORMAT_PEM,
      };
    } else {
      blob = {
        data: new util.Base64Helper().decodeSync(encoded),
        encodingFormat: cert.EncodingFormat.FORMAT_DER,
      };
    }

    const x509 = await cert.createX509Cert(blob);
    const notAfterMs = parseAsn1Time(x509.getNotAfterTime());
    if (isNaN(notAfterMs)) {
      return null;
    }
    const date = new Date(notAfterMs);
    const text = date.getFullYear().toString() + '-' +
      pad2(date.getMonth() + 1) + '-' + pad2(date.getDate());
    return {
      notAfterMs,
      notAfterText: text,
      daysLeft: Math.floor((notAfterMs - Date.now()) / 86400000),
    };
  } catch (error) {
    console.warn('[Phantom] read signing expiry failed: ' + JSON.stringify(error));
    return null;
  }
}
