/** A Cloud size in decimal units (1 MB is 1,000,000 bytes), the units plans
 *  are sold in. */
export function formatCloudBytes(bytes: number): string {
  if (bytes < 1_000) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let value = bytes / 1_000
  let unit = units[0]
  for (const candidate of units.slice(1)) {
    if (value < 1_000) break
    value /= 1_000
    unit = candidate
  }
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)} ${unit}`
}
