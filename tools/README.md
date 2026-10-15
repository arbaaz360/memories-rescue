# tools/

## make_fixture: synthetic export fixtures with ground truth

`tools/make_fixture` builds the SRS v0.2 §11.2 test exports. Everything in
them is synthetic: solid-colour test images with the item ID drawn on them,
2-second ffmpeg test-pattern clips (or deterministic placeholder bytes where
only indexing and matching are under test), random UUIDs and dates, and dummy
URLs on the reserved `.invalid` domain. No real user data, ever.

### Requirements

- Python 3.10 or newer with [Pillow](https://pypi.org/project/pillow/) (`pip install pillow`).
- ffmpeg on `PATH`, optional, used by `small-2026b` and `small-2026a` only.
  Without it their video *files* are left out of the ZIP; the JSON entries are
  still written, `truth.json` marks each one with `main_skipped: "ffmpeg_missing"`,
  and the tests allow for that. Both GitHub runners lack ffmpeg.

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
| `--fixture NAME` | Build one fixture (repeatable); names in the table below. |
| `--out DIR` | Output directory. Default `<repo>/fixtures`, which is gitignored. |
| `--seed N` | Random seed (default `1`). Same seed, same output, byte for byte, except real video bytes which depend on the ffmpeg build. |
| `--zip-order json` or `shuffled` | Order of `memories/` entries inside each ZIP. `json` (default) follows the JSON array, which is the assumption behind the SRS §3.6 ordinal matching; `shuffled` breaks it on purpose. Recorded in `truth.json` under `generator.zip_order`. Works for every fixture. |
| `--multipart-profile 2026b` or `2026a` | Profile of the multipart family (F-04, F-05, F-06). `2026a` puts `Download Link` with `sid`/`mid` on every entry, which gives `mr-cli order-check` its ground truth. |
| `--no-video` | Skip real video files even when ffmpeg is present. |
| `--quiet` | No progress output. |

Each fixture is written as `fixtures/<name>/<part>.zip` (one or more parts)
plus `fixtures/<name>/truth.json`. The output directory is wiped and rebuilt
on every run. The whole set takes about ten seconds.

### Fixtures

| Fixture | SRS | Contents |
|---|---|---|
| `small-2026b` | F-01 | 50 items in one part: 30 photos (a few PNG), 20 videos, 15 overlays (30%), 35 with GPS (70%); JSON newest-first with URL fields empty, `N/A` or missing (`2026b` profile); media under `memories/`. |
| `small-2026a` | F-15 | The same 50 items, but every entry has a `Download Link` on a synthetic host carrying `sid` and `mid` equal to the filename UUID (`2026a` profile, exact matching). |
| `small-legacy` | F-02 | 50 JSON entries with signed, long-expired dummy `Download Link` / `Media Download Url` values carrying `sid`/`mid` UUIDs; no media bundled. |
| `weird-json` | F-10 | 30 hand-written cases in one JSON file: UTF-8 BOM, CRLF, tab indent, unknown top-level keys before and after the array, unknown entry fields, missing/empty/non-string `Location`, `0.0, 0.0`, out-of-range coordinates, ISO 8601 and other date variants, legacy URLs, escaped text, case-folded keys, an empty entry. |
| `multipart` | F-04 | 1,000 items over 4 parts of 250, every overlay in the part after its main's, part 2 renamed to `mydata (1).zip`. Same-day items share a place so that positional matching is right; three multi-item days have locations kilometres apart so the §3.6 location policy fires (7 items). Videos are placeholder bytes. `2026b` by default. |
| `missing-part` | F-05 | F-04 without part 3: 250 mains lost, 74 overlays lost, 77 orphan overlays. |
| `truncated` | F-06 | F-04 with part 2 cut at 70% of its bytes (238 entries readable, 1 partial, 87 lost) and one CRC-corrupt stored entry in part 1. |
| `zipslip` | F-14 | Two good items next to six entries that must be refused: `../`, `memories/../../`, an absolute path, a drive letter, a backslash traversal, a symlink (NFR-PRV-07). |

### truth.json (schema 1)

```text
schema            1
fixture, srs_id   e.g. "multipart", "F-04"
format            "2026a" | "2026b" | "legacy"
generator         name, version, seed, zip_order, json_order, ffmpeg{available, version, encoder | note}
parts[]           {file, role, entries[]} plus, for multi-part fixtures, number, renamed, present
json              {bom, crlf, indent, top_level_keys_before[], top_level_keys_after[]}
counts            json_entries, items, photos, videos, (unknown_type), overlays, with_location,
                  (with_instant), with_url, (with_tokens), media_files (all files under memories/),
                  main_files (main files present), main_files_skipped (items whose video
                  could not be rendered; main_files + main_files_skipped == items);
                  the multipart family adds multi_item_groups, items_in_multi_item_groups,
                  expected_location_uncertain, expected_second_precision
items[]           one object per JSON entry, keyed by json_index
```

Item fields by fixture:

- `small-2026b` / `small-2026a`: `id`, `date_utc` (RFC 3339), `date_field` (as written),
  `filename_date`, `media_type`, `media_type_field`, `ext`, `main`,
  `main_skipped`, `overlay`, `location` `{lat, lon}` or `null`,
  `location_field`, the two URL fields as written, `sid` / `mid` (equal to
  `id` in `small-2026a`, `null` in `small-2026b`), `zip_index_main`,
  `zip_index_overlay`.
- `small-legacy`: `date_utc`, `media_type`, `location`, `download_link`,
  `media_download_url`, `download_link_redacted`, `sid`, `mid`.
- `weird-json`: `case` (a name), `fields` (the raw entry) and `expected`
  with `date_utc`, `date_kind` (`instant` | `day_only` | `unparseable` |
  `missing`), `media_type` (`photo` | `video` | `unknown`), `location`,
  `location_reason` (`found` | `missing` | `unrecognised` | `null_island` |
  `out_of_range`), the URL fields, `sid`, `mid`, `unknown_fields`,
  `non_string_fields`.
- `multipart`, `missing-part`, `truncated`: `id`, `date_utc`, `media_type`,
  `ext`, `main {part, name, zip_index}`, `overlay {part, name, zip_index}` or
  `null`, `location`, `sid` / `mid` (`2026a` only), `day_group`
  (`YYYY-MM-DD/photo|video`), `day_group_size`, and `expected {confidence,
  time_precision, location_decision, status}` computed from the §3.6 rules in
  Python. In `missing-part` and `truncated`, items whose main is gone have
  `expected {status: "missing_media"}` and items whose overlay is gone have
  `overlay_present: false`. Top-level extras: `missing_part {number,
  lost_mains, lost_overlays, orphan_overlays[]}`; `truncated_part {number,
  cut_bytes, full_bytes, readable[], partial, lost[]}` and `corrupt_entry
  {part, name, crc32, flipped_offset}`.
- `zipslip`: `entries[] {name, refused, reason}` (`parent_traversal` |
  `absolute_path` | `drive_letter` | `symlink`) next to the two items.

Expected values are stated explicitly in `fixtures.py` and `fixtures_m1.py`;
the generator does not run the Rust parser to derive them. The §3.6 decisions
in the multipart family are an independent Python reading of the same rules.

### How the tests use it

`crates/mr-core/tests/fixtures.rs` (parsers, M0) and
`crates/mr-core/tests/m1.rs` (discovery, index, matching, M1) read the
fixtures and compare every count and decision with `truth.json`. If any
`fixtures/<name>/truth.json` is missing, the test binary runs
`python tools/make_fixture --all` itself before the first test (it tries
`python`, then `python3`, then `py`). The M1 tests also generate variants into
a temp directory (`--zip-order shuffled`, `--multipart-profile 2026a`) to prove
that a shuffled archive is detected and refused rather than matched wrong, and
that `order-check` says HOLDS on an ordered export and FAILS on a shuffled one.
CI generates the fixtures as an explicit step instead, so a generator failure
is reported on its own.

### Adding a fixture

Add a builder to `fixtures.py` or `fixtures_m1.py` and register it in the
`BUILDERS` map; keep the common `truth.json` keys, add fixture-specific item
fields, and document them above. Still to come: F-03 mixed-format, F-07
rotation, F-08 clips, F-09 codecs, F-11 tz-edge, F-12 unicode paths, F-13
free-limit, F-16 uncertain-location (M2+).
