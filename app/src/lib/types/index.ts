// Types for every command payload and event. They mirror the Rust structs in `src-tauri/src` (camelCase).

export type Os = 'windows' | 'macos' | 'linux';

export type Direction = 'send' | 'receive';

export type Locale = 'en' | 'km' | 'ar';

/** An error from a command or a transfer. `code` is an `ErrorCode` name; translate it with `errors.<code>`. */
export interface CommandError {
  code: string;
  params: Record<string, unknown>;
}

/** A device found on the network. */
export interface Peer {
  id: string;
  name: string;
  os: Os;
  /** Short form, e.g. `A1B2-C3D4-E5F6`. */
  fingerprint: string;
  trusted: boolean;
}

/** This device. */
export interface SelfInfo {
  id: string;
  name: string;
  fingerprint: string;
  port: number;
  downloadDir: string;
}

/** A paired device. */
export interface TrustedDevice {
  id: string;
  name: string;
  fingerprint: string;
  /** Milliseconds since 1970-01-01 UTC. */
  pairedAt: number;
  autoAccept: boolean;
  /** Share the clipboard with it; it must turn this on for this device too. */
  syncClipboard: boolean;
}

/** Event `incoming-offer`: another device offers a file. */
export interface IncomingOffer {
  requestId: number;
  deviceId: string;
  deviceName: string;
  fingerprint: string;
  trusted: boolean;
  /** The file or folder name (the first one, if several were sent). */
  fileName: string;
  /** Number of files, not counting folders. */
  fileCount: number;
  size: number;
}

/** Event `transfer-started`. */
export interface TransferStarted {
  id: number;
  direction: Direction;
  peerId: string;
  peerName: string;
  /** The file or folder name (the first one, if several were sent). */
  fileName: string;
  fileCount: number;
  size: number;
}

/** Event `transfer-progress`, at most 10 per second per transfer. */
export interface TransferProgress {
  id: number;
  transferred: number;
  total: number;
  paused: boolean;
  /** The file being transferred, when there are several. */
  currentFile: string | null;
}

/** Event `transfer-complete`. `path` is set for received files. */
export interface TransferComplete {
  id: number;
  path: string | null;
}

/** Event `transfer-failed`. */
export interface TransferFailed {
  id: number;
  error: CommandError;
}

/** Event `text-received`: a paired device sent a text. It is kept only until the user closes it. */
export interface ReceivedText {
  deviceId: string;
  deviceName: string;
  fingerprint: string;
  text: string;
}

/** Event `clipboard-received`: a paired device's copy is now on the clipboard. The text is not sent to the window. */
export interface ClipboardReceived {
  deviceName: string;
}

/** The most bytes of UTF-8 one text may have, as `MAX_TEXT_BYTES` in `lanshare-core`. */
export const MAX_TEXT_BYTES = 64 * 1024;

/** Event `pairing-pin`: another device asks to pair; show the PIN. */
export interface PairingPin {
  requestId: number;
  deviceName: string;
  fingerprint: string;
  pin: string;
}

/** Event `pairing-result`: a pairing ended, started from either device. */
export interface PairingResult {
  requestId: number;
  device: TrustedDevice | null;
  error: CommandError | null;
}

export type HistoryStatus = 'completed' | 'failed' | 'cancelled';

/** A finished transfer. */
export interface HistoryEntry {
  id: number;
  direction: Direction;
  peerName: string;
  fileName: string;
  size: number;
  status: HistoryStatus;
  errorCode: string | null;
  path: string | null;
  /** Milliseconds since 1970-01-01 UTC. */
  finishedAt: number;
}

export const THEMES = ['light', 'dark'] as const;

export type Theme = (typeof THEMES)[number];

export function isTheme(value: string): value is Theme {
  return (THEMES as readonly string[]).includes(value);
}

/** Everything the Settings page edits. */
export interface Settings {
  deviceName: string;
  downloadDir: string;
  port: number;
  streams: number;
  chunkSizeMib: number;
  bandwidthLimitMbps: number;
  allowUntrusted: boolean;
  language: Locale | null;
  theme: Theme | null;
  closeToTray: boolean;
  startAtLogin: boolean;
}

/** Tray menu labels, translated by the frontend. */
export interface TrayLabels {
  open: string;
  pauseAll: string;
  quit: string;
}

/** A transfer as the UI tracks it. */
export interface Transfer extends TransferStarted {
  transferred: number;
  paused: boolean;
  status: 'active' | HistoryStatus;
  /** Smoothed bytes per second. */
  speed: number;
  error: CommandError | null;
  path: string | null;
  currentFile: string | null;
}

/** Whether a value looks like a `CommandError`. Commands reject with one; anything else is a bug. */
export function isCommandError(value: unknown): value is CommandError {
  return typeof value === 'object' && value !== null && 'code' in value && typeof value.code === 'string';
}
