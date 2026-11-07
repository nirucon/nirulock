# Quotes

NIRULOCK and NIRUSCREENSAVER share one set of quote collections, shown by
the `cava` layout when `visual` is `auto` (and no music plays) or `quotes`.
This page is identical in both repositories.


The quotes live in `${XDG_CONFIG_HOME:-~/.config}/niru/quotes/`, shared by
NIRULOCK and NIRUSCREENSAVER. Every `NAME.txt` file there is one
*collection*, named `NAME`. When the directory is missing, either program
creates it (atomically; directory 0755, files 0644) with the built-in
collections:

| Collection | Contents |
|---|---|
| `havamal-en` | Hávamál in English |
| `havamal-sv` | Hávamál in Swedish |
| `stoic-en` | Stoic quotes in English |
| `stoic-sv` | Stoic quotes in Swedish |

After that the directory is yours: neither program ever writes into it
again, so an edited collection stays edited and a deleted one stays deleted.
To get the built-in collections back, remove (or rename) the directory; it
is created again the next time the lock or screensaver starts. Each file's
own header explains the format with an example.

## The format

```text
# Lines starting with # are comments.
Quotes are separated by one or more blank lines.
A quote may span several lines;
its line breaks are kept.
— A line starting with "— " (em dash, space) or "-- " is the attribution

He who waits for something good
never waits too long.
-- Swedish proverb
```

- **Add a quote:** write it anywhere in a collection, for example at the end,
  with a blank line before and after it.
- **Remove a quote:** delete its lines, or put `#` in front of each of them.
- **Add a collection:** create another `.txt` file in the directory, for
  example `mine.txt` (the collection `mine`). Files whose names start with a
  dot, and files that are not `.txt`, are ignored.
- **Remove a collection:** delete its file, or leave it out of `quotes` below.

Files must be UTF-8. Very long quotes are shortened with an ellipsis so that
they fit. At most 64 collections are read (in name order), and a file larger
than 256 KiB is skipped. Changes are picked up the next time the lock or
screensaver starts and, while one is showing, at the next quote change. The
files are read on their own thread, never on the input or drawing path.

## Choosing collections (`quotes`)

`quotes` in `~/.config/niruui/lock.toml` (NIRULOCK) and
`~/.config/niruui/screensaver.toml` (NIRUSCREENSAVER) selects collections by
name. `*` matches any run of characters and `?` any single character. One
pattern or an array of them:

```toml
quotes = ["*"]                       # the default: every collection
quotes = "havamal-sv"                # one collection
quotes = ["havamal-*", "stoic-en"]   # several, with a pattern
quotes = ["*-en", "mine"]            # English collections and your own
```

The quotes of all selected collections are put in one pool and shown in a
shuffled order: every quote once before any is repeated, and never the same
quote twice in a row, so a small collection is not shown more often than a
large one. The two programs can select different collections.

If nothing is selected, or the selected collections are missing, unreadable
or hold no quote, the built-in collections are shown instead (those the
patterns name, otherwise all of them), so the panel is never empty. To show
no quotes at all, set `visual = "cava"` or `visual = "none"`.

## The old `quotes.txt`

NIRULOCK 0.19 and NIRUSCREENSAVER 0.11 used a single file,
`~/.config/niru/quotes.txt`. If you never edited it, it is ignored now (its
stanzas are the `havamal-sv` collection) and you can delete it. If you did
edit it, it still counts as the collection `quotes`; move your quotes into a
file in `quotes/` when convenient and delete the old file.

## Sources

- `havamal-sv`: 22 stanzas of Hávamál ("Den höges sång") in Erik Brate's
  Swedish translation, *Eddan. De nordiska guda- och hjältesångerna* (1913),
  public domain (Brate died in 1924), verbatim from Project Runeberg,
  <https://runeberg.org/eddan/se-02.html>. These are the stanzas of the old
  default `quotes.txt`.
- `havamal-en`: the same 22 stanzas in Henry Adams Bellows' English
  translation, *The Poetic Edda* (1923), public domain (Bellows died in 1939),
  from Wikisource. Stanza numbers are Bellows'; four differ by one from
  Brate's.
- `stoic-en`: 32 passages on moderation, few words, friendship, fate,
  mortality and simple living: Marcus Aurelius and Epictetus' *Encheiridion*
  in George Long's translations (1862 and later; Long died in 1879), and
  Seneca's *Minor Dialogues* and *On Benefits* in Aubrey Stewart's
  translations (1887–1889; Stewart died in 1918), from Project Gutenberg.
- `stoic-sv`: the same 32 passages in Swedish. Three Seneca passages are
  verbatim from Johan Bergman's translation, *Om livets korthet* (2nd ed.
  1918), public domain (Bergman died in 1951), from Project Runeberg. The
  other 29 are **not** a published translation: they are Swedish renderings
  made for NIRUX from Long's and Stewart's English, and their attributions
  say "NIRUX-tolkning".

`…` marks an omission inside a passage. Every quote was checked word for
word against its source; the exact passages, URLs and editorial changes are
listed in [`quotes/SOURCES.md`](../quotes/SOURCES.md).

