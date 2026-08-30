// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// Served from GitHub Pages at /bedouin/, so every internal link needs the base.
export default defineConfig({
  site: 'https://samishal1998.github.io',
  base: '/bedouin',
  integrations: [
    starlight({
      title: 'Bedouin',
      description:
        'Declarative environment manager. One YAML config, every machine. Zero runtime dependencies.',
      logo: {
        light: './src/assets/mark-light.svg',
        dark: './src/assets/mark-dark.svg',
        replacesTitle: false,
      },
      favicon: '/favicon.svg',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/samishal1998/bedouin' },
      ],
      customCss: ['./src/styles/bedouin.css'],
      editLink: {
        baseUrl: 'https://github.com/samishal1998/bedouin/edit/main/docs-site/',
      },
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'Why Bedouin', slug: 'guides/why' },
            { label: 'Install', slug: 'guides/install' },
            { label: 'Your first config', slug: 'guides/first-config' },
          ],
        },
        {
          label: 'The config',
          items: [
            { label: 'Conditional values', slug: 'config/conditionals' },
            { label: 'Shell & frameworks', slug: 'config/shell' },
            { label: 'Packages & languages', slug: 'config/packages' },
            { label: 'Files, rc blocks & PATH', slug: 'config/files' },
            { label: 'Repos', slug: 'config/repos' },
            { label: 'Aliases & completions', slug: 'config/aliases' },
            { label: 'Facts reference', slug: 'config/facts' },
          ],
        },
        {
          label: 'Commands',
          items: [
            { label: 'plan & apply', slug: 'commands/plan-apply' },
            { label: 'env', slug: 'commands/env' },
            { label: 'doctor & absorb', slug: 'commands/doctor-absorb' },
            { label: 'add, remove & sync', slug: 'commands/manage' },
            { label: 'reconcile & daemon', slug: 'commands/daemon' },
          ],
        },
        {
          label: 'How it works',
          items: [
            { label: 'The execution model', slug: 'internals/execution' },
            { label: 'State & ownership', slug: 'internals/state' },
          ],
        },
      ],
    }),
  ],
});
