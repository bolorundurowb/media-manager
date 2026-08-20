//! Parser unit tests. No real filesystem: these exercise `parse_media_name`,
//! `parse_episode`, `identity_key` and `version_label` only.

use media_manager::parse::{
    identity_key, parse_episode, parse_media_name, version_label, LibraryKind, ParseError,
};

mod movies {
    use super::*;

    #[test]
    fn three_hundred_resolution_versions() {
        let p1080 = parse_media_name("300 (2006) [1080p]", LibraryKind::Movies).unwrap();
        assert_eq!(p1080.title, "300");
        assert_eq!(p1080.year, Some(2006));
        assert_eq!(p1080.resolution.as_deref(), Some("1080p"));
        assert_eq!(version_label(&p1080).as_deref(), Some("1080p"));

        let p2160 = parse_media_name("300 (2006) [2160p]", LibraryKind::Movies).unwrap();
        assert_eq!(p2160.title, "300");
        assert_eq!(p2160.year, Some(2006));
        assert_eq!(p2160.resolution.as_deref(), Some("2160p"));
        assert_eq!(version_label(&p2160).as_deref(), Some("2160p"));
    }

    #[test]
    fn onward_dotted_name() {
        let p = parse_media_name(
            "Onward.2020.2160p.HDR.WEB-DL.DD5.1.HEVC-EVO[TGx]",
            LibraryKind::Movies,
        )
        .unwrap();
        assert_eq!(p.title, "Onward");
        assert_eq!(p.year, Some(2020));
        assert_eq!(p.resolution.as_deref(), Some("2160p"));
        assert_eq!(p.source.as_deref(), Some("WEB-DL"));
        assert_eq!(version_label(&p).as_deref(), Some("2160p"));
    }

    #[test]
    fn numeric_title_with_paren_year() {
        let p = parse_media_name("2001: A Space Odyssey (1968)", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "2001: A Space Odyssey");
        assert_eq!(p.year, Some(1968));

        let p = parse_media_name("2012 (2009)", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "2012");
        assert_eq!(p.year, Some(2009));

        let p = parse_media_name("Blade Runner 2049 (2017)", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Blade Runner 2049");
        assert_eq!(p.year, Some(2017));
    }

    #[test]
    fn bare_year_after_title_is_the_year() {
        let p = parse_media_name("Seven Samurai 1954", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Seven Samurai");
        assert_eq!(p.year, Some(1954));
    }

    #[test]
    fn numeric_title_without_year_stays_a_title() {
        let p = parse_media_name("2012", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "2012");
        assert_eq!(p.year, None);
    }

    #[test]
    fn edition_becomes_version_when_resolution_absent() {
        let p = parse_media_name("Some.Movie.2019.Unrated.BluRay", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Some Movie");
        assert_eq!(p.year, Some(2019));
        assert_eq!(p.edition.as_deref(), Some("Unrated"));
        assert_eq!(p.source.as_deref(), Some("BluRay"));
        assert_eq!(p.resolution, None);
        assert_eq!(version_label(&p).as_deref(), Some("Unrated"));
    }

    #[test]
    fn known_non_title_tag_is_last_resort_version() {
        let p = parse_media_name("Movie (2020) HDR", LibraryKind::Movies).unwrap();
        assert_eq!(p.fallback_tag.as_deref(), Some("HDR"));
        assert_eq!(version_label(&p).as_deref(), Some("HDR"));

        let with_source = parse_media_name("Movie (2020) WEB-DL HDR", LibraryKind::Movies).unwrap();
        assert_eq!(version_label(&with_source).as_deref(), Some("WEB-DL"));
    }

    #[test]
    fn source_is_version_fallback_after_edition() {
        let p = parse_media_name("Movie (2001) BluRay", LibraryKind::Movies).unwrap();
        assert_eq!(version_label(&p).as_deref(), Some("BluRay"));
    }

    #[test]
    fn resolution_preferred_over_edition_and_source() {
        let p =
            parse_media_name("Movie (2001) 1080p BluRay Extended", LibraryKind::Movies).unwrap();
        assert_eq!(version_label(&p).as_deref(), Some("1080p"));
    }

    #[test]
    fn season_tokens_are_not_extracted_in_movie_mode() {
        let p = parse_media_name("Narcos (2015) Season 1 S01", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Narcos Season 1 S01");
        assert_eq!(p.season, None);
    }

    #[test]
    fn identity_key_normalises_for_matching_only() {
        assert_eq!(identity_key("The.Wire"), "the wire");
        assert_eq!(identity_key("Dungeons & Dragons"), "dungeons and dragons");
        assert_eq!(identity_key("Romeo + Juliet"), "romeo and juliet");
        assert_eq!(identity_key("O'Brien"), "obrien");
        assert_eq!(identity_key("Star Trek: Discovery"), "star trek discovery");
        assert_eq!(identity_key("THE MATRIX"), "the matrix");
    }

    #[test]
    fn directors_cut_and_special_edition() {
        let p = parse_media_name(
            "Blade Runner (1982) - Director's Cut - 1080p",
            LibraryKind::Movies,
        )
        .unwrap();
        assert_eq!(p.title, "Blade Runner");
        assert_eq!(p.year, Some(1982));
        assert_eq!(p.edition.as_deref(), Some("Director's Cut"));
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
        assert_eq!(version_label(&p).as_deref(), Some("1080p"));

        let p = parse_media_name("Amelie (2001) Director's Cut", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Amelie");
        assert_eq!(p.edition.as_deref(), Some("Director's Cut"));
        assert_eq!(version_label(&p).as_deref(), Some("Director's Cut"));

        let p = parse_media_name("Movie (2010) Special Edition", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Movie");
        assert_eq!(p.edition.as_deref(), Some("Special Edition"));
    }

    #[test]
    fn hyphenated_and_apostrophe_titles() {
        let p = parse_media_name("Spider-Man (2002) [1080p]", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Spider-Man");
        assert_eq!(p.year, Some(2002));

        let p = parse_media_name("Ocean's Eleven (2001)", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Ocean's Eleven");
        assert_eq!(identity_key(&p.title), "oceans eleven");
    }

    #[test]
    fn similar_titles_have_different_identity_keys() {
        assert_ne!(identity_key("300"), identity_key("300 Rise of an Empire"));
        assert_ne!(identity_key("The Wire"), identity_key("The Wired"));
    }

    #[test]
    fn ampersand_matches_and_for_identity_only() {
        assert_eq!(
            identity_key("Dungeons & Dragons"),
            identity_key("Dungeons and Dragons")
        );
        let p = parse_media_name("Dungeons & Dragons (2000)", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "Dungeons & Dragons");
    }

    #[test]
    fn cut_in_a_title_is_not_an_edition() {
        let p = parse_media_name("The Cut (2014)", LibraryKind::Movies).unwrap();
        assert_eq!(p.title, "The Cut");
        assert_eq!(p.edition, None);
        assert_eq!(p.year, Some(2014));
    }

    #[test]
    fn identity_key_applies_unicode_nfc() {
        let decomposed = "Pok\u{65}\u{301}mon";
        let composed = "Pok\u{e9}mon";
        assert_eq!(identity_key(decomposed), identity_key(composed));
        assert_eq!(identity_key(decomposed), "pok\u{e9}mon");
    }
}

mod tv {
    use super::*;

    #[test]
    fn narcos_redundant_season() {
        let p = parse_media_name(
            "Narcos (2015) Season 1 S01 (1080p BluRay x265 HEVC 10bit AAC 5.1 Vyndros)",
            LibraryKind::Tv,
        )
        .unwrap();
        assert_eq!(p.title, "Narcos");
        assert_eq!(p.year, Some(2015));
        assert_eq!(p.season, Some(1));
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
    }

    #[test]
    fn dotted_sxx_after_year_is_season_not_an_extension() {
        let p = parse_media_name("Ambiguous.2020.S01", LibraryKind::Tv).unwrap();
        assert_eq!(p.title, "Ambiguous");
        assert_eq!(p.year, Some(2020));
        assert_eq!(p.season, Some(1));
    }

    #[test]
    fn the_wire_dotted_name() {
        let p = parse_media_name("The.Wire.S01.1080p.BluRay.x265-RARBG", LibraryKind::Tv).unwrap();
        assert_eq!(p.title, "The Wire");
        assert_eq!(p.year, None);
        assert_eq!(p.season, Some(1));
        assert_eq!(p.resolution.as_deref(), Some("1080p"));
        assert_eq!(p.source.as_deref(), Some("BluRay"));
    }

    #[test]
    fn season_from_word_only() {
        let p = parse_media_name("Show (2010) Season 5 720p", LibraryKind::Tv).unwrap();
        assert_eq!(p.title, "Show");
        assert_eq!(p.year, Some(2010));
        assert_eq!(p.season, Some(5));
    }

    #[test]
    fn missing_season_is_error() {
        let err = parse_media_name("Narcos (2015)", LibraryKind::Tv).unwrap_err();
        assert_eq!(err, ParseError::MissingSeason);
    }

    #[test]
    fn season_mismatch_is_error() {
        let err = parse_media_name("Show Season 1 S02 1080p", LibraryKind::Tv).unwrap_err();
        match err {
            ParseError::SeasonMismatch { a, b } => {
                assert!(a == 1 || b == 1);
                assert!(a == 2 || b == 2);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn episode_sxxexx_and_span() {
    let e = parse_episode("Narcos.S01E03.1080p.mkv").unwrap();
    assert_eq!(e.season, 1);
    assert_eq!(e.episode, 3);
    assert_eq!(e.episode_end, None);

    let m = parse_episode("Show S01E01-E02.mkv").unwrap();
    assert_eq!(m.season, 1);
    assert_eq!(m.episode, 1);
    assert_eq!(m.episode_end, Some(2));

    let x = parse_episode("show.1x05.mkv").unwrap();
    assert_eq!(x.season, 1);
    assert_eq!(x.episode, 5);
}

#[test]
fn episode_missing_number_is_none() {
    assert!(parse_episode("movie.mkv").is_none());
    assert!(parse_episode("Show.S01.nfo").is_none());
}
