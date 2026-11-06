//! What the CAVA panel of the `cava` layout shows: the spectrum while music
//! plays, a quote otherwise.
//!
//! `visual = "auto"` (the default) starts the CAVA child only when the
//! [`audio`](crate::audio) detector hears music, and stops it, not merely
//! pauses it, after its silence window. Without music the panel shows one
//! quote from the [`quotes`](crate::quotes) collections, changed every
//! [`ROTATE_EVERY`] with a soft fade and not redrawn in between. `"cava"` is
//! the spectrum alone, exactly as before; `"quotes"` is quotes alone.
//!
//! Every change of content fades the old one out and the new one in; the
//! spectrum's own ease from flat bars (see `cava::Spectrum`) runs under the
//! fade-in, so the first frames never snap. With the display off nothing
//! runs: no CAVA, no peak monitor, no quote changes.
//!
//! The decisions are in [`Panel`], free of I/O so that they can be tested.
//! Shared verbatim with NIRUSCREENSAVER, like `cava.rs`.

use crate::{
    audio::{Audio, Heard},
    cava::{self, Cava},
    core::{Surface, TextRenderer},
    palette::Palette,
    quotes::{self, Arranged, Deck, Loader, Quote},
};
use std::{
    io::Read,
    os::{
        fd::{AsRawFd, RawFd},
        unix::net::UnixStream,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// The `visual` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Cava,
    Quotes,
    None,
}

impl Mode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "cava" => Some(Self::Cava),
            "quotes" => Some(Self::Quotes),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cava => "cava",
            Self::Quotes => "quotes",
            Self::None => "none",
        }
    }
}

/// How long a quote stays before the next one.
pub const ROTATE_EVERY: Duration = Duration::from_secs(60);
/// After start or a display wake-up, how long an undecided audio state
/// (a stream plays but has not been loud long enough) keeps the panel
/// empty, so music does not show a quote for a moment first.
pub const DECIDE_WITHIN: Duration = Duration::from_millis(800);
const QUOTE_OUT: Duration = Duration::from_millis(400);
const QUOTE_IN: Duration = Duration::from_millis(700);
const CAVA_OUT: Duration = Duration::from_millis(400);
const CAVA_IN: Duration = Duration::from_millis(300);

/// What the panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    Blank,
    Quote,
    Cava,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Steady,
    Out(Instant),
    In(Instant),
}

/// What [`Panel::step`] asks the owner to do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Actions {
    /// Start the CAVA child now (it warms up during the fade-out).
    pub start_cava: bool,
    /// Stop and reap the CAVA child.
    pub stop_cava: bool,
    /// Pick another quote (the previous one has faded out).
    pub new_quote: bool,
    /// Ask the loader whether the file changed.
    pub reload: bool,
    /// Draw the panel once (a change without a fade).
    pub repaint: bool,
}

/// The content and fade state machine.
#[derive(Debug)]
pub struct Panel {
    mode: Mode,
    fades: bool,
    shown: Content,
    phase: Phase,
    display_on: bool,
    next_rotation: Option<Instant>,
    decide_until: Instant,
    /// The quote changes at the end of the current fade-out.
    rotating: bool,
}

impl Panel {
    pub fn new(mode: Mode, fades: bool, now: Instant) -> Self {
        Self {
            mode,
            fades,
            shown: Content::Blank,
            phase: Phase::Steady,
            display_on: true,
            next_rotation: None,
            decide_until: now + DECIDE_WITHIN,
            rotating: false,
        }
    }

    pub fn shown(&self) -> Content {
        self.shown
    }

    /// What should be shown now.
    fn target(&self, heard: Heard, have_quotes: bool, now: Instant) -> Content {
        let quote = if have_quotes {
            Content::Quote
        } else {
            Content::Blank
        };
        match self.mode {
            Mode::None => Content::Blank,
            Mode::Cava => Content::Cava,
            Mode::Quotes => quote,
            Mode::Auto => match heard {
                Heard::Playing => Content::Cava,
                // Undecided right after start: wait for the detector rather
                // than flash a quote before the spectrum.
                Heard::Listening if self.shown == Content::Blank && now < self.decide_until => {
                    Content::Blank
                }
                _ => quote,
            },
        }
    }

    /// Brightness of the shown content, 0.0..=1.0.
    pub fn level(&self, now: Instant) -> f32 {
        match self.phase {
            Phase::Steady => 1.0,
            Phase::Out(since) => 1.0 - ease(now, since, self.fade_out()),
            Phase::In(since) => ease(now, since, self.fade_in()),
        }
    }

    fn fade_out(&self) -> Duration {
        match self.shown {
            Content::Cava => CAVA_OUT,
            _ => QUOTE_OUT,
        }
    }

    fn fade_in(&self) -> Duration {
        match self.shown {
            Content::Cava => CAVA_IN,
            _ => QUOTE_IN,
        }
    }

    /// True while a fade runs and the panel must be drawn every frame.
    pub fn fading(&self) -> bool {
        self.phase != Phase::Steady
    }

    /// The next moment `step` has something to do without any input.
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        if !self.display_on || self.fading() {
            return None;
        }
        let decide =
            (self.shown == Content::Blank && now < self.decide_until).then_some(self.decide_until);
        match (self.next_rotation, decide) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Follow the display power state. Off ends every fade and timer; the
    /// spectrum is stopped (a quote stays as the static frame), on starts a
    /// fresh decision window.
    pub fn display(&mut self, on: bool, now: Instant) -> Actions {
        let mut actions = Actions::default();
        if on == self.display_on {
            return actions;
        }
        self.display_on = on;
        if on {
            self.decide_until = now + DECIDE_WITHIN;
            if self.shown == Content::Quote {
                self.next_rotation = Some(now + ROTATE_EVERY);
            }
            return actions;
        }
        self.phase = Phase::Steady;
        self.rotating = false;
        self.next_rotation = None;
        if self.shown == Content::Cava && self.mode != Mode::Cava {
            actions.stop_cava = true;
            self.shown = Content::Blank;
        }
        actions.repaint = true;
        actions
    }

    /// Advance the state machine.
    pub fn step(&mut self, heard: Heard, have_quotes: bool, now: Instant) -> Actions {
        let mut actions = Actions::default();
        if !self.display_on {
            return actions;
        }
        let target = self.target(heard, have_quotes, now);
        match self.phase {
            Phase::Steady => {
                let rotate = self.shown == Content::Quote
                    && target == Content::Quote
                    && self.next_rotation.is_some_and(|at| now >= at);
                if rotate {
                    self.rotating = true;
                    actions.reload = true;
                }
                if target != self.shown || rotate {
                    self.next_rotation = None;
                    if target == Content::Cava {
                        actions.start_cava = true;
                    }
                    if self.shown == Content::Blank {
                        self.enter(target, now, &mut actions);
                    } else {
                        self.phase = Phase::Out(now);
                    }
                }
            }
            Phase::Out(since) => {
                if target == Content::Cava && self.shown != Content::Cava {
                    // Idempotent for the owner: started at most once.
                    actions.start_cava = true;
                }
                if now.saturating_duration_since(since) >= self.fade_out() {
                    if self.shown == Content::Cava && target != Content::Cava {
                        actions.stop_cava = true;
                    }
                    self.rotating = false;
                    self.enter(target, now, &mut actions);
                }
            }
            Phase::In(since) => {
                if now.saturating_duration_since(since) >= self.fade_in() {
                    self.phase = Phase::Steady;
                    actions.repaint = true;
                    if self.shown == Content::Quote {
                        self.next_rotation = Some(now + ROTATE_EVERY);
                    }
                }
            }
        }
        if !self.fades && self.fading() {
            // Reduced motion: jump to the end of the fade.
            let end = now + Duration::from_secs(3600);
            let more = self.step(heard, have_quotes, end);
            actions.merge(more);
            if let Some(at) = self.next_rotation.as_mut() {
                *at = now + ROTATE_EVERY;
            }
            actions.repaint = true;
        }
        actions
    }

    fn enter(&mut self, target: Content, now: Instant, actions: &mut Actions) {
        self.shown = target;
        if target == Content::Quote {
            actions.new_quote = true;
        }
        // Mode cava shows the spectrum at once, as before quotes existed.
        self.phase = if target == Content::Blank || self.mode == Mode::Cava {
            actions.repaint = true;
            Phase::Steady
        } else {
            Phase::In(now)
        };
    }
}

impl Actions {
    fn merge(&mut self, other: Actions) {
        self.start_cava |= other.start_cava;
        self.stop_cava |= other.stop_cava;
        self.new_quote |= other.new_quote;
        self.reload |= other.reload;
        self.repaint |= other.repaint;
    }
}

/// Smoothstep progress of a fade of `length` that began at `since`.
fn ease(now: Instant, since: Instant, length: Duration) -> f32 {
    let t = (now.saturating_duration_since(since).as_secs_f32() / length.as_secs_f32().max(1e-3))
        .clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |shift: u32| {
        let a = ((a >> shift) & 255) as f32;
        let b = ((b >> shift) & 255) as f32;
        ((a + (b - a) * t).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// The palette faded towards its background by `level` (1.0 is unchanged).
pub fn faded(pal: &Palette, level: f32) -> Palette {
    if level >= 1.0 {
        return pal.clone();
    }
    let fade = |colour| mix(pal.background, colour, level);
    Palette {
        background: pal.background,
        foreground: fade(pal.foreground),
        muted: fade(pal.muted),
        border: fade(pal.border),
        focus: fade(pal.focus),
    }
}

/// The panel with its CAVA child, audio detector, quotes and fonts.
pub struct Visual {
    panel: Panel,
    cava: Option<Cava>,
    audio: Option<Audio>,
    loader: Option<Loader>,
    quotes: Vec<Quote>,
    quote: Option<Quote>,
    arranged: Option<((i16, i16, u16, u16), Arranged)>,
    fonts: Vec<TextRenderer>,
    base: TextRenderer,
    base_size: f32,
    /// Woken by the detector and the loader.
    wake: Option<(UnixStream, UnixStream)>,
    display_on: bool,
    /// A change that needs one repaint without a fade.
    repaint: bool,
    deck: Deck,
}

impl Visual {
    /// `renderer` supplies the theme font; `base_size` is the largest quote
    /// size. `app` names the client for the sound server. `collections` is
    /// the `quotes` setting: patterns of the collections to show.
    pub fn new(
        mode: Mode,
        fades: bool,
        renderer: &TextRenderer,
        base_size: f32,
        app: &str,
        collections: &[String],
    ) -> Self {
        let now = Instant::now();
        let wake = UnixStream::pair().ok().and_then(|(read, write)| {
            read.set_nonblocking(true).ok()?;
            write.set_nonblocking(true).ok()?;
            Some((read, write))
        });
        let writer = || wake.as_ref().and_then(|(_, w)| w.try_clone().ok());
        let audio = (mode == Mode::Auto).then(|| Audio::start(app, writer()));
        let loader = matches!(mode, Mode::Auto | Mode::Quotes)
            .then(|| Loader::start(writer(), collections.to_vec()))
            .flatten();
        let mut visual = Self {
            panel: Panel::new(mode, fades, now),
            cava: None,
            audio,
            loader,
            quotes: Vec::new(),
            quote: None,
            arranged: None,
            fonts: Vec::new(),
            base: renderer.at_size(base_size),
            base_size,
            wake,
            display_on: true,
            repaint: false,
            deck: Deck::new(seed()),
        };
        visual.tick(now);
        visual
    }

    /// True while the spectrum is live (sound, display on, not fading).
    pub fn cava_live(&self) -> bool {
        self.panel.shown() == Content::Cava && self.cava.as_ref().is_some_and(Cava::animating)
    }

    /// True while the panel must be repainted at 30 fps.
    pub fn animating(&self) -> bool {
        self.display_on && (self.panel.fading() || self.cava_live())
    }

    /// The next moment [`Visual::tick`] has work without any input.
    pub fn deadline(&self) -> Option<Instant> {
        self.panel.deadline(Instant::now())
    }

    /// Descriptors to wait on: the spectrum's silence wake-up and the
    /// detector/loader wake-up.
    pub fn wake_fds(&self) -> [Option<RawFd>; 2] {
        let cava = if self.panel.fading() {
            None
        } else {
            self.cava.as_ref().and_then(Cava::wake_fd)
        };
        [cava, self.wake.as_ref().map(|(read, _)| read.as_raw_fd())]
    }

    /// Consume the wake-ups that `wake_fds` reported.
    pub fn woke(&mut self, ready: [bool; 2]) {
        if ready[0] {
            if let Some(cava) = self.cava.as_mut() {
                cava.woke();
            }
        }
        if ready[1] {
            if let Some((read, _)) = self.wake.as_mut() {
                let mut buf = [0u8; 64];
                while matches!(read.read(&mut buf), Ok(n) if n > 0) {}
            }
        }
        self.tick(Instant::now());
    }

    /// Follow the display power state.
    pub fn set_display(&mut self, on: bool) {
        if on == self.display_on {
            return;
        }
        self.display_on = on;
        let now = Instant::now();
        if let Some(audio) = self.audio.as_ref() {
            audio.set_enabled(on);
        }
        if let Some(cava) = self.cava.as_mut() {
            // Off stops and reaps the child; on restarts it (mode cava).
            cava.set_display(on);
        }
        let actions = self.panel.display(on, now);
        self.apply(actions);
        self.tick(now);
    }

    /// True once after a change that needs a single repaint.
    pub fn take_repaint(&mut self) -> bool {
        std::mem::take(&mut self.repaint)
    }

    /// Advance: follow the detector, the loader and the timers.
    pub fn tick(&mut self, now: Instant) {
        if let Some(quotes) = self.loader.as_ref().and_then(Loader::take) {
            self.quotes = quotes;
            self.deck.reset();
        }
        let heard = self.audio.as_ref().map_or(Heard::Silent, Audio::heard);
        let actions = self.panel.step(heard, !self.quotes.is_empty(), now);
        self.apply(actions);
    }

    fn apply(&mut self, actions: Actions) {
        self.repaint |= actions.repaint;
        if actions.stop_cava {
            // Drop kills and reaps the child and joins its reader.
            self.cava = None;
        }
        if actions.start_cava && self.cava.is_none() && self.display_on {
            self.cava = Some(Cava::new());
        }
        if actions.reload {
            if let Some(loader) = self.loader.as_ref() {
                loader.check();
            }
        }
        if actions.new_quote {
            self.pick();
        }
    }

    fn pick(&mut self) {
        self.arranged = None;
        let len = self.quotes.len();
        let mut index = self.deck.draw(len);
        // After a new pool the deck starts afresh; skip the quote on screen.
        if len > 1 && index.and_then(|i| self.quotes.get(i)) == self.quote.as_ref() {
            index = self.deck.draw(len);
        }
        self.quote = index.and_then(|i| self.quotes.get(i)).cloned();
    }

    fn arrange(&mut self, rect: (i16, i16, u16, u16)) {
        if self.arranged.as_ref().is_some_and(|(r, _)| *r == rect) {
            return;
        }
        self.arranged = None;
        let Some(quote) = self.quote.as_ref() else {
            return;
        };
        let (_, _, w, h) = rect;
        let pad = (i32::from(h) / 12).max(4);
        let em = i32::from(self.base.width("m")).max(1);
        // A line of at most about 64 characters, inside the panel.
        let max_w = (i32::from(w) - 2 * pad).min(em * 64).max(em * 8);
        let max_h = (i32::from(h) - 2 * pad).max(1);
        let sizes = sizes(self.base_size);
        if self.fonts.len() != sizes.len() {
            self.fonts = sizes.iter().map(|s| self.base.at_size(*s)).collect();
        }
        let fonts = &self.fonts;
        let index = |size: f32| sizes.iter().position(|s| *s == size).unwrap_or(0);
        let mut width = |size: f32, text: &str| i32::from(fonts[index(size)].width(text));
        let line_height = |size: f32| i32::from(fonts[index(size)].line_height());
        self.arranged = quotes::arrange(quote, max_w, max_h, &sizes, &mut width, &line_height)
            .map(|arranged| (rect, arranged));
    }

    /// Paint the panel into the backing pixmap with the palette `pal`.
    pub fn paint(
        &mut self,
        surface: &Surface,
        pal: &Palette,
        rect: (i16, i16, u16, u16),
    ) -> crate::Result<()> {
        let (x, y, w, h) = rect;
        let level = self.panel.level(Instant::now());
        match self.panel.shown() {
            Content::Blank => quotes::fill(surface, pal.background, x, y, w, h),
            Content::Cava => {
                let values = self
                    .cava
                    .as_mut()
                    .map(|cava| cava.values().to_vec())
                    .unwrap_or_else(|| vec![0.0; cava::BARS]);
                cava::paint(surface, &faded(pal, level), x, y, w, h, &values)
            }
            Content::Quote => {
                self.arrange(rect);
                let Some((_, arranged)) = self.arranged.as_ref() else {
                    return quotes::fill(surface, pal.background, x, y, w, h);
                };
                let sizes = sizes(self.base_size);
                let index = sizes.iter().position(|s| *s == arranged.size).unwrap_or(0);
                let pal = faded(pal, level);
                let Some(font) = self.fonts.get_mut(index) else {
                    return Ok(());
                };
                quotes::paint(
                    surface,
                    font,
                    arranged,
                    rect,
                    pal.foreground,
                    pal.muted,
                    pal.background,
                )
            }
        }
    }
}

/// Quote font sizes from `base` down to about half of it, never below 11 px.
fn sizes(base: f32) -> Vec<f32> {
    let smallest = (base * 0.5).max(11.0).min(base);
    let step = (base * 0.08).max(1.0);
    let mut sizes = Vec::new();
    let mut size = base;
    while size > smallest {
        sizes.push(size);
        size -= step;
    }
    sizes.push(smallest);
    sizes
}

fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (nanos ^ (u64::from(std::process::id()) << 32)) | 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn run(panel: &mut Panel, heard: Heard, from: Instant, to: Instant) -> Vec<Actions> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            out.push(panel.step(heard, true, t));
            t += MS * 33;
        }
        out
    }

    #[test]
    fn modes_parse() {
        for mode in [Mode::Auto, Mode::Cava, Mode::Quotes, Mode::None] {
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(Mode::parse(" QUOTES "), Some(Mode::Quotes));
        assert_eq!(Mode::parse("spectrum"), None);
    }

    #[test]
    fn silence_shows_a_quote_at_once_and_no_cava_starts() {
        let t = Instant::now();
        let mut panel = Panel::new(Mode::Auto, true, t);
        let a = panel.step(Heard::Silent, true, t);
        assert!(a.new_quote && !a.start_cava);
        assert_eq!(panel.shown(), Content::Quote);
        assert!(panel.fading());
        let actions = run(&mut panel, Heard::Silent, t, t + Duration::from_secs(2));
        assert!(actions.iter().all(|a| !a.start_cava));
        assert!(!panel.fading());
        // Steady: nothing until the rotation is due.
        let next = panel.deadline(t + Duration::from_secs(2)).unwrap();
        assert!(next >= t + ROTATE_EVERY);
    }

    #[test]
    fn an_undecided_stream_waits_briefly_then_shows_a_quote() {
        let t = Instant::now();
        let mut panel = Panel::new(Mode::Auto, true, t);
        assert_eq!(panel.step(Heard::Listening, true, t), Actions::default());
        assert_eq!(panel.shown(), Content::Blank);
        assert_eq!(panel.deadline(t), Some(t + DECIDE_WITHIN));
        // Music is confirmed inside the window: straight to the spectrum.
        let a = panel.step(Heard::Playing, true, t + MS * 500);
        assert!(a.start_cava && !a.new_quote);
        assert_eq!(panel.shown(), Content::Cava);
        // A silent synthesizer stream instead: the quote after the window.
        let mut panel = Panel::new(Mode::Auto, true, t);
        panel.step(Heard::Listening, true, t + MS * 500);
        assert_eq!(panel.shown(), Content::Blank);
        assert!(
            panel
                .step(Heard::Listening, true, t + DECIDE_WITHIN)
                .new_quote
        );
    }

    #[test]
    fn music_fades_the_quote_out_then_the_spectrum_in() {
        let t = Instant::now();
        let mut panel = Panel::new(Mode::Auto, true, t);
        run(&mut panel, Heard::Silent, t, t + Duration::from_secs(2));
        let m = t + Duration::from_secs(10);
        let a = panel.step(Heard::Playing, true, m);
        // CAVA starts at once so it is warm when the quote has gone.
        assert!(a.start_cava);
        assert_eq!(panel.shown(), Content::Quote);
        let half = panel.level(m + QUOTE_OUT / 2);
        assert!(half > 0.3 && half < 0.7);
        panel.step(Heard::Playing, true, m + QUOTE_OUT);
        assert_eq!(panel.shown(), Content::Cava);
        assert!(panel.level(m + QUOTE_OUT) < 0.01);
        panel.step(Heard::Playing, true, m + QUOTE_OUT + CAVA_IN);
        assert!(!panel.fading());
        assert_eq!(panel.level(m + QUOTE_OUT + CAVA_IN), 1.0);
        // No rotation timer while the spectrum shows.
        assert_eq!(panel.deadline(m + Duration::from_secs(1)), None);
    }

    #[test]
    fn levels_never_jump_during_a_change() {
        let t = Instant::now();
        let mut panel = Panel::new(Mode::Auto, true, t);
        run(&mut panel, Heard::Silent, t, t + Duration::from_secs(2));
        let mut previous = panel.level(t + Duration::from_secs(2));
        let mut time = t + Duration::from_secs(3);
        let mut fell = false;
        for _ in 0..60 {
            panel.step(Heard::Playing, true, time);
            let level = panel.level(time);
            // At most a fifth of the range per 33 ms frame, except at the
            // single swap of content at the bottom of the fade.
            if (level - previous).abs() > 0.2 {
                assert!(previous < 0.05 && level < 0.25, "{previous} -> {level}");
            }
            fell |= level < 0.05;
            previous = level;
            time += MS * 33;
        }
        assert!(fell);
        assert_eq!(panel.shown(), Content::Cava);
        assert_eq!(previous, 1.0);
    }

    #[test]
    fn music_stopping_returns_to_a_quote_and_stops_cava() {
        let t = Instant::now();
        let mut panel = Panel::new(Mode::Auto, true, t);
        panel.step(Heard::Playing, true, t);
        run(&mut panel, Heard::Playing, t, t + Duration::from_secs(1));
        assert_eq!(panel.shown(), Content::Cava);
        // The detector reports the end of music after its own silence window.
        let s = t + Duration::from_secs(20);
        let a = panel.step(Heard::Silent, true, s);
        assert!(!a.stop_cava);
        let a = panel.step(Heard::Silent, true, s + CAVA_OUT);
        assert!(a.stop_cava && a.new_quote);
        assert_eq!(panel.shown(), Content::Quote);
    }

    #[test]
    fn quotes_rotate_with_a_reload_check_and_a_fade() {
        let t = Instant::now();
        let mut panel = Panel::new(Mode::Quotes, true, t);
        run(&mut panel, Heard::Silent, t, t + Duration::from_secs(1));
        let due = panel.deadline(t + Duration::from_secs(1)).unwrap();
        assert_eq!(
            panel.step(Heard::Silent, true, due - MS),
            Actions::default()
        );
        let a = panel.step(Heard::Silent, true, due);
        assert!(a.reload && !a.new_quote);
        assert!(panel.fading());
        let a = panel.step(Heard::Silent, true, due + QUOTE_OUT);
        assert!(a.new_quote);
        // Quotes mode never starts CAVA, whatever plays.
        let actions = run(
            &mut panel,
            Heard::Playing,
            due,
            due + Duration::from_secs(5),
        );
        assert!(actions.iter().all(|a| !a.start_cava));
    }

    #[test]
    fn display_off_stops_everything_and_on_resumes() {
        let t = Instant::now();
        let mut panel = Panel::new(Mode::Auto, true, t);
        panel.step(Heard::Playing, true, t);
        let a = panel.display(false, t + MS * 100);
        assert!(a.stop_cava);
        assert!(!panel.fading());
        assert_eq!(panel.deadline(t + MS * 100), None);
        assert_eq!(
            panel.step(Heard::Playing, true, t + MS * 200),
            Actions::default()
        );
        panel.display(true, t + Duration::from_secs(60));
        let a = panel.step(Heard::Playing, true, t + Duration::from_secs(60));
        assert!(a.start_cava);
        // Mode cava: the spectrum itself follows the display (cava.rs).
        let mut panel = Panel::new(Mode::Cava, true, t);
        assert!(panel.step(Heard::Silent, false, t).start_cava);
        assert!(!panel.display(false, t).stop_cava);
    }

    #[test]
    fn no_quotes_means_an_empty_panel_and_no_timers() {
        let t = Instant::now();
        let mut panel = Panel::new(Mode::Auto, true, t);
        let a = panel.step(Heard::Silent, false, t);
        assert!(!a.new_quote);
        assert_eq!(panel.shown(), Content::Blank);
        assert_eq!(panel.deadline(t + DECIDE_WITHIN), None);
    }

    #[test]
    fn reduced_motion_changes_at_once() {
        let t = Instant::now();
        let mut panel = Panel::new(Mode::Auto, false, t);
        let a = panel.step(Heard::Silent, true, t);
        assert!(a.new_quote && a.repaint);
        assert!(!panel.fading());
        let a = panel.step(Heard::Playing, true, t + MS);
        assert!(a.start_cava && a.repaint);
        assert_eq!(panel.shown(), Content::Cava);
        assert!(!panel.fading());
    }

    #[test]
    fn faded_palettes_go_to_the_background() {
        let pal = Palette::default();
        let none = faded(&pal, 0.0);
        assert_eq!(none.foreground, pal.background);
        assert_eq!(none.focus, pal.background);
        assert_eq!(faded(&pal, 1.0).muted, pal.muted);
    }

    #[test]
    fn sizes_shrink_to_a_readable_minimum() {
        let s = sizes(22.0);
        assert_eq!(s[0], 22.0);
        assert_eq!(*s.last().unwrap(), 11.0);
        assert!(s.windows(2).all(|w| w[0] > w[1]));
        assert_eq!(sizes(10.0), vec![10.0]);
    }
}
