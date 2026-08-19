//! Filename → structured fields parser (§3). Pure, no I/O.

pub mod extractors;
pub mod model;
pub mod parser;
pub mod render;
pub mod tokens;
pub mod vocab;

pub use extractors::{CopyNumberExtractor, split_copy_suffix};
pub use model::{MediaParse, ParseOptions, ParsedMovie};
pub use parser::{parse_movie, parse_movie_filename};
