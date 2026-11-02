/**
 * Help entries for features this port does not have (see `HELP_HIDDEN` in
 * vendored-copy.ts). Runs where help.ts is evaluated, on its own arrays, so
 * every reader of Help sees the same thing: the view, its search and its tabs.
 */
export interface HelpLists {
  cards: Array<Array<{ title: string }>>
  settings: Array<{ title: string; items: Array<{ label: string }> }>
  clearCards: Array<Array<unknown>>
}

export interface HiddenHelp {
  cardTitles: readonly string[]
  settingsSections: readonly string[]
  settingsRows: readonly string[]
}

function removeWhere<T>(list: T[], hidden: (item: T) => boolean): void {
  for (let i = list.length - 1; i >= 0; i--) {
    if (hidden(list[i] as T)) list.splice(i, 1)
  }
}

export function hideHelpEntries(lists: HelpLists, hidden: HiddenHelp): void {
  for (const list of lists.clearCards) list.length = 0
  for (const list of lists.cards) removeWhere(list, (card) => hidden.cardTitles.includes(card.title))
  removeWhere(lists.settings, (section) => hidden.settingsSections.includes(section.title))
  for (const section of lists.settings) {
    removeWhere(section.items, (row) => hidden.settingsRows.includes(row.label))
  }
}
