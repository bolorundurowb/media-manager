# Media Manager — Rust Implementation Plan

Status: proposal / pre-implementation
Target: Rust 2024 edition
Platforms: Windows (first-class), Linux, macOS
Decisions taken: pure-Rust metadata (no ffmpeg/ffprobe), egui/eframe GUI

> Two rounds of adversarial technical review are folded in. Sections carrying a correction say so
> inline, so the reasoning stays auditable rather than being silently overwritten. Genuinely-open
> items are in §15.
>
> Substantive changes from review, in rough order of importance:
>
> - **§2.5** — the claim that FAT/exFAT/SMB have no no-replace rename primitive was **false** and
>   survived two drafts. `renameat2(RENAME_NOREPLACE)` works on vfat/exfat/cifs and `MoveFileExW`
>   works on FAT; only macOS on non-APFS volumes genuinely lacks one. `reserve` (`create_new`)
>   is retained for that case, for cross-device moves, and for network volumes — where the real
>   argument is client-side stale-dentry evaluation, not a missing capability.
> - **§6.7** — the `.mm-part-*` temp-file mechanism was dead code with five live references.
>   Removed; interrupted reservations are identified through the journal instead.
> - **§14** (error taxonomy) and **§5.0** (what "in place" means) are new. Both were referenced
>   repeatedly but never specified, and §5.0 turned out to be hiding the common case of root
>   being a show or artist directory.
> - **§3.3** — `{track_artist}` removed from the default music template rather than deferring its
>   invertibility problem to Phase 6.
> - **§2.4** — Windows case-sensitivity detection used the wrong API (`FILE_CASE_SENSITIVE_SEARCH`
>   means "supports", not "behaves"), which would have classified every NTFS volume as
>   case-sensitive.
> - **§6.4** — the resume state table was missing two of six states, including the only one with
>   no safe automatic action.

---

## 0. Guiding decisions

These are the choices that shape everything else. Each is a deliberate trade-off, not a default.

| Decision | Choice | Why |
|---|---|---|
| Concurrency model | Threads + bounded pool, **not** async/tokio | The workload is filesystem-bound. `tokio::fs` is a threadpool behind an async facade — it buys nothing here and costs a runtime, `Send + 'static` constraints on every closure, and harder debugging. A `rayon` pool with configured width gives the same bounded parallelism with plain blocking code. |
| Plan/execute split | Plan is a **serialisable value** (`serde`) | One artifact serves dry-run output, GUI preview, `--json` for automation, an on-disk plan file, and resume. Dry-run and apply provably share planning logic because apply consumes the same struct. |
| Execution shape | **Phased**: create dirs → move files → rename dirs → remove dirs, each phase a barrier | Source and destination trees overlap (organisation is in place), so operations have real ordering dependencies. Phase barriers are cheap and make the dependency structure explicit rather than emergent. |
| Parallelism unit | One task per **destination directory**, within the move phase only | Destination-directory ownership removes in-directory collisions and conflicting moves. It does *not* remove source-directory contention — see §6.3. |
| Confidence | Every parsed field carries its **source and confidence** | §24 demands "report, don't guess". That is only enforceable if the type system carries provenance. A field is not a `String`; it is a `Field<String>`. |
| Filesystem access | Behind a `FileSystem` trait | Permission failures, read-only mounts, `EXDEV`, and mid-operation failures (§22.5) are otherwise untestable without root and real network shares. |
| Path truth | `PathBuf`/`OsStr` is canonical; strings are derived | Non-UTF-8 filenames exist. Parsing needs `&str`. So: derive a lossy string, record whether the conversion was lossy, and treat lossy names as low-confidence input. No `camino`/`Utf8PathBuf` anywhere in the path layer. |
| Naming | Small typed template, not a template engine | `"{title}[ ({year})][ - {resolution}]"` with a validated placeholder whitelist and optional-segment brackets. Validation at config load rejects an unbracketed placeholder bound to an optional field. |
| Idempotency | Enforced by **field-level** round-trip, not string round-trip | A string fixed point can be reached while the parsed fields are wrong. See §3.3 — this is the correction that most changes the parser design. |

### MSRV

**Workspace MSRV is 1.95**, set by `egui`/`eframe` 0.36. `lofty` requires 1.89, `criterion` 1.86. An MSRV *promise* is not compatible with an egui dependency (egui raises its MSRV most releases), so MSRV is documented per-crate and CI tests `stable` and `stable − 2`.

Per-crate floors: `mm-core` **1.87** — `io::ErrorKind::InvalidFilename` (needed for path-too-long, §14) stabilised in 1.87, `CrossesDevices` in 1.85, `ReadOnlyFilesystem`/`StorageFull` in 1.83. `mm-parse` has no such constraint and floors lower, which keeps both reusable independently of the GUI.

---

## 1. Workspace layout

```text
media-manager/
├── Cargo.toml                     # [workspace], shared lints + dep versions
├── rust-toolchain.toml
├── deny.toml                      # cargo-deny: licence + advisory gate
├── crates/
│   ├── mm-core/                   # domain model, config, errors, FileSystem trait, path layer
│   ├── mm-parse/                  # filename → structured fields (pure, no I/O)
│   ├── mm-probe/                  # container/tag probing (video dims, audio tags)
│   ├── mm-engine/                 # scan → classify → group → plan → execute
│   ├── mm-provider/               # metadata provider trait + NFO; TMDB behind feature
│   ├── mm-cli/                    # bin: media-manager
│   └── mm-gui/                    # bin: media-manager-gui
├── testdata/
│   ├── names/                     # parser corpus (TOML tables of name → expectation)
│   ├── fixtures/                  # declarative directory trees for engine tests
│   └── media/                     # minimal valid mkv/mp4/flac/mp3 (few KB each)
└── docs/
    ├── naming.md                  # the naming contract, with examples
    └── adr/                       # architecture decision records
```

Seven crates is deliberate: it keeps `mm-parse` genuinely dependency-free and independently testable (spec §9), and makes "planning is independent from execution" (§12) a compile-time fact rather than a code-review convention. Only `mm-gui` pulls a heavy tree.

### Dependencies

Pin exact versions with `cargo add` at scaffold time.

**mm-core** — `serde`, `thiserror` v2, `toml`, `directories`, `unicode-segmentation` (grapheme-safe truncation), `unicode-normalization`, `windows-sys` (win only: volume info, `MoveFileExW`), `libc` (unix: `renameat2`/`renamex_np`, `pathconf`). Uses `std::sync::LazyLock`, not `once_cell`.
**mm-parse** — `regex`, `aho-corasick`, `unicode-normalization`. No I/O deps at all.
**mm-probe** — `lofty` (audio tags), `matroska` (MKV/WebM track dimensions), `re_mp4` (ISO-BMFF; see below).
**mm-engine** — `walkdir`, `rayon`, `crossbeam-channel`, `blake3`, `tempfile`, `tracing`, and **one** `Mutex<HashMap<..>>` for source-directory bookkeeping (§6.2 — no `dashmap`; the contention is low and a plain mutex is easier to reason about).
**mm-cli** — `clap` v4 (derive), `tracing-subscriber`, `indicatif`, `anyhow`, `owo-colors`
**mm-gui** — `eframe`, `egui`, `egui_extras` (virtualised tables), `rfd`
**dev** — `insta`, `proptest`, `tempfile`, `assert_cmd`, `trycmd`, `criterion`

**ISO-BMFF crate choice.** The obvious pick, `mp4` 0.14, is effectively unmaintained (last release 2023), MIT-only, still on `thiserror` v1, and rejects `.mov` files lacking an `ftyp` box. Use **`re_mp4`** (Rerun's maintained fork, MIT). Fallback option is `mp4parse` (Mozilla, MPL-2.0) — acceptable but note it re-introduces the licence class cited as a reason not to use `symphonia`, so if MPL is acceptable then `symphonia-format-isomp4` is also on the table and worth re-evaluating at Phase 4.

**`deny.toml` must pre-allow `CC0-1.0`** (`blake3`) and **`Unlicense`** (`walkdir`, `aho-corasick`) or the licence gate fails on first run. `insta` is Apache-2.0 only.

---

## 2. Core domain model (`mm-core`)

### 2.1 Provenance and confidence

Central to spec §24. Nothing gets renamed on a guess.

```rust
pub enum Source {
    EmbeddedTag,     // ID3, Vorbis comment, MP4 ilst, Matroska tag
    ContainerHeader, // pixel dimensions from tkhd / TrackEntry
    Nfo,
    Provider,        // TMDB / MusicBrainz (opt-in)
    Filename,
    ParentDir,
    Fallback,        // configured default — never justifies a rename
}

pub enum Confidence { Low, Medium, High }

pub enum Field<T> {
    Known { value: T, source: Source, confidence: Confidence },
    Unknown { attempted: Vec<Source> },
}
```

`Field` is an enum, not `Option<T>` plus a `Confidence::Unknown` variant — one representation of absence, and `Unknown` carries *what was tried*, which is the material for the "explain what could not be determined" requirement (§24 point 3).

**Source preference is a per-field table, not enum declaration order.** A single global ranking cannot be right: `ContainerHeader` must beat `Nfo` for pixel dimensions, while `Provider` must beat `Filename` for episode titles, and §10 separately requires that a provider never override an embedded tag. The table lives in config, is validated at load, and is the single place the §4.4 preference order is expressed.

`Source::Fallback` is enforced, not just documented: the router takes `Field<T>` and a *minimum* source rank, and a `Fallback`-sourced field can only produce `Readiness::NeedsReview`, never a `Move`.

### 2.2 Identity keys

```rust
pub struct MovieId  { title: Norm, year: Option<u16>, edition: Option<Edition> }
pub struct ShowId   { title: Norm, year: Option<u16> }
pub struct EpisodeId{ show: ShowId, season: u16, episodes: Vec<u16> }
pub struct AlbumId  { album_artist: Norm, album: Norm, year: Option<u16> }
pub struct TrackId  { album: AlbumId, disc: Option<u16>, track: Option<u16>, title: Option<String> }
```

`Edition` inside `MovieId` is how "Director's Cut is not a duplicate of Theatrical" (§4.5) becomes structural. `Vec<u16>` in `EpisodeId` is how multi-episode files (§6.5) are a first-class identity rather than a pair some later code might helpfully split.

**Keys must not be derived-`Eq` over optional fields, and grouping is two-pass.** If `ShowId.year` participates in equality, episodes whose filenames omit the year land in a second show directory. Likewise a MusicBrainz album id present on only some tracks splits an album. So:

- **Pass 1** — group on the *mandatory* discriminator only (normalised title / album+artist), collecting all candidate years and ids.
- **Pass 2** — resolve one canonical year/id per group by source rank then majority, write it back into every member, then key on the complete id.

MBIDs are a *pass-1 merge hint* (two title spellings sharing an MBID merge) and never part of the equality key. `Norm` is a newtype over the normalised form — NFC, case-folded, punctuation-collapsed, optional article stripping — carrying the display form alongside.

### 2.3 Classification

```rust
pub enum FileClass { Video, Audio, Subtitle, Artwork, Metadata, Unknown }
```

By extension, plus filename heuristics for artwork (`cover`, `folder`, `poster`, `fanart`, `backdrop`, `banner`, `album`, `front`, `thumb`) and metadata (`.nfo`, `.xml`, `.json`, `.cue`, `.m3u`). Extension sets are config-driven (spec §3.1–3.3). `Unknown` files are never moved and always reported (spec §3.4). There is no filename-based carve-out for the tool's own artifacts — interrupted reservations are identified through the journal instead (§6.7), because they bear ordinary media names.

### 2.4 Path layer

One module owns every platform path concern, because these rules interact and getting them wrong in scattered places is how libraries get corrupted.

**Sanitisation is unconditional**, and specifically *not* conditioned on whether the syscall path is verbatim:

- Replace `< > : " / \ | ? *` and control chars `0x00–0x1F` via a configurable map (`:` → ` -`, `?` → ``). `/` and `\` are **not** configurable — always replaced.
- Strip trailing dots and spaces.
- Reserved device names: `CON PRN AUX NUL CONIN$ CONOUT$ CLOCK$ COM0–COM9 LPT0–LPT9`. The rule is *the name up to the **first** period*, case-insensitively, applied **after** trailing dot/space stripping — so `CON.2010.mkv` and `CON .txt` are both reserved. Suffix with `_`.
- Component length caps are **per target**: 255 UTF-16 code units on NTFS/HFS+, 255 bytes on ext4/XFS/APFS. A 200-character CJK title is legal on NTFS but ~600 bytes; a global byte cap would needlessly mangle non-Latin titles. Truncate on a grapheme cluster boundary (`unicode-segmentation`).
- Cap total path length: Windows via the long-path route below; Linux `PATH_MAX` 4096.
- A name that sanitises to empty is a hard `NeedsReview`, never a fallback name.
- Normalise to NFC for storage and comparison.

**Long paths on Windows.** Rust's `std::fs` already converts to verbatim form internally for long paths, so the plan does **not** prepend `\\?\` to paths it stores. Doing so would (a) leak `\\?\` into the `Plan`, journal, GUI table and `insta` snapshots, (b) break the §5.6 containment comparison, and (c) *disable* the trailing-dot/space and reserved-name protections above — you would successfully create `Movie (2010) .mkv` and `CON.mkv`, which Explorer, robocopy and Jellyfin then cannot open. Verbatim conversion, if ever needed explicitly, happens at the syscall boundary inside `RealFs` and nowhere else.

**Case and normalisation sensitivity are queried, never probed by writing.** Writing a probe file into the user's library during planning would violate both "planning touches nothing" (§5) and "dry-run changes nothing" (spec §2.6), and fails on the read-only mounts the test suite specifically covers. Instead:

- Windows: the **per-directory** `FILE_CASE_SENSITIVE_INFORMATION` flag (Win10 1803+), defaulting to case-**insensitive** otherwise. Explicitly **not** `GetVolumeInformationW`'s `FILE_CASE_SENSITIVE_SEARCH`: that bit means the volume *supports* case-sensitive names, and NTFS reports it while Win32 path resolution is case-insensitive by default. Using it would classify every NTFS volume as case-sensitive and silently corrupt the §5.6 collision key — the exact failure this detection exists to prevent.
- macOS: `getattrlist` → `VOL_CAP_FORMAT_CASE_SENSITIVE`.
- Linux: `statfs` fstype, plus `pathconf(_PC_CASE_SENSITIVE)` where available.

**When detection fails, assume the conservative case: insensitive and normalisation-insensitive.** Unrecognised fstypes are common (FUSE, sshfs, overlayfs, any new NAS mount), and treating "unknown" as fatal would let one odd mount abort a 40 000-file run. The conservative assumption over-merges collision keys, which produces a spurious *conflict report* — safe and visible — rather than a missed collision, which is data loss. Detection failure is a `Warning` (§14).

**Unicode normalisation sensitivity is tracked alongside case, not instead of it.** APFS (default since 10.13) is normalisation-*insensitive* but normalisation-*preserving*: writing NFC is stable, so there is no rename loop — the earlier claim that NFD causes endless renames on modern macOS was wrong. The real hazard is the insensitivity, which is structurally identical to case-insensitivity: `Café (2010)` in NFC and in NFD are **one** file on APFS/HFS+ and **two** on ext4. So the volume descriptor is:

```rust
pub struct VolumeSemantics { case_sensitive: bool, normalisation_sensitive: bool, max_component: ComponentLimit }
```

and the §5.6 collision key is built from both flags. Getting only case right silently disagrees with the filesystem on any library with accented titles.

### 2.5 Filesystem abstraction

```rust
pub trait FileSystem: Send + Sync {
    fn metadata(&self, p: &Path) -> io::Result<FileMeta>;
    fn symlink_metadata(&self, p: &Path) -> io::Result<FileMeta>;
    fn read_link(&self, p: &Path) -> io::Result<PathBuf>;
    fn file_id(&self, p: &Path) -> io::Result<FileId>;      // (dev,ino) | GetFileInformationByHandleEx
    fn read_dir(&self, p: &Path) -> io::Result<ReadDirIter>; // streaming, not Vec
    fn is_dir_empty(&self, p: &Path) -> io::Result<bool>;
    fn volume_semantics(&self, p: &Path) -> io::Result<VolumeSemantics>;
    fn create_dir_all(&self, p: &Path) -> io::Result<()>;
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn create_new(&self, p: &Path) -> io::Result<Self::Handle>;   // atomic reservation
    fn copy_into(&self, from: &Path, h: &mut Self::Handle, cancel: &CancelToken) -> io::Result<u64>;
    fn sync_dir(&self, p: &Path) -> io::Result<()>;
    fn set_mtime(&self, p: &Path, t: SystemTime) -> io::Result<()>;
    fn remove_file(&self, p: &Path) -> io::Result<()>;
    fn remove_dir(&self, p: &Path) -> io::Result<()>;
    fn hash(&self, p: &Path, cancel: &CancelToken) -> io::Result<Hash>;
}
```

`symlink_metadata`/`read_link`/`file_id` exist because the §5.1 symlink policy is otherwise unimplementable against the trait. `read_dir` streams rather than returning a `Vec`, because the benchmark case is a flat 100 000-file directory. `set_mtime` exists because Jellyfin and Plex use mtime for "date added" and sort order — resetting it on every cross-device move is user-visible damage.

Implementations: `RealFs`, `MemFs` (fast unit tests), `FaultyFs<F>` (injects `PermissionDenied`, `ReadOnlyFilesystem`, `CrossesDevices`, or a hard failure at the *n*th call). `FaultyFs` is what makes spec §22.5 testable in CI.

#### `rename_no_replace` — the platform shim

This is the mechanism behind "never silently overwrite" (§11, §21), and it is *not* achievable with `std::fs::rename`. Precise statement of why: `std::fs::rename` clobbers an existing **file**; it does not clobber a **directory** (on Unix, and on Windows 10 1607+ via `FileRenameInfoEx`, a non-directory source cannot replace a directory target). File clobbering is the case that matters here, and it is unconditional.

| Platform | Mechanism | Caveats |
|---|---|---|
| Windows | `MoveFileExW` **without** `MOVEFILE_REPLACE_EXISTING` | Fails if target exists, with the check in the kernel — no *userspace* TOCTOU. **Not documented as atomic**; do not claim more. Works on NTFS **and FAT/exFAT**. Without `MOVEFILE_COPY_ALLOWED`, a cross-volume move returns `ERROR_NOT_SAME_DEVICE`, which feeds the copy path. Note std now dispatches to `MoveFileExW` *or* `SetFileInformationByHandle`, so a hand-rolled shim tracks a moving target. |
| Linux | `renameat2(RENAME_NOREPLACE)` | Works on ext4/XFS/btrfs **and on vfat, exfat and cifs** — the VFS enforces the no-replace check itself. Fallback must key on `EINVAL`, `EOPNOTSUPP` **and** `ENOSYS` (old kernels/glibc need the raw syscall). |
| Linux fallback | `link` + `unlink` | `link` returns `EEXIST` on an occupied target — correct semantics. Cannot target **directories** (`EPERM`), and unavailable on FAT/exFAT. Rarely reached given the row above. |
| macOS | `renamex_np(RENAME_EXCL)` | Returns `ENOTSUP` unless the volume advertises `VOL_CAP_INT_RENAME_EXCL`. **SMB, NFS and FAT volumes on macOS have no no-replace rename at all.** |
| Any | **`create_new` reservation** (below) | `O_CREAT\|O_EXCL` / `CREATE_NEW`, atomic exclusive-create. Used as the cross-device path always, and as the no-replace path where the rows above fail. |

**Correcting the premise.** An earlier review round asserted — and this document repeated — that FAT32/exFAT and SMB have no no-replace rename primitive at all, making them "the most significant unfixable gap". That is **false on Linux and Windows**: `renameat2(RENAME_NOREPLACE)` is enforced by the Linux VFS and works on vfat, exfat and cifs, and `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` works on FAT and exFAT. The claim is true only on **macOS**, where `renamex_np` returns `ENOTSUP` on SMB/NFS/FAT.

So the honest scope of the problem is much narrower, and the *reason* to prefer reservation on network volumes is different from — and better than — a capability gap:

> On cifs and nfs the `RENAME_NOREPLACE` check is evaluated **client-side against a possibly-stale dentry**, whereas `O_EXCL`/`CREATE_NEW` is resolved **server-side**. On a share with another writer, the rename check can pass on stale cache and still clobber; the exclusive create cannot.

`OpenOptions::new().create_new(true)` compiles to `O_CREAT|O_EXCL` on Unix and `CREATE_NEW` on Windows. std documents it as atomic, and it rejects even a dangling symlink at the target. On NFS it is reliable for **NFSv3 and later** (`O_EXCL` was racy on NFSv2, which is long dead and out of scope). This is the strategy:

```text
reserve:  create_new(dest) -> File     ← atomic; AlreadyExists if occupied
write:    stream source → the handle   ← into the handle, never re-opened by path
durable:  fsync(file), fsync(dest_dir)
verify:   size (and hash if configured)
finish:   preserve mtime, then remove_file(source)
```

**Strategy selection** (§7 `moves.no_replace_strategy`):

| Strategy | When | Guarantee | Cost |
|---|---|---|---|
| `native` | Default wherever a rename primitive works — which is Linux and Windows on every filesystem in scope, and macOS on APFS/HFS+ | Kernel-side no-replace | Instant metadata rename |
| `reserve` | Always for cross-device moves; default on **macOS SMB/NFS/FAT**; default on **any network volume** for the stale-dentry reason above | Server-side atomic exclusive create | Full copy even same-volume |
| `check_then_rename` | Opt-in only | Racy window | Instant |

`reserve` is therefore not the everywhere-default the previous draft implied — it is the cross-device path plus a narrow, well-argued set of volumes. That matters for the roadmap: a Linux FAT loopback selects `native`, not `reserve`, so §11/§12 must test selection against a macOS SMB mount or a forced override rather than a FAT image.

**Two consequences that need stating.**

*The §6.4 resume table has six states, not four*, because destination *presence* no longer implies completion under `reserve` — size must match, and the mismatch cases need their own classifications.

*Reservation must be handle-based, not path-based.* The trait therefore exposes `create_new(&Path) -> io::Result<File>` and a copy that writes **into that handle**. If the copy re-opened the destination by path (as `fs::copy` would, with create+truncate), a TOCTOU window would reappear between reservation and write — reintroducing exactly the race the strategy exists to close. This is also what lets `FaultyFs` inject `AlreadyExists` at the reservation point.

*`AlreadyExists` is not the only occupied-target error.* On Unix, `O_EXCL` against an existing **directory** gives `EEXIST` → `AlreadyExists`. On Windows, `CREATE_NEW` against an existing directory gives `ERROR_ACCESS_DENIED` → `PermissionDenied`. The shim normalises both to a single `DestinationOccupied` error, or the destination-is-a-directory case is misreported as a permissions failure on Windows and routed to `Failure` instead of `Conflict` (§14).

`CrossesDevices` is a stable `io::ErrorKind` since **1.85** (`ReadOnlyFilesystem` and `StorageFull` since 1.83; `InvalidFilename`, which §14 needs for path-too-long, since 1.87). The `mm-core` MSRV floor is therefore 1.87, not lower.

---

## 3. Filename parser (`mm-parse`)

The parser is the highest-risk component and the one with the best return on test investment.

### 3.1 Approach: template-aware span consumption

A monolithic regex per naming convention does not scale. Instead:

1. **Normalise**: strip extension; unify `.`/`_`/`+` separators to spaces; collapse whitespace; note (but keep) bracketed and parenthesised spans; detect and strip a trailing release-group token (`-RARBG`, `-NTb`, `[YTS.MX]`).
2. **Consume tags**: run extractors in priority order over the token stream. Each claims a *span* and records a `Field`. Claimed spans leave the residual.
3. **Assign residuals positionally.**

Extractor order (order is semantically significant):

`SubtitleFlags → EpisodeMarkers → Resolution → Source → Codec → AudioFormat → HdrFormat → Edition → Language → Year → DiscNumber → TrackNumber → ReleaseGroup`

Episode markers run **before** year extraction so `1x01` is never read as a year. Span consumption is what makes this correct: by the time the year extractor runs, `1080p` and `S01E01` are gone from the stream.

**Step 3 is not "the leading residual is the title".** That was wrong and it broke the most important case. Consider `Show (2011) - S01E01 - Winter Is Coming - 1080p.mkv`: consuming `S01E01` and `1080p` leaves **two** residual runs — `Show (2011)` and `Winter Is Coming`. Taking the leading run discards the episode title, `[ - {episode_title}]` is omitted on re-render, and the file renames on **every run**. The same failure hits music `{title}` and movie `[ - {edition}]`.

So residual assignment is **positional and template-aware**: the parser is told the shape it is reading against (or tries each configured shape and scores), and residual runs are assigned to named slots by their position relative to the consumed anchors — text before the first episode anchor is the show title, text between the episode anchor and the next consumed tag is the episode title. Ambiguous multi-residual layouts that no shape explains yield `NeedsReview`. This is a materially larger commitment than a single title slot, and it is scheduled explicitly in Phase 1/Phase 5 rather than assumed.

### 3.2 Tricky cases and their rules

| Case | Rule |
|---|---|
| Year in title (`Blade Runner 2049 (2017)`) | Parenthesised/bracketed year wins over bare. Otherwise take the **rightmost** bare year in `1888..=current+2`; if consuming it would empty the title, back off and leave the year `Unknown`. |
| Multi-episode | `S01E01E02`, `S01E01-E02`, `S01E01-S01E02`, `S01E01 & S01E02`, `1x01x02`, `E01-E03`. Emit `episodes: vec![1,2]`. Ranges expand only within one season; a cross-season range is a diagnostic, not a guess. |
| Specials | `S00Exx`, `Special NN`, `SP NN`, `OVA NN`, and title-only forms (`Christmas Special`). Title-only forms get `season: 0`, episode `Unknown` → **NeedsReview**, never an invented number. |
| Anime absolute numbering (`Show - 137`) | Recognised but ambiguous by design; reported unless `--absolute-numbering` is given. |
| Movie vs episode ambiguity | If both interpretations score above threshold → `Ambiguous`, untouched and reported (§24). |
| Non-UTF-8 name | Parse the lossy form; cap confidence at `Low`; require confirmation. |
| Already-organised name | Must round-trip at the **field** level. See §3.3. |

### 3.3 The round-trip law — stated over fields, not strings

The original formulation was `render(parse(render(parse(n)))) == render(parse(n))`. That law is too weak to be worth much, and here is the counterexample that shows it. Take the music template `{track:02} - [{track_artist} - ]{title}` with `track=1, track_artist="Nina Simone", title="Feeling Good"`, rendering `01 - Nina Simone - Feeling Good.mp3`. Re-parsing cannot distinguish that from a track *titled* `Nina Simone - Feeling Good` with no track artist. The parser picks one reading and re-renders the **same string** — so the string law **passes while the fields are wrong**.

The law that actually protects the library:

```rust
// for all field sets F that the router considers Ready:
parse(render(F)).fields_relevant_to_naming() == F.fields_relevant_to_naming()
```

Generated by `proptest` over field sets (not over strings), for every configured template. Any template that fails is rejected **at config load**, not at runtime.

The `{track_artist}` template above does not satisfy the law and never can, because ` - ` is both the separator and a common substring of real titles. **Decided now rather than deferred to Phase 6**, because the resolution changes the default config:

**`{track_artist}` is removed from the default music file template.** The spec's §8.6 example shows `01 - Artist A - Song.flac`, but its actual requirement is that "individual track artists should be preserved" — and they already are, in the embedded tags, which is where Jellyfin and Plex read them from. Encoding the track artist into the filename is redundant for the media server and destroys invertibility for us. So the default is `[{track:02} - ]{title}` and compilations get their per-track artists from tags.

The prefix remains available as an opt-in (`naming.music.compilation_prefix = true`) with two consequences stated in the config docs: the template is flagged non-invertible at load, and items rendered through it are excluded from string-based `NoOp` detection.

Which raises the general rule this case exposes. For music, §8.4 already requires preferring embedded tags, so **filename parsing is a fallback** — and the ambiguity only bites when tags are absent *and* the file already looks like `N - A - B`. In that specific case the parser cannot tell artist from title, so the item is `NeedsReview`. It does not guess, and it does not rename. That is §24 applied to the one case where the naming grammar is genuinely lossy.

### 3.4 Non-panic guarantee

`proptest` over arbitrary `String` (control characters, 10 KB names, RTL text, emoji, combining marks) asserting only: *does not panic, terminates*. Backs spec §2.8.

### 3.5 Extensibility

Tag vocabularies (`x264`, `HEVC`, `Atmos`, `REMUX`, `IMAX`, …) live in TOML compiled in via `include_str!` and overridable from user config. Adding a release tag needs no code change (§4.4). Adding a *pattern* means one `Extractor` impl and a registration — no engine change.

---

## 4. Probing (`mm-probe`)

```rust
pub trait Prober: Send + Sync {
    fn supports(&self, ext: &str) -> bool;
    fn probe(&self, p: &Path) -> Result<Probe>;
}

pub struct Probe {
    pub video: Option<VideoInfo>,   // pixel + display dimensions, codec
    pub audio: Option<AudioTags>,   // artist, album_artist, album, year, track, disc, title, genre, compilation, mbids
    pub duration: Option<Duration>,
    pub subtitle_tracks: Vec<SubtitleTrackInfo>,
}
```

- **MKV / WebM** → `matroska`: `Video { pixel_width, pixel_height, display_width, display_height, .. }`.
- **MP4 / M4V / MOV** → `re_mp4`: `tkhd` dimensions cross-checked against sample description.
- **Audio** → `lofty`: uniform tags across ID3v2, Vorbis comments, MP4 `ilst`, APE, WMA.
- **AVI / WMV / TS / M2TS** → no pure-Rust prober planned. Falls back to filename with `Source::Filename`, and diagnostics record that container-based detection was unavailable (§4.3). An optional `ffprobe-fallback` feature can be added later without touching the trait.

**`lofty` is a read/write tag library.** It does not enforce read-only; it simply does not write unless `save_to` is called. So spec §8.4 ("embedded metadata must not be modified") is guaranteed by *our* code never calling a write API, and verified by the §11 hash-before/after test — not by a library property. `save_to` is banned by a `clippy.toml` disallowed-methods entry so the guarantee is mechanical.

**HDR cannot come from the container with these crates.** Verified: `matroska` exposes no `Colour`/`MasteringMetadata`, and `re_mp4`/`mp4` expose no `colr`/`mdcv`. So `HdrFormat` is a **filename-only** signal, `Source::Filename` at best, and it is used for edition/version disambiguation only — never as a claim about the stream. Recording this now avoids a traceability row that overstates what Phase 4 can deliver.

### Resolution labelling

The banding must key on **`max(width, height)` after display-aspect correction**, not on height. Banding on height mislabels every scope-ratio encode: 1920×800 is a 1080p release, and a height table sends it to 720p. 3840×1600 would land on 1440p. A large fraction of a real movie library is 2.40:1.

| `max(w,h)` after DAR correction | Label |
|---|---|
| ≥ 7000 | 4320p |
| ≥ 3000 | 2160p |
| ≥ 2200 | 1440p |
| ≥ 1700 | 1080p |
| ≥ 1100 | 720p |
| ≥ 900 | 576p |
| ≥ 700 | 480p |
| else | reported as unknown |

Display dimensions (`DisplayWidth`/`DisplayHeight`, or `tkhd` vs sample dimensions) feed the band where present, pixel dimensions otherwise — this matters for DVD rips, where 720×576 pixel and 1024×576 display describe the same file. The exact thresholds get calibrated against a real library during Phase 4 and live in config.

Probing is the most expensive per-file step, so it is parallelised, run **after provisional grouping and before resolution** — stage 5 of §5's pipeline, not eagerly — and cached. (The ordering is: group provisionally on filename-derived identity, probe only the files that could become a `Move`, then resolve fields with probe data in hand, then re-key. §5 explains why this split is necessary rather than circular.) **The cache is keyed on `(file_id, size, mtime)`, not on path** — a path key misses on every file after the first run moves it, which would make every subsequent run re-probe the entire library and quietly destroy the performance story.

---

## 5. Pipeline (`mm-engine`)

Implements spec §12. All eleven stages are pure planning and touch nothing; only execution (§6) writes.

```text
 1 scan
 2 classify
 3 parse            ← filename only, no I/O
 4 group            ← provisional: mandatory discriminators only (§2.2 pass 1)
 5 probe            ← only files in a group that could become a Move
 6 resolve          ← merge tag/container/filename candidates per §2.1 table
 7 regroup          ← canonical: resolved year/id written back, full key (§2.2 pass 2)
 8 associate        ← subtitles, artwork, metadata sidecars
 9 route            ← templates → destination paths → sanitise
10 validate         ← containment, length, reserved names
11 reconcile        ← intra-plan collisions, existing-file conflicts, duplicates
                    ▼
                  Plan ──┬──▶ render (dry-run / GUI preview / --json)
                         └──▶ execute
```

Three orderings here are load-bearing and were wrong or missing in the first draft:

- **`probe` is stage 5, not stage 3.** Probing is the most expensive per-file step, so it must run *after* provisional grouping — otherwise the tool probes every file that will end up `Unknown` or `NeedsReview`, which on a messy library is a large fraction of it.
- **Grouping is split around `probe`** (stages 4 and 7). This is the honest resolution of an apparent circularity: grouping needs resolved fields, but resolution needs probe data, and probe should only run on files that matter. So group provisionally on mandatory discriminators, probe, resolve, then re-key on the complete identity. Collapsing this into one stage is what forced the earlier draft into a contradiction between §5's stage list and §4's "only probe files that reached grouping".
- **`associate` is a stage.** Subtitle/artwork/sidecar association needs completed groups. Previously it was described in §5.4 but appeared nowhere in the pipeline, so it was unscheduled work.

`reconcile` produces `Conflict` **and** `Duplicate` outcomes, which are different findings (§5.3) — this is the stage that consumes `blake3`.

### 5.0 What "in place" actually means

Spec §2.3 says organisation happens inside the supplied directory and no library-root directory is created. That is clear as far as it goes, but it leaves three situations the traceability table was quietly glossing over.

**Root is itself a level of the target hierarchy.** The user passes `D:\Media\Movies\The Matrix (1999)` (a movie folder), or `D:\TV\Breaking Bad` (a show folder), or `D:\Music\Nina Simone` (an artist folder). Naively the tool nests: `Breaking Bad/Breaking Bad/Season 01/`. The general rule, which covers all three:

> Compute the destination path for each item. Then find the **longest prefix of that path which root already satisfies**, and strip it. Root is never a destination-directory candidate, and never a component the tool creates.

So with root = `Breaking Bad`, an episode's full destination `Breaking Bad (2008)/Season 01/…` has its first component satisfied by root, leaving `Season 01/…` to create inside it. With root = `Nina Simone`, `Nina Simone/Album (1965)/…` leaves `Album (1965)/…`. With root = `The Matrix (1999)`, the whole directory portion is satisfied and only the file is renamed.

Prefix satisfaction is matched under `VolumeSemantics` (§2.4), so `breaking bad` satisfies `Breaking Bad (2008)` on a case-insensitive volume — and the missing year in root's name does not defeat the match, because comparison is on the normalised title component, not the rendered string. **Root's own name is never rewritten**: renaming the directory the user pointed at is out of scope, and is surfaced as a suggestion in diagnostics instead.

This generalises what an earlier draft handled only for the single-movie case, whose guard ("root parses as one entity *and* contains only that entity's files") failed for show and artist directories — which are the more common way users invoke this.

**Root contains a half-organised subtree.** Common after an interrupted run or a partial manual tidy-up: some movies in correct folders, some loose. Already-correct files produce `NoOp`, loose files produce `Move`, and a correct folder is not disturbed merely because this run did not create it. Same code path as idempotency — which is why §11's "starts organised" fixture suite is the test that matters, not a special case.

**A file's destination is its own current directory, renamed.** `RandomFolder/Movie.2020.mkv` → `Movie (2020)/Movie (2020).mkv` could in principle be done by renaming `RandomFolder`. The tool does **not** do this: it creates the destination, moves the file, then removes the emptied source in phase 4. Renaming an arbitrary user directory to a computed name risks capturing unrelated sibling files that happen to live in it — a strictly worse failure mode than an extra move.

Directory renames (`dir_renames`, §5.7) are therefore restricted to directories **whose existing names already match the tool's naming scheme** and need only a case or normalisation fix — `season 01` → `Season 01`. Note this is deliberately *not* "directories the tool itself created": §11's idempotency suite (b) runs against trees this tool has never touched, and fixing their casing is exactly the capability spec §19 requires.

### 5.1 Scan

`walkdir` from root. Symlink policy from config, and the `walkdir` setting follows it (`follow_links(true)` for `follow` — a hardcoded `false` cannot implement a `follow` policy):

- `skip` (default) — reported, never traversed, never a destination.
- `follow` — `walkdir`'s own loop detection plus a `FileId` visited set (`(dev,ino)` on Unix, `GetFileInformationByHandleEx` on Windows, since there is no `ino` there).
- `treat_as_file`.

Ignore patterns (`.git`, `@eaDir`, `#recycle`, `.Trash*`, `lost+found`, `*.part`, `*.!qB`, `*.tmp`) are configurable — NAS and download-client noise is universal in real libraries.

Every path is stored **relative to root** alongside the absolute form. This makes plans and journals portable and snapshot-stable; it does **not** by itself guarantee containment, which is still checked explicitly in §5.6.

### 5.2 Resolve

Merge candidates per field via the per-field source-preference table (§2.1). Produce a per-item `Readiness`:

```rust
pub enum Readiness {
    Ready,
    NeedsReview { missing: Vec<FieldName>, reasons: Vec<String> },
    Ambiguous { interpretations: Vec<Interpretation> },
}
```

Required fields per media kind are config-declared, and are the fields whose *absence makes the destination meaningless* — not merely fields the template mentions:

| Kind | Required | Notes |
|---|---|---|
| Movies | `title` (Medium+), `year` | `year` by policy (`require_year_for_movies`) |
| TV | `title`, `season`, `episodes` | `year` optional (`require_year_for_tv = false`) |
| Music | `album_artist`, `album`, `title` | `track` **not** required |

`track` is deliberately not required. An earlier draft required it "because the file template leads with `{track:02}` and would otherwise render a leading separator" — but that justification died when §5.5 bracketed the placeholder as `[{track:02} - ]`. A track without a number still has a well-defined destination (`Album (1965)/Feeling Good.flac`), so refusing to organise it would be exactly the over-cautious behaviour that leaves libraries half-sorted for no safety benefit. The bracket handles it.

Note the field is `title` throughout, disambiguated by context rather than by name — `{title}` means movie title under `[naming.movies]` and track title under `[naming.music]`. The config-load placeholder whitelist is **scoped per template section**, so `{episode_title}` in a music template is a startup error.

Anything short of the requirement never becomes a move.

### 5.3 Group

Two-pass per §2.2, then:

- **Movies** — group by resolved `MovieId`. Multiple videos sharing an id are *versions*, not duplicates.
- **TV** — `ShowId` → season → `EpisodeId`.
- **Music** — `AlbumId`, with MBID as a pass-1 merge hint. Compilation detected from the `compilation`/`TCMP` flag, an album artist in a configurable "Various Artists" list, or ≥ *N* distinct track artists in one album key (§8.6).

**Version disambiguation (spec §4.5) needs more than resolution.** `Edition` in `MovieId` only separates edition-vs-edition. Two rips of the same movie at the same resolution but different source (`BluRay` vs `WEB-DL`) or codec (`x264` vs `x265`) would render identical destinations and be reported as a collision — i.e. treated as duplicates, which §4.5 forbids. The same applies to two AVI/WMV files where §4 yields no resolution at all, and to TV episodes. So the file template carries an **escalating discriminator chain**, appended only as far as needed to make destinations unique within a group:

```text
[ - {edition}][ - {resolution}][ - {hdr}][ - {source}][ - {video_codec}][ - {audio_format}]
```

If the chain is exhausted and destinations still collide, *then* it is a genuine duplicate and becomes a `Duplicate` outcome. Which brings up: **duplicate detection is its own stage output**, not a synonym for `Conflict`. `blake3` is listed for content comparison and there must be something that consumes it — a real duplicate (same bytes, same identity) and a distinct version (different bytes, same rendered name) are different findings and must not produce the same plan item.

### 5.4 Association — hard gate, then score

Spec §5.3 and §7 are explicit that proximity is not enough. Purely additive scoring was too coarse: `subs` dir (25) plus sole-video (15) cleared a 40 threshold with **zero** name evidence, and stem-prefix (35) plus sole-video (15) exactly tied an exact episode-id match. So:

**Gate** — a subtitle may only be associated if at least one *name-evidence* signal is present: a matching parsed episode id, a matching parsed title+year, or a stem prefix matching the video after normalisation. Location signals alone can never associate.

**Score** — among gate-passing candidates, rank by evidence strength (episode id > title+year > stem prefix), breaking ties with location (sibling > `subs`/`Subs`/`Subtitles` sibling > elsewhere). A tie that survives both is an orphan.

Orphans are untouched and reported. Language extraction (`en`/`eng`/`English`, `fr`/`fre`/`fra`/`French`, …) normalises to ISO 639-1 via a compiled-in table covering 639-2/B, 639-2/T, English names and endonyms; failure yields `und`, never a guess (§5). `forced` and `sdh`/`hi`/`cc` become a flag set rendered as `.en.forced.srt` / `.en.sdh.srt`.

**Artwork and metadata sidecars route here too**, and they need their own templates — previously missing entirely, which meant `poster.jpg`, `fanart.jpg`, `movie.nfo`, `cover.jpg` and `.cue` had no computed destination. Left behind, they also permanently block §6.6 cleanup, since removal requires a genuinely empty directory.

### 5.5 Route — destination computation

```toml
[naming.movies]
dir       = "{title}[ ({year})]"
file      = "{title}[ ({year})]{discriminators}"
subs_dir  = "subs"
sub_file  = "{title}[ ({year})].{language}[.{flags}]"
artwork   = "poster"                    # extension preserved
nfo       = "{title}[ ({year})]"

[naming.tv]
show_dir     = "{title}[ ({year})]"
season_dir   = "Season {season:02}"
specials_dir = "Specials"
file         = "{title}[ ({year})] - {episode_code}[ - {episode_title}]{discriminators}"
sub_file     = "{title}[ ({year})] - {episode_code}[ - {episode_title}].{language}[.{flags}]"

[naming.music]
artist_dir = "{album_artist}"
album_dir  = "{album}[ ({year})]"
disc_dir   = "CD {disc}"                # only when the album spans multiple discs
file       = "[{track:02} - ]{title}"   # track_artist deliberately absent — see §3.3
artwork    = "cover"
compilation_prefix = false              # true adds "[{track_artist} - ]"; non-invertible
```

Every optional placeholder is bracketed — including `{year}` and `{track}`, which were previously bare and would render `Title ().mkv` and ` - Title.mp3` whenever the optional field was absent. `[...]` omits the whole segment when its placeholders are empty. **Config-load validation rejects an unbracketed placeholder bound to an optional field**, so this class of bug is a startup error rather than a corrupted library. Validation also runs the §3.3 round-trip law against each template and rejects templates that fail it.

`{discriminators}` expands to the escalating chain from §5.3.

**Multi-disc collision.** `disc_dir` applies only when the album spans multiple discs, so a two-disc album where some tracks lack `discnumber` collapses into one directory and `01 - X.flac` collides with `01 - Y.flac`. This surfaces as a conflict rather than data loss, but it is avoidable: if any track in an album has a disc number and any other does not, the album is `NeedsReview` rather than partially organised.

Then sanitisation and length capping per §2.4.

### 5.6 Validate and detect conflicts

- **Containment.** Canonicalise the deepest **existing** ancestor and compare **component-wise** — a string prefix compare accepts `/media/Films2` for root `/media/Films`. On Windows, `canonicalize` returns a verbatim path, so both sides must be brought to the same form before comparing (another reason `\\?\` cannot be a leaf-level detail). Under `symlinks = "follow"`, a logical path inside root can canonicalise **outside** root; the check is performed on the canonical form, and cleanup (§6.6) uses the same form.
- **Intra-plan collisions.** Bucket items by destination key; any bucket with more than one `Ready` item collides. The key is built from `VolumeSemantics` — both case and normalisation sensitivity (§2.4).
- **Existing-file conflicts.**

```rust
pub enum ConflictPolicy {
    Report,              // default: plan a Conflict item, change nothing
    Skip,
    SkipIfIdentical,     // size, then blake3 — only then skip
    RenameNew,           // " (2)", " (3)" — see below
    Replace,             // requires config setting AND explicit CLI flag
}
```

Default is `Report` (§11). `Replace` needs both config and flag, because a config file edited months ago should not silently authorise overwrites today.

`RenameNew` must be **recognised on re-parse**: the ` (N)` suffix is part of the naming grammar, so `Movie (2010) (2).mkv` parses back to `Movie`, 2010, copy 2 and renders identically. Without that it re-renders to `Movie (2010).mkv`, re-collides, and gets ` (2)` re-applied every run — converging by luck rather than by design, and failing the §3.3 law.

- **Case-only and normalisation-only renames** need a two-step rename through a temporary name, or the OS treats them as no-ops and the library is never fixed. This applies to **directories** as well as files, which is why `Plan` carries `dir_renames` alongside `items` (§5.7).

  These are **always executed as renames, never as reservations**, regardless of the volume's `no_replace_strategy`. Under `reserve` the operation is otherwise impossible: on a case-insensitive volume `create_new("MOVIE.mkv")` when `movie.mkv` exists returns an occupied-target error, so the fix would be misclassified as a conflict and the library never corrected. Since source and target are the *same file*, exclusivity is not the property needed here — this is a pure metadata operation and safe to rename even where a no-replace primitive is absent. The two-step temp name is what provides the safety.

- **`Replace` and `SkipIfIdentical` under `reserve`.** Both need defining, because `create_new` fails before any comparison is possible. `SkipIfIdentical` compares first (via `metadata`/`hash`) and skips without ever reserving. `Replace` reserves a sibling temp name, copies, verifies, then does a **replacing** rename over the target — the one place the tool deliberately uses replace semantics, reachable only with both the config setting and the CLI flag.

### 5.7 Plan

```rust
#[derive(Serialize, Deserialize)]
pub struct Plan {
    pub version: u32,
    pub run_id: Uuid,
    pub root: PathBuf,
    pub kind: MediaKind,
    pub config_digest: String,
    pub volume: VolumeSemantics,
    pub items: Vec<PlanItem>,          // PlanItem { id: ItemId, action: Action, .. }
    pub dir_creates: BTreeSet<PathBuf>,
    pub dir_renames: Vec<DirRename>,   // id-bearing; case/normalisation fixes
    pub dir_removals: Vec<DirRemoval>, // id-bearing, deepest-first
    pub diagnostics: Vec<Diagnostic>,
    pub stats: PlanStats,
}

#[derive(Serialize, Deserialize)]
pub enum Action {
    NoOp,
    Move { from: PathBuf, to: PathBuf },
    Skip { reason: SkipReason },
    Conflict { from: PathBuf, to: PathBuf, existing: ExistingInfo },
    Duplicate { from: PathBuf, identical_to: PathBuf },
    NeedsReview { path: PathBuf, missing: Vec<FieldName> },
}
```

`dir_renames` was missing before, which meant an already-organised library containing `season 01` instead of `Season 01` could never be corrected — a real §19/§21 gap, not a nit. `dir_removals` and `dir_renames` carry ids so the GUI can deselect them individually (§9). `config_digest` and `run_id` are what make resume sound (§6.4).

---

## 6. Execution (`mm-engine::exec`)

### 6.1 Phase structure

Because organisation is in place, destinations are created *inside* sources and one item's source can sit under another item's destination. Partitioning by destination directory creates no dependency edges, so execution is phased, each phase a barrier:

1. **Create directories** — serial pass over `dir_creates`, shallowest-first.
2. **Move files** — parallel, partitioned by destination directory.
3. **Rename directories** — serial, two-step where case/normalisation-only.
4. **Remove empty directories** — serial, deepest-first.
5. **Reclaim** — delete this run's unfinished reservations (§6.7).

Phases 1, 3, 4 and 5 are serial and cheap; phase 2 carries essentially all the work and all the parallelism. This is simpler than a topological dependency graph and sufficient, because the only cross-item dependencies run *between* these categories, not within them.

Directory creation being serial also removes a race the earlier design asserted away: `Show (2011)/Season 01` and `Show (2011)/Season 02` have different owners but a shared parent, and concurrent `create_dir_all` of `Show (2011)` on Windows can transiently return `ERROR_ACCESS_DENIED` or a sharing violation. Serial creation plus retry-on-transient makes it a non-issue.

### 6.2 Per-file move order (spec §13, non-negotiable)

Two strategies, selected per volume by §2.5. Both obey the same invariant.

**`native` — the volume has a no-replace rename primitive:**

1. Re-validate destination — the world may have changed since planning.
2. Ensure destination directory exists (idempotent; normally created in phase 1).
3. `rename_no_replace(from, to)`.
4. On `CrossesDevices`, fall through to `reserve` below.
5. Verify destination exists and size matches (optionally hash).
6. Record the source directory as a cleanup candidate for phase 4.

**`reserve` — no rename primitive, or cross-device:**

1. Re-validate destination; free-space precheck.
2. Ensure destination directory exists.
3. `create_new(dest)` — atomic reservation; `AlreadyExists` means conflict, handled by policy.
4. `copy_into(source, &mut handle)` — writes into the reserved handle, never re-opening by path.
5. `fsync` the file, then `fsync` the destination **directory**.
6. **Verify** size (and hash if configured); preserve mtime.
7. **Only now** `remove_file(source)`.
8. Record the source directory as a cleanup candidate for phase 4.

The invariant both paths share: **a source file is only ever removed after its destination has been verified.** The earlier draft's cross-device path unlinked the source at step 4 and verified at step 5, contradicting the guarantee stated one paragraph below it.

The directory `fsync` matters more than it looks: without it the rename (or the reservation) can be lost on power failure while the file data survives, which is precisely the state the journal cannot distinguish from "never started" — undermining the whole recovery argument.

`reserve` writes to the **final destination name**, not to a `.mm-part-*` temp. That is deliberate — the reservation *is* the exclusivity guarantee, and renaming in from a temp would reintroduce the rename semantics the strategy exists to avoid.

**There is no `.mm-part-*` mechanism anywhere in this design.** An earlier draft had one for the cross-device path, then made that path fall through to `reserve`, which left the temp-file name unreachable while five other sections still depended on it — a dead mechanism with live references. Removing it leaves one artifact class to account for: a **short file at a real destination name**, created by an interrupted reservation. It is not recognisable by name, so it cannot be garbage-collected by name, and §6.6 handles it through the journal instead.

### 6.3 Parallelism, and where the races actually are

Within phase 2, group `Move` items by destination directory; each group is one unit of work executed sequentially on a `rayon` pool sized by config:

```toml
[concurrency]
workers = "auto"        # auto = min(cpus, 8); capped at 4 when root looks like a network path
probe_workers = "auto"
hash_workers = "auto"
```

Destination-directory ownership removes in-directory filename collisions and conflicting moves. It does **not** remove all shared state, and the earlier claim that it made races "impossible by construction rather than by locking" was wrong on two counts:

- **Source-directory bookkeeping is shared.** One source directory routinely feeds many destinations — a flat `Downloads/` of 500 movies feeds 500 destination directories. Step 6 above therefore has N workers mutating one map keyed by source path. That is a `Mutex<HashMap<PathBuf, _>>`, it is unavoidable, and contention is low enough that a plain mutex beats a concurrent map for reasoning value.
- **The cleanup predicate cannot be evaluated concurrently.** If two workers are draining the same source directory, whichever checks `is_dir_empty` first sees the other's not-yet-moved files and skips removal — nondeterministic leftovers, while the §11 concurrency test asserts a deterministic outcome. This is precisely why cleanup is a **serial phase-4 barrier** and why phase 2 only *records candidates* rather than deciding.

Network roots (UNC prefix, or mount-type check on Unix) get a lower default width; parallelism on SMB/NFS usually hurts.

### 6.4 Journal, and resume that actually works (spec §14, §18)

Append-only JSONL under the user's data dir, written **twice per operation**: `intent` before, `outcome` after. Every line carries `run_id`, `root` and `config_digest`:

```json
{"ts":"2026-08-19T10:31:02.114Z","run_id":"0192...","root":"D:\\Media\\Films","config_digest":"b1f4...","seq":41,"phase":"intent","op":"MOVE","from":"Inception.2010.1080p.mkv","to":"Inception (2010)/Inception (2010) - 1080p.mkv"}
{"ts":"2026-08-19T10:31:03.902Z","run_id":"0192...","seq":41,"phase":"outcome","status":"SUCCESS","bytes":8412300}
```

Without those three fields resume is not implementable: the journal is one file shared across every root ever organised, so a dangling `intent` could not be attributed to a root, a plan, or a config. The `Plan` is also persisted next to the journal under `run_id`, so resume replays the original plan rather than re-deriving destinations under possibly-changed config.

Resume semantics, spelled out because the interesting case looks like a failure:

| Source | Destination | Meaning |
|---|---|---|
| missing | present, size matches | **Success** — the move completed before the crash. Record outcome, continue. Not a failure. |
| present | absent | Not started, or the reservation was never made. Re-execute. |
| present | present, size **mismatches** | Interrupted `reserve` copy. The destination is this run's own partial write: truncate and re-copy, or delete and re-execute. Safe because the source is still intact. |
| present | present, size matches | Copy finished but the source removal did not. Re-verify (hash if configured), then remove the source. |
| missing | present, size **mismatches** | **Ambiguous** — either a pre-existing wrong-size file whose source was removed out-of-band, or a reservation whose source vanished. The tool cannot distinguish these, so it does nothing: reported as a `Warning` requiring manual resolution. Never truncated, never deleted. |
| missing | absent | Genuine loss — `Fatal`, reported prominently, run halts. |

Six states, not the four an earlier draft listed: source × destination is `{present, missing}` × `{absent, present-matching, present-mismatching}`. The fifth row above is the one that was missing, and it is the only cell where the tool has no safe action — which is precisely why it must be enumerated rather than fall through a match arm.

Under `reserve`, **destination presence alone is never proof of completion** — size must match. This is the one place the reservation strategy costs something in complexity, and it is why the journal `intent` must be durable before the reservation is made rather than after.

A destination that is present with a mismatching size and *no* matching `intent` in the journal is **not** ours: it is treated as a pre-existing file and handed to `ConflictPolicy`. Resume never truncates a file it cannot prove it created.

An `intent` with no matching `outcome` is exactly the set to re-verify, which is why the write ordering matters. This one file underwrites logging (spec §14), resume (spec §18), reservation reclaim (§6.7) and a future `undo` (spec §23).

> **Numbering convention.** From here on, `spec §N` refers to the requirements document and bare `§N` to this plan. The two numbering schemes collide (this plan's §14 is the error taxonomy; the spec's §14 is logging), and the collision became actively misleading once this document grew its own §14 and §15.

**Cost.** Two `fsync`s per file is 200 000 `fsync`s on the Phase 7 100 000-file benchmark, and brutal on the SMB/NFS mounts where §6.3 already reduces width. Intents are therefore batched up to a bounded group size on network volumes, which weakens recovery granularity to that group — a stated, configurable trade, not a silent one.

### 6.5 Cancellation

A `CancelToken` (`Arc<AtomicBool>`) checked between operations **and inside `copy_into`'s loop**. The "never mid-move, so no torn state" claim holds only for `native` same-volume renames; under `reserve` — every cross-device move, plus the volumes in §2.5's table — a 60 GB remux copy would otherwise give unbounded cancel latency, and spec §18 requires responsive cancellation.

On cancel: stop scheduling, let in-flight renames finish, abort copies at a chunk boundary, delete the aborted reservation (the source is intact and the journal `intent` is unmatched, so this is provably safe — §6.7), report completed and remaining counts.

### 6.6 Empty-directory cleanup and GC (spec §2.4 — the highest-risk operation)

Serial, deepest-first, after all moves. Removal requires **all** of:

1. Phase 2 recorded it as a directory that files were successfully moved *out of*.
2. Its **canonical** form is inside root (the same form used by the §5.6 containment check).
3. It is genuinely empty.
4. It is not root, and not a directory this run created.
5. `cleanup.remove_empty_dirs = true`.

Junk-file tolerance (`.DS_Store`, `Thumbs.db`, `desktop.ini`) is **opt-in and off by default**: deleting a file in order to enable deleting a directory is a destructive act the spec does not authorise by default. Failure to remove is a warning, never a run failure.

### 6.7 Reclaiming interrupted reservations

The only artifact this design can leave behind is a short file at a real destination name, from a reservation whose copy did not finish (§6.2). It has no distinguishing name, so **the journal is the registry** — a reservation is identified by an `intent` with no matching `outcome`, scoped by `run_id` and `root`.

| Situation | Behaviour |
|---|---|
| Current run, normal end or cancel | Phase 5 deletes reservations whose `intent` is unmatched. Provably safe: the source is still present and unmodified. |
| Prior run, same root, `resume` | Classified by the §6.4 table — truncate and re-copy, or delete and re-execute. |
| Prior run, same root, plain `organize` | Startup reads the journal for unmatched intents under this root and **reports** them, with a one-line hint to run `resume` or `gc`. They are not silently deleted, and not silently overwritten. |
| No journal entry at all | Not ours. Treated as a pre-existing file and handed to `ConflictPolicy` — see §6.4. |

`media-manager gc <dir>` performs the reclaim explicitly, listing what it would remove and requiring `--yes` for anything it cannot match to an unmatched `intent`.

This closes a gap the previous draft had: a `SIGKILL` mid-copy left a zero-byte file that no command could clear, because a fresh `organize` classified it as a pre-existing conflict forever. Journal-scoped identity fixes that without ever letting the tool delete a file it cannot prove it wrote.

---

## 7. Configuration (`mm-core::config`)

Layered, later wins: **built-in defaults → system config → user config → project config (`.media-manager.toml` in root) → `MM_*` env vars → CLI flags**.

```toml
[extensions]
video    = ["mkv","mp4","m4v","avi","mov","wmv","ts","m2ts","webm"]
audio    = ["mp3","flac","m4a","aac","ogg","opus","wav","wma","alac"]
subtitle = ["srt","ass","ssa","sub","idx","vtt","sup"]
artwork  = ["jpg","jpeg","png","webp","tbn"]
metadata = ["nfo","xml","json","cue"]

[behaviour]
symlinks = "skip"              # skip | follow | treat_as_file
create_subs_dir = true
normalise_artwork = false      # never overwrites existing artwork regardless
require_year_for_movies = true
require_year_for_tv = false
min_confidence = "medium"

[moves]
no_replace_strategy = "auto"   # auto | native | reserve | check_then_rename
                               # auto = native where a rename primitive exists, reserve otherwise
verify = "size"                # size | hash
preserve_mtime = true

[conflict]
policy = "report"
compare = ["size","blake3"]

[cleanup]
remove_empty_dirs = true
tolerate_junk = false

[providers]
enabled = false                # offline-capable by default (§10)
```

Config is data, versioned, digested into every plan and journal line. `media-manager config print` emits the fully resolved config with each value's origin layer — invaluable for "why did it do that".

The journal and probe cache live in the user's data/cache directories, which is a **stated exception** to "never operate outside the source directory": that invariant governs *media* operations, and the exception is documented rather than left as a silent contradiction.

---

## 8. CLI (`mm-cli`)

```text
media-manager scan     <dir> --type <movies|tv|music> [--json]
media-manager plan     <dir> --type ... [--json] [-o plan.json]
media-manager organize <dir> --type ... [--dry-run] [--yes] [--from-plan plan.json]
                              [--strict] [--fail-fast]
media-manager verify   <dir> --type ...
media-manager resume   --run <run-id> | --latest
media-manager gc       <dir> [--yes]
media-manager config   print | path
media-manager completions <shell>

Global: --config <path> --log-level <lvl> --log-file <path> --json --workers <n>
```

`--dry-run` renders the same `Plan` that `organize` would execute — not a parallel code path. `plan` + `--from-plan` gives review-then-apply for cautious operators and CI.

### Exit codes

Automation needs a **precedence rule**, because a realistic run hits several conditions at once. Highest applicable code wins:

| Code | Condition (computed from `RunReport`) |
|---|---|
| 0 | Nothing outstanding. Everything actionable was applied; no review, conflicts, duplicates or failures. Includes "already organised". |
| 10 | `verify`/`plan` only: `pending` is non-empty — changes are needed. Distinct from 0 so a script can tell "already clean" from "work pending". |
| 20 | `NeedsReview` or `Ambiguous` items present |
| 25 | `Duplicated` items present |
| 30 | `Conflicted` items present |
| 40 | `Failed` items present |
| 50 | `fatal.is_some()` — the run aborted (root unreadable, journal unwritable, resume detected loss) |
| 64 | Usage error, including invalid config or a template that fails the §3.3 law |
| 70 | Internal error (panic caught at the boundary, invariant violation) |
| 130 | Cancelled |

**Precedence: the highest applicable code wins**, evaluated in the order above. A realistic run hits several conditions at once, and without a stated rule two callers would disagree about the same report.

Skipped and unclassified files are **not** a non-zero condition. A healthy run over a library containing one `readme.txt` must exit 0, or every cron consumer learns to append `|| true` and the exit codes stop meaning anything. `dirs_not_removable` is likewise a warning only. All of these appear in the report and in `--json` regardless. `--strict` promotes review, duplicate and skip conditions to failures; `--fail-fast` (§14) additionally stops at the first `Failure` instead of continuing.

Human output is a diff-style table; `--json` emits the serialised `Plan` and `RunReport`. `tracing` with `--log-level` and `--log-file`.

---

## 9. GUI (`mm-gui`, egui/eframe)

Engine on a dedicated thread; the UI thread sends commands and drains events:

```rust
enum Command {
    Scan { root: PathBuf, kind: MediaKind },
    Plan,
    Apply { items: Vec<ItemId>, dir_renames: Vec<DirRenameId>, dir_removals: Vec<DirRemovalId> },
    Cancel,
}
enum Event {
    Progress { done: u64, total: u64, current: PathBuf },
    PlanReady(Plan),
    PlanInvalidated,          // selection changed the predicted cleanup set
    ItemDone(ItemId, Outcome),
    Finished(RunReport),
    Error(String),
}
```

`Apply` carries selections for directory renames and removals, not just items — otherwise "individually deselectable" cleanup has no wire representation.

**Deselecting a move invalidates the predicted removals**: a source directory will not empty if one of its files is skipped. So deselection triggers a cheap re-plan of the cleanup set and a `PlanInvalidated` event, and the preview updates. Without that, the GUI would show removals that then silently do not happen — exactly what §17 and §2.6 exist to prevent. Because §6.2 only records candidates and §6.6 decides serially against the real filesystem, plan-time and execution-time semantics agree.

Layout:

- **Top bar** — folder picker (`rfd`), media-type selector, Scan / Preview / Apply / Cancel.
- **Preview table** (`egui_extras::TableBuilder`, virtualised for 50 000+ rows) — Source → Destination, status chip (`Move` / `NoOp` / `Conflict` / `Duplicate` / `Review` / `Skip`), per-row checkbox, filter and search.
- **Cleanup panel** — directory renames and removals, individually deselectable.
- **Diagnostics panel** — unclassified files, ambiguous items, orphan subtitles, each with the specific reason a decision could not be made (§24 point 3).
- **Progress + stats**, **Settings** bound to the same `Config` struct via serde so GUI and CLI cannot drift.

Apply is disabled until a plan exists. Nothing executes that the user has not seen.

---

## 10. Metadata providers (`mm-provider`)

```rust
pub trait MetadataProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn supports(&self, kind: MediaKind) -> bool;
    fn lookup(&self, query: &Query, cancel: &CancelToken) -> Result<Vec<Candidate>>;
}
```

Blocking, on the engine's pool — consistent with §0's no-async decision. (An earlier draft had `#[async_trait]` on a non-async method, which was both a no-op and a contradiction.)

Ship `NfoProvider` (local `.nfo`, Kodi-style) first: offline, free, covers many real libraries. `TmdbProvider` and `MusicBrainzProvider` go behind cargo features, **disabled by default** — the app must be fully useful with no network (§10). Providers only *raise* confidence per the §2.1 preference table, never override an embedded tag, and a match below a similarity threshold is recorded as evidence rather than applied. Results cached on disk with a TTL.

**Enabling a provider later is a library-wide rename event** — it introduces `{episode_title}` where there was none. So provider changes bump `config_digest`, and the CLI warns that the next run will re-render affected items, with `plan` available to preview the scale first.

---

## 11. Testing strategy

| Layer | Technique |
|---|---|
| `mm-parse` | Corpus in `testdata/names/*.toml` (≥ 400 cases covering every item in §22.1); `insta` snapshots; `proptest` for the §3.3 **field-level** round-trip law and the non-panic guarantee |
| Templates | Every configured template is round-trip-checked at config load *and* in tests; a failing template is a startup error |
| `mm-probe` | Committed minimal-but-valid mkv/mp4/flac/mp3/m4a fixtures; assert dimensions and tags; hash before and after to prove no mutation; `clippy.toml` bans `lofty::save_to` |
| Planning | Declarative fixture trees (`testdata/fixtures/*.toml` → `TempDir`), snapshot the serialised `Plan`. Snapshots serialise **root-relative paths only**, with `insta` filters for the `TempDir` component and `run_id` — otherwise every snapshot is nondeterministic |
| Idempotency | Two suites: (a) run → re-plan → assert all `NoOp`; (b) fixtures that **start** in the target layout and were never produced by this tool — that second case is the actual §19 requirement and the first suite does not cover it |
| Execution | Fixtures against `RealFs` in a `TempDir`; assert the resulting tree listing |
| Fault injection | `FaultyFs` for permission denied, read-only, `EXDEV`, failure at op *n* — asserts partial-failure accounting and that **no source file is ever lost** |
| Move strategies | Every execution test runs under **both** `native` and `reserve` via config override — they must be behaviourally identical, and `reserve` is the cross-device path on every platform. Assert a pre-existing destination yields `DestinationOccupied` (not an overwrite) under both, including the Windows directory-target case that surfaces as `PermissionDenied` before normalisation. A FAT loopback asserts `auto` selects `native` |
| Resume | Truncate the journal mid-run at each of the **six** §6.4 states and assert the correct classification — especially that source-missing/destination-matching is a *success*, that a short destination with no matching `intent` is never truncated, and that source-missing/destination-mismatching halts for manual resolution |
| Reservation reclaim | `SIGKILL` mid-copy, then assert: a plain `organize` reports the leftover rather than overwriting or conflicting permanently, `resume` completes it, and `gc` clears it (§6.7) |
| Error taxonomy | Assert that only §14's `Fatal` set aborts a run: inject `PermissionDenied`, `ReadOnlyFilesystem`, disk-full and a corrupt container mid-run and assert the run completes with the remaining items processed |
| Concurrency | Hundreds of files converging on few destinations at high worker count; assert deterministic outcome; cancellation at random points asserting tree consistency and no leftover reservations |
| Platform | `#[cfg(windows)]`: reserved names incl. `CON.2010.mkv`, long paths, case-insensitive collisions, case-only file *and directory* renames. `#[cfg(unix)]`: non-UTF-8 filenames, symlink loops, `renameat2` on a FAT-formatted loopback image (asserting it *succeeds*, per §2.5) plus its `EINVAL`/`EOPNOTSUPP`/`ENOSYS` fallbacks. `#[cfg(macos)]`: NFC/NFD collision on APFS, and `renamex_np` returning `ENOTSUP` on a non-APFS volume so `auto` selects `reserve` |
| CLI | `assert_cmd` + `trycmd` with **per-platform snapshot directories** and separator-normalising filters — cross-OS stdout snapshots differ otherwise |
| Performance | `criterion` on a synthetic 100 000-file tree, guarding against accidental O(n²) in grouping or conflict detection, and against probe-cache misses |

CI matrix: `windows-latest`, `ubuntu-latest`, `macos-latest` × stable and stable−2. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-features`, `cargo deny check`.

---

## 12. Phased roadmap

Each phase ends shippable and green. Movies first: the simplest complete vertical slice through the whole pipeline.

**Phase 0 — Foundation.** Workspace, toolchain, CI matrix, lint and licence gates (`deny.toml` with `CC0-1.0` and `Unlicense` allowed). `mm-core`: `Field`/`Source`/`Confidence`, error taxonomy, `FileSystem` trait + `RealFs`/`MemFs`/`FaultyFs`, the full path layer (sanitiser, per-target length caps, `VolumeSemantics` querying, `rename_no_replace` shims for all three platforms plus the `reserve` path), config loader with layering and `config print`.
Both move strategies (`native` and `reserve`) land here, including the handle-based reservation API and the `DestinationOccupied` error normalisation.
*Exit:* path layer and config fully tested on all three OSes. A FAT loopback asserts `auto` selects **`native`** (the Linux VFS enforces `RENAME_NOREPLACE` on vfat — §2.5), and strategy selection for `reserve` is tested via an explicit override plus, where CI allows, a network mount. Case-insensitive volume detection uses the per-directory flag, not the volume capability bit. Error taxonomy (§14) defined. No media logic yet.

**Phase 1 — Parser, movies.** Token stream, extractor registry, movie extractors, tag vocabularies as data, **positional multi-residual assignment** (§3.1). Corpus of ≥ 150 movie names. Field-level round-trip and non-panic property tests.
*Exit:* corpus green; round-trip law holds for every default movie template; parser has zero I/O dependencies.

**Phase 2 — Planning, movies.** Scan, classify, resolve, two-pass grouping, association (subtitles + artwork + nfo), routing with the discriminator chain, containment and collision detection, `Plan` + serde. `scan`, `plan`, `organize --dry-run`. Human and JSON renderers.
*Exit:* dry-run produces correct plans for the §2.3 and §4 examples, and correctly reports two same-resolution different-source rips as distinct versions rather than a conflict. Still zero writes.

**Phase 3 — Execution.** Phase structure, move ordering with post-verify source removal, cross-device copy with directory fsync and mtime preservation, journal with `run_id`/`root`/`config_digest`, conflict policies with re-parseable `RenameNew` and defined `Replace`/`SkipIfIdentical` under `reserve`, directory renames (always renames, never reservations), serial cleanup, journal-scoped reservation reclaim. Real `organize`, `verify`, `gc`.
*Exit:* §22.2 and both §22.6 suites green; `FaultyFs` suite proves no file is ever lost; case-only directory rename works on Windows and macOS.

**Phase 4 — Probing.** `matroska` + `re_mp4` dimension extraction, DAR-corrected `max(w,h)` banding calibrated against a real library, `(file_id, size, mtime)` probe cache, filename fallback with an explicit diagnostic. Re-evaluate the ISO-BMFF crate choice against `symphonia-format-isomp4` if MPL proves acceptable.
*Exit:* scope-ratio encodes label correctly; unsupported containers degrade gracefully and say so.

**Phase 5 — TV.** Episode extractors (all §6.5 forms), multi-episode identity, specials, season directories, two-pass show-year resolution across a mixed directory, episode-identity gated subtitle association, discriminator chain for episodes.
*Exit:* §22.3 green, including multi-episode files never split, and the `Show (2011) - S01E01 - Winter Is Coming - 1080p.mkv` round-trip.

**Phase 6 — Music.** `lofty` tag reading, `AlbumId` two-pass resolution with album-artist preference, multi-disc (including the partial-disc-number `NeedsReview` rule), compilations and Various Artists, artwork recognition and optional normalisation (never overwriting), filename fallback with the `N - A - B` ambiguity resolving to `NeedsReview` (§3.3).
*Exit:* §22.4 green; before/after hashes prove no tag was written; a tagless compilation in `N - A - B` form is reported rather than guessed.

**Phase 7 — Scale.** Destination partitioning, `rayon` pool with configured width, network detection and reduced width, cancellable copies, `resume` with all six state classifications and reservation reclaim, journal batching on network volumes, progress reporting, `criterion` on 100 000 files.
*Exit:* resume test matrix green; no benchmark regression; probe cache hit rate ~100% on a second run.

**Phase 8 — GUI.** eframe shell, engine thread + channels, virtualised preview table with per-row selection, cleanup panel with deselection and `PlanInvalidated` re-planning, diagnostics panel, progress, settings bound to `Config`.
*Exit:* every §17 requirement demonstrable; UI never blocks; deselecting a move visibly updates the predicted cleanup set.

**Phase 9 — Providers.** Provider trait, `NfoProvider`, disk cache, `config_digest` change warning. TMDB and MusicBrainz behind default-off features.
*Exit:* full test suite passes with networking disabled.

**Phase 10 — Polish.** `undo` from journal, shell completions, man pages, packaged builds (MSI and portable zip for Windows, `.deb`, Homebrew formula, container image; **static musl for `mm-cli` only** — a statically linked musl GUI with native dialogs is not practical), `docs/naming.md`.

---

## 13. Requirements traceability

| Spec § | Where addressed |
|---|---|
| 1 Purpose, CLI + GUI | §1 layout, §8 CLI, §9 GUI |
| 2.1 Source dirs, UNC, NAS | §5.1 scan, §2.4 long paths, §6.3 network width |
| 2.2 Media type | `MediaKind` on `Plan`; `--type` |
| 2.3 In-place organisation | **§5.0** states the three non-obvious cases explicitly: root is never a destination candidate (so pointing at a movie folder does not nest it), half-organised subtrees are the idempotency path, and arbitrary user directories are never renamed into computed names. Plus §6.1 phasing, which exists *because* source and destination trees overlap |
| 2.4 Obsolete directory removal | §6.6 five-condition gate, serial deepest-first |
| 2.5 Non-destructive | §6.2 order (source removed only after verify); probers read-only with `save_to` banned; §11 hash-before/after |
| 2.6 Dry-run | §5.7 shared `Plan`; §8 `--dry-run`; §2.4 no probe-writes during planning |
| 2.7 Bounded parallelism | §6.1 phases, §6.3 destination ownership **plus the mutex the earlier draft denied needing** |
| 2.8 Robustness | §3.4 non-panic property test; `Readiness::NeedsReview` |
| 3.1–3.4 Formats, classification | §2.3, §7 extension sets, §5.4 artwork/metadata routing |
| 4 Movies | §5.3 grouping, §5.5 templates, §3 parser |
| 4.3 Resolution | §4 DAR-corrected `max(w,h)` banding + fallback diagnostic. **HDR is filename-only** with these crates |
| 4.5 Editions and versions | `Edition` in `MovieId` **plus** the §5.3 escalating discriminator chain — edition alone is insufficient |
| 5 Subtitles, forced, SDH | §5.4 name-evidence gate then ranked score; ISO 639-1 table; flag set |
| 6 TV, specials, multi-episode | §3.2 rules, `episodes: Vec<u16>`, Phase 5 |
| 7 TV subtitles | §5.4 episode id is the strongest gate signal |
| 8 Music | §5.3 two-pass `AlbumId`, §5.5 templates, §3.3 track-artist decision (tags, not filenames), Phase 6 |
| 9 Parsing | `mm-parse` I/O-free; §3.1 positional residuals; §3.5 extensibility |
| 10 Providers | §10, default-off, `config_digest` churn warning |
| 11 Conflicts and duplicates | §5.6 `ConflictPolicy`; §2.5 `reserve` gives atomic no-overwrite on **every** filesystem, including FAT/SMB; §5.3 `Duplicate` as a distinct outcome from `Conflict`, consumed by stage 11 |
| 12 Pipeline | §5 eleven-stage list, with grouping split around probe (stages 4 and 7) and `associate`/`reconcile` as real stages |
| 13 Operation ordering | §6.2, both strategies, source removed only after verification |
| 14 Logging | §6.4 journal + `tracing` |
| 15 Error handling | **§14** severity taxonomy classified by blast radius, with a deliberately tiny `Fatal` set; `RunReport` as the single value feeding CLI exit codes, `--json` and the GUI stats panel |
| 16 CLI | §8 |
| 17 GUI | §9 including cleanup deselection and `PlanInvalidated` |
| 18 Cancellation, recovery | §6.5 cancellable copies with safe reservation cleanup, §6.4 resume with run id + root + config digest and the **five**-state table (size, not mere presence, proves completion) |
| 19 Idempotency | §3.3 **field-level** law, `Action::NoOp`, `Plan.dir_renames` so case-wrong directories are fixable even when this tool did not create them (§5.0), §11 both suites incl. never-touched trees |
| 20 Configuration | §7, with template validation at load |
| 21 Filesystem safety | §2.4 sanitiser (unconditional, not weakened by verbatim paths), §5.6 component-wise canonical containment incl. followed symlinks, §6.6 cleanup on canonical form, §7 stated data/cache exception |
| 22 Testing | §11 |
| 23 Extensibility | Trait-based probers/providers/extractors; journal enables undo. **`MediaKind` is not cheap to extend** — a new kind touches parser shapes, templates, grouping and required-field config. Honest cost, not a claim of free extension |
| 24 Correctness over guessing | `Field`/`Readiness` in the type system; `Source::Fallback` cannot justify a move; ambiguity, duplication and orphans are first-class plan outcomes |

---

## 14. Error taxonomy and reporting

Spec §15 requires that one file's failure never terminates the run, and that the report distinguishes successes, skips, warnings, errors, unclassified files, conflicts and un-removable directories. The plan referenced an "error taxonomy" three times without giving one; here it is.

The organising principle: **errors are classified by blast radius, not by cause.** What the caller needs to know is whether to keep going, not which syscall failed.

```rust
pub enum Severity {
    Info,     // recorded, not surfaced by default (NoOp, cache hit)
    Notice,   // per-item, expected: Skip, Unknown file, orphan subtitle
    Warning,  // per-item, actionable: NeedsReview, Ambiguous, dir not removable,
              //   resolution undetectable, best-effort overwrite protection
    Failure,  // per-item, unexpected: the item did not complete
    Fatal,    // run-scoped: continuing would risk the library
}
```

**Only `Fatal` stops the run.** Its membership is deliberately tiny, because every extra member is a way for one bad file to abort a 40 000-file job:

| Fatal condition | Why the run cannot continue |
|---|---|
| Root missing, unreadable, or not a directory | Nothing to do, and a wrong root is a likely user error |
| Journal cannot be created or `fsync`ed | Recovery and resume become impossible; proceeding would leave an unrecoverable partial state |
| Resume finds source **and** destination missing (§6.4 last row) | Evidence of data loss; stop and report rather than continue past it |

Three conditions, and two that an earlier draft wrongly included:

- **"Volume semantics undeterminable" is not fatal.** §2.4 now assumes the conservative case and emits a `Warning`. Making it fatal would let a single sshfs or overlayfs mount abort the whole run, which contradicts this section's own stated principle.
- **"Config or template invalid" is not fatal either** — it is a *usage* error, caught at load before any work, and it exits 64. Putting it in a taxonomy defined by blast radius was a category error: there is no run for it to have a radius within.
- **"Root escapes containment" was incoherent.** Containment (§5.6) is a property of destinations *relative to* root; root cannot escape itself. The real condition is a destination that canonicalises outside root, which is a per-item plan-time `Failure` — the item is dropped and reported, and the run continues.

Everything else — permission denied, read-only mount, disk full, path too long, a container that will not parse, a filename that will not sanitise, a directory that will not delete — is **per-item**. Note where the boundary falls: disk full is `Failure`, not `Fatal`, because a run often spans several volumes and the next item may well succeed. `--fail-fast` exists for operators who want the opposite.

Per-item errors carry the item id, the stage that produced them, and the underlying `io::Error` kind, so the report can group by cause without the taxonomy encoding causes.

```rust
#[derive(PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Outcome {
    NoOp, Moved, Skipped, Unclassified,
    NeedsReview, Ambiguous, Conflicted, Duplicated, Failed,
}

pub struct RunReport {
    pub run_id: Uuid,
    pub mode: RunMode,                    // Plan | DryRun | Apply | Verify | Resume
    pub counts: BTreeMap<Outcome, u64>,
    pub pending: BTreeMap<Outcome, u64>,  // planned-but-not-applied; the basis for exit 10
    pub diagnostics: Vec<Diagnostic>,     // severity, item, stage, message, io_kind
    pub dirs_removed: u64,
    pub dirs_not_removable: Vec<(PathBuf, String)>,
    pub reservations_reclaimed: u64,
    pub fatal: Option<FatalReason>,
    pub cancelled: bool,
    pub duration: Duration,
}
```

Four corrections over the first draft of this struct, each because the exit-code precedence in §8 could not otherwise be computed from it:

- **`Outcome` is now defined**, with a derived `Ord` — it is a `BTreeMap` key here and appears in §9's `ItemDone`, so the ordering requirement was implicit and unstated.
- **`Ambiguous` is a carrier.** `Readiness::Ambiguous` existed in §5.2 and §13 claimed ambiguity was a "first-class plan outcome", but nothing downstream could represent it — not `Action`, not the GUI status chips, not the counts. It now appears in all three.
- **`pending` vs `counts`** separates planned from applied work. Without it, `verify`'s exit 10 ("changes are needed") is not derivable, because `counts[Moved]` is ambiguous between "moved 400 files" and "would move 400 files".
- **`fatal`** is explicit, so the most severe outcome the tool can produce has a representation rather than being inferred from an empty report.

`RunReport` is the single value that §8's exit codes are computed from, that `--json` serialises, and that the GUI's statistics panel renders — so the CLI and GUI cannot disagree about what happened. It maps directly onto the spec's `97 processed / 2 skipped / 1 failed` example.

---

## 15. Open risks

| Risk | Mitigation / status |
|---|---|
| No no-replace rename primitive on FAT32/exFAT or SMB | **Was never true, and the plan repeated it for two drafts.** `renameat2(RENAME_NOREPLACE)` works on vfat/exfat/cifs (Linux VFS enforces it) and `MoveFileExW` works on FAT/exFAT. Only **macOS on SMB/NFS/FAT** genuinely lacks a primitive, and `reserve` covers it. Worth recording as a reminder that a confident review finding is still a claim to verify. |
| Network volumes: client-side stale-dentry no-replace check | Real, and the actual reason `reserve` is the default on network mounts — `O_EXCL`/`CREATE_NEW` resolves server-side. Only matters with a concurrent writer on the share. |
| `reserve` leaves short files at real destination names | Journal-scoped identity (§6.7): `intent` is durable before the reservation, and a mismatching file with no matching `intent` is never truncated or deleted. `gc` and `resume` can both clear them; a plain `organize` reports rather than acts. |
| `reserve` costs a full copy where it applies | Narrow scope (cross-device, macOS non-APFS, network) limits the blast radius. `no_replace_strategy` is overridable per config with the trade documented. |
| Resume state: source missing + destination size-mismatched | **Unresolvable by the tool** — indistinguishable from a pre-existing wrong-size file whose source was deleted out of band. Reported for manual resolution; never guessed. The one cell of the six-state matrix with no safe automatic action. |
| No pure-Rust prober for AVI/WMV/TS/M2TS | Filename fallback with explicit diagnostic; optional `ffprobe-fallback` feature later. Trait boundary makes it additive. |
| ISO-BMFF crate churn | `mp4` is unmaintained and MIT-only; `re_mp4` chosen; `mp4parse`/`symphonia-format-isomp4` (both MPL-2.0) are the fallback if MPL becomes acceptable. Decision revisited at Phase 4. |
| MSRV vs egui | egui raises MSRV most releases. Project MSRV is 1.95 (egui 0.36); `mm-core` floors at **1.87** (`InvalidFilename`), `mm-parse` lower still. The GUI crate floats with stable. No project-wide MSRV promise. |
| `{track_artist}` template is not invertible | **Closed.** Removed from the default template (§3.3); track artists come from tags, which is where media servers read them anyway. Opt-in prefix is flagged non-invertible at config load. |
| Positional multi-residual parsing is harder than single-slot | Open in the sense of being genuinely hard, but scoped: scheduled in Phases 1 and 5, with the `Show (2011) - S01E01 - Winter Is Coming - 1080p.mkv` round-trip as a Phase 5 exit criterion. |
| Parser accuracy on messy real libraries | Corpus-driven. `NeedsReview` is an expected outcome, not a failure. `--report-only` builds confidence on a real library before applying. |
| Multi-version conventions differ between Jellyfin and Plex | Discriminator chain format is configurable; verify against current server docs before locking Phase 2. |
| Normalisation-insensitive volumes | `VolumeSemantics` models it alongside case; APFS NFC/NFD collision is a Phase 0 test. |
| Journal fsync cost on network volumes | Bounded batching, with the recovery-granularity trade stated and configurable. |
| Scope creep from §23 | Nothing on that list ships before Phase 10. Architecture keeps doors open; the roadmap keeps them shut. |
