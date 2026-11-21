use std::{iter, time::Duration};

const CREDITS: &[&str] = &[
    "Thank you.",
    "Thank you for watching.",
    "Thanks for watching!",
    "Subtitles by the Amara.org community",
    "Untertitel der Amara.org-Community",
    "Sous-titrage Société Radio-Canada",
    "Sous-titrage ST' 501",
    "Sous-titres réalisés par la communauté d'Amara.org",
    "Sous-titres par la communauté d'Amara.org",
    "Subtítulos realizados por la comunidad de Amara.org",
    "Sottotitoli creati dalla comunità Amara.org",
    "İzlediğiniz için teşekkür ederim.",
    "Altyazı M.K.",
    "Altyazı M.",
    "ご視聴ありがとうございました",
    "시청해주셔서 감사합니다.",
    "字幕由Amara.org社区提供",
];

const BRIEF: Duration = Duration::from_secs(1);

pub(crate) fn invented<'a>(
    heard: impl Iterator<Item = &'a str> + Clone,
    audible: Duration,
    prompt: Option<&str>,
) -> bool {
    let echoes = |text| folded(text).eq(heard.clone().flat_map(folded));
    audible < BRIEF && (CREDITS.iter().copied().any(echoes) || prompt.is_some_and(echoes))
}

fn folded(text: &str) -> impl Iterator<Item = char> + Clone + '_ {
    text.split(|character: char| !character.is_alphanumeric() && !is_mark(character))
        .filter(|word| word.contains(char::is_alphanumeric))
        .flat_map(|word| {
            iter::once(' ').chain(
                word.chars()
                    .flat_map(char::to_lowercase)
                    .filter(|character| !is_mark(*character))
                    .map(|character| if character == 'ı' { 'i' } else { character }),
            )
        })
}

const fn is_mark(character: char) -> bool {
    matches!(character, '\u{300}'..='\u{36f}')
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIEFLY: Duration = Duration::from_millis(900);

    fn says(text: &str, audible: Duration) -> bool {
        invented(iter::once(text), audible, None)
    }

    #[test]
    fn a_lone_credit_from_a_brief_recording_is_invented_however_it_is_written() {
        for heard in [
            "Thank you.",
            " thank you ",
            "THANK YOU!",
            "Thank you",
            "Thanks for watching.",
            "subtitles by the amara org community",
            "İzlediğiniz için teşekkür ederim.",
            "IZLEDIĞINIZ İÇİN TEŞEKKÜR EDERİM",
            "I\u{307}zlediğiniz için teşekkür ederim",
            "ALTYAZI M.K.",
            "ご視聴ありがとうございました。",
        ] {
            assert!(says(heard, BRIEFLY), "{heard}");
        }
        assert!(invented(
            ["Thank", " you."].into_iter(),
            Duration::ZERO,
            None
        ));
    }

    #[test]
    fn a_brief_recording_that_repeats_every_hint_is_invented() {
        let prompt = "Example Name, Kubernetes";
        for heard in ["Example Name, Kubernetes.", "example name kubernetes"] {
            assert!(
                invented(iter::once(heard), BRIEFLY, Some(prompt)),
                "{heard}"
            );
        }
        assert!(!invented(iter::once("Kubernetes."), BRIEFLY, Some(prompt)));
        assert!(!invented(iter::once(prompt), BRIEF, Some(prompt)));
    }

    #[test]
    fn speech_around_a_credit_or_a_second_of_sound_keeps_it() {
        for heard in [
            "Thank you for the report.",
            "No, thank you.",
            "Thank you. Thank you.",
            "Thanks",
            "Altyazı",
            "",
        ] {
            assert!(!says(heard, Duration::ZERO), "{heard}");
        }
        assert!(!says("Thank you.", BRIEF));
        assert!(!says("Thanks for watching!", Duration::from_secs(4)));
    }
}
