/**
 * The main-frame half of upstream's `will-navigate`: the window never leaves
 * the app. Rust's guard (external_urls.rs) cannot do this part, because WRY
 * reports iframes to it without saying which frame asked, so blocking http(s)
 * there would break embeds. app-core handles its own links first; this catches
 * what is left: a link it did not claim, or a URL dragged from another app.
 */
import { invoke } from './ipc'

/** The anchor a click would follow, if the click is still going to follow it. */
function followedAnchor(event: MouseEvent): HTMLAnchorElement | null {
  if (event.defaultPrevented || event.button !== 0) return null
  const target = event.target as Element | null
  const anchor = target?.closest?.('a[href]') as HTMLAnchorElement | null
  if (!anchor || anchor.hasAttribute('download')) return null
  return anchor
}

/** Same page, another fragment: the one navigation that stays harmless. */
function isSameDocument(url: URL): boolean {
  const here = window.location
  return url.origin === here.origin && url.pathname === here.pathname && url.search === here.search
}

function isAssetUrl(url: URL): boolean {
  return url.protocol === 'syn-asset:' || url.hostname === 'syn-asset.localhost'
}

function isThemeAssetUrl(url: URL): boolean {
  return url.protocol === 'zen-theme:' || url.hostname === 'zen-theme.localhost'
}

export function handleAnchorClick(event: MouseEvent): void {
  const anchor = followedAnchor(event)
  if (!anchor) return
  let url: URL
  try {
    url = new URL(anchor.href)
  } catch {
    event.preventDefault()
    return
  }
  if (isSameDocument(url)) return
  event.preventDefault()
  if (url.origin === window.location.origin && url.protocol !== 'blob:') return
  if (isThemeAssetUrl(url)) return
  if (isAssetUrl(url)) {
    void invoke('app_open_vault_asset_url', { url: url.href }).catch(() => {})
    return
  }
  void invoke('app_open_external_url', { url: url.href }).catch(() => {})
}

/** A dropped link would otherwise load in the window. Files are app-core's. */
function isDraggedUrl(event: DragEvent): boolean {
  const types = Array.from(event.dataTransfer?.types ?? [])
  return types.includes('text/uri-list') && !types.includes('Files')
}

export function installNavigationGuard(): void {
  window.addEventListener('click', handleAnchorClick)
  window.addEventListener('dragover', (event) => {
    if (event.defaultPrevented || !isDraggedUrl(event)) return
    event.preventDefault()
    if (event.dataTransfer) event.dataTransfer.dropEffect = 'none'
  })
  window.addEventListener('drop', (event) => {
    if (!event.defaultPrevented && isDraggedUrl(event)) event.preventDefault()
  })
}
