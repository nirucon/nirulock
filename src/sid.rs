//! The tunes for `[pause_music]`: the PSID/RSID header of a Commodore 64 SID
//! tune, the HVSC song length database, the play order and the info lines of
//! SID tunes and audio files.
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

/// The info line of an audio file (NIRUX's own music, or any `.ogg`,
/// `.flac`, ... in the folder). `nirux-N-some-name.ogg` is "♪ Some Name  ·
/// NIRUX N"; any other file is its stem with `-` and `_` as spaces. Then
/// "  ·  Esc stops the music", as for a SID tune.
pub fn audio_info_line(file: &Path) -> String {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = match nirux_tune(&stem) {
        Some((number, name)) => format!("{}  ·  NIRUX {number}", title_case(name)),
        None => words(&stem).join(" "),
    };
    format!("♪ {title}  ·  Esc stops the music")
}

/// The tune as "Artist - Title" for the demo layout's scroller and the
/// attract layout's HUD: only who and what, never the info line's numbers,
/// years or hints. A SID tune is "Author - Title" from its header (the
/// title alone without an author, the file name without a title).
pub fn tune_name(header: Option<&Header>, file: &Path) -> String {
    let title = header
        .and_then(|h| known(&h.title))
        .map_or_else(|| words(&stem_of(file)).join(" "), str::to_string);
    match header.and_then(|h| known(&h.author)) {
        Some(author) => format!("{author} - {title}"),
        None => title,
    }
}

/// An audio file as "Artist - Title": the `ARTIST` and `TITLE` tags when
/// the file has them (`tags`, from [`vorbis_tags`]); otherwise
/// `nirux-N-some-name.ogg` is "NIRUX - Some Name" and any other file its
/// stem with `-` and `_` as spaces (a `TITLE` alone without an `ARTIST`
/// is shown alone).
pub fn audio_tune_name(file: &Path, tags: &Tags) -> String {
    let stem = stem_of(file);
    let tag = |t: &Option<String>| {
        t.as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    match (tag(&tags.artist), tag(&tags.title)) {
        (Some(artist), Some(title)) => format!("{artist} - {title}"),
        (None, Some(title)) if nirux_tune(&stem).is_none() => title,
        _ => match nirux_tune(&stem) {
            Some((_, name)) => format!("NIRUX - {}", title_case(name)),
            None => words(&stem).join(" "),
        },
    }
}

fn stem_of(file: &Path) -> String {
    file.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The `ARTIST` and `TITLE` of an audio file's Vorbis comments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tags {
    pub artist: Option<String>,
    pub title: Option<String>,
}

/// How much of an audio file [`vorbis_tags`] looks at: the comment header
/// of Ogg Vorbis, Opus and FLAC is near the start.
pub const TAG_BYTES: usize = 64 * 1024;

/// The Vorbis comments in the first bytes of an Ogg Vorbis, Ogg Opus or
/// FLAC file. Anything else, or a comment block that does not fit or does
/// not parse, gives no tags (and the caller falls back to the file name).
/// Only `ARTIST` and `TITLE` are read: a `COMMENT` or `DESCRIPTION` never
/// shows.
pub fn vorbis_tags(data: &[u8]) -> Tags {
    let block = if data.starts_with(b"OggS") {
        find(data, b"\x03vorbis")
            .map(|at| at + 7)
            .or_else(|| find(data, b"OpusTags").map(|at| at + 8))
    } else if data.starts_with(b"fLaC") {
        flac_comments(data)
    } else {
        None
    };
    block
        .and_then(|at| data.get(at..))
        .and_then(comments)
        .unwrap_or_default()
}

fn find(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).position(|w| w == needle)
}

/// The start of a FLAC file's VORBIS_COMMENT block.
fn flac_comments(data: &[u8]) -> Option<usize> {
    let mut at = 4;
    loop {
        let head = data.get(at..at + 4)?;
        let kind = head[0] & 0x7f;
        let len = usize::from(head[1]) << 16 | usize::from(head[2]) << 8 | usize::from(head[3]);
        if kind == 4 {
            return Some(at + 4);
        }
        if head[0] & 0x80 != 0 {
            return None;
        }
        at += 4 + len;
    }
}

/// A Vorbis comment block: vendor string, count, `KEY=value` strings, all
/// lengths 32-bit little endian.
fn comments(data: &[u8]) -> Option<Tags> {
    let le32 = |at: usize| -> Option<usize> {
        let b = data.get(at..at + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let mut at = 4usize.checked_add(le32(0)?)?;
    let count = le32(at)?.min(1024);
    at += 4;
    let mut tags = Tags::default();
    for _ in 0..count {
        let len = le32(at)?;
        let text = data.get(at + 4..(at + 4).checked_add(len)?)?;
        at += 4 + len;
        let Ok(text) = std::str::from_utf8(text) else {
            continue;
        };
        let Some((key, value)) = text.split_once('=') else {
            continue;
        };
        let value: String = value.chars().filter(|c| !c.is_control()).take(80).collect();
        if key.eq_ignore_ascii_case("ARTIST") && tags.artist.is_none() {
            tags.artist = Some(value);
        } else if key.eq_ignore_ascii_case("TITLE") && tags.title.is_none() {
            tags.title = Some(value);
        }
    }
    Some(tags)
}

/// The words of a file stem, split at `-` and `_`.
fn words(stem: &str) -> Vec<&str> {
    stem.split(['-', '_']).filter(|w| !w.is_empty()).collect()
}

/// `nirux-N-name`: the number (without leading zeros) and the name.
fn nirux_tune(stem: &str) -> Option<(&str, &str)> {
    let rest = stem
        .get(..6)
        .filter(|prefix| prefix.eq_ignore_ascii_case("nirux-"))
        .and_then(|_| stem.get(6..))?;
    let (number, name) = rest.split_once('-')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) || words(name).is_empty() {
        return None;
    }
    let number = number.trim_start_matches('0');
    Some((if number.is_empty() { "0" } else { number }, name))
}

/// "some-name" as "Some Name": each word starts with a capital letter, the
/// rest is kept as written.
fn title_case(name: &str) -> String {
    words(name)
        .iter()
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    fn tune_names_are_artist_and_title_only() {
        let none = Tags::default();
        assert_eq!(
            audio_tune_name(Path::new("/m/nirux-11-gimle.ogg"), &none),
            "NIRUX - Gimle"
        );
        // Tags win over the file name: NIRUX's Gymnopedie is by Satie.
        let tagged = Tags {
            artist: Some("Erik Satie".into()),
            title: Some("Gymnopedie No. 1".into()),
        };
        assert_eq!(
            audio_tune_name(Path::new("nirux-12-gymnopedie.ogg"), &tagged),
            "Erik Satie - Gymnopedie No. 1"
        );
        assert_eq!(
            audio_tune_name(Path::new("nirux-3-the-long_night.ogg"), &none),
            "NIRUX - The Long Night"
        );
        let gimle = Tags {
            artist: Some("NIRUX".into()),
            title: Some("Gimle".into()),
        };
        assert_eq!(
            audio_tune_name(Path::new("nirux-11-gimle.ogg"), &gimle),
            "NIRUX - Gimle"
        );
        assert_eq!(
            audio_tune_name(Path::new("my_song-mix.flac"), &tagged),
            "Erik Satie - Gymnopedie No. 1"
        );
        assert_eq!(
            audio_tune_name(Path::new("my_song-mix.flac"), &none),
            "my song mix"
        );
        let title_only = Tags {
            artist: Some("  ".into()),
            title: Some("Alone".into()),
        };
        assert_eq!(audio_tune_name(Path::new("x.ogg"), &title_only), "Alone");
        let h = parse_header(&header(
            b"PSID",
            "Comic Bakery",
            "Martin Galway",
            "1984 Imagine",
        ));
        assert_eq!(
            tune_name(h.as_ref(), Path::new("a.sid")),
            "Martin Galway - Comic Bakery"
        );
        let h = parse_header(&header(b"PSID", "<?>", "<?>", "1984"));
        assert_eq!(
            tune_name(h.as_ref(), Path::new("/m/my_tune.sid")),
            "my tune"
        );
        assert_eq!(tune_name(None, Path::new("/m/1-test.sid")), "1 test");
        for name in [
            audio_tune_name(Path::new("nirux-11-gimle.ogg"), &none),
            tune_name(h.as_ref(), Path::new("a.sid")),
        ] {
            assert!(!name.contains("Esc") && !name.contains('·') && !name.contains('♪'));
        }
    }

    fn comment_block(items: &[&str]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&6u32.to_le_bytes());
        b.extend_from_slice(b"vendor");
        b.extend_from_slice(&(items.len() as u32).to_le_bytes());
        for item in items {
            b.extend_from_slice(&(item.len() as u32).to_le_bytes());
            b.extend_from_slice(item.as_bytes());
        }
        b
    }

    #[test]
    fn vorbis_comments_give_artist_and_title_only() {
        let items = [
            "DESCRIPTION=A minor, 64 BPM: a calm, hopeful close",
            "title=Gimle",
            "ARTIST=NIRUX",
            "ALBUM=NIRUX music",
        ];
        let mut ogg = b"OggS\0\x02junk\x01vorbis....OggS\0\0\x03vorbis".to_vec();
        ogg.extend(comment_block(&items));
        let want = Tags {
            artist: Some("NIRUX".into()),
            title: Some("Gimle".into()),
        };
        assert_eq!(vorbis_tags(&ogg), want);
        let mut opus = b"OggS....OpusHead....OggS....OpusTags".to_vec();
        opus.extend(comment_block(&items));
        assert_eq!(vorbis_tags(&opus), want);
        // FLAC: STREAMINFO, then the comments as the last block.
        let mut flac = b"fLaC".to_vec();
        flac.extend([0, 0, 0, 2, 9, 9]);
        let block = comment_block(&items);
        flac.extend([0x84, 0, 0, block.len() as u8]);
        flac.extend(block);
        assert_eq!(vorbis_tags(&flac), want);
        // Truncated, foreign or empty data: no tags, no panic.
        assert_eq!(vorbis_tags(&ogg[..ogg.len() - 10]), Tags::default());
        assert_eq!(vorbis_tags(b"RIFF....WAVE"), Tags::default());
        assert_eq!(vorbis_tags(b""), Tags::default());
        assert_eq!(vorbis_tags(b"fLaC\0\0"), Tags::default());
        let mut huge = b"OggS\x03vorbis".to_vec();
        huge.extend(u32::MAX.to_le_bytes());
        assert_eq!(vorbis_tags(&huge), Tags::default());
    }

    #[test]
    fn audio_files_are_named_from_the_file_name() {
        assert_eq!(
            audio_info_line(Path::new("/m/nirux-1-ginnungagap.ogg")),
            "♪ Ginnungagap  ·  NIRUX 1  ·  Esc stops the music"
        );
        assert_eq!(
            audio_info_line(Path::new("nirux-11-the-long_night.OGG")),
            "♪ The Long Night  ·  NIRUX 11  ·  Esc stops the music"
        );
        assert_eq!(
            audio_info_line(Path::new("NIRUX-03-Odin-RIDES.flac")),
            "♪ Odin RIDES  ·  NIRUX 3  ·  Esc stops the music"
        );
        assert_eq!(
            audio_info_line(Path::new("/m/my_song-mix.wav")),
            "♪ my song mix  ·  Esc stops the music"
        );
        // Not the NIRUX pattern: the plain stem.
        for (file, line) in [
            ("nirux-x-name.ogg", "♪ nirux x name  ·  Esc stops the music"),
            ("nirux-2.ogg", "♪ nirux 2  ·  Esc stops the music"),
            ("nirux-2-.ogg", "♪ nirux 2  ·  Esc stops the music"),
            ("niruxen-2-a.ogg", "♪ niruxen 2 a  ·  Esc stops the music"),
            ("ö.opus", "♪ ö  ·  Esc stops the music"),
        ] {
            assert_eq!(audio_info_line(Path::new(file)), line, "{file}");
        }
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
