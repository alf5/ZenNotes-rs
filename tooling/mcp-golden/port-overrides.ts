/**
 * Where `zennotes-rs mcp` differs from upstream's server on purpose. Everything
 * recorded from upstream passes through here, so a re-record keeps these, and a
 * reworded passage fails the run instead of silently bringing the old text back.
 */

const replaceOnce = (text: string, find: string, replace: string): string => {
  const found = text.split(find).length - 1
  if (found !== 1) {
    throw new Error(`port-overrides: expected one match, found ${found}:\n${find}\nUpdate tooling/mcp-golden/port-overrides.ts.`)
  }
  return text.replace(find, () => replace)
}

/** The port's deep-link scheme: `zennotes://` opens upstream's app, if any. */
export const portLinks = (text: string): string => text.replaceAll('zennotes://', 'zennotes-rs://')

/** This build has no TikZ renderer and no remote vaults; the rest is upstream's. */
export function portInstructions(upstream: string): string {
  const swaps: Array<[string, string]> = [
    ['rendered live with KaTeX, TikZ, function-plot, JSXGraph,', 'rendered live with KaTeX, function-plot, JSXGraph,'],
    ['for math, TikZ / function-plot / JSXGraph / Mermaid / D2 for every\n   diagram. ', 'for math, function-plot / JSXGraph / Mermaid / D2 for every\n   diagram. This build does not render ```tikz, so never emit it.\n   '],
    [
      'mode this vault uses. It also says where the vault lives: a folder on\nthis machine, or a self-hosted ZenNotes server the desktop app is\nconnected to (the tools behave the same either way; paths are what the\nserver reports). The folder enum',
      'mode this vault uses. It also says where the vault lives, a folder on\nthis machine. The folder enum'
    ],
    [
      'catch yourself about to make one, delete it and emit a ```tikz (or\n```function-plot / ```jsxgraph / ```mermaid / ```d2) block instead.\nTriggers that mean "use TikZ, not ASCII": axes,',
      'catch yourself about to make one, delete it and emit a ```function-plot\n(or ```jsxgraph / ```mermaid / ```d2) block instead.\nTriggers that mean "draw it, not ASCII": axes,'
    ],
    [
      '- ```tikz: vectors, geometric figures, commutative diagrams,\n  math pictures. Default for anything you\'d draw in LaTeX.\n',
      ''
    ],
    ['- ```jsxgraph: interactive / draggable constructions.', '- ```jsxgraph: vectors, geometric figures, interactive / draggable\n  constructions.'],
    [
      '- `tikz`: emit the TikZ content itself, usually a bare\n  `\\begin{tikzpicture} … \\end{tikzpicture}` block. If you need\n  extra libraries or packages, add `\\usetikzlibrary{…}` /\n  `\\usepackage{…}` above it. Never emit `\\documentclass`.\n',
      ''
    ],
    ['arranged as a picture → convert to ```tikz.', 'arranged as a picture → convert to ```jsxgraph or ```mermaid.'],
    [
      'Vectors / geometric figures (TikZ):\n\n```tikz\n\\usetikzlibrary{arrows.meta,calc}\n\\begin{tikzpicture}[>=Stealth,scale=1.1]\n  \\draw[->,gray] (-0.5,0)--(4.5,0) node[right]{$x$};\n  \\draw[->,gray] (0,-0.5)--(0,3.5) node[above]{$y$};\n  \\draw[->,very thick,blue] (0,0)--(3,2)\n    node[midway,above left]{$\\mathbf{u}$};\n  \\node[below right] at (3,2) {$(3,2)$};\n\\end{tikzpicture}\n```\n\n',
      ''
    ]
  ]
  return portLinks(swaps.reduce((text, [find, replace]) => replaceOnce(text, find, replace), upstream))
}
