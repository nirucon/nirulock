//! Commodore 64 SID tunes for `[pause_music]`: the PSID/RSID header, the
//! HVSC song length database, the play order and the info line.
//!
//! Everything here is pure (no processes, no X) so that it can be tested;
//! [`crate::music`] does the I/O.

use std::{
    cmp::Ordering,
    collections::HashMap,
    path::{Path, PathBuf},
};

/// What the info line shows about a tune, from its PSID/RSID header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub title: String,
    pub author: String,
    pub released: String,
    /// Number of subtunes (at least 1).
    pub songs: u16,
    /// The subtune that plays by default, 1-based.
    pub start_song: u16,
}

/// Parse a PSID or RSID header (versions 1 to 4). Returns `None` for
/// anything that is not one.
pub fn parse_header(data: &[u8]) -> Option<Header> {
    if data.len() < 0x76 || !(data.starts_with(b"PSID") || data.starts_with(b"RSID")) {
        return None;
    }
    let be16 = |at: usize| u16::from_be_bytes([data[at], data[at + 1]]);
    let version = be16(4);
    let data_offset = usize::from(be16(6));
    if !(1..=4).contains(&version) || data_offset < 0x76 || data_offset > data.len() {
        return None;
    }
    let songs = be16(0x0e).max(1);
    let start_song = match be16(0x10) {
        0 => 1,
        n => n.min(songs),
    };
    Some(Header {
        title: latin1_field(&data[0x16..0x36]),
        author: latin1_field(&data[0x36..0x56]),
        released: latin1_field(&data[0x56..0x76]),
        songs,
        start_song,
    })
}

/// A 32-byte header string: Latin-1 (the HVSC convention), NUL padded,
/// control characters removed.
fn latin1_field(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .map(|&b| char::from(b))
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// A header field that says "unknown".
fn known(field: &str) -> Option<&str> {
    let field = field.trim();
    (!field.is_empty() && field != "<?>" && field != "?").then_some(field)
}

/// The year from a `released` field such as "1987 Firebird", "1989-90 Ocean"
/// or "19?? Unknown"; `None` when the field does not start with one.
pub fn year(released: &str) -> Option<&str> {
    let released = known(released)?;
    let end = released
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '?' || *c == '-'))
        .map_or(released.len(), |(i, _)| i);
    let year = released[..end].trim_end_matches('-');
    (year.len() >= 4 && year.starts_with(|c: char| c.is_ascii_digit())).then_some(year)
}

/// "♪ Title — Author · Year  ·  Esc stops the music". A missing title falls
/// back to the file name; a missing author or year is left out.
pub fn info_line(header: Option<&Header>, file: &Path) -> String {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().replace('_', " "))
        .unwrap_or_default();
    let title = header
        .and_then(|h| known(&h.title))
        .map_or(stem, str::to_string);
    let mut line = format!("♪ {title}");
    if let Some(author) = header.and_then(|h| known(&h.author)) {
        line.push_str(" — ");
        line.push_str(author);
    }
    if let Some(year) = header.and_then(|h| year(&h.released)) {
        line.push_str(" · ");
        line.push_str(year);
    }
    line.push_str("  ·  Esc stops the music");
    line
}

/// Natural order: digit runs compare as numbers ("2-" before "10-"), the
/// rest compares without case. Ties fall back to the plain bytes, so the
/// order is total and stable.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut run = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        run.push(c);
                        it.next();
                    }
                    run
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (m0, n0) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let order = m0.len().cmp(&n0.len()).then_with(|| m0.cmp(n0));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(p), Some(q)) => {
                let order = p.to_lowercase().cmp(q.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// Sort paths (relative to the music folder) in natural order.
pub fn natural_sort(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
}

/// The HVSC song length database (`Songlengths.md5`): lengths in seconds per
/// subtune, by the MD5 of the whole file, and the MD5 by the file name from
/// the comment above each entry.
#[derive(Debug, Default)]
pub struct Songlengths {
    by_md5: HashMap<String, Vec<u32>>,
    md5_by_name: HashMap<String, Option<String>>,
}

impl Songlengths {
    pub fn parse(text: &str) -> Self {
        let mut db = Self::default();
        let mut name: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(comment) = line.strip_prefix(';') {
                name = comment
                    .trim()
                    .rsplit('/')
                    .next()
                    .filter(|n| !n.is_empty())
                    .map(str::to_lowercase);
                continue;
            }
            let Some((md5, lengths)) = line.split_once('=') else {
                continue;
            };
            let md5 = md5.trim().to_ascii_lowercase();
            if md5.len() != 32 || !md5.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let lengths: Vec<u32> = lengths
                .split_whitespace()
                .filter_map(parse_length)
                .collect();
            if lengths.is_empty() {
                continue;
            }
            if let Some(name) = name.take() {
                // A name that appears twice cannot be trusted for lookups.
                db.md5_by_name
                    .entry(name)
                    .and_modify(|m| *m = None)
                    .or_insert_with(|| Some(md5.clone()));
            }
            db.by_md5.insert(md5, lengths);
        }
        db
    }

    pub fn is_empty(&self) -> bool {
        self.by_md5.is_empty()
    }

    /// The length of subtune `song` (1-based) of the file with this MD5, or
    /// failing that of the only database entry with this file name.
    pub fn length(&self, md5: &str, file_name: &str, song: u16) -> Option<u32> {
        let lengths = self.by_md5.get(md5).or_else(|| {
            let md5 = self.md5_by_name.get(&file_name.to_lowercase())?.as_ref()?;
            self.by_md5.get(md5)
        })?;
        let index = usize::from(song.max(1) - 1);
        lengths.get(index).or(lengths.first()).copied()
    }
}

/// "3:25", "0:07.500" or "2:41(G)" to whole seconds, rounded up; a zero
/// length is unknown.
fn parse_length(text: &str) -> Option<u32> {
    let text = text.split('(').next()?;
    let (minutes, seconds) = text.split_once(':')?;
    let minutes: u32 = minutes.parse().ok()?;
    let seconds: f64 = seconds.parse().ok()?;
    if !(0.0..60.0).contains(&seconds) {
        return None;
    }
    let total = (f64::from(minutes) * 60.0 + seconds).ceil() as u32;
    (total > 0).then_some(total)
}

/// MD5 (RFC 1321) as lowercase hex: the key of `Songlengths.md5`.
pub fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    for chunk in message.chunks_exact(64) {
        let m: Vec<u32> = chunk
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    }
    state
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A minimal PSID v2 header with the given strings.
    pub fn header(magic: &[u8; 4], title: &str, author: &str, released: &str) -> Vec<u8> {
        let mut h = vec![0u8; 0x7c];
        h[..4].copy_from_slice(magic);
        h[4..6].copy_from_slice(&2u16.to_be_bytes());
        h[6..8].copy_from_slice(&0x7cu16.to_be_bytes());
        h[0x0e..0x10].copy_from_slice(&3u16.to_be_bytes());
        h[0x10..0x12].copy_from_slice(&2u16.to_be_bytes());
        for (at, text) in [(0x16, title), (0x36, author), (0x56, released)] {
            let bytes: Vec<u8> = text.chars().map(|c| c as u32 as u8).collect();
            h[at..at + bytes.len()].copy_from_slice(&bytes);
        }
        h
    }

    #[test]
    fn natural_order_sorts_numbers_by_value() {
        let mut names: Vec<PathBuf> = ["10-ten.sid", "2-two.sid", "1-one.sid", "B.sid", "a.sid"]
            .iter()
            .map(PathBuf::from)
            .collect();
        natural_sort(&mut names);
        let names: Vec<_> = names
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["1-one.sid", "2-two.sid", "10-ten.sid", "a.sid", "B.sid"]
        );
        assert_eq!(natural_cmp("track9", "track10"), Ordering::Less);
        assert_eq!(natural_cmp("Track 02", "track 2"), Ordering::Less);
        assert_eq!(natural_cmp("x", "x"), Ordering::Equal);
        assert_eq!(natural_cmp("a/2.sid", "a/10.sid"), Ordering::Less);
        assert_eq!(natural_cmp("abc", "ab"), Ordering::Greater);
        // A long digit run does not overflow anything.
        assert_eq!(
            natural_cmp("99999999999999999999999", "100000000000000000000000"),
            Ordering::Less
        );
    }

    #[test]
    fn psid_and_rsid_headers_are_parsed() {
        let h = parse_header(&header(
            b"PSID",
            "Comic Bakery",
            "Martin Galway",
            "1984 Imagine",
        ))
        .unwrap();
        assert_eq!(h.title, "Comic Bakery");
        assert_eq!(h.author, "Martin Galway");
        assert_eq!(h.released, "1984 Imagine");
        assert_eq!((h.songs, h.start_song), (3, 2));
        let r = parse_header(&header(b"RSID", "Ålesund", "Jörg", "19?? Ödla")).unwrap();
        assert_eq!(r.title, "Ålesund");
        assert_eq!(r.author, "Jörg");
        assert_eq!(year(&r.released), Some("19??"));
    }

    #[test]
    fn bad_headers_are_rejected() {
        assert!(parse_header(b"PSID").is_none());
        assert!(parse_header(&[0u8; 200]).is_none());
        let mut bad = header(b"PSID", "x", "y", "z");
        bad[4..6].copy_from_slice(&9u16.to_be_bytes());
        assert!(parse_header(&bad).is_none());
        let mut bad = header(b"PSID", "x", "y", "z");
        bad[6..8].copy_from_slice(&0xffffu16.to_be_bytes());
        assert!(parse_header(&bad).is_none());
        // Control characters never reach the screen; start song 0 means 1.
        let mut odd = header(b"PSID", "a\x07b", "y", "z");
        odd[0x10..0x12].copy_from_slice(&0u16.to_be_bytes());
        let odd = parse_header(&odd).unwrap();
        assert_eq!(odd.title, "ab");
        assert_eq!(odd.start_song, 1);
    }

    #[test]
    fn years_come_from_the_released_field() {
        assert_eq!(year("1987 Firebird"), Some("1987"));
        assert_eq!(year("1989-90 Ocean"), Some("1989-90"));
        assert_eq!(year("2004 "), Some("2004"));
        assert_eq!(year("Firebird"), None);
        assert_eq!(year("<?>"), None);
        assert_eq!(year("12 x"), None);
    }

    #[test]
    fn the_info_line_names_title_author_and_year() {
        let h = parse_header(&header(b"PSID", "Delta", "Rob Hubbard", "1987 Thalamus")).unwrap();
        assert_eq!(
            info_line(Some(&h), Path::new("/m/1-Delta.sid")),
            "♪ Delta — Rob Hubbard · 1987  ·  Esc stops the music"
        );
        let unknown = parse_header(&header(b"PSID", "<?>", "<?>", "<?>")).unwrap();
        assert_eq!(
            info_line(Some(&unknown), Path::new("/m/My_Tune.sid")),
            "♪ My Tune  ·  Esc stops the music"
        );
        assert_eq!(
            info_line(None, Path::new("x.sid")),
            "♪ x  ·  Esc stops the music"
        );
    }

    #[test]
    fn songlengths_are_parsed_and_looked_up() {
        let db = Songlengths::parse(
            "[Database]\n\
             ; /MUSICIANS/H/Hubbard_Rob/Delta.sid\n\
             0123456789abcdef0123456789ABCDEF=3:25 0:07.500 1:00(G)\n\
             ; /DEMOS/A/Same.sid\n\
             11111111111111111111111111111111=1:00\n\
             ; /GAMES/B/Same.sid\n\
             22222222222222222222222222222222=2:00\n\
             garbage line\n\
             3333=1:00\n\
             44444444444444444444444444444444=0:00\n",
        );
        assert!(!db.is_empty());
        let md5 = "0123456789abcdef0123456789abcdef";
        assert_eq!(db.length(md5, "x.sid", 1), Some(205));
        assert_eq!(db.length(md5, "x.sid", 2), Some(8));
        assert_eq!(db.length(md5, "x.sid", 3), Some(60));
        // A subtune past the list uses the first length.
        assert_eq!(db.length(md5, "x.sid", 9), Some(205));
        // By file name when the MD5 differs, unless the name is ambiguous.
        assert_eq!(
            db.length("f".repeat(32).as_str(), "delta.SID", 1),
            Some(205)
        );
        assert_eq!(db.length("f".repeat(32).as_str(), "Same.sid", 1), None);
        assert_eq!(db.length(&"4".repeat(32), "z.sid", 1), None);
        assert!(Songlengths::parse("").is_empty());
    }

    #[test]
    fn md5_matches_rfc_1321() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }
}
