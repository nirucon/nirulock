//! Why the lock was started, which decides whether `[pause_music]` may play.
//!
//! Callers say it with `--reason NAME` or, for compatibility with releases
//! that reject unknown arguments, the `NIRULOCK_REASON` environment variable
//! (the argument wins). NIRUWM's lock key, `niructl lock` typed in a terminal
//! and the launcher's lock actions say `manual`; NIRUIDLE says `idle`, the
//! lock before suspend says `suspend` and `niructl lock --silent` says
//! `script`. A caller that says nothing, or names a reason this release does
//! not know, gets [`Reason::Unspecified`]: the lock is exactly the same, only
//! the pause music stays off by default. The reason never changes how the
//! lock is dismissed.

use std::fmt;

/// The environment variable a caller may set instead of `--reason`.
pub const ENV: &str = "NIRULOCK_REASON";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The owner locked by hand: a key, a menu, `niructl lock` in a terminal.
    Manual,
    /// The idle ladder (NIRUIDLE).
    Idle,
    /// Lock before suspend, a lid close or `niructl suspend`.
    Suspend,
    /// A script, SSH or another program (`niructl lock --silent`).
    Script,
    /// Locked as the session starts.
    Session,
    /// The caller gave no reason (an older caller) or an unknown one.
    Unspecified,
}

impl Reason {
    pub const ALL: [Reason; 6] = [
        Reason::Manual,
        Reason::Idle,
        Reason::Suspend,
        Reason::Script,
        Reason::Session,
        Reason::Unspecified,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Reason::Manual => "manual",
            Reason::Idle => "idle",
            Reason::Suspend => "suspend",
            Reason::Script => "script",
            Reason::Session => "session",
            Reason::Unspecified => "unspecified",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        Self::ALL
            .into_iter()
            .find(|r| r.name().eq_ignore_ascii_case(text))
    }

    /// The reason from `--reason` (if given) or the environment. Never
    /// fails: a lock must start whatever its caller wrote, so an unknown
    /// name only costs the music.
    pub fn resolve(arg: Option<&str>, env: Option<&str>) -> Self {
        let Some(text) = arg.or(env).filter(|t| !t.trim().is_empty()) else {
            return Reason::Unspecified;
        };
        Self::parse(text).unwrap_or_else(|| {
            eprintln!(
                "nirulock: unknown lock reason {:?}; locking without pause music",
                text.chars().take(32).collect::<String>()
            );
            Reason::Unspecified
        })
    }

    /// `manual, idle, ...` for messages.
    pub fn names() -> String {
        Self::ALL.map(Reason::name).join(", ")
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_round_trips() {
        for r in Reason::ALL {
            assert_eq!(Reason::parse(r.name()), Some(r));
        }
        assert_eq!(Reason::parse(" Manual "), Some(Reason::Manual));
        assert_eq!(Reason::parse("bogus"), None);
    }

    #[test]
    fn no_reason_is_unspecified() {
        assert_eq!(Reason::resolve(None, None), Reason::Unspecified);
        assert_eq!(Reason::resolve(Some(""), None), Reason::Unspecified);
        assert_eq!(Reason::resolve(None, Some("  ")), Reason::Unspecified);
    }

    #[test]
    fn the_argument_wins_over_the_environment() {
        assert_eq!(Reason::resolve(Some("idle"), Some("manual")), Reason::Idle);
        assert_eq!(Reason::resolve(None, Some("manual")), Reason::Manual);
    }

    #[test]
    fn an_unknown_reason_still_locks_silently() {
        assert_eq!(Reason::resolve(Some("party"), None), Reason::Unspecified);
        assert_eq!(Reason::resolve(None, Some("party")), Reason::Unspecified);
    }
}
