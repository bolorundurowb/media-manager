//! Filename → structured fields parser (§3). Pure, no I/O.

pub mod extractors;
pub mod model;
pub mod music;
pub mod parser;
pub mod render;
pub mod tokens;
pub mod tv;
pub mod vocab;

pub use extractors::{CopyNumberExtractor, split_copy_suffix};
pub use model::{MediaParse, ParseOptions, ParsedEpisode, ParsedMovie, ParsedTrack};
pub use music::parse_track_filename;
pub use parser::{parse_movie, parse_movie_filename};
pub use tv::parse_episode_filename;
