//! The `bbs` layout's modem: a dial-up handshake in SID style, synthesized
//! here as a small WAV file (mono, 16 bit, [`RATE`] Hz, about six seconds).
//!
//! It is what a 14400 bps modem sounded like when it called a board: the
//! dial tone, the number in touch tones, one ring, the answer tone and the
//! scrambled training hiss, until the carrier is up. The voices are a SID's:
//! pulse (square) and triangle waves and the noise generator, each with a
//! short attack and release, through a gentle low-pass so nothing is harsh.
//! The loudest moment stays at [`PEAK`] of full scale.
//!
//! It plays once, as the start of the pause music (the same rules, the same
//! player and the same isolation, see [`crate::music`]), and the `bbs`
//! layout's dialing text follows [`RINGING`] and [`CONNECT`].
//!
//! Everything here is pure, so it can be tested.

/// Samples a second.
pub const RATE: u32 = 22_050;
/// The loudest sample, as a fraction of full scale.
pub const PEAK: f32 = 0.16;
/// The number dialled: 555 is the made-up exchange of films, and 64789 is
/// NIRUX on a phone's keypad.
pub const NUMBER: &str = "555-64789";
/// Seconds from the start of the sound: the ring back starts, and the
/// carrier is up.
pub const RINGING: f32 = 2.0;
pub const CONNECT: f32 = 5.75;
/// The whole sound.
pub const LENGTH: f32 = 5.9;

/// One voice of the sound.
#[derive(Clone, Copy, Debug)]
enum Voice {
    /// Two pulse waves at once (a dial tone, a touch tone).
    Pulses(f32, f32),
    /// A triangle wave whose pitch flips every `.2` seconds between `.0`
    /// and `.1` (the same pitch twice for a steady tone).
    Triangle(f32, f32, f32),
    /// The noise generator, clocked at this rate, with a faint carrier.
    Noise(f32),
}

/// (start, end, voice, level) in seconds and fractions of [`PEAK`].
fn score() -> Vec<(f32, f32, Voice, f32)> {
    let mut out = vec![
        // The dial tone.
        (0.30, 0.78, Voice::Pulses(350.0, 440.0), 0.55),
    ];
    // The number in touch tones.
    let mut t = 0.86;
    for digit in NUMBER.chars().filter_map(|c| c.to_digit(10)) {
        if let Some((low, high)) = dtmf(digit) {
            out.push((t, t + 0.075, Voice::Pulses(low, high), 0.5));
        }
        t += 0.13;
    }
    out.extend([
        // One ring back.
        (RINGING, RINGING + 0.95, Voice::Pulses(440.0, 480.0), 0.45),
        // The answer tone, with a phase reversal.
        (3.15, 3.55, Voice::Triangle(2100.0, 2100.0, 1.0), 0.6),
        (3.56, 3.95, Voice::Triangle(2100.0, 2100.0, 1.0), 0.6),
        // The bing bong of the two modems finding each other.
        (4.02, 4.16, Voice::Triangle(1800.0, 1800.0, 1.0), 0.65),
        (4.18, 4.42, Voice::Triangle(1200.0, 2400.0, 0.03), 0.5),
        // The training: scrambled noise, then quiet.
        (4.46, CONNECT - 0.05, Voice::Noise(9000.0), 0.7),
    ]);
    out
}

/// The two tones of a touch-tone digit.
fn dtmf(digit: u32) -> Option<(f32, f32)> {
    let (row, col) = match digit {
        1..=9 => ((digit - 1) / 3, (digit - 1) % 3),
        0 => (3, 1),
        _ => return None,
    };
    let rows = [697.0, 770.0, 852.0, 941.0];
    let cols = [1209.0, 1336.0, 1477.0];
    Some((rows[row as usize], cols[col as usize]))
}

/// The sound as samples, -1..1.
pub fn samples() -> Vec<f32> {
    let n = (LENGTH * RATE as f32) as usize;
    let mut out = vec![0.0f32; n];
    let dt = 1.0 / RATE as f32;
    for (start, end, voice, level) in score() {
        let (a, b) = (
            (start * RATE as f32) as usize,
            ((end * RATE as f32) as usize).min(n),
        );
        let mut lfsr: u32 = 0x7f_fff8;
        let mut noise = 0.0f32;
        let mut clock = 0.0f32;
        for (i, sample) in out.iter_mut().enumerate().take(b).skip(a) {
            let t = (i - a) as f32 * dt;
            let len = (b - a) as f32 * dt;
            // 8 ms attack and release: no clicks.
            let env = (t / 0.008).min(1.0) * ((len - t) / 0.008).clamp(0.0, 1.0);
            let s = match voice {
                Voice::Pulses(f1, f2) => 0.5 * (pulse(f1 * t) + pulse(f2 * t)),
                Voice::Triangle(f1, f2, every) => {
                    let f = if ((t / every) as u32) % 2 == 0 {
                        f1
                    } else {
                        f2
                    };
                    triangle(f * t)
                }
                Voice::Noise(rate) => {
                    // The SID's 23-bit noise register, clocked at `rate`.
                    clock += rate * dt;
                    while clock >= 1.0 {
                        clock -= 1.0;
                        let bit = ((lfsr >> 22) ^ (lfsr >> 17)) & 1;
                        lfsr = ((lfsr << 1) | bit) & 0x7f_ffff;
                        noise = (lfsr & 0xff) as f32 / 127.5 - 1.0;
                    }
                    // Bursts that come and go, fading towards the end,
                    // over a faint carrier.
                    let burst = 0.65 + 0.35 * (t * 37.0).sin() * (t * 11.0).cos();
                    let fade = 1.0 - 0.5 * (t / len);
                    (0.8 * noise * burst + 0.2 * triangle(1800.0 * t)) * fade
                }
            };
            *sample += s * env * level;
        }
    }
    // A gentle low-pass (about 3.5 kHz): square edges and noise lose their
    // bite.
    let k = 1.0 - (-2.0 * std::f32::consts::PI * 3500.0 / RATE as f32).exp();
    let mut y = 0.0;
    for sample in &mut out {
        y += k * (*sample - y);
        *sample = (y * PEAK).clamp(-PEAK, PEAK);
    }
    out
}

/// A square wave, -1..1, of phase `p` in cycles.
fn pulse(p: f32) -> f32 {
    if p.fract() < 0.5 {
        1.0
    } else {
        -1.0
    }
}

/// A triangle wave, -1..1.
fn triangle(p: f32) -> f32 {
    let f = p.fract();
    if f < 0.5 {
        4.0 * f - 1.0
    } else {
        3.0 - 4.0 * f
    }
}

/// The sound as a WAV file.
pub fn wav() -> Vec<u8> {
    let samples = samples();
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wav_is_a_short_quiet_mono_file() {
        let w = wav();
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([w[22], w[23]]), 1);
        assert_eq!(u32::from_le_bytes([w[24], w[25], w[26], w[27]]), RATE);
        let data = u32::from_le_bytes([w[40], w[41], w[42], w[43]]) as usize;
        assert_eq!(w.len(), 44 + data);
        let seconds = data as f32 / 2.0 / RATE as f32;
        assert!((5.0..7.0).contains(&seconds), "{seconds}");
        assert_eq!(wav(), w, "the same every time");
        let peak = w[44..]
            .chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(peak <= (PEAK * 32767.0) as u16 + 1, "{peak}");
        assert!(peak > 2000, "not silent: {peak}");
    }

    #[test]
    fn it_starts_quiet_and_every_part_is_heard() {
        let s = samples();
        let loud = |t0: f32, t1: f32| {
            let (a, b) = ((t0 * RATE as f32) as usize, (t1 * RATE as f32) as usize);
            s[a..b].iter().map(|x| x * x).sum::<f32>() / (b - a) as f32
        };
        assert!(loud(0.0, 0.29) < 1e-9, "silence while the player starts");
        for (t0, t1) in [
            (0.4, 0.7),
            (0.87, 0.93),
            (RINGING + 0.1, RINGING + 0.8),
            (3.2, 3.5),
            (4.6, 5.5),
        ] {
            assert!(loud(t0, t1) > 1e-4, "{t0}..{t1}: {}", loud(t0, t1));
        }
        assert!(loud(CONNECT, LENGTH) < 1e-7, "quiet once connected");
        assert!(s.iter().all(|x| x.abs() <= PEAK));
    }

    #[test]
    fn touch_tones_are_the_real_pairs() {
        assert_eq!(dtmf(5), Some((770.0, 1336.0)));
        assert_eq!(dtmf(9), Some((852.0, 1477.0)));
        assert_eq!(dtmf(0), Some((941.0, 1336.0)));
        assert_eq!(NUMBER.chars().filter(char::is_ascii_digit).count(), 8);
    }
}
