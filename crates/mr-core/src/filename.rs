//! Filename patterns for files under `memories/` (SRS §3.2).
//!
//! Three shapes are recognised, tried in this order because the ID character
//! class includes `-` and a `-main` suffix would otherwise be swallowed into
//! the ID by the suffix-less pattern:
//!
//! | Pattern | Meaning |
//! |---|---|
//! | `<date>_<id>-main.<jpg\|jpeg\|png\|mp4\|mov>` | main media |
//! | `<date>_<id>-overlay.png` | overlay for the same `(date, id)` |
//! | `<date>_<id>.<jpg\|mp4>` | main media without the `-main` suffix (tolerated) |
//!
//! The suffix words and extensions are matched case-insensitively; the ID is
//! kept as written and compared case-insensitively through
//! [`ParsedName::pair_key`]. A name whose date digits do not form a real
//! calendar date (for example `2021-13-45`) is treated as unrecognised.

use std::sync::LazyLock;

use chrono::NaiveDate;
use regex::Regex;

/// Which role a recognised `memories/` file plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// `…-main.<ext>`: the base photo or video.
    Main,
    /// `…-overlay.png`: the transparent caption, sticker or drawing layer.
    Overlay,
    /// `<date>_<id>.<jpg|mp4>`: main media without the `-main` suffix.
    MainNoSuffix,
}

impl FileKind {
    /// `true` for both main-media shapes.
    pub fn is_main(self) -> bool {
        matches!(self, FileKind::Main | FileKind::MainNoSuffix)
    }
}

/// The parts of a recognised filename.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedName {
    /// Export-provided UTC calendar date from the filename.
    pub date: NaiveDate,
    /// Item ID exactly as written in the filename.
    pub id: String,
    pub kind: FileKind,
    /// Lower-cased extension without the dot (`jpg`, `mp4`, `png`, …).
    pub ext: String,
}

impl ParsedName {
    /// Key on which a main file and an overlay are paired (FR-12): the date
    /// plus the ID folded to upper case, because IDs match case-insensitively.
    pub fn pair_key(&self) -> (NaiveDate, String) {
        (self.date, self.id.to_ascii_uppercase())
    }
}

const DATE: &str = r"([0-9]{4}-[0-9]{2}-[0-9]{2})";
const ID: &str = r"([A-Za-z0-9-]+)";

static MAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^{DATE}_{ID}-(?i:main)\.((?i:jpg|jpeg|png|mp4|mov))$"
    ))
    .expect("valid regex")
});
static OVERLAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^{DATE}_{ID}-(?i:overlay)\.(?i:png)$")).expect("valid regex")
});
static NO_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^{DATE}_{ID}\.((?i:jpg|mp4))$")).expect("valid regex"));

/// Parse a bare filename (no directory part). Returns `None` for anything
/// that is not one of the three recognised shapes; such files are logged as
/// "unrecognised" by the pipeline.
pub fn parse_filename(name: &str) -> Option<ParsedName> {
    let (kind, caps) = if let Some(c) = MAIN.captures(name) {
        (FileKind::Main, c)
    } else if let Some(c) = OVERLAY.captures(name) {
        (FileKind::Overlay, c)
    } else {
        (FileKind::MainNoSuffix, NO_SUFFIX.captures(name)?)
    };
    let date = NaiveDate::parse_from_str(&caps[1], "%Y-%m-%d").ok()?;
    let id = caps[2].to_string();
    let ext = match kind {
        FileKind::Overlay => "png".to_string(),
        FileKind::Main | FileKind::MainNoSuffix => caps[3].to_ascii_lowercase(),
    };
    Some(ParsedName {
        date,
        id,
        kind,
        ext,
    })
}

/// Parse the last component of a ZIP entry name or filesystem path, accepting
/// either separator (`memories/2021-12-25_A81B-main.jpg`).
pub fn parse_entry_path(path: &str) -> Option<ParsedName> {
    parse_filename(file_name_of(path))
}

/// Last path component for either separator; the whole string if there is none.
pub fn file_name_of(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn main_jpg() {
        let p = parse_filename("2021-12-25_A81B-main.jpg").unwrap();
        assert_eq!(p.date, d(2021, 12, 25));
        assert_eq!(p.id, "A81B");
        assert_eq!(p.kind, FileKind::Main);
        assert_eq!(p.ext, "jpg");
    }

    #[test]
    fn main_all_extensions() {
        for ext in ["jpg", "jpeg", "png", "mp4", "mov"] {
            let p = parse_filename(&format!("2019-07-04_3F2A9C1E-main.{ext}")).unwrap();
            assert_eq!(p.kind, FileKind::Main);
            assert_eq!(p.ext, ext);
        }
        assert!(parse_filename("2019-07-04_3F2A9C1E-main.gif").is_none());
        assert!(parse_filename("2019-07-04_3F2A9C1E-main.heic").is_none());
    }

    #[test]
    fn overlay() {
        let p = parse_filename("2019-07-04_3F2A9C1E-1234-overlay.png").unwrap();
        assert_eq!(p.kind, FileKind::Overlay);
        assert_eq!(p.id, "3F2A9C1E-1234");
        assert_eq!(p.ext, "png");
        // An "overlay" with a jpg extension is not an overlay; by the §3.2
        // patterns it is a suffix-less main whose ID happens to end in
        // "-overlay".
        let odd = parse_filename("2019-07-04_3F2A9C1E-overlay.jpg").unwrap();
        assert_eq!(odd.kind, FileKind::MainNoSuffix);
        assert_eq!(odd.id, "3F2A9C1E-overlay");
    }

    #[test]
    fn no_suffix_variant() {
        let p = parse_filename("2018-05-12_b0f3c2a1.mp4").unwrap();
        assert_eq!(p.kind, FileKind::MainNoSuffix);
        assert_eq!(p.id, "b0f3c2a1");
        assert_eq!(p.ext, "mp4");
        assert!(
            parse_filename("2018-05-12_b0f3c2a1.png").is_none(),
            "png only with -main"
        );
        assert!(
            parse_filename("2018-05-12_b0f3c2a1.mov").is_none(),
            "mov only with -main"
        );
    }

    #[test]
    fn main_wins_over_no_suffix_even_though_dash_is_an_id_char() {
        let p = parse_filename("2021-12-25_ABC-main.jpg").unwrap();
        assert_eq!((p.kind, p.id.as_str()), (FileKind::Main, "ABC"));
    }

    #[test]
    fn suffix_and_extension_are_case_insensitive_but_id_is_preserved() {
        let p = parse_filename("2021-12-25_aBc-MAIN.JPG").unwrap();
        assert_eq!(p.kind, FileKind::Main);
        assert_eq!(p.id, "aBc");
        assert_eq!(p.ext, "jpg");
        let o = parse_filename("2021-12-25_ABC-Overlay.PNG").unwrap();
        assert_eq!(o.kind, FileKind::Overlay);
        assert_eq!(p.pair_key(), o.pair_key());
    }

    #[test]
    fn rejects_other_shapes() {
        for bad in [
            "",
            "memories_history.json",
            "2021-12-25-ABC-main.jpg",
            "2021-12-25_ABC_main.jpg",
            "2021-12-25_-main.png",
            "2021-12-25_ABC-main.jpg.bak",
            "2021-12-25_ABC-main",
            "21-12-25_ABC-main.jpg",
            "2021-12-25_\u{c4}BC-main.jpg",
            "2021-12-25_A B-main.jpg",
            "\u{662}\u{660}\u{662}\u{661}-12-25_ABC-main.jpg",
        ] {
            assert!(parse_filename(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn empty_id_before_main_degrades_to_the_suffixless_shape() {
        // The main pattern needs at least one ID character, so this falls
        // through to the suffix-less pattern, where "-main" is a legal ID.
        let p = parse_filename("2021-12-25_-main.jpg").unwrap();
        assert_eq!((p.kind, p.id.as_str()), (FileKind::MainNoSuffix, "-main"));
    }

    #[test]
    fn impossible_calendar_date_is_unrecognised() {
        assert!(parse_filename("2021-13-01_ABC-main.jpg").is_none());
        assert!(parse_filename("2021-02-30_ABC-main.jpg").is_none());
        assert!(
            parse_filename("2020-02-29_ABC-main.jpg").is_some(),
            "leap day"
        );
    }

    #[test]
    fn entry_paths() {
        let p = parse_entry_path("memories/2021-12-25_A81B-main.jpg").unwrap();
        assert_eq!(p.id, "A81B");
        let p = parse_entry_path(r"mydata\memories\2021-12-25_A81B-overlay.png").unwrap();
        assert_eq!(p.kind, FileKind::Overlay);
        assert_eq!(file_name_of("plain.jpg"), "plain.jpg");
        assert_eq!(file_name_of("a/b/"), "");
    }
}
