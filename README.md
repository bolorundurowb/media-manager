# media-manager

[![CI](https://github.com/bolorundurowb/media-manager/actions/workflows/ci.yml/badge.svg)](https://github.com/bolorundurowb/media-manager/actions/workflows/ci.yml) [![codecov](https://codecov.io/gh/bolorundurowb/media-manager/graph/badge.svg?token=vlGI2r3lm0)](https://codecov.io/gh/bolorundurowb/media-manager)

🎬 A tool to help with formatting and reordering media collections to match expected formats for self-hosted media servers.

Rewrites a movie or TV library into a [Jellyfin](https://jellyfin.org)-friendly
folder layout. The CLI is always **in place**; the optional GUI can instead
use one explicitly selected destination root.

```text
media-manager <root> --type movies [--apply] [--verbose]
media-manager <root> --type tv     [--apply] [--verbose]
```

## Build and test

```sh
cargo build
cargo test
```

No feature flags are required for the CLI. `cargo build` produces a single
binary, `media-manager`.

## Usage

`--type` is **required** — `movies` or `tv` (aliases `movie` / `show` /
`shows` also work). There is no auto-detection: the whole root is processed
as that kind of library. A folder that mixes movies and shows needs two runs
against split roots.

```sh
# See what would happen, without touching anything:
media-manager /path/to/library --type movies

# Actually perform the moves:
media-manager /path/to/library --type movies --apply

media-manager /path/to/library --type tv --apply
media-manager /path/to/library --type tv --apply --verbose
```

**Dry-run is the default.** Nothing is written until you pass `--apply`.
The dry-run output is the exact plan `--apply` would execute: directories to
create, files to move, and anything skipped (with a reason).

### Destination layout

```text
--type movies:
  <root>/<Title> (<Year>)/<Title> (<Year>) - <Version>.<ext>

--type tv:
  <root>/<Show> (<Year>)/Season XX/<Show> SXXEYY.<ext>
```

Year is omitted when it can't be determined. Subtitles that sit next to a
video (or in a nested `subs`/`subtitles` folder) are renamed and moved
alongside it. A folder under `subs/` whose name matches the video's file
stem — the usual RARBG `subs/<release-name>/2_English.srt` layout — is
treated as that video's sidecars; known language names are mapped to
Jellyfin suffixes (`.en`, `.es`, …). Extra files of the same language
keep their track number (`.en.4`) so they do not collide. Other files
(`.nfo`, artwork, etc.) are moved into the destination folder once every
video in the source folder has been handled successfully.

Loose video files directly under the selected root are supported too. They
are parsed from their filename and moved into the same movie/show layout as
videos discovered inside folders.

Discovery descends through at most eight container levels and collects
videos up to three levels beneath a media folder. These conservative limits
avoid accidentally walking unrelated or cyclic directory trees.

## Safety

This tool moves your media, so it is deliberately conservative:

- **No overwriting.** A move is never performed if the destination already
  exists, or if two planned destinations would collide — including
  destinations that differ only by letter case, since Windows and macOS
  treat those as the same file.
- **Nothing is deleted except folders your own files just vacated.** A
  source folder is only removed once every file that was in it — including
  anything the tool declined to move — is gone. A folder with a skipped file
  left inside it is never deleted.
- **No guessing.** If a version, episode number, or season can't be read
  confidently from a name, that item is skipped and logged rather than
  auto-corrected or overwritten with a made-up label.
- **Ctrl+C is safe.** Interrupting a run stops it from *starting* any new
  directory creation, move, or cleanup step. A rename that has already begun
  is a single filesystem operation and always finishes; nothing that has
  already moved is rolled back.
- **A journal is kept during `--apply`.** Every step (attempted and result)
  is appended to `.media-manager-journal.log` in the destination root (the
  library root for in-place runs), so a run interrupted by a crash or kill
  can be reconstructed afterwards. It's diagnostic only — nothing reads it
  back to decide what to do.
- **Every step logs and continues.** Inaccessible directories, permission
  errors, and individual move failures are logged and skipped; they never
  abort the rest of the run. If a destination directory cannot be created,
  only moves into that directory are skipped — other movies and shows in
  the same plan still proceed.

At the end of a run, the log prints a summary: how many items were
processed, how many groups were merged (e.g. multiple resolutions of the
same movie, or multiple seasons of the same show), how many were skipped,
and how many failed. Per-file plan/apply events are always printed;
`--verbose` additionally enables debug-level parser and scanner diagnostics.

## Architecture

The pipeline is `scan → parse → group → plan → validate → execute`. Root
children are scanned and parsed in a bounded worker pool (up to eight
threads), then joined for global identity grouping and collision validation.
Only after that global safety barrier do independent movie/show destination
jobs execute concurrently; files targeting the same title/show remain one
serial job.

`execute` goes through a small `FileSystem` trait (enumerate, exists,
create-dir, rename-without-replace, remove-empty-dir) so its behaviour —
including destination collisions, permission failures, and a rename that
fails partway through a batch — can be tested against in-memory and
fault-injecting backends without touching real disk.

## GUI (optional)

A small [egui](https://github.com/emilk/egui) window is available behind a
`gui` Cargo feature, so a plain `cargo build` / `cargo test` never pulls in
GUI dependencies:

```sh
cargo run --features gui --bin media-manager-gui
```

Unlike the CLI (whole root, one `--type` per run), the GUI lets you assign
**type per selection**:

1. **Browse** to a source folder. Its immediate children are listed below.
2. **Dest (optional):** leave empty to organise in place, exactly like the
   CLI. Set it to write the reorganised layout into a different folder
   instead — the source folder is still what gets scanned.
3. Select one or more children in the list, then **Mark as Movies** or
   **Mark as TV**. A child can be re-assigned or cleared at any time before
   Start. Unassigned children are left completely untouched — not scanned,
   not moved.
4. Choose **Dry-run** (default) or **Apply**, then **Start**. The log panel
   streams the same wording the CLI prints (`CREATE`, `MOVE ... -> ...`,
   `SKIP ... (reason)`, `FAIL`, and a final summary line) as the run
   progresses on a background thread, so the window stays responsive.
5. **Stop** requests cancellation the same way Ctrl+C does on the CLI: no
   new step is *started*, but a move already in flight always finishes and
   nothing already moved is rolled back.

If the same title/year is assigned as both a movie and a TV show, neither
side wins a race for that destination folder: both assignments are skipped
and logged rather than one silently overwriting the other.

Cross-volume destinations are supported on Windows, Linux, and macOS. The
engine copies into a unique temporary file on the destination volume,
atomically publishes it without replacing an existing path, and removes the
source only after the complete copy has been synced. Failed copies clean up
their temporary file and leave the source intact.

The GUI calls the same `run_items` engine entry point the tests exercise —
it does not reimplement scanning, parsing, grouping, planning, or executing.
Its log streams interleaved job start/finish and file-operation events from
the same bounded parallel engine used by the CLI.
