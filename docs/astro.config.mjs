import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
export default defineConfig({
  site: 'https://codestz.github.io',
  base: '/krait',
  redirects: {
    '/docs': '/getting-started/installation',
    '/docs/': '/getting-started/installation',
  },
  integrations: [
    starlight({
      title: 'krait',
      description: 'Code intelligence CLI for AI agents',
      logo: {
        light: './src/assets/logo-light.svg',
        dark: './src/assets/logo-dark.svg',
        replacesTitle: false,
      },
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/Codestz/krait' },
      ],
      customCss: ['./src/styles/custom.css'],
      sidebar: [
        {
          label: 'Getting Started',
          autogenerate: { directory: 'getting-started' },
        },
        {
          label: 'Commands',
          autogenerate: { directory: 'commands' },
        },
        {
          label: 'Language Support',
          autogenerate: { directory: 'languages' },
        },
        {
          label: 'Reference',
          items: [
            { label: 'Configuration', link: '/configuration/' },
            { label: 'Output Formats', link: '/output-formats/' },
            { label: 'How It Works', link: '/how-it-works/' },
          ],
        },
        { label: 'Troubleshooting', link: '/troubleshooting/' },
        { label: 'Contributing', link: '/contributing/' },
      ],
      head: [
        {
          tag: 'meta',
          attrs: { name: 'theme-color', content: '#00D4A8' },
        },
      ],
    }),
  ],
});
