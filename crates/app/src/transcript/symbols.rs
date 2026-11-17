use std::slice;

const PAUSES: [char; 2] = [',', ';'];

const QUOTES: [char; 4] = ['"', '\'', '“', '‘'];

const NAMING: &[&str] = &[
    "a", "an", "the", "another", "leading", "trailing", "double", "single", "extra",
];

const STOPS: [char; 3] = ['.', '!', '?'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Symbols {
    Write,
    Keep,
}

#[derive(Clone, Copy)]
enum Mark {
    Break(&'static str),
    Join(&'static str),
    Open(char),
    Close(char),
    Pair(char),
}

const COMMANDS: &[(&[&str], Mark)] = &[
    (&["new", "paragraph"], Mark::Break("\n\n")),
    (&["new", "line"], Mark::Break("\n")),
    (&["newline"], Mark::Break("\n")),
    (&["underscore"], Mark::Join("_")),
    (&["forward", "slash"], Mark::Join("/")),
    (&["backslash"], Mark::Join("\\")),
    (&["open", "paren"], Mark::Open('(')),
    (&["close", "paren"], Mark::Close(')')),
    (&["open", "bracket"], Mark::Open('[')),
    (&["close", "bracket"], Mark::Close(']')),
    (&["open", "brace"], Mark::Open('{')),
    (&["close", "brace"], Mark::Close('}')),
    (&["open", "quote"], Mark::Open('"')),
    (&["close", "quote"], Mark::Close('"')),
    (&["backtick"], Mark::Pair('`')),
];

pub(crate) fn written(text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut output = Writer::default();
    let mut rest = tokens.as_slice();
    let mut previous = None;
    while let Some(token) = rest.first() {
        let spoken = if let Some((mark, spoken)) = command(previous, rest, &output) {
            output.mark(mark, spoken.last().map_or("", |last| trailing(last)));
            spoken
        } else {
            output.word(token);
            slice::from_ref(token)
        };
        previous = spoken.last().copied();
        rest = rest.get(spoken.len()..).unwrap_or_default();
    }
    output.text
}

fn command<'a>(
    previous: Option<&str>,
    tokens: &'a [&'a str],
    written: &Writer,
) -> Option<(Mark, &'a [&'a str])> {
    COMMANDS.iter().find_map(|(words, mark)| {
        let spoken = tokens.get(..words.len())?;
        let said = spoken
            .iter()
            .zip(words.iter())
            .all(|(token, word)| bare(token).eq_ignore_ascii_case(word));
        let ends = spoken.len() == tokens.len();
        let alone = || {
            let before = previous.is_none_or(|token| !trailing(token).is_empty());
            let after = ends
                || spoken
                    .last()
                    .is_some_and(|token| !trailing(token).is_empty());
            before && after
        };
        let named = || {
            spoken
                .first()
                .is_some_and(|token| token.starts_with(QUOTES))
                || previous.is_some_and(|token| {
                    trailing(token).is_empty()
                        && NAMING
                            .iter()
                            .any(|word| bare(token).eq_ignore_ascii_case(word))
                })
        };
        let stops = || {
            spoken
                .last()
                .is_some_and(|token| trailing(token).contains(STOPS))
        };
        let opens = |symbol| {
            !named()
                && tokens
                    .get(spoken.len()..)
                    .is_some_and(|after| closed_later(symbol, after))
        };
        let fits = match *mark {
            Mark::Break(_) => alone(),
            Mark::Join(_) => previous.is_some() && !ends && !named() && !stops(),
            Mark::Open(symbol) => opens(symbol),
            Mark::Close(symbol) => written.pending(symbol),
            Mark::Pair(symbol) => written.pending(symbol) || opens(symbol),
        };
        (said && fits).then_some((*mark, spoken))
    })
}

const fn opener(close: char) -> char {
    match close {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        other => other,
    }
}

fn closed_later(open: char, after: &[&str]) -> bool {
    COMMANDS.iter().any(|(words, mark)| {
        let closes = match *mark {
            Mark::Close(symbol) | Mark::Pair(symbol) => opener(symbol) == open,
            Mark::Break(_) | Mark::Join(_) | Mark::Open(_) => false,
        };
        closes
            && after.windows(words.len()).any(|window| {
                window
                    .iter()
                    .zip(words.iter())
                    .all(|(token, word)| bare(token).eq_ignore_ascii_case(word))
            })
    })
}

fn bare(token: &str) -> &str {
    token.trim_matches(|character: char| !character.is_alphanumeric())
}

fn trailing(token: &str) -> &str {
    let end = token
        .trim_end_matches(|character: char| !character.is_alphanumeric())
        .len();
    token.get(end..).unwrap_or_default()
}

#[derive(Default)]
struct Writer {
    text: String,
    attached: bool,
}

impl Writer {
    fn word(&mut self, token: &str) {
        self.space();
        self.text.push_str(token);
        self.attached = false;
    }

    fn space(&mut self) {
        if !self.attached && !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push(' ');
        }
    }

    fn mark(&mut self, mark: Mark, ending: &str) {
        match mark {
            Mark::Break(symbol) | Mark::Join(symbol) => {
                self.drop_pause();
                self.text.push_str(symbol);
                self.attached = true;
            },
            Mark::Open(symbol) => self.open(symbol),
            Mark::Close(symbol) => self.close(symbol, ending),
            Mark::Pair(symbol) => {
                if self.text.matches(symbol).count() % 2 == 0 {
                    self.open(symbol);
                } else {
                    self.close(symbol, ending);
                }
            },
        }
    }

    fn open(&mut self, symbol: char) {
        self.space();
        self.text.push(symbol);
        self.attached = true;
    }

    fn close(&mut self, symbol: char, ending: &str) {
        self.drop_pause();
        self.text.push(symbol);
        self.text.push_str(ending);
        self.attached = false;
    }

    fn pending(&self, close: char) -> bool {
        let open = opener(close);
        let opened = self.text.matches(open).count();
        if open == close {
            opened % 2 == 1
        } else {
            opened > self.text.matches(close).count()
        }
    }

    fn drop_pause(&mut self) {
        let kept = self.text.trim_end_matches(PAUSES).len();
        self.text.truncate(kept);
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn spoken_breaks_drop_the_punctuation_recognition_guessed_around_them() {
        for (heard, expected) in [
            (
                "Fix the bug. New line. Then run the tests.",
                "Fix the bug.\nThen run the tests.",
            ),
            (
                "First point, new paragraph, second point",
                "First point\n\nsecond point",
            ),
            ("One. New line. New line. Two.", "One.\n\nTwo."),
            ("Newline. Hello", "\nHello"),
            ("Done. New line.", "Done.\n"),
        ] {
            assert_eq!(written(heard), expected, "{heard}");
        }
    }

    #[test]
    fn joining_symbols_close_the_gap_on_both_sides() {
        for (heard, expected) in [
            ("Rename user underscore ID.", "Rename user_ID."),
            ("user, underscore, id", "user_id"),
            ("src forward slash main.rs", "src/main.rs"),
            ("C: backslash Users", "C:\\Users"),
            ("my underscore var", "my_var"),
            ("one forward slash two", "one/two"),
        ] {
            assert_eq!(written(heard), expected, "{heard}");
        }
    }

    #[test]
    fn opening_and_closing_symbols_hug_what_they_enclose() {
        for (heard, expected) in [
            ("Call foo open paren close paren.", "Call foo ()."),
            ("Say open quote hello, close quote now", "Say \"hello\" now"),
            ("items open bracket zero close bracket", "items [zero]"),
            (
                "Run backtick cargo test backtick, then push.",
                "Run `cargo test`, then push.",
            ),
            (
                "arr open bracket one close bracket equals two",
                "arr [one] equals two",
            ),
            ("f open paren a close paren", "f (a)"),
            ("open quote yes or no close quote", "\"yes or no\""),
        ] {
            assert_eq!(written(heard), expected, "{heard}");
        }
    }

    #[test]
    fn words_that_only_resemble_commands_stay() {
        for heard in [
            "A new line of products.",
            "Open the bracket drawer.",
            "Look at sign language.",
            "We need a new line.",
            "Start a new paragraph, then stop.",
            "The slash command works.",
            "New lines.",
            "Prefix it with an underscore.",
            "The variable has an underscore in it.",
            "Windows paths use a backslash.",
            "Type the word \"underscore\" here.",
            "Add a backtick before the name.",
            "Escape it with a forward slash, then retry.",
            "End with an underscore",
            "Please close quote 12 today.",
            "Then type close paren.",
            "I typed backslash. It failed.",
            "Run the backtick cargo test backtick target.",
            "Say open quote and stop.",
            "I prefer backtick over quote.",
        ] {
            assert_eq!(written(heard), heard);
        }
    }

    fn ordinary_word() -> impl Strategy<Value = String> {
        "[A-Za-z0-9.,!?'-]{1,12}".prop_filter("Not part of a spoken command", |word| {
            COMMANDS
                .iter()
                .flat_map(|(words, _)| words.iter())
                .all(|command| !bare(word).eq_ignore_ascii_case(command))
        })
    }

    proptest! {
        #[test]
        fn text_without_commands_only_has_its_spacing_normalized(
            words in prop::collection::vec(ordinary_word(), 0..24),
            gaps in prop::collection::vec("[ \t]{1,3}", 24),
        ) {
            let heard: String = words
                .iter()
                .zip(gaps.iter())
                .flat_map(|(word, gap)| [gap.as_str(), word.as_str()])
                .collect();
            prop_assert_eq!(written(&heard), words.join(" "));
        }
    }
}
