import preset from './packages/app-core/build/tailwind-preset.cjs'

// Mirror of upstream apps/web/tailwind.config.js. The theme itself lives in
// the vendored app-core preset, so a re-vendor updates it with no edit here.
/** @type {import('tailwindcss').Config} */
export default {
  presets: [preset],
  content: ['./index.html', './src/**/*.{ts,tsx}', './packages/app-core/src/**/*.{ts,tsx}']
}
