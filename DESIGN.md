# Design

## Source of truth
- Status: Draft
- Last refreshed: 2026-08-04
- Primary product surfaces: Windows-first native main window, hotkey-invoked lookup window, settings page, history/favorites drawers.
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
  - Lookup screen: query input, completion popover, result content, per-word actions.
  - Recall drawer: history and favorites, opened on demand from clock/star/library controls.
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
- Recall is a drawer. History and favorites are useful, but they should appear on demand as a side sheet, command panel, or compact chips.
- Result reading must stay calm. Completion overlays can cover content briefly, but permanent chrome should not squeeze definitions.
- Completion is a floating layer. Candidate count changes must not insert or remove layout rows, otherwise typing causes the result area to jump.
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
  - Collapsed recall rail or toolbar recall button.
  - History/favorites drawer with segmented tabs.
  - Compact recent-headword chips below or near the query field.
  - Source/status strip that does not compete with the result.
  - Empty-state panel, no-result actions, translation-source warning banner, settings skin picker.
- Variants and states:
  - Drawer closed, drawer open, empty history, unavailable user data, active favorite, unselected favorite, query-source warning for generated content.
- Token/component ownership:
  - Colors must resolve through role/skin tokens in native code.

## Accessibility
- Target standard: WCAG AA for text contrast where feasible.
- Keyboard/focus behavior: query receives focus on show; Escape hides window or closes topmost drawer/popover first; Tab order follows query, result actions, source controls, recall controls.
- Contrast/readability: muted text must remain readable on all skins; do not use low-contrast decorative labels for required information.
- Screen-reader semantics: icon buttons need labels; segmented history/favorites tabs need selected state.
- Reduced motion and sensory considerations: drawer animation should be optional and quick.

## Responsive Behavior
- Supported breakpoints/devices: Windows desktop, default 920x620, minimum 720x480.
- Layout adaptations:
  - At narrow width, hide side metadata first; keep query and result.
  - History/favorites remain drawers, not fixed sidebars.
  - Recent chips wrap or collapse into the history button.
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
  - History management belongs in settings or recall drawer, not primary empty-state copy.

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
