# media-manager

[![CI](https://github.com/bolorundurowb/media-manager/actions/workflows/ci.yml/badge.svg)](https://github.com/bolorundurowb/media-manager/actions/workflows/ci.yml) [![codecov](https://codecov.io/gh/bolorundurowb/media-manager/graph/badge.svg?token=vlGI2r3lm0)](https://codecov.io/gh/bolorundurowb/media-manager)

🎬 Tidy up your movie and TV folders so [Jellyfin](https://jellyfin.org) (and
similar media servers) recognise them. Available as a command-line tool and a
desktop app.

## Install

Download the archive for your system from the
[latest release](https://github.com/bolorundurowb/media-manager/releases/latest)
(Windows, macOS or Linux) and unzip it. It contains:

- `media-manager`: the command-line tool
- `media-manager-gui`: the desktop app

## What it does

It renames and moves your files into this layout:

```text
Movies:
  Inception (2010)/Inception (2010) - 1080p.mkv

TV shows:
  Severance (2022)/Season 01/Severance S01E01.mkv
```

Subtitles are renamed alongside their video, and other files (`.nfo`,
artwork) follow once the video has moved. Loose video files are handled too.

## Desktop app

Run `media-manager-gui`.

1. **Source:** choose the folder that holds your media. Its items are listed.
2. **Destination (optional):** leave empty to tidy in place, or pick another
   folder to write the result into.
3. Select items and click **Mark as Movies** or **Mark as TV**. Unassigned
   items are never touched.
4. Choose **Preview only** (the default) or **Apply changes**, then run.
   The activity log shows what is planned or done.
5. **Stop** halts the run safely. A move already in progress finishes, and
   nothing is undone.

## Command line

```sh
# Preview only. Nothing is changed:
media-manager /path/to/library --type movies

# Do it for real:
media-manager /path/to/library --type movies --apply
media-manager /path/to/library --type tv --apply
```

`--type` is required: `movies` or `tv`. A folder that mixes both needs two
runs on separate folders. Add `--verbose` for extra detail. The CLI always
works in place.

**Nothing is changed until you add `--apply`.** Always preview first.

## Safety

- Existing files are never overwritten. Clashing items are skipped and logged.
- Files are never deleted. Only empty folders left behind are removed.
- If a season or episode number can't be read, the item is skipped rather
  than guessed.
- Ctrl+C (or **Stop**) is safe: no new work starts, nothing is rolled back.
- Errors on one item are logged, and the rest of the run carries on.
- Applied runs write a log to `.media-manager-journal.log` in the library
  folder, which is useful if something goes wrong.

When a run finishes it prints a summary:
`<moved> moved, <merged> merged, <skipped> skipped, <failed> failed`.

## Updates

The desktop app checks GitHub on launch and every 6 hours. When a newer
version exists, a banner appears with a **View release** button that opens the
download page. Nothing is installed automatically. To turn the check off, set
the `MEDIA_MANAGER_NO_UPDATE_CHECK` environment variable.

## Building from source

```sh
cargo build --release                  # CLI only
cargo build --release --features gui   # CLI and desktop app
```
