/**
 * The CSS variables the editor and Preview read for size, line height, reading
 * width and the three font families. The app sets them from its preferences;
 * the Quick Look preview sets them from the same preferences in config.toml,
 * so a note looks alike in both.
 */
export interface TypographyVariables {
  editorFontSize: number
  mathFontScale: number
  editorLineHeight: number
  previewMaxWidth: number
  editorMaxWidth?: number
  contentAlign: string
  completedTaskStyle: string
  mathRenderer: string
  lineNumberPosition?: string
  interfaceFont: string | null
  textFont: string | null
  monoFont: string | null
}

// Each family has its own fallback stack, so leaving a font unset uses the
// platform default.
const INTERFACE_FONT_FALLBACK = '-apple-system, BlinkMacSystemFont, "SF Pro Text", Inter, system-ui, sans-serif'
const TEXT_FONT_FALLBACK = '"SF Mono", "SFMono-Regular", ui-monospace, "JetBrains Mono", Menlo, Consolas, monospace'
const MONO_FONT_FALLBACK = '"SF Mono", "SFMono-Regular", ui-monospace, "JetBrains Mono", Menlo, Consolas, monospace'

export function applyTypographyVariables(html: HTMLElement, values: TypographyVariables): void {
  html.style.setProperty('--z-editor-font-size', `${values.editorFontSize}px`)
  html.style.setProperty('--z-math-scale', String(values.mathFontScale / 100))
  html.style.setProperty('--z-editor-line-height', String(values.editorLineHeight))
  html.style.setProperty('--z-preview-max-width', `${values.previewMaxWidth}px`)
  if (values.editorMaxWidth !== undefined) {
    html.style.setProperty('--z-editor-max-width', `${values.editorMaxWidth}px`)
  }
  html.dataset.contentAlign = values.contentAlign
  html.dataset.completedTaskStyle = values.completedTaskStyle
  html.dataset.mathRenderer = values.mathRenderer
  if (values.lineNumberPosition !== undefined) html.dataset.lineNumberPosition = values.lineNumberPosition

  const setFont = (name: string, value: string | null, fallback: string): void => {
    if (value) html.style.setProperty(name, `"${value}", ${fallback}`)
    else html.style.removeProperty(name)
  }
  setFont('--z-interface-font', values.interfaceFont, INTERFACE_FONT_FALLBACK)
  setFont('--z-text-font', values.textFont, TEXT_FONT_FALLBACK)
  setFont('--z-mono-font', values.monoFont, MONO_FONT_FALLBACK)
}
