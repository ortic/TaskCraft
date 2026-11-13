# Translating TaskCraft

TaskCraft speaks English, German, French and Italian. This guide is for improving a translation or
adding a language: where the texts are, how they're written, and how to check your work.

- [Where the texts are](#where-the-texts-are)
- [How a catalog looks](#how-a-catalog-looks)
- [Writing good translations](#writing-good-translations)
- [Adding a language](#adding-a-language)
- [Checking](#checking)

## Where the texts are

| Texts | Catalogs | Written in the code as |
|---|---|---|
| The web app and the service desk portal | `web/src/locales/<code>.json` | `t('Save changes')`, `t('{count} open issues', { count })` |
| E-mails, chat messages and other texts the server writes | `crates/server/locales/<code>.json` | `tr(lang, "Hi {name},", &[("name", …)])` |

Both use the **English text as the key**: there are no ids to look up, and a missing translation
simply shows the English one. `web/src/locales/messages.json` lists every key of the web app; it
is generated, don't edit it by hand.

## How a catalog looks

```json
{
  "Save changes": "Änderungen speichern",
  "{count} open issues": { "one": "{count} offener Vorgang", "other": "{count} offene Vorgänge" },
  "Open [[task filter]]": "Offen"
}
```

- **Placeholders** in braces — `{name}`, `{count}`, `{date}` — are filled in by the app. Keep them
  exactly as they are (you may move them around); the check reports lost ones.
- **Plurals**: when a text depends on a number, write an object with the plural categories of your
  language (`one`, `other`; some languages also need `zero`, `two`, `few`, `many` — see the
  [Unicode plural rules](https://www.unicode.org/cldr/charts/latest/supplemental/language_plural_rules.html)).
  English plural forms live in `en.json`; it holds nothing else.
- **Notes for translators** at the end of a key in double brackets, `Open [[task filter]]`, tell
  apart English words that need different translations. They're never shown; don't translate them
  into the value.

## Writing good translations

- **Address people the way the language does in software**: German uses the formal *Sie*
  throughout; French uses *vous*; Italian addresses people informally (*tu*). Stay consistent.
- **Use the established terms** of the catalog: look up how *issue*, *sprint*, *release*,
  *workspace*, *time log* are already translated and use the same words (German: *Vorgang*,
  *Sprint*, *Release*, *Arbeitsbereich*, *Zeiteintrag*).
- **Keep it short** where space is short — buttons, menu items, column headers.
- **Typography**: use your language's quotes and spaces (French: `« … »` and a space before `:`,
  `;`, `?`, `!`), and the apostrophe `’`.
- **Keep product and technical names**: TaskCraft, WikiCraft, TQL, ClamAV, OpenID Connect, and
  names of environment variables such as `TASKCRAFT_MAX_UPLOAD_MB`.
- Dates, times and numbers are formatted by the browser for the language; don't put them in the
  texts.

## Adding a language

1. **Web app**: copy `web/src/locales/de.json` to `web/src/locales/<code>.json` (a two-letter
   code), translate the values, and make sure the language is listed in `LANGUAGES` in
   `web/src/i18n.ts` — it then appears in the language choices by itself.
2. **Server**: add `crates/server/locales/<code>.json` and list it in `crates/server/src/i18n.rs`
   (the `include_str!` list at the top).
3. **Search**: word forms are understood through PostgreSQL's text search configurations. Map the
   code to its configuration (`es` → `spanish`, `nl` → `dutch`, …) in a new migration that
   replaces the `tc_search_lang` function (see `crates/server/migrations/0024_search_language.sql`).
   Languages PostgreSQL has no configuration for search by exact words and prefixes.
4. Right-to-left languages (Arabic, Hebrew) are marked `rtl` in `LANGUAGES`; check the layout in
   the browser.

## Checking

```bash
cd web
node scripts/i18n.mjs extract          # updates messages.json from the code
node scripts/i18n.mjs check de fr it   # missing, unused and lost-placeholder keys per language
```

`check` exits with an error while something is missing, and lists unused keys (left over from
texts that changed) — remove those. On the server, `cargo test -p taskcraft i18n` fails when a
`tr(…)` text has no translation in one of the catalogs.

Then look at the result: switch the language in *Profile & settings*, and walk through the pages
you translated — long words that break a layout show up only on screen.
