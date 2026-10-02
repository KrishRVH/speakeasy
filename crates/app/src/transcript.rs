//! Deterministic transcript cleanup before native insertion.

/// Whether to remove English hesitation tokens or keep literal engine words.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fillers {
    Preserve,
    Remove,
}

/// Joins engine segment whitespace and optionally removes standalone `um` / `uh`, ignoring ASCII
/// case. Compounds, identifiers, and individually quoted tokens stay literal. Work is linear in the
/// input length, with at most one output allocation and no token collections or per-word allocations.
pub(crate) fn for_insertion(text: &str, fillers: Fillers) -> String {
    let mut output = String::with_capacity(text.len());
    for token in text.split_whitespace() {
        let word = token.trim_matches(is_pause_punctuation);
        if fillers == Fillers::Remove
            && (word.eq_ignore_ascii_case("um") || word.eq_ignore_ascii_case("uh"))
        {
            // A comma before a hesitation belongs to that pause; a sentence ending still matters.
            if output.ends_with(',') {
                output.pop();
            }
            if !output.is_empty() && !output.ends_with(['.', '!', '?']) {
                let ending = token
                    .trim_start_matches(is_pause_punctuation)
                    .strip_prefix(word)
                    .unwrap_or_default()
                    .trim_matches([',', ';', ':', '…']);
                output.push_str(ending);
            }
            continue;
        }
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(token);
    }
    output
}

fn is_pause_punctuation(character: char) -> bool {
    matches!(character, ',' | '.' | ';' | ':' | '!' | '?' | '…')
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
