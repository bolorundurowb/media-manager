//! Built-in defaults (§7, §5.5).

use super::{
    Behaviour, Cleanup, Concurrency, Config, Conflict, Extensions, Moves, MovieNaming, MusicNaming,
    Naming, Providers, TvNaming,
};
use crate::identity::SourcePreference;
use crate::template::Template;

fn t(s: &str) -> Template {
    Template::parse(s).expect("built-in template must parse")
}

pub fn default_config() -> Config {
    Config {
        schema_version: 1,
        extensions: default_extensions(),
        behaviour: Behaviour::default(),
        moves: Moves::default(),
        conflict: Conflict::default(),
        cleanup: Cleanup::default(),
        providers: Providers::default(),
        concurrency: Concurrency::default(),
        naming: default_naming(),
        source_preference: SourcePreference::conservative_default(),
    }
}

pub fn default_extensions() -> Extensions {
    Extensions {
        video: vec![
            "mkv".into(),
            "mp4".into(),
            "m4v".into(),
            "avi".into(),
            "mov".into(),
            "wmv".into(),
            "ts".into(),
            "m2ts".into(),
            "webm".into(),
        ],
        audio: vec![
            "mp3".into(),
            "flac".into(),
            "m4a".into(),
            "aac".into(),
            "ogg".into(),
            "opus".into(),
            "wav".into(),
            "wma".into(),
            "alac".into(),
        ],
        subtitle: vec![
            "srt".into(),
            "ass".into(),
            "ssa".into(),
            "sub".into(),
            "idx".into(),
            "vtt".into(),
            "sup".into(),
        ],
        artwork: vec![
            "jpg".into(),
            "jpeg".into(),
            "png".into(),
            "webp".into(),
            "tbn".into(),
        ],
        metadata: vec!["nfo".into(), "xml".into(), "json".into(), "cue".into()],
    }
}

pub fn default_naming() -> Naming {
    Naming {
        movies: MovieNaming {
            dir: t("{title}[ ({year})]"),
            file: t("{title}[ ({year})]{discriminators}"),
            subs_dir: "subs".into(),
            sub_file: t("{title}[ ({year})].{language}[.{flags}]"),
            artwork: "poster".into(),
            nfo: t("{title}[ ({year})]"),
        },
        tv: TvNaming {
            show_dir: t("{title}[ ({year})]"),
            season_dir: t("Season {season:02}"),
            specials_dir: "Specials".into(),
            file: t("{title}[ ({year})] - {episode_code}[ - {episode_title}]{discriminators}"),
            sub_file: t(
                "{title}[ ({year})] - {episode_code}[ - {episode_title}].{language}[.{flags}]",
            ),
        },
        music: MusicNaming {
            artist_dir: t("{album_artist}"),
            album_dir: t("{album}[ ({year})]"),
            disc_dir: t("CD {disc}"),
            file: t("[{track:02} - ]{title}"),
            artwork: "cover".into(),
            compilation_prefix: false,
        },
    }
}
