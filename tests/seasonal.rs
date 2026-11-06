use media_manager::{run, LibraryKind, Options};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static SEQ: AtomicU64 = AtomicU64::new(0);
struct Library(PathBuf);
impl Library {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "media-manager-seasonal-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, name).unwrap();
    }
    fn apply(&self) -> media_manager::Summary {
        let summary = run(Options {
            root: self.0.clone(),
            kind: LibraryKind::Tv,
            apply: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(summary.failed, 0);
        summary
    }
    fn exists(&self, path: &str) -> bool {
        self.0.join(path).is_file()
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn season_sidecars_move_in_one_run_and_second_run_is_idle() {
    let lib = Library::new();
    lib.file("House, M.D. Season 6 (1080p x265 Joy)/House, M.D. S06E01 Broken.mkv");
    lib.file("House, M.D. Season 6 (1080p x265 Joy)/Subs you don't need/House, M.D. - S06E01 English.srt");
    lib.file("House, M.D. Season 6 (1080p x265 Joy)/Extra/House, M.D. S06Extra Before Broken.mkv");
    lib.file("House, M.D. Season 6 (1080p x265 Joy)/Extra/nested/notes.txt");
    assert_eq!(lib.apply().moved, 4);
    assert!(lib.exists("House, M D/Season 06/subs/House, M D S06E01.en.srt"));
    assert!(lib.exists("House, M D/Season 06/Extras/House, M.D. S06Extra Before Broken.mkv"));
    assert!(lib.exists("House, M D/Season 06/Extras/nested/notes.txt"));
    let second = lib.apply();
    assert_eq!(second.planned_moves, 0);
    assert_eq!(second.skipped, 0);
}

#[test]
fn recovers_sidecars_when_original_season_has_no_episodes() {
    let lib = Library::new();
    lib.file("House, M D/Season 04/House, M D S04E01.mkv");
    lib.file("House, M.D. Season 4 (1080p x265 Joy)/Subs that you don't need/House, M.D. - S04E01 Alone.srt");
    lib.file("House, M.D. Season 4 (1080p x265 Joy)/Extras/Anatomy of a Scene.mkv");
    assert_eq!(lib.apply().moved, 2);
    assert!(lib.exists("House, M D/Season 04/subs/House, M D S04E01.srt"));
    assert!(lib.exists("House, M D/Season 04/Extras/Anatomy of a Scene.mkv"));
}

#[test]
fn explicit_bonus_season_is_honored_and_subtitle_collisions_are_preserved() {
    let lib = Library::new();
    lib.file("House, M D/Season 02/House, M D S02E01.mkv");
    lib.file("House, M D/Season 02/subs/House, M D S02E01.srt");
    let source = "House, M.D. Season 2 (1080p x265 Joy)";
    lib.file(&format!(
        "{source}/Extras - Season 1/House S01Extra Casting.mkv"
    ));
    lib.file(&format!(
        "{source}/Subs Season Two/House, M.D. S02E01 Acceptance.srt"
    ));
    lib.file(&format!("{source}/Subs Season Two/Other Show S02E01.srt"));
    lib.file(&format!("{source}/Subs Season Two/House, M.D. S02E99.srt"));
    let before = fs::read(
        lib.0
            .join("House, M D/Season 02/subs/House, M D S02E01.srt"),
    )
    .unwrap();
    let summary = lib.apply();
    assert_eq!(summary.moved, 1);
    assert_eq!(summary.skipped, 3);
    assert!(lib.exists("House, M D/Season 01/Extras/House S01Extra Casting.mkv"));
    assert!(lib.exists(&format!(
        "{source}/Subs Season Two/House, M.D. S02E01 Acceptance.srt"
    )));
    assert_eq!(
        before,
        fs::read(
            lib.0
                .join(Path::new("House, M D/Season 02/subs/House, M D S02E01.srt"))
        )
        .unwrap()
    );
}
