/**
 * Host-OS detection for the renderer.
 *
 * The UI branches on the host in a handful of places — titlebar insets,
 * which permission rows are meaningful, which modifier glyphs to draw — and
 * each one had grown its own inline `navigator.userAgent` test. Several
 * disagreed: some read `navigator.platform`, some `navigator.userAgent`, and
 * a "not Windows" check silently meant "macOS" and so did the wrong thing on
 * Linux. One shared answer instead.
 *
 * `navigator.platform` is deprecated but is still the more reliable of the
 * two inside a WebKitGTK/WKWebView shell, where the user-agent string is the
 * webview's rather than the OS's. Both are consulted.
 */

const descriptor = (): string => {
  if (typeof navigator === 'undefined') return '';
  return `${navigator.platform ?? ''} ${navigator.userAgent ?? ''}`.toLowerCase();
};

export const isMacOS = (): boolean => /mac|iphone|ipad/.test(descriptor());

export const isWindows = (): boolean => /win/.test(descriptor());

/**
 * True on Linux specifically — note that Android also reports "linux", so it
 * is excluded explicitly rather than by accident.
 */
export const isLinux = (): boolean => {
  const platform = descriptor();
  return platform.includes('linux') && !platform.includes('android');
};
