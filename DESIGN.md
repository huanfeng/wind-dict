# Design

## Source of truth
- Status: Draft
- Last refreshed: 2026-08-04
- Primary product surfaces: Windows-first native main window, hotkey-invoked lookup window, two-pane lookup layout (left: query + candidate/history/favorites list; right: dictionary tabs + definitions), settings page.
- Evidence reviewed:
  - `CONTEXT.md`: product terminology and query-source boundaries.
  - `src/ui.rs`: current main window, history/favorites sidebar, completion popover, result card, settings page.
  - `src/skin.rs`: light, paper, and dark skin palettes.
  - `docs/adr/0005-windows-first.md`: Windows-first delivery constraint.
  - `docs/adr/0007-esc-hides-focus-loss-does-not.md`: main window remains visible for reading/copying.
  - `docs/adr/0011-user-data-outlives-deployment.md`: history and favorites persistence.
  - `docs/ui-redesign-mockups.html`: first-round UI exploration with visible history sidebar.
  - `docs/ui-redesign-search-first.html`: search-first redesign with floating completion and recall drawers.
  - `docs/ui-redesign-screens.html`: screen-state gallery for empty, no-result, Chinese entry, favorites drawer, settings, and translation source views.
  - External references, reviewed 2026-08-04:
    - Merriam-Webster homepage and dictionary guidance: `https://www.merriam-webster.com/`, `https://www.merriam-webster.com/grammar/how-to-use-the-dictionary`
    - Oxford Learner's Dictionaries homepage/search: `https://www.oxfordlearnersdictionaries.com/`
    - Google Translate history help: `https://support.google.com/translate/answer/6142480?co=GENIE.Platform%3DDesktop&hl=en`
    - DeepL translation history help: `https://support.deepl.com/hc/en-us/articles/5873890995100-About-the-translation-history-feature`

## Brand
- Personality: fast, quiet, trustworthy, native, low-friction.
- Trust signals: offline dictionary is clearly identified; generated translation sources are visibly distinct; dictionary results prioritize structured fields over promotional content.
- Avoid: learning-app framing, "生词本" wording, decorative dashboards, permanently visible low-frequency panels, marketing-style hero screens.

## Product goals
- Goals:
  - Make hotkey lookup feel immediate.
  - Keep the query field and current result as the primary visual hierarchy.
  - Preserve history and favorites as reliable recall tools without making them the home screen.
  - Support both English-to-Chinese and Chinese-to-English lookup without direction controls.
- Non-goals:
  - Do not become a vocabulary training or spaced-repetition app.
  - Do not auto-call translation sources when the offline dictionary misses.
  - Do not make history the main navigation model.
- Success signals:
  - A returning user can invoke, type, select a candidate, read, copy, and hide without scanning side navigation.
  - History is discoverable but normally collapsed.
  - Favorite status is obvious on the current word.

## Personas and jobs
- Primary personas: Windows users who need quick English/Chinese lookup while reading, writing, coding, or translating.
- User jobs:
  - Look up a word or Chinese term quickly.
  - Compare pronunciation, inflections, and grouped definitions.
  - Mark a useful headword for later.
  - Occasionally revisit a previous lookup.
- Key contexts of use: hotkey invocation over another app, short focused lookup sessions, occasional longer reading/copying.

## Information architecture
- Primary navigation: search/query first; current query source and settings live in title/toolbar controls.
- Core routes/screens:
  - Lookup screen, two panes: left pane holds the query input and one list (candidates / history / favorites, segmented); right pane holds the direction tabs (全部 / 英汉 / 汉英) and the result content with per-word actions.
  - Recall lives in the left pane, not a drawer: history and favorites are the same list as completion candidates, switched by the segmented control. The list defaults to history when the query is empty.
  - Settings screen: skin, hotkey, startup, dictionary paths, data management.
  - Empty and no-result screens: quiet current-query states, not history dashboards.
  - Translation source screen: generated/source-produced content with explicit trust labeling.
- Content hierarchy:
  - Level 1: query field and current result.
  - Level 2: candidate completion and current headword actions.
  - Level 3: query source switcher, favorites, copy, pin.
  - Level 4: history, bulk data management, secondary statistics.

## Design Principles
- Search is home. The default visible interface should help the next lookup, not review past lookups.
- Definition text is selectable. Headword, phonetics, glosses, and the English-English section are rich text the user can drag-select and copy (right-click offers copy/select-all). Badges stay outside that: star ratings, exam tags, and inflection chips carry their meaning in shape and color, so selecting them would only yield context-free fragments.
- Focus is shown by the cursor row, not by a ring around the list. A ring around a whole pane reads as an error state; the row highlight is already there and moves when you press an arrow key, which says more about where the keyboard is pointed.
- The global hotkey toggles. Pressing it while the window is visible hides it; the visibility comes from the framework, not an app-side flag, because hiding can originate inside the framework (Escape, close-to-hide).
- The two panes are resizable and the width persists. Left-pane width is stored in px, not as a ratio, and Settings offers a reset.
- Looking a word up must not rewrite the query field. Clicking a row in the left pane runs the lookup and nothing else, so the same list stays put and the user can click through it to compare entries. Only accept-completion (Right) puts a word into the field, because that is the user explicitly asking to keep editing it. Tab is reserved for focus navigation.
- Recall shares the query's pane. History, favorites, and completion candidates are all one column of headwords, so they share one list next to the query field instead of a separate drawer. This keeps recall one click from the input and avoids a third column at the 720px minimum width.
- Result reading must stay calm. Completion overlays can cover content briefly, but permanent chrome should not squeeze definitions.
- Typing must never move the result. Candidate count changes must not insert or remove rows in the result area. The two-pane layout satisfies this structurally: candidates live in the left pane, definitions in the right, so the two cannot push each other around.
- Native controls beat novelty. Use familiar icon buttons, segmented controls, switches, and compact menus.
- Terminology is product logic. Use "查询", "补全", "词头", "历史记录", "收藏", "查询源"; never use "生词本".

## Visual Language
- Color: neutral light as default, paper and dark as skins; avoid one-hue domination and decorative gradients.
- Typography: Segoe UI / Microsoft YaHei UI for UI; Georgia for English headwords and part-of-speech chips.
- Spacing/layout rhythm: compact desktop density with 8px rhythm; result text capped to readable width.
- Shape/radius/elevation: 6-8px for most controls/cards; elevation only for floating completion and drawers.
- Motion: short open/close transitions for drawers and popovers; no continuous animation.
- Imagery/iconography: no decorative imagery; use symbolic icons for history, favorite, copy, settings, source, pin.

## Components
- Existing components to reuse:
  - title bar/window buttons, query input, candidate panel, result card, star button, settings rows, skin cards.
- New/changed components:
  - Left pane: query input, an empty-state line, one shared list, and the segmented 查询/历史/收藏 control (the first segment is labelled 查询, not 候选: 候选 is the internal term for what completion produces, and putting it on screen reads as a different thing) at the bottom (below the list, so Tab out of the query field lands on the list).
  - Right pane: direction tab bar (全部/英汉/汉英) filtering the already-fetched cards; it never re-routes the query. Entry bodies are rich text (selectable); grading and inflection badges remain regular elements.
  - Draggable splitter between the panes; it draws the divider line itself so it can thicken on hover.
  - Full-width notice strip at the bottom, since its sources span both panes.
  - Source/status strip that does not compete with the result.
  - Empty-state panel, no-result actions, translation-source warning banner, settings skin picker.
- Variants and states:
  - Left pane on each of its three tabs, each with its own empty state; direction tab holding no cards for the current lookup; empty history; unavailable user data; active favorite; unselected favorite; query-source warning for generated content.
- Token/component ownership:
  - Colors must resolve through role/skin tokens in native code.

## Accessibility
- Target standard: WCAG AA for text contrast where feasible.
- Window-level shortcuts (listed read-only in Settings, and in the SHORTCUTS table in ui.rs): Ctrl+L focuses the query field and selects its contents; Ctrl+R re-runs the current lookup; Ctrl+Left/Right walk the lookup path; Ctrl+W collapses the window (it does not quit — the tray keeps running); Escape returns from Settings rather than collapsing the window. These are dispatched only after the focused control declines the key, so typing is never intercepted.
- The wake hotkey accepts a single function key. F1–F12 do not take part in text entry, so they are safe on their own; letters and digits still require a modifier, or they would swallow that character in every application.
- Keyboard/focus behavior: waking the window returns focus to the query field and re-selects its contents, even if focus had been left on another control. Up/Down move the left-pane row cursor and look the row up live (without recording history); Right accepts the completion into the query field; Enter looks the row up and records history. Tab is left to focus navigation and goes query -> list -> segmented: the clear button is out of the tab ring (it is an attachment to the field, not a destination, and landing on it puts a destructive action under the space bar), and so are the individual rows, so the list takes focus as one unit and moves by Up/Down inside it (roving tabindex). Clicking a row also hands focus to the list, so the mouse and the keyboard connect. Escape hides the window.
- Contrast/readability: muted text must remain readable on all skins; do not use low-contrast decorative labels for required information.
- Screen-reader semantics: icon buttons need labels; segmented history/favorites tabs need selected state.
- Reduced motion and sensory considerations: tab-indicator and popover animation should be short; nothing animates continuously.

## Responsive Behavior
- Supported breakpoints/devices: Windows desktop, default 920x620, minimum 720x480.
- Layout adaptations:
  - At narrow width, hide side metadata first; keep query and result.
  - The left pane defaults to 280px and is user-resizable by dragging the splitter; the right pane always keeps at least 380px. Below the 720px window minimum the left pane should become collapsible rather than squeeze the definitions further.
- Touch/hover differences: hover affordances are optional; clickable areas remain at least 32px.

## Interaction States
- Loading: dictionary lookup should normally feel instant; translation sources need explicit loading state.
- Empty: show a quiet prompt in the result area and optionally a compact recent chip row.
- Error: user data errors appear as a narrow warning; favorite/settings failures are explicit.
- Success: favorite/save actions give immediate state change.
- Disabled: unavailable sources or settings controls show muted state plus reason.
- Offline/slow network: offline dictionary remains available; translation sources show source-specific status.

## Content Voice
- Tone: plain, compact, operational.
- Terminology: follow `CONTEXT.md`.
- Microcopy rules:
  - Do not call favorites "生词本".
  - Do not call translation sources "词典".
  - Placeholder can say "输入中文或英文".
  - History management belongs in settings or the left pane, not primary empty-state copy.

## Implementation Constraints
- Framework/styling system: Rust + windui; keep to existing `Element` patterns.
- Design-token constraints: app UI should use `Role` / `RoleAlpha`; literal colors belong in `skin.rs` or static mockups only.
- Performance constraints: no polling; completion remains reactive and capped.
- Compatibility constraints: Windows-first native behavior; main window is not focus-loss-hidden.
- Test/screenshot expectations: before native UI changes, capture at default 920x620 and minimum 720x480 for all skins.

## Open Questions
- [ ] Should the default main window show a compact recent-chip row when no query is active, or stay fully blank except for the query prompt? Owner: product/design. Impact: empty-state density.
- [ ] Should favorites be more prominent than history because they are intentional user data? Owner: product/design. Impact: toolbar ordering.
- [ ] Should there be a separate small hotkey lookup window in addition to the main window? Owner: product/engineering. Impact: window lifecycle and state sharing.
