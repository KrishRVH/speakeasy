//! Deterministic transcript cleanup before native insertion or a Show.

use speakeasy_show::{Narration, Word};

const PAUSES: [char; 4] = [',', ';', ':', '…'];
const SENTENCE_ENDINGS: [char; 3] = ['.', '!', '?'];

/// Whether to remove English hesitation tokens or keep literal engine words.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fillers {
    Preserve,
    Remove,
}

/// Joins engine segments with single spaces, never line breaks, and optionally removes standalone
/// `um` / `uh`, ignoring ASCII case. Compounds, identifiers, and individually quoted tokens stay
/// literal. Work is linear in the input length, with at most one output allocation and no token
/// collections or per-word allocations.
pub(crate) fn for_insertion(text: &str, fillers: Fillers) -> String {
    let mut output = String::with_capacity(text.len());
    for token in text.split_whitespace() {
        if fillers == Fillers::Remove
            && let Some(ending) = hesitation(token)
        {
            close_after_hesitation(&mut output, ending);
            continue;
        }
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(token);
    }
    output
}

/// The same cleanup for a Show's narration. Timed words lose their hesitations one by one, and a
/// sentence ending a hesitation carried moves to the word before, where clause breaks read it.
pub(crate) fn for_show(narration: Narration, fillers: Fillers) -> Narration {
    match (narration, fillers) {
        (Narration::Text(text), _) => Narration::Text(for_insertion(&text, fillers)),
        (timed, Fillers::Preserve) => timed,
        (Narration::Words(words), Fillers::Remove) => {
            let mut kept: Vec<Word> = Vec::with_capacity(words.len());
            for word in words {
                match hesitation(&word.text) {
                    Some(ending) => {
                        if let Some(previous) = kept.last_mut() {
                            close_after_hesitation(&mut previous.text, ending);
                        }
                    },
                    None => kept.push(word),
                }
            }
            Narration::Words(kept)
        },
    }
}

/// When `token` is a standalone `um` or `uh`, the sentence ending it carried.
fn hesitation(token: &str) -> Option<&str> {
    let spoken = token.trim_start_matches(is_pause_or_ending);
    let word = spoken.trim_end_matches(is_pause_or_ending);
    let trailing = spoken.strip_prefix(word)?;
    (word.eq_ignore_ascii_case("um") || word.eq_ignore_ascii_case("uh"))
        .then(|| trailing.trim_matches(PAUSES))
}

/// Ends the text before a removed hesitation: a comma before it belonged to that pause, but a
/// sentence ending still matters.
fn close_after_hesitation(text: &mut String, ending: &str) {
    if text.ends_with(',') {
        text.pop();
    }
    if !text.is_empty() && !text.ends_with(SENTENCE_ENDINGS) {
        text.push_str(ending);
    }
}

fn is_pause_or_ending(character: char) -> bool {
    PAUSES.contains(&character) || SENTENCE_ENDINGS.contains(&character)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fillers_at_each_position_leave_clean_spacing_and_punctuation() {
        for (input, expected) in [
            ("Um, send uh the report.", "send the report."),
            ("Send, uM, uh, the report, UH.", "Send the report."),
            ("Ready uh?", "Ready?"),
            ("Ready uh?!", "Ready?!"),
            ("Wait um...", "Wait..."),
            ("Done. Um. Next!", "Done. Next!"),
            ("um… uh: hello; um; there!", "hello; there!"),
            ("um", ""),
            (" UM, uh. Um! UH? ", ""),
        ] {
            assert_eq!(for_insertion(input, Fillers::Remove), expected);
        }
    }

    #[test]
    fn meaningful_words_compounds_and_literal_terms_survive() {
        let input = "umbrella thumb album uhura uh-oh um-based um_uh uh2 2um ü umé um\u{301} 中文um um中文 like you know hmm er \"um\" ‘uh’ (um)";
        assert_eq!(for_insertion(input, Fillers::Remove), input);
    }

    #[test]
    fn timed_words_lose_hesitations_but_keep_their_sentence_endings() {
        let word = |text: &str| Word {
            span: speakeasy_show::Span::default(),
            text: text.to_owned(),
        };
        let words = ["Um,", "it", "works", "um.", "Then,", "uh", "nothing!"]
            .map(word)
            .to_vec();
        let Narration::Words(kept) = for_show(Narration::Words(words.clone()), Fillers::Remove)
        else {
            panic!("timed words stay timed");
        };
        let texts: Vec<&str> = kept.iter().map(|word| word.text.as_str()).collect();
        assert_eq!(texts, ["it", "works.", "Then", "nothing!"]);
        assert_eq!(
            for_show(Narration::Words(words.clone()), Fillers::Preserve),
            Narration::Words(words)
        );
        assert_eq!(
            for_show(Narration::Text("Um, ready.".into()), Fillers::Remove),
            Narration::Text("ready.".into())
        );
    }

    #[test]
    fn segment_and_unicode_whitespace_never_become_editor_line_breaks() {
        assert_eq!(
            for_insertion("\tUm,\r\nBonjour\u{a0}uh\u{2003}世界.\n", Fillers::Remove),
            "Bonjour 世界."
        );
        assert_eq!(for_insertion("\r\n\t\u{a0}", Fillers::Remove), "");
        assert_eq!(
            for_insertion("\r\nKeep  this\tpunctuation!\n", Fillers::Remove),
            "Keep this punctuation!"
        );
        assert_eq!(
            for_insertion("\nUm, keep uh all words.\n", Fillers::Preserve),
            "Um, keep uh all words."
        );
    }
}
