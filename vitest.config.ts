import { existsSync } from 'fs';
import { resolve } from 'path';
import { defineConfig } from 'vitest/config';

const setupFile = resolve(__dirname, 'tests/setup.ts');

export default defineConfig({
  test: {
    // Pure-logic suites run in node; component/store suites opt into
    // jsdom with a `// @vitest-environment jsdom` pragma at the top of the file.
    environment: 'node',
    include: ['tests/**/*.test.{ts,tsx}'],
    setupFiles: existsSync(setupFile) ? ['./tests/setup.ts'] : [],
  },
});
