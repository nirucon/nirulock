//! NIRUX RAID, the game the `attract` layout plays by itself: an original
//! vertical scrolling shoot 'em up in the style of a Commodore 64 game's
//! attract mode. Nobody plays it: a small AI flies the hero.
//!
//! A rune craft flies up a river through a Norse forest, over runestones
//! and the roots of Yggdrasil, against will-o'-wisp formations, iron
//! drones, rune towers on the banks and longships on the river. Power
//! runes widen the shot or give a shield. Three lives; when the last is
//! lost it is GAME OVER, then the title screen, the high scores and a new
//! game with a new seed, like an arcade cabinet waiting for a coin.
//!
//! The game runs in fixed ticks ([`TICK`], 25 a second) on a 320x200
//! [`Canvas`]. It is fully determined by its seed: the music only changes
//! how bright explosions are drawn, and nothing typed reaches it.

use crate::chunky::{slot::*, Canvas};
use std::f32::consts::TAU;

/// The canvas size, the C64's 320x200.
pub const W: i32 = 320;
pub const H: i32 = 200;
/// Ticks a second.
pub const TICKS: u32 = 25;
/// The HUD strips at the top and the bottom.
const HUD: i32 = 9;
/// Scroll speed, pixels a tick.
const SCROLL: f32 = 1.0;
/// How long the screens between games last, in ticks.
const TITLE_TICKS: u32 = 8 * TICKS;
const SCORES_TICKS: u32 = 7 * TICKS;
const OVER_TICKS: u32 = 4 * TICKS;
const READY_TICKS: u32 = 2 * TICKS;

const HERO: &[&str] = &[
    "...11...", "...44...", "..4334..", "..4334..", ".243342.", "24433442", "2.4334.2", "..2332..",
    "..2..2..",
];
const FLAME: [&[&str]; 2] = [&[".c....c."], &[".b....b.", ".c....c."]];
const WISP: [&[&str]; 2] = [
    &[".444.", "41114", "41c14", "41114", ".4.4."],
    &[".444.", "41114", "41b14", "41114", "4.4.4"],
];
const DRONE: &[&str] = &["d.....d", "ed...de", ".edbde.", "..ebe..", "...e..."];
const TOWER: &[&str] = &[
    ".dddd.", "de33ed", "d3113d", "de33ed", "deeeed", "d3ee3d", "deeeed", ".dddd.",
];
const SHIP: &[&str] = &[
    "..a..", ".9a9.", "89998", "8bab8", "89a98", "8bab8", "89998", "89a98", "89998", ".898.",
    "..8..",
];
const TREE: &[&str] = &[
    "...9...", "..999..", ".99a99.", "..999..", ".9999a.", "999a999", "..9a9..", "...d...",
];
const STONE: &[&str] = &[".ee.", "efee", "e3ee", "ee3e", "e3ee", "eeee", "dddd"];

/// A small deterministic random number generator (xorshift64*).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15 | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// 0..1.
    pub fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn below(&mut self, n: u32) -> u32 {
        (self.next() % u64::from(n.max(1))) as u32
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

fn hash(seed: u64, n: i64) -> u64 {
    let mut x = seed ^ (n as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Wisp,
    Drone,
    Tower,
    Ship,
}

impl Kind {
    fn sprite(self, t: u32) -> &'static [&'static str] {
        match self {
            Self::Wisp => WISP[(t / 6 % 2) as usize],
            Self::Drone => DRONE,
            Self::Tower => TOWER,
            Self::Ship => SHIP,
        }
    }

    /// Half the width and height in pixels (all sprites are wide).
    fn half(self) -> (f32, f32) {
        let s = self.sprite(0);
        (s[0].len() as f32, s.len() as f32 / 2.0)
    }

    fn score(self) -> u32 {
        match self {
            Self::Wisp => 100,
            Self::Drone => 150,
            Self::Tower => 300,
            Self::Ship => 500,
        }
    }

    fn hp(self) -> i32 {
        match self {
            Self::Wisp => 1,
            Self::Drone => 2,
            Self::Tower => 4,
            Self::Ship => 6,
        }
    }

    fn on_ground(self) -> bool {
        matches!(self, Self::Tower | Self::Ship)
    }
}

#[derive(Clone, Debug)]
struct Enemy {
    kind: Kind,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    hp: i32,
    t: u32,
    cooldown: u32,
    flash: u32,
    /// The wisps' path: where the wave swings around, its phase.
    base: f32,
    phase: f32,
}

impl Enemy {
    /// Where it will be in `k` ticks: wisps follow their wave, the rest
    /// fly straight on.
    fn ahead(&self, k: u32) -> (f32, f32) {
        let kf = k as f32;
        match self.kind {
            Kind::Wisp => (
                wisp_x(self.base, self.t + k, self.phase),
                self.y + self.vy * kf,
            ),
            _ => (self.x + self.vx * kf, self.y + self.vy * kf),
        }
    }
}

fn wisp_x(base: f32, t: u32, phase: f32) -> f32 {
    base + 44.0 * (t as f32 * 0.04 + phase).sin()
}

#[derive(Clone, Copy, Debug)]
struct Mover {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

#[derive(Clone, Copy, Debug)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    life: u8,
    max: u8,
}

#[derive(Clone, Copy, Debug)]
struct Pickup {
    x: f32,
    y: f32,
    shield: bool,
    t: u32,
}

#[derive(Clone, Debug)]
struct Hero {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    alive: bool,
    /// Ticks until the next life comes in.
    respawn: u32,
    /// Ticks the hero still blinks and cannot be hit.
    safe: u32,
    cooldown: u32,
    /// 0 single shot, 1 double, 2 spread.
    power: u8,
    shield: bool,
    /// Ticks the AI keeps flying straight without looking: it is not
    /// perfect, and that is how a game ends.
    lapse: u32,
}

/// The phase of the attract loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Title,
    Scores,
    Play,
    Over,
}

/// One game in progress.
#[derive(Clone, Debug)]
struct Game {
    seed: u64,
    rng: Rng,
    /// Ticks played, and the world rows scrolled.
    t: u32,
    cam: f32,
    hero: Hero,
    lives: u32,
    score: u32,
    enemies: Vec<Enemy>,
    shots: Vec<Mover>,
    bullets: Vec<Mover>,
    particles: Vec<Particle>,
    pickups: Vec<Pickup>,
    /// The world's shape, from the seed.
    phases: [f32; 4],
}

/// What the screen shows besides the game: the tune and the beat.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extras<'a> {
    /// "Artist - Title" of the tune that plays.
    pub tune: Option<&'a str>,
    /// 0..1, 1 on a beat.
    pub beat: f32,
}

/// The whole attract loop: title, high scores, a game, game over.
#[derive(Clone, Debug)]
pub struct Attract {
    phase: Phase,
    /// Ticks in this phase, and in all.
    since: u32,
    ticks: u64,
    game: Game,
    scores: Vec<(u32, &'static str)>,
    /// The newest entry in the table, to highlight.
    newest: Option<usize>,
    seed: u64,
    games: u64,
}

impl Attract {
    pub fn new(seed: u64) -> Self {
        let scores = vec![
            (50_000, "ODN"),
            (40_000, "FRG"),
            (30_000, "TYR"),
            (25_000, "SIF"),
            (20_000, "BRG"),
            (15_000, "IDN"),
            (10_000, "HDR"),
            (5_000, "VAL"),
        ];
        Self {
            phase: Phase::Title,
            since: 0,
            ticks: 0,
            game: Game::new(seed),
            scores,
            newest: None,
            seed,
            games: 0,
        }
    }

    #[cfg(test)]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    #[cfg(test)]
    pub fn score(&self) -> u32 {
        self.game.score
    }

    #[cfg(test)]
    pub fn lives(&self) -> u32 {
        self.game.lives
    }

    #[cfg(test)]
    pub fn games(&self) -> u64 {
        self.games
    }

    #[cfg(test)]
    /// The hero's position while alive.
    pub fn hero(&self) -> Option<(f32, f32)> {
        (self.phase == Phase::Play && self.game.hero.alive)
            .then_some((self.game.hero.x, self.game.hero.y))
    }

    #[cfg(test)]
    /// Everything that moves, for tests: (x, y) of enemies, shots,
    /// bullets, particles and pickups.
    pub fn movers(&self) -> Vec<(f32, f32)> {
        let g = &self.game;
        g.enemies
            .iter()
            .map(|e| (e.x, e.y))
            .chain(g.shots.iter().map(|m| (m.x, m.y)))
            .chain(g.bullets.iter().map(|m| (m.x, m.y)))
            .chain(g.particles.iter().map(|p| (p.x, p.y)))
            .chain(g.pickups.iter().map(|p| (p.x, p.y)))
            .collect()
    }

    /// One tick.
    pub fn step(&mut self) {
        self.ticks += 1;
        self.since += 1;
        match self.phase {
            Phase::Title if self.since >= TITLE_TICKS => self.enter(Phase::Scores),
            Phase::Scores if self.since >= SCORES_TICKS => {
                self.games += 1;
                let seed = hash(self.seed, self.games as i64);
                self.game = Game::new(seed);
                self.enter(Phase::Play);
            }
            Phase::Play => {
                self.game.step(true);
                if self.game.lives == 0 && !self.game.hero.alive {
                    self.record();
                    self.enter(Phase::Over);
                }
            }
            Phase::Over => {
                self.game.step(false);
                if self.since >= OVER_TICKS {
                    self.enter(Phase::Title);
                }
            }
            _ => {}
        }
    }

    fn enter(&mut self, phase: Phase) {
        self.phase = phase;
        self.since = 0;
    }

    fn record(&mut self) {
        let score = self.game.score;
        self.newest = None;
        if score > self.scores.last().map_or(0, |s| s.0) {
            let at = self.scores.iter().position(|s| s.0 < score).unwrap_or(0);
            self.scores.insert(at, (score, "NRX"));
            self.scores.truncate(8);
            self.newest = Some(at);
        }
    }

    fn hi(&self) -> u32 {
        self.scores.first().map_or(0, |s| s.0).max(self.game.score)
    }

    /// Draw the current tick.
    pub fn render(&self, c: &mut Canvas, extras: Extras) {
        let t = self.ticks as u32;
        match self.phase {
            Phase::Title => self.title(c, t),
            Phase::Scores => self.table(c, t),
            Phase::Play | Phase::Over => {
                self.game.render(c, extras.beat);
                if self.phase == Phase::Play && self.game.t < READY_TICKS {
                    banner(c, "PLAYER 1  GET READY", 92, FLASH);
                }
                if self.phase == Phase::Over {
                    let colour = if self.since / 8 % 2 == 0 {
                        WARNING
                    } else {
                        FLASH
                    };
                    c.fill(64, 82, 192, 36, BLACK);
                    c.text_centred(86, "GAME OVER", colour, 3);
                }
            }
        }
        self.hud(c, extras, t);
    }

    fn hud(&self, c: &mut Canvas, extras: Extras, t: u32) {
        c.fill(0, 0, W, HUD, BLACK);
        c.fill(0, H - HUD, W, HUD, BLACK);
        c.text(4, 1, &format!("1UP {:06}", self.game.score), WHITE, 1);
        c.text(116, 1, &format!("HI {:06}", self.hi()), ACCENT_LIGHT, 1);
        let lives = if self.phase == Phase::Play {
            self.game.lives
        } else {
            0
        };
        for i in 0..lives.min(5) {
            let x = W - 12 - i as i32 * 14;
            c.sprite(x, 1, &["..4..", ".434.", "43334"], false, None);
            c.fill(x + 1, 4, 3, 3, ACCENT_DARK);
        }
        let text = match extras.tune {
            Some(tune) => format!(
                "NOW PLAYING  {}",
                crate::pixfont::plain(tune).to_uppercase()
            ),
            None => "NIRUX RAID  *  GAME DEMO".to_string(),
        };
        let chars = text.chars().count() as i32;
        if chars <= 39 {
            c.text_centred(H - HUD + 1, &text, COOL_LIGHT, 1);
        } else {
            // A marquee: the text runs through, then a pause.
            let span = chars * 8 + W;
            let at = W - (t as i32 * 2) % span;
            c.text(at, H - HUD + 1, &text, COOL_LIGHT, 1);
        }
    }

    fn title(&self, c: &mut Canvas, t: u32) {
        c.clear(BLACK);
        // Raster bars swinging through the logo, each a ramp of one hue.
        let ramps = [
            [
                ACCENT_DARK,
                ACCENT,
                ACCENT_LIGHT,
                WHITE,
                ACCENT_LIGHT,
                ACCENT,
                ACCENT_DARK,
            ],
            [
                COOL_DARK, COOL, COOL_LIGHT, WHITE, COOL_LIGHT, COOL, COOL_DARK,
            ],
            [
                WARM_DARK, WARM, WARM_LIGHT, FLASH, WARM_LIGHT, WARM, WARM_DARK,
            ],
        ];
        for (i, ramp) in ramps.iter().enumerate() {
            let centre = 52.0 + 34.0 * ((t as f32) * 0.07 + i as f32 * 2.1).sin();
            for (k, colour) in ramp.iter().enumerate() {
                let y = centre as i32 - 7 + k as i32 * 2;
                c.fill(0, y, W, 2, *colour);
            }
        }
        // The logo: black under each letter's pixels so it reads over the
        // bars, then the letters in a ramp from the light accent down.
        for (text, top, scale) in [("NIRUX", 22, 6), ("RAID", 74, 4)] {
            let width = text.len() as i32 * 8 * scale;
            let left = (W - width) / 2;
            c.text(left + scale / 2, top + scale / 2, text, BLACK, scale);
            for (i, ch) in text.chars().enumerate() {
                let x = left + i as i32 * 8 * scale;
                let wobble = ((t as f32 * 0.15 + i as f32).sin() * 2.0) as i32;
                let colour = [WHITE, ACCENT_LIGHT, FLASH, ACCENT_LIGHT, WHITE][i % 5];
                c.text(x, top + wobble, &ch.to_string(), colour, scale);
            }
        }
        c.text_centred(118, "DEFEND THE ROOTS OF YGGDRASIL", WARM_LIGHT, 1);
        c.text_centred(134, "WISP  100   DRONE  150", COOL_LIGHT, 1);
        c.text_centred(144, "TOWER 300   SHIP   500", COOL_LIGHT, 1);
        if t / 12 % 2 == 0 {
            c.text_centred(162, "INSERT COIN", FLASH, 1);
        }
        c.text_centred(178, "(C) 2026 NIRUX  ALL RUNES RESERVED", GREY, 1);
    }

    fn table(&self, c: &mut Canvas, t: u32) {
        c.clear(BLACK);
        c.text_centred(20, "HALL OF HEROES", ACCENT_LIGHT, 2);
        let ramp = [
            ACCENT,
            ACCENT_LIGHT,
            WHITE,
            COOL_LIGHT,
            COOL,
            WARM_LIGHT,
            FLASH,
            WARM,
        ];
        for (i, (score, name)) in self.scores.iter().enumerate() {
            let colour = if self.newest == Some(i) && t / 6 % 2 == 0 {
                WARNING
            } else {
                ramp[(i + (t / 4) as usize) % ramp.len()]
            };
            let line = format!("{}.  {:06}   {}", i + 1, score, name);
            c.text_centred(52 + i as i32 * 14, &line, colour, 1);
        }
        c.text_centred(172, "THE MACHINE PLAYS. YOU TYPE.", GREY_LIGHT, 1);
    }
}

fn banner(c: &mut Canvas, text: &str, y: i32, colour: u8) {
    let w = text.chars().count() as i32 * 8 + 8;
    c.fill((W - w) / 2, y - 3, w, 14, BLACK);
    c.text_centred(y, text, colour, 1);
}

/// The river at world row `w`: its centre and half width, 4-pixel steps.
fn river(phases: &[f32; 4], w: i64) -> (i32, i32) {
    let b = w.div_euclid(4) as f32;
    let centre =
        160.0 + 62.0 * (b * 0.031 + phases[0]).sin() + 28.0 * (b * 0.083 + phases[1]).sin();
    let half = 30.0 + 12.0 * (b * 0.057 + phases[2]).sin();
    ((centre as i32) / 4 * 4, (half as i32) / 4 * 4)
}

impl Game {
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let phases = [
            rng.unit() * TAU,
            rng.unit() * TAU,
            rng.unit() * TAU,
            rng.unit() * TAU,
        ];
        Self {
            seed,
            rng,
            t: 0,
            cam: 0.0,
            hero: Hero {
                x: 160.0,
                y: 230.0,
                vx: 0.0,
                vy: 0.0,
                alive: true,
                respawn: 0,
                safe: READY_TICKS + 2 * TICKS,
                cooldown: 0,
                power: 0,
                shield: false,
                lapse: 0,
            },
            lives: 3,
            score: 0,
            enemies: Vec::new(),
            shots: Vec::new(),
            bullets: Vec::new(),
            particles: Vec::new(),
            pickups: Vec::new(),
            phases,
        }
    }

    /// How hard it is: 1 at the start, one more every 40 seconds.
    fn level(&self) -> f32 {
        1.0 + self.t as f32 / (40 * TICKS) as f32
    }

    /// The world row on screen row `y`.
    fn world(&self, y: f32) -> i64 {
        (self.cam + H as f32 - y) as i64
    }

    fn step(&mut self, playing: bool) {
        self.t += 1;
        self.cam += SCROLL;
        if playing {
            self.spawn();
            self.fly();
        }
        self.move_all();
        if playing {
            self.collide();
        }
    }

    fn spawn(&mut self) {
        if self.t < READY_TICKS {
            return;
        }
        let level = self.level();
        let rng = &mut self.rng;
        // A wisp wave: five to eight swinging down in a line.
        if rng.chance(0.009 * level.min(4.0)) {
            let n = 5 + rng.below(4);
            let base = 60.0 + rng.unit() * 200.0;
            let phase = rng.unit() * TAU;
            let speed = 0.9 + 0.15 * level.min(5.0);
            for i in 0..n {
                self.enemies.push(Enemy {
                    kind: Kind::Wisp,
                    x: base,
                    y: -8.0 - i as f32 * 14.0,
                    vx: 0.0,
                    vy: speed,
                    hp: Kind::Wisp.hp(),
                    t: 0,
                    cooldown: 0,
                    flash: 0,
                    base,
                    phase: phase + i as f32 * 0.5,
                });
            }
        }
        if rng.chance(0.006 * level.min(5.0)) {
            let x = 20.0 + rng.unit() * 280.0;
            self.enemies.push(Enemy {
                kind: Kind::Drone,
                x,
                y: -6.0,
                vx: 0.0,
                vy: 1.4,
                hp: Kind::Drone.hp(),
                t: 0,
                cooldown: 20 + rng.below(30),
                flash: 0,
                base: x,
                phase: 0.0,
            });
        }
        // Towers stand on a bank, ships sail the river.
        let top = (self.cam + H as f32 + 8.0) as i64;
        let (centre, half) = river(&self.phases, top);
        if rng.chance(0.007 * level.min(4.0)) {
            let left = rng.chance(0.5);
            let x = if left {
                (centre - half - 24) as f32 - rng.unit() * 40.0
            } else {
                (centre + half + 24) as f32 + rng.unit() * 40.0
            };
            if (24.0..296.0).contains(&x) {
                self.enemies.push(Enemy {
                    kind: Kind::Tower,
                    x,
                    y: -8.0,
                    vx: 0.0,
                    vy: SCROLL,
                    hp: Kind::Tower.hp(),
                    t: 0,
                    cooldown: 30 + rng.below(40),
                    flash: 0,
                    base: x,
                    phase: 0.0,
                });
            }
        }
        let ships = self.enemies.iter().filter(|e| e.kind == Kind::Ship).count();
        if ships == 0 && rng.chance(0.004 * level.min(3.0)) {
            self.enemies.push(Enemy {
                kind: Kind::Ship,
                x: centre as f32,
                y: -12.0,
                vx: 0.0,
                vy: SCROLL * 0.6,
                hp: Kind::Ship.hp(),
                t: 0,
                cooldown: 40,
                flash: 0,
                base: centre as f32,
                phase: 0.0,
            });
        }
    }

    /// The AI: look a few ticks ahead at bullets and enemies, pick the
    /// move that keeps clear of them and lines up under a target or a
    /// power rune, and keep firing.
    fn fly(&mut self) {
        let level = self.level();
        let hero = &mut self.hero;
        if !hero.alive {
            if hero.respawn > 0 {
                hero.respawn -= 1;
            } else if self.lives > 0 {
                hero.alive = true;
                hero.x = 160.0;
                hero.y = 230.0;
                hero.safe = 3 * TICKS;
                hero.power = hero.power.saturating_sub(1);
            }
            return;
        }
        hero.safe = hero.safe.saturating_sub(1);
        // Fly in from below.
        if hero.y > 172.0 {
            hero.y -= 2.0;
            return;
        }
        if hero.lapse > 0 {
            hero.lapse -= 1;
        } else if self.rng.chance(0.0008 + 0.0012 * (level - 1.0)) {
            hero.lapse = 6 + self.rng.below(12);
        }
        if hero.lapse == 0 {
            let target = self
                .pickups
                .iter()
                .map(|p| p.x)
                .next()
                .or_else(|| {
                    self.enemies
                        .iter()
                        .filter(|e| e.y > 10.0 && e.y < hero.y - 20.0)
                        .max_by(|a, b| a.y.total_cmp(&b.y))
                        .map(|e| e.x)
                })
                .unwrap_or(160.0);
            let mut best = (f32::MAX, 0.0, 0.0);
            for vx in [-3.0f32, -1.5, 0.0, 1.5, 3.0] {
                for vy in [-2.0f32, 0.0, 2.0] {
                    let mut cost = (hero.x + vx * 6.0 - target).abs() * 0.05
                        + (hero.y + vy * 6.0 - 160.0).abs() * 0.02
                        + (vx - hero.vx).abs() * 0.05;
                    for k in 1..=12 {
                        let kf = k as f32;
                        let hx = (hero.x + vx * kf).clamp(12.0, 308.0);
                        let hy = (hero.y + vy * kf).clamp(120.0, 178.0);
                        let weight = 13.0 - kf;
                        for b in &self.bullets {
                            let (dx, dy) = (b.x + b.vx * kf - hx, b.y + b.vy * kf - hy);
                            if dx * dx + dy * dy < 81.0 {
                                cost += 10.0 * weight;
                            }
                        }
                        for e in self.enemies.iter().filter(|e| !e.kind.on_ground()) {
                            let (ex, ey) = e.ahead(k);
                            let (dx, dy) = (ex - hx, ey - hy);
                            if dx * dx + dy * dy < 196.0 {
                                cost += 8.0 * weight;
                            }
                        }
                    }
                    if cost < best.0 {
                        best = (cost, vx, vy);
                    }
                }
            }
            hero.vx = best.1;
            hero.vy = best.2;
        }
        hero.x = (hero.x + hero.vx).clamp(12.0, 308.0);
        hero.y = (hero.y + hero.vy).clamp(120.0, 178.0);
        // Fire.
        if hero.cooldown > 0 {
            hero.cooldown -= 1;
        } else {
            hero.cooldown = 5;
            let y = hero.y - 6.0;
            let shot = |x: f32, vx: f32| Mover { x, y, vx, vy: -5.0 };
            match hero.power {
                0 => self.shots.push(shot(hero.x, 0.0)),
                1 => {
                    self.shots.push(shot(hero.x - 4.0, 0.0));
                    self.shots.push(shot(hero.x + 4.0, 0.0));
                }
                _ => {
                    self.shots.push(shot(hero.x, 0.0));
                    self.shots.push(shot(hero.x - 3.0, -1.1));
                    self.shots.push(shot(hero.x + 3.0, 1.1));
                }
            }
        }
    }

    fn move_all(&mut self) {
        let level = self.level();
        let (hx, hy, alive) = (self.hero.x, self.hero.y, self.hero.alive);
        let mut fired = Vec::new();
        for e in &mut self.enemies {
            e.t += 1;
            e.flash = e.flash.saturating_sub(1);
            match e.kind {
                Kind::Wisp => {
                    let x = wisp_x(e.base, e.t, e.phase);
                    e.vx = x - e.x;
                    e.x = x;
                    e.y += e.vy;
                }
                Kind::Drone => {
                    // Drift towards the hero, then dive straight on.
                    if e.y < 90.0 && alive {
                        e.vx = ((hx - e.x) * 0.02).clamp(-1.0, 1.0);
                    }
                    e.x += e.vx;
                    e.y += e.vy;
                }
                Kind::Tower => e.y += e.vy,
                Kind::Ship => {
                    let world = (self.cam + H as f32 - e.y) as i64;
                    let (centre, _) = river(&self.phases, world);
                    e.vx = ((centre as f32 - e.x) * 0.1).clamp(-1.0, 1.0);
                    e.x += e.vx;
                    e.y += e.vy;
                }
            }
            if e.kind != Kind::Wisp && alive && e.y > 12.0 && e.y < hy - 30.0 {
                if e.cooldown > 0 {
                    e.cooldown -= 1;
                } else {
                    let (dx, dy) = (hx - e.x, hy - e.y);
                    let len = (dx * dx + dy * dy).sqrt().max(1.0);
                    let speed = 1.5 + 0.15 * level.min(6.0);
                    fired.push(Mover {
                        x: e.x,
                        y: e.y + 4.0,
                        vx: dx / len * speed,
                        vy: dy / len * speed,
                    });
                    if e.kind == Kind::Ship {
                        for turn in [-0.35f32, 0.35] {
                            let (s, c) = turn.sin_cos();
                            fired.push(Mover {
                                x: e.x,
                                y: e.y + 4.0,
                                vx: (dx * c - dy * s) / len * speed,
                                vy: (dx * s + dy * c) / len * speed,
                            });
                        }
                    }
                    e.cooldown = match e.kind {
                        Kind::Drone => 999,
                        _ => (70.0 / level.min(3.5)) as u32 + 10,
                    };
                }
            }
        }
        self.bullets.extend(fired);
        self.enemies
            .retain(|e| e.y < H as f32 + 16.0 && e.x > -20.0 && e.x < W as f32 + 20.0);
        for m in self.shots.iter_mut().chain(self.bullets.iter_mut()) {
            m.x += m.vx;
            m.y += m.vy;
        }
        let inside =
            |m: &Mover| m.x > -4.0 && m.x < W as f32 + 4.0 && m.y > -4.0 && m.y < H as f32 + 4.0;
        self.shots.retain(inside);
        self.bullets.retain(inside);
        for p in &mut self.particles {
            p.x += p.vx;
            p.y += p.vy + SCROLL * 0.5;
            p.vx *= 0.92;
            p.vy *= 0.92;
            p.life = p.life.saturating_sub(1);
        }
        self.particles.retain(|p| p.life > 0);
        for p in &mut self.pickups {
            p.t += 1;
            p.y += SCROLL * 0.8;
        }
        self.pickups.retain(|p| p.y < H as f32 + 8.0);
    }

    fn burst(&mut self, x: f32, y: f32, n: u32, speed: f32) {
        for _ in 0..n {
            let a = self.rng.unit() * TAU;
            let v = speed * (0.3 + self.rng.unit());
            let life = 10 + self.rng.below(14) as u8;
            self.particles.push(Particle {
                x,
                y,
                vx: a.cos() * v,
                vy: a.sin() * v,
                life,
                max: life,
            });
        }
        if self.particles.len() > 400 {
            let extra = self.particles.len() - 400;
            self.particles.drain(..extra);
        }
    }

    fn collide(&mut self) {
        // Shots against enemies.
        let mut kills = Vec::new();
        let mut sparks = Vec::new();
        for s in &mut self.shots {
            for (i, e) in self.enemies.iter_mut().enumerate() {
                if e.hp <= 0 {
                    continue;
                }
                let (hw, hh) = e.kind.half();
                if (s.x - e.x).abs() < hw + 1.0 && (s.y - e.y).abs() < hh + 2.0 {
                    e.hp -= 1;
                    e.flash = 3;
                    s.y = -100.0;
                    if e.hp <= 0 {
                        kills.push(i);
                    } else {
                        sparks.push((s.x, e.y + hh));
                    }
                    break;
                }
            }
        }
        self.shots.retain(|s| s.y > -50.0);
        for (x, y) in sparks {
            self.burst(x, y, 3, 1.2);
        }
        kills.sort_unstable();
        kills.dedup();
        for &i in kills.iter().rev() {
            let e = self.enemies.remove(i);
            self.score += e.kind.score();
            let big = e.kind.on_ground();
            self.burst(
                e.x,
                e.y,
                if big { 26 } else { 12 },
                if big { 2.4 } else { 1.6 },
            );
            let drop = match e.kind {
                Kind::Ship => true,
                Kind::Tower => self.rng.chance(0.3),
                _ => false,
            };
            if drop && self.pickups.is_empty() {
                let shield = self.rng.chance(0.35);
                self.pickups.push(Pickup {
                    x: e.x,
                    y: e.y,
                    shield,
                    t: 0,
                });
            }
        }
        if !self.hero.alive {
            return;
        }
        let (hx, hy) = (self.hero.x, self.hero.y);
        // Power runes.
        if let Some(i) = self
            .pickups
            .iter()
            .position(|p| (p.x - hx).abs() < 10.0 && (p.y - hy).abs() < 10.0)
        {
            let p = self.pickups.remove(i);
            if p.shield {
                self.hero.shield = true;
            } else {
                self.hero.power = (self.hero.power + 1).min(2);
            }
            self.score += 250;
        }
        if self.hero.safe > 0 {
            return;
        }
        let hit_bullet = self
            .bullets
            .iter()
            .position(|b| (b.x - hx).abs() < 3.5 && (b.y - hy).abs() < 4.0);
        let hit_enemy = self
            .enemies
            .iter()
            .position(|e| !e.kind.on_ground() && (e.x - hx).abs() < 7.0 && (e.y - hy).abs() < 6.0);
        if hit_bullet.is_none() && hit_enemy.is_none() {
            return;
        }
        if let Some(i) = hit_bullet {
            self.bullets.remove(i);
        }
        if let Some(i) = hit_enemy {
            let e = self.enemies.remove(i);
            self.burst(e.x, e.y, 10, 1.6);
        }
        if self.hero.shield {
            self.hero.shield = false;
            self.hero.safe = TICKS;
            self.burst(hx, hy, 8, 1.0);
            return;
        }
        self.burst(hx, hy, 40, 2.8);
        self.hero.alive = false;
        self.hero.respawn = 2 * TICKS;
        self.lives = self.lives.saturating_sub(1);
        self.bullets.clear();
    }

    fn render(&self, c: &mut Canvas, beat: f32) {
        // The ground: forest, a river with banks, roots across.
        for y in 0..H {
            let w = self.world(y as f32);
            let row = c.row_mut(y);
            terrain_row(self.seed, &self.phases, w, row);
        }
        // Trees and runestones, from the world rows on the screen.
        let first = self.world(H as f32).div_euclid(8);
        let last = self.world(0.0).div_euclid(8) + 1;
        for cell in first..=last {
            let h = hash(self.seed ^ 0x7a3e, cell);
            if h % 3 != 0 {
                continue;
            }
            let x = (h >> 8) as i32 % (W - 12) + 2;
            let world = cell * 8;
            let (centre, half) = river(&self.phases, world);
            if (x + 10 > centre - half - 6) && (x < centre + half + 6) {
                continue;
            }
            let y = (self.cam + H as f32 - world as f32) as i32;
            if (h >> 20) % 9 == 0 {
                c.sprite(x, y - 6, STONE, true, None);
            } else {
                c.sprite(x, y - 6, TREE, false, None);
            }
        }
        for p in &self.pickups {
            let colour = if p.t / 4 % 2 == 0 {
                ACCENT_LIGHT
            } else {
                WHITE
            };
            let (x, y) = (p.x as i32 - 5, p.y as i32 - 5);
            c.fill(x, y, 11, 11, colour);
            c.fill(x + 1, y + 1, 9, 9, BLACK);
            c.text(x + 2, y + 2, if p.shield { "S" } else { "P" }, colour, 1);
        }
        // Ground enemies first, then the air.
        for ground in [true, false] {
            for e in self.enemies.iter().filter(|e| e.kind.on_ground() == ground) {
                let (hw, hh) = e.kind.half();
                let tint = (e.flash > 0).then_some(WHITE);
                c.sprite(
                    (e.x - hw) as i32,
                    (e.y - hh) as i32,
                    e.kind.sprite(e.t),
                    true,
                    tint,
                );
            }
        }
        let hero = &self.hero;
        if hero.alive && (hero.safe == 0 || hero.safe / 3 % 2 == 0) {
            let (x, y) = (hero.x as i32 - 8, hero.y as i32 - 5);
            c.sprite(x, y, HERO, true, None);
            c.sprite(x, y + 9, FLAME[(self.t / 2 % 2) as usize], true, None);
            if hero.shield {
                let colour = if self.t / 3 % 2 == 0 {
                    COOL_LIGHT
                } else {
                    WHITE
                };
                for (dx, dy) in [
                    (-10, 0),
                    (9, 0),
                    (0, -9),
                    (0, 8),
                    (-7, -6),
                    (6, -6),
                    (-7, 6),
                    (6, 6),
                ] {
                    c.fill(hero.x as i32 + dx, hero.y as i32 + dy, 2, 2, colour);
                }
            }
            if hero.cooldown >= 4 {
                // The muzzle flash, bigger on the beat.
                let size = if beat > 0.5 { 4 } else { 2 };
                c.fill(hero.x as i32 - size / 2, y - size, size, size, FLASH);
            }
        }
        for s in &self.shots {
            c.fill(s.x as i32, s.y as i32 - 3, 1, 4, FLASH);
            c.fill(s.x as i32, s.y as i32 - 3, 1, 1, WHITE);
        }
        for b in &self.bullets {
            let colour = if self.t / 2 % 2 == 0 { WARNING } else { FLASH };
            c.fill(b.x as i32 - 1, b.y as i32 - 1, 3, 3, colour);
            c.set(b.x as i32, b.y as i32, WHITE);
        }
        for p in &self.particles {
            let age = 1.0 - f32::from(p.life) / f32::from(p.max.max(1));
            let colour = if age < 0.25 || (beat > 0.5 && age < 0.5) {
                WHITE
            } else if age < 0.5 {
                FLASH
            } else if age < 0.75 {
                WARNING
            } else {
                GREY_DARK
            };
            let size = if beat > 0.5 && age < 0.5 { 3 } else { 2 };
            c.fill(p.x as i32, p.y as i32, size, size - 1, colour);
        }
    }
}

/// One row of the ground at world row `w`.
fn terrain_row(seed: u64, phases: &[f32; 4], w: i64, row: &mut [u8]) {
    let band = w.div_euclid(4);
    // A root of Yggdrasil crosses everything now and then.
    let root = (band + (seed % 97) as i64).rem_euclid(120);
    if root < 3 {
        row.fill(if root == 1 { WARM_LIGHT } else { WARM });
        let knot = (hash(seed, band) % 280) as usize + 10;
        row[knot..knot + 12].fill(GREY_DARK);
        return;
    }
    row.fill(WARM_DARK);
    // Dense forest at both edges, ragged in 4-pixel steps.
    let edge = |n: i64| ((hash(seed, n) % 4) as usize + 2) * 4;
    let (left, right) = (edge(band * 2), edge(band * 2 + 1));
    row[..left].fill(WARM);
    row[W as usize - right..].fill(WARM);
    let (centre, half) = river(phases, w);
    let span = |a: i32, b: i32| (a.clamp(0, W) as usize, b.clamp(0, W) as usize);
    let (a, b) = span(centre - half - 4, centre + half + 4);
    row[a..b].fill(WARM_LIGHT);
    let (a, b) = span(centre - half, centre + half);
    row[a..b].fill(COOL);
    let (a, b) = span(centre - half / 3, centre + half / 3);
    row[a..b].fill(COOL_DARK);
    let h = hash(seed ^ 0xf0a3, band);
    if h % 5 == 0 {
        let x = centre - half + 4 + ((h >> 8) % (half.max(4) as u64 * 2 - 4).max(1)) as i32 / 4 * 4;
        let (a, b) = span(x, x + 4);
        row[a..b].fill(COOL_LIGHT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(seed: u64, ticks: u32) -> (Attract, Canvas) {
        let mut a = Attract::new(seed);
        let mut c = Canvas::new(W, H);
        for _ in 0..ticks {
            a.step();
        }
        a.render(&mut c, Extras::default());
        (a, c)
    }

    #[test]
    fn a_seed_always_plays_the_same_game() {
        let (a, c) = run(42, 2500);
        let (b, d) = run(42, 2500);
        assert_eq!(c, d);
        assert_eq!((a.score(), a.phase()), (b.score(), b.phase()));
        let (_, e) = run(43, 2500);
        assert_ne!(c, e, "another seed plays another game");
    }

    #[test]
    fn the_loop_goes_title_scores_play_over_and_round_again() {
        let mut a = Attract::new(7);
        let mut seen = vec![a.phase()];
        for _ in 0..(40 * 60 * TICKS) {
            a.step();
            if seen.last() != Some(&a.phase()) {
                seen.push(a.phase());
            }
            if a.games() >= 3 && a.phase() == Phase::Title {
                break;
            }
        }
        assert!(a.games() >= 3, "{seen:?}");
        assert_eq!(
            &seen[..5],
            &[
                Phase::Title,
                Phase::Scores,
                Phase::Play,
                Phase::Over,
                Phase::Title
            ]
        );
    }

    #[test]
    fn the_ai_survives_a_while_and_dies_in_the_end() {
        let mut lengths = Vec::new();
        for seed in 1..=8u64 {
            let mut a = Attract::new(seed * 7919);
            while a.phase() != Phase::Play {
                a.step();
            }
            let mut first_death = None;
            let mut ticks = 0u32;
            while a.phase() == Phase::Play && ticks < 30 * 60 * TICKS {
                a.step();
                ticks += 1;
                if first_death.is_none() && a.lives() < 3 {
                    first_death = Some(ticks);
                }
            }
            assert_eq!(a.phase(), Phase::Over, "seed {seed}: the game never ended");
            let first = first_death.unwrap();
            assert!(
                first > 15 * TICKS,
                "seed {seed}: the first life lasted {first} ticks"
            );
            assert!(a.score() > 2000, "seed {seed}: score {}", a.score());
            lengths.push(ticks / TICKS);
        }
        let mean = lengths.iter().sum::<u32>() / lengths.len() as u32;
        assert!((60..=600).contains(&mean), "games last {lengths:?} seconds");
    }

    #[test]
    fn everything_stays_on_the_canvas_and_the_hero_in_its_band() {
        let mut a = Attract::new(99);
        let mut c = Canvas::new(W, H);
        for i in 0..6000 {
            a.step();
            if let Some((x, y)) = a.hero() {
                assert!((12.0..=308.0).contains(&x) && y <= 230.0, "{x} {y}");
            }
            for (x, y) in a.movers() {
                assert!(x > -40.0 && x < 360.0 && y > -150.0 && y < 240.0, "{x} {y}");
            }
            if i % 50 == 0 {
                // Rendering never panics and keeps the HUD strips black
                // under the text.
                a.render(
                    &mut c,
                    Extras {
                        tune: Some("Erik Satie - Gymnopedie No. 1, a very long name indeed"),
                        beat: (i % 100) as f32 / 100.0,
                    },
                );
                assert_eq!(c.get(2, 0), BLACK);
                assert_eq!(c.get(2, H - 1), BLACK);
            }
        }
    }

    #[test]
    fn a_frame_stays_within_the_rectangle_budget() {
        let mut a = Attract::new(5);
        let mut c = Canvas::new(W, H);
        let mut prev: Option<Canvas> = None;
        let mut out: [Vec<crate::chunky::Logical>; 16] = Default::default();
        let mut worst = 0;
        for i in 0..5000 {
            a.step();
            a.render(&mut c, Extras::default());
            let n = crate::chunky::encode(&c, prev.as_ref(), &mut out);
            worst = worst.max(n);
            if i % 2 == 0 {
                prev = Some(c.clone());
            }
        }
        assert!(worst < 4000, "{worst} rectangles in one frame");
    }

    #[test]
    fn the_tune_shows_in_the_hud_as_artist_and_title() {
        let mut a = Attract::new(1);
        a.step();
        let mut with = Canvas::new(W, H);
        let mut without = Canvas::new(W, H);
        a.render(
            &mut with,
            Extras {
                tune: Some("NIRUX - Gimle"),
                beat: 0.0,
            },
        );
        a.render(&mut without, Extras::default());
        let mut text = Canvas::new(W, H);
        text.text_centred(H - HUD + 1, "NOW PLAYING  NIRUX - GIMLE", COOL_LIGHT, 1);
        for x in 0..W {
            for y in H - HUD..H {
                assert_eq!(with.get(x, y), text.get(x, y), "{x},{y}");
            }
        }
        assert_ne!(with, without);
    }
}
