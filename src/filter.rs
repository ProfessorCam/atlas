//! Filter expression parsing for the toolbar filter box.
//!
//! A filter is a set of whitespace-separated tokens, ANDed together. Supported
//! token forms (case-insensitive):
//!
//!   * `foo`            — name contains "foo"
//!   * `*.jpg`          — file extension is "jpg"
//!   * `type:image`     — entry belongs to a category (image/video/dir/…)
//!   * `>10mb` `<1gb`   — size comparison (units: b, k/kb, m/mb, g/gb, t/tb)
//!   * `>=500k` `<=2g`  — inclusive size comparison
//!
//! Example: `type:video >200mb` matches video files larger than 200 MiB.

use crate::colors::{get_category, FileCategory};
use crate::scanner::FileEntry;

#[derive(Clone, Copy, PartialEq)]
enum SizeOp {
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Clone)]
enum Predicate {
    Size(SizeOp, u64),
    Ext(String),
    Type(FileCategory),
    Name(String),
}

/// A compiled filter. An empty filter matches everything.
#[derive(Clone, Default)]
pub struct Filter {
    predicates: Vec<Predicate>,
}

impl Filter {
    pub fn parse(input: &str) -> Self {
        let predicates = input.split_whitespace().filter_map(parse_token).collect();
        Filter { predicates }
    }

    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }

    /// True when `entry` satisfies every predicate.
    pub fn matches(&self, entry: &FileEntry) -> bool {
        self.predicates.iter().all(|p| p.matches(entry))
    }
}

impl Predicate {
    fn matches(&self, e: &FileEntry) -> bool {
        match self {
            Predicate::Size(op, b) => match op {
                SizeOp::Gt => e.size > *b,
                SizeOp::Ge => e.size >= *b,
                SizeOp::Lt => e.size < *b,
                SizeOp::Le => e.size <= *b,
            },
            Predicate::Ext(ext) => e
                .extension()
                .map(|x| x.eq_ignore_ascii_case(ext))
                .unwrap_or(false),
            Predicate::Type(cat) => get_category(e) == *cat,
            Predicate::Name(sub) => e.name.to_lowercase().contains(sub),
        }
    }
}

fn parse_token(tok: &str) -> Option<Predicate> {
    let lower = tok.to_lowercase();
    if let Some(rest) = lower.strip_prefix(">=") {
        return parse_size(rest, SizeOp::Ge);
    }
    if let Some(rest) = lower.strip_prefix("<=") {
        return parse_size(rest, SizeOp::Le);
    }
    if let Some(rest) = lower.strip_prefix('>') {
        return parse_size(rest, SizeOp::Gt);
    }
    if let Some(rest) = lower.strip_prefix('<') {
        return parse_size(rest, SizeOp::Lt);
    }
    if let Some(rest) = lower.strip_prefix("type:") {
        return parse_type(rest).map(Predicate::Type);
    }
    if let Some(rest) = lower.strip_prefix("*.") {
        if rest.is_empty() {
            return None;
        }
        return Some(Predicate::Ext(rest.to_string()));
    }
    if lower.is_empty() {
        return None;
    }
    Some(Predicate::Name(lower))
}

fn parse_size(s: &str, op: SizeOp) -> Option<Predicate> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Split the numeric prefix from the unit suffix.
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let val: f64 = num.parse().ok()?;
    let mult: f64 = match unit.trim() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(Predicate::Size(op, (val * mult) as u64))
}

fn parse_type(s: &str) -> Option<FileCategory> {
    Some(match s {
        "dir" | "directory" | "folder" => FileCategory::Directory,
        "image" | "img" | "images" | "picture" | "pictures" => FileCategory::Image,
        "video" | "movie" | "movies" | "videos" => FileCategory::Video,
        "audio" | "music" | "sound" => FileCategory::Audio,
        "archive" | "archives" | "compressed" => FileCategory::Archive,
        "document" | "doc" | "docs" | "documents" => FileCategory::Document,
        "code" | "source" | "src" => FileCategory::Code,
        "exe" | "executable" | "binary" | "bin" => FileCategory::Executable,
        "font" | "fonts" => FileCategory::Font,
        "data" => FileCategory::Data,
        "other" => FileCategory::Other,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(name: &str, size: u64) -> FileEntry {
        FileEntry {
            path: PathBuf::from(format!("/x/{name}")),
            name: name.to_string(),
            size,
            is_dir: false,
            is_unscanned: false,
            children: vec![],
            file_count: 1,
            modified: None,
        }
    }

    #[test]
    fn empty_matches_all() {
        let f = Filter::parse("   ");
        assert!(f.is_empty());
        assert!(f.matches(&file("anything.txt", 10)));
    }

    #[test]
    fn size_comparisons() {
        let f = Filter::parse(">10mb");
        assert!(f.matches(&file("big.bin", 11 * 1024 * 1024)));
        assert!(!f.matches(&file("small.bin", 5 * 1024 * 1024)));

        let f = Filter::parse("<=1kb");
        assert!(f.matches(&file("tiny", 1024)));
        assert!(!f.matches(&file("huge", 2048)));
    }

    #[test]
    fn extension_and_type() {
        let f = Filter::parse("*.jpg");
        assert!(f.matches(&file("photo.jpg", 1)));
        assert!(f.matches(&file("photo.JPG", 1)));
        assert!(!f.matches(&file("photo.png", 1)));

        let f = Filter::parse("type:image");
        assert!(f.matches(&file("photo.png", 1)));
        assert!(!f.matches(&file("song.mp3", 1)));
    }

    #[test]
    fn multiple_tokens_are_anded() {
        let f = Filter::parse("type:video >200mb");
        assert!(f.matches(&file("movie.mkv", 300 * 1024 * 1024)));
        assert!(!f.matches(&file("movie.mkv", 100 * 1024 * 1024)));
        assert!(!f.matches(&file("clip.txt", 300 * 1024 * 1024)));
    }

    #[test]
    fn substring_name() {
        let f = Filter::parse("report");
        assert!(f.matches(&file("annual_report.pdf", 1)));
        assert!(!f.matches(&file("photo.jpg", 1)));
    }
}
