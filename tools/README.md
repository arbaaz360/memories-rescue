# tools/

## make_fixture: synthetic export fixtures with ground truth

`tools/make_fixture` builds the SRS §11.2 test exports. Everything in them is
synthetic: solid-colour test images with the item ID drawn on them,
2-second ffmpeg test-pattern clips, random UUIDs and dates, and dummy URLs on
the reserved `.invalid` domain. No real user data, ever.

### Requirements

- Python 3.10 or newer with [Pillow](https://pypi.org/project/pillow/) (`pip install pillow`).
- ffmpeg on `PATH`, optional. Without it the video *files* are left out of
  the ZIP; the JSON entries for them are still written, `truth.json` marks each
  one with `main_skipped: "ffmpeg_missing"`, and the tests allow for that.

### Usage

```bash
python tools/make_fixture --all
```

or, the same thing through npm:

```bash
npm run fixtures
```

| Flag | Meaning |
|---|---|
| `--all` | Build every fixture. |
| `--fixture NAME` | Build one fixture (repeatable): `small-2026`, `small-legacy`, `weird-json`. |
| `--out DIR` | Output directory. Default `<repo>/fixtures`, which is gitignored. |
| `--seed N` | Random seed (default `1`). Same seed, same output, byte for byte, except video bytes which depend on the ffmpeg build. |
| `--zip-order json\|shuffled` | Order of `memories/` entries inside the ZIP. `json` (default) follows the JSON array, which is the assumption behind the SRS §3.6 ordinal matching; `shuffled` breaks it on purpose. The choice is recorded in `truth.json` under `generator.zip_order`. |
| `--no-video` | Skip video files even when ffmpeg is present. |
| `--quiet` | No progress output. |

Each fixture is written as `fixtures/<name>/mydata~1760000000000.zip` plus
`fixtures/<name>/truth.json`. The output directory is wiped and rebuilt on
every run.

### Fixtures

| Fixture | SRS | Contents |
|---|---|---|
| `small-2026` | F-01 | 50 items in one part: 30 photos (a few PNG), 20 videos, 15 overlays (30%), 35 with GPS (70%); JSON newest-first with URL fields empty, `N/A` or missing; media under `memories/`. |
| `small-legacy` | F-02 | 50 JSON entries with signed, long-expired dummy `Download Link` / `Media Download Url` values carrying `sid`/`mid` UUIDs; no media bundled. |
| `weird-json` | F-10 | 30 hand-written cases in one JSON file: UTF-8 BOM, CRLF, tab indent, unknown top-level keys before and after the array, unknown entry fields, missing/empty/non-string `Location`, `0.0, 0.0`, out-of-range coordinates, ISO 8601 and other date variants, legacy URLs, escaped text, case-folded keys, an empty entry. |

### truth.json (schema 1)

```text
schema            1
fixture, srs_id   e.g. "small-2026", "F-01"
format            "2026" | "legacy"
generator         name, version, seed, zip_order, json_order, ffmpeg{available, version, encoder | note}
parts[]           {file, role, entries[]}   entries = ZIP entry names in archive order
json              {bom, crlf, indent, top_level_keys_before[], top_level_keys_after[]}
counts            json_entries, items, photos, videos, (unknown_type), overlays, with_location,
                  (with_instant), with_url, media_files (all files under memories/),
                  main_files (main files present), main_files_skipped (items whose video
                  could not be rendered; main_files + main_files_skipped == items)
items[]           one object per JSON entry, keyed by json_index
```

Item fields by fixture:

- `small-2026`: `id`, `date_utc` (RFC 3339), `date_field` (as written),
  `filename_date`, `media_type`, `media_type_field`, `ext`, `main`,
  `main_skipped`, `overlay`, `location` `{lat, lon}` or `null`,
  `location_field`, the two URL fields as written, `zip_index_main`,
  `zip_index_overlay`.
- `small-legacy`: `date_utc`, `media_type`, `location`, `download_link`,
  `media_download_url`, `download_link_redacted`, `sid`, `mid`.
- `weird-json`: `case` (a name), `fields` (the raw entry) and `expected`
  with `date_utc`, `date_kind` (`instant` | `day_only` | `unparseable` |
  `missing`), `media_type` (`photo` | `video` | `unknown`), `location`,
  `location_reason` (`found` | `missing` | `unrecognised` | `null_island` |
  `out_of_range`), the URL fields, `sid`, `mid`, `unknown_fields`,
  `non_string_fields`.

Every expected value is stated explicitly in `fixtures.py`; the generator does
not re-implement the parser to derive them.

### How the tests use it

`crates/mr-core/tests/fixtures.rs` reads the three fixtures and compares every
count and parsed value with `truth.json`. If `fixtures/<name>/truth.json` is
missing, the test binary runs `python tools/make_fixture --all` itself before
the first test (it tries `python`, then `python3`, then `py`). CI generates the
fixtures as an explicit step instead, so a generator failure is reported on its
own.

### Adding a fixture

Add a builder to `fixtures.py` and register it in `BUILDERS`; keep the common
`truth.json` keys, add fixture-specific item fields, and document them above.
F-03 onward (multi-part, truncated, rotation, clips, codecs, tz edges, zip-slip)
are M1+ work.
