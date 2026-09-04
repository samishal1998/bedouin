import { defineConfig } from 'astro/config';

// Static, and inlined. The output is embedded in `bedouin-ui` with
// `include_dir!` and served from memory: a sidecar that has to find sibling
// files at runtime is a sidecar that breaks when it is moved, and this one is
// moved by definition — it is fetched into a directory of its own.
export default defineConfig({
  output: 'static',
  build: { inlineStylesheets: 'always', assets: '_a' },
  vite: {
    build: {
      assetsInlineLimit: 1024 * 1024,
      rollupOptions: { output: { inlineDynamicImports: true } },
    },
  },
});
