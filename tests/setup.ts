import { afterEach, vi } from 'vitest';

afterEach(async () => {
  vi.restoreAllMocks();
  if (typeof document !== 'undefined') {
    const { cleanup } = await import('@testing-library/react');
    cleanup();
  }
});
