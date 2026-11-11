import { afterEach, describe, expect, it, vi } from 'vitest';

import { errorMessage, type Translate } from './errors';

const echo: Translate = (key, options) => `${key} ${JSON.stringify(options?.values ?? {})}`;

describe('errorMessage', () => {
  it('formats byte params and adds context', () => {
    const error = { code: 'INSUFFICIENT_SPACE', params: { needed: 2_000_000, available: 1_000 } };
    expect(errorMessage(echo, error, 'en', { device: 'Desk' })).toBe(
      'errors.INSUFFICIENT_SPACE {"device":"Desk","needed":"2 MB","available":"1 kB"}',
    );
  });

  it('keeps other numbers as they are', () => {
    const error = { code: 'PAIRING_LOCKED', params: { seconds: 300 } };
    expect(errorMessage(echo, error, 'en')).toBe('errors.PAIRING_LOCKED {"device":"","seconds":300}');
  });

  it('formats the text size limit as bytes', () => {
    const error = { code: 'TEXT_TOO_LONG', params: { maxBytes: 65_536 } };
    expect(errorMessage(echo, error, 'en')).toBe('errors.TEXT_TOO_LONG {"device":"","maxBytes":"65.5 kB"}');
  });

  it('passes the folder of a refused write through', () => {
    const error = { code: 'PERMISSION_DENIED', params: { folder: '/data' } };
    expect(errorMessage(echo, error, 'en')).toBe('errors.PERMISSION_DENIED {"device":"","folder":"/data"}');
  });

  it('adds why the connection failed and what to do', () => {
    const error = {
      code: 'PEER_UNREACHABLE',
      params: { quic: 'timedOut', tcp: 'noRoute', port: 52000, hint: 'firewallLinux' },
    };
    expect(errorMessage(echo, error, 'en', { device: 'ubuntu' })).toBe(
      'errors.PEER_UNREACHABLE {"device":"ubuntu","quic":"timedOut","tcp":"noRoute","port":52000,"hint":"firewallLinux"} ' +
        'errors.connect.quic {"failure":"errors.connect.failure.timedOut {}"} ' +
        'errors.connect.tcp {"failure":"errors.connect.failure.noRoute {}"} ' +
        'errors.connect.hint.firewallLinux {"device":"ubuntu","quic":"timedOut","tcp":"noRoute","port":52000,' +
        '"hint":"firewallLinux"}',
    );
  });

  it('leaves out a transport that was not tried', () => {
    const error = { code: 'TRANSPORT_UNAVAILABLE', params: { tcp: 'refused', port: 52000 } };
    const message = errorMessage(echo, error, 'en');
    expect(message).toContain('errors.connect.tcp');
    expect(message).not.toContain('errors.connect.quic');
    expect(message).not.toContain('errors.connect.hint');
  });

  describe('on macOS', () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it('names the Local Network permission for connection errors only', () => {
      vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)' });
      expect(errorMessage(echo, { code: 'PEER_UNREACHABLE', params: {} }, 'en')).toContain(
        'errors.macos_local_network',
      );
      expect(errorMessage(echo, { code: 'BUSY', params: {} }, 'en')).not.toContain('errors.macos_local_network');
    });

    it('uses the diagnosis instead of the general permission note when there is one', () => {
      vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)' });
      const error = { code: 'PEER_UNREACHABLE', params: { tcp: 'noRoute', port: 52000, hint: 'macosLocalNetwork' } };
      const message = errorMessage(echo, error, 'en');
      expect(message).toContain('errors.connect.hint.macosLocalNetwork');
      expect(message).not.toContain('errors.macos_local_network');
    });
  });
});
