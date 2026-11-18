use std::borrow::Cow;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::{Inserted, commit::NativeInsert};

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(Clone, Debug, Default)]
#[expect(
    clippy::disallowed_types,
    reason = "The Windows keyboard hook and the Mac event tap count keys on their native threads for the insertion worker to compare between pastes; an event per keystroke would wake that worker for every key typed"
)]
pub(crate) struct KeyCount(Arc<AtomicU64>);

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl KeyCount {
    pub(crate) fn add(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn read(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

pub(crate) struct Continuation<N: NativeInsert>(Option<Trail<N::Target, N::Activity>>);

impl<N: NativeInsert> Default for Continuation<N> {
    fn default() -> Self {
        Self(None)
    }
}

impl<N: NativeInsert> Continuation<N> {
    pub(crate) fn lead<'text>(&self, native: &mut N, text: &'text str) -> Cow<'text, str> {
        let continues = self.0.as_ref().is_some_and(|trail| {
            native.target().is_some_and(|target| {
                native
                    .activity()
                    .is_some_and(|activity| trail.continues(&target, &activity, text))
            })
        });
        if continues {
            Cow::Owned(format!(" {text}"))
        } else {
            Cow::Borrowed(text)
        }
    }

    pub(crate) fn follow(self, native: &mut N, text: &str, inserted: &Inserted) -> Self {
        match inserted {
            Inserted::Sent => Self(
                native
                    .target()
                    .zip(native.activity())
                    .and_then(|(target, activity)| Trail::after(text, target, activity)),
            ),
            Inserted::Cancelled => self,
            Inserted::Copied(_) | Inserted::Unavailable(_) => Self::default(),
        }
    }
}

struct Trail<T, A> {
    target: T,
    activity: A,
    ends: char,
}

impl<T: PartialEq, A: PartialEq> Trail<T, A> {
    fn after(text: &str, target: T, activity: A) -> Option<Self> {
        Some(Self {
            target,
            activity,
            ends: text.chars().next_back()?,
        })
    }

    fn continues(&self, target: &T, activity: &A, next: &str) -> bool {
        self.target == *target && self.activity == *activity && spaced(self.ends, next)
    }
}

fn spaced(ends: char, next: &str) -> bool {
    next.chars().next().is_some_and(|starts| {
        !ends.is_whitespace()
            && !OPENERS.contains(&ends)
            && !starts.is_whitespace()
            && !CLOSERS.contains(&starts)
            && !unspaced_script(ends)
            && !unspaced_script(starts)
    })
}

const OPENERS: [char; 7] = ['(', '[', '{', '“', '‘', '¿', '¡'];

const CLOSERS: [char; 22] = [
    '.', ',', ';', ':', '!', '?', ')', ']', '}', '”', '’', '…', '%', '‼', '⁇', '⁈', '⁉',
    '\u{037E}', '،', '؛', '؟', '।',
];

const fn unspaced_script(character: char) -> bool {
    matches!(
        character,
        '\u{0E00}'..='\u{0EFF}'
            | '\u{1000}'..='\u{109F}'
            | '\u{1780}'..='\u{17FF}'
            | '\u{3000}'..='\u{30FF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF00}'..='\u{FFEF}'
            | '\u{20000}'..='\u{3FFFF}'
    )
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{Trail, spaced};

    #[test]
    fn a_new_word_after_a_finished_one_gets_one_space() {
        for (ends, next) in [
            ('?', "Oh yeah"),
            ('o', "world"),
            ('.', "“Quoted”"),
            ('?', "\"Quoted\""),
            ('"', "next"),
            ('3', "4"),
            ('o', "$20"),
            ('o', "#tag"),
        ] {
            assert!(spaced(ends, next), "{ends:?} then {next:?}");
        }
    }

    #[test]
    fn closing_punctuation_and_unspaced_scripts_join_directly() {
        for (ends, next) in [
            ('o', ", then"),
            ('o', "."),
            ('o', ")"),
            ('o', "…"),
            ('ا', "؟"),
            ('क', "।"),
            ('\n', "Next line"),
            ('(', "inside"),
            ('。', "次の文"),
            ('a', "日本語"),
            ('ก', "ไทย"),
            ('o', ""),
        ] {
            assert!(!spaced(ends, next), "{ends:?} then {next:?}");
        }
    }

    #[test]
    fn hangul_keeps_its_spaces() {
        assert!(spaced('다', "그리고"));
    }

    #[test]
    fn a_new_target_or_any_activity_ends_the_trail() {
        let trail = Trail::after("Hi, how's your day?", 1_u8, 0_u8).unwrap();
        assert!(trail.continues(&1, &0, "Oh yeah"));
        assert!(
            !trail.continues(&2, &0, "Oh yeah"),
            "Focus moved to another window"
        );
        assert!(
            !trail.continues(&1, &1, "Oh yeah"),
            "The user typed, clicked, or moved the pointer"
        );
        assert!(Trail::after("", 1_u8, 0_u8).is_none());
    }

    proptest! {
        #[test]
        fn whitespace_on_either_side_never_doubles(
            ends in any::<char>(),
            next in "\\PC{0,8}",
            space in prop::sample::select(vec![' ', '\t', '\n', '\u{a0}', '\u{3000}']),
        ) {
            prop_assert!(!spaced(space, &next));
            let spaced_next = [space.to_string(), next].concat();
            prop_assert!(!spaced(ends, &spaced_next));
        }
    }
}
