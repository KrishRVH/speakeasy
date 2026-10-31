//! Curated local model choices. The persisted runner stays separate from its model's capabilities.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Model {
    Parakeet,
    Nemotron,
}

impl Model {
    pub(crate) const fn file_name(self) -> &'static str {
        match self {
            Self::Parakeet => "parakeet-tdt-0.6b-v3.q8_0.gguf",
            Self::Nemotron => "nemotron-3.5-asr-streaming-0.6b.q8_0.gguf",
        }
    }

    pub(crate) fn at(path: &std::path::Path) -> Option<Self> {
        [Self::Parakeet, Self::Nemotron].into_iter().find(|choice| {
            path.file_name()
                .is_some_and(|name| name == choice.file_name())
        })
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Parakeet => "Parakeet v3",
            Self::Nemotron => "Nemotron 3.5",
        }
    }
}

/// Recovered Shows can carry another engine's language spelling. Unsupported locales request
/// detection rather than forwarding an invalid decoder prompt.
pub(crate) fn nemotron_language(language: &str) -> &str {
    if LANGUAGES.iter().any(|(code, _)| *code == language) {
        language
    } else {
        "auto"
    }
}

/// The unadapted Nemotron checkpoint supports these 32 locales. Other tokenizer languages require
/// fine-tuning and must not be offered as working choices for this download.
pub(crate) const LANGUAGES: &[(&str, &str)] = &[
    ("auto", "Automatic"),
    ("en-US", "English (US)"),
    ("en-GB", "English (UK)"),
    ("es-US", "Spanish (US)"),
    ("es-ES", "Spanish (Spain)"),
    ("fr-FR", "French (France)"),
    ("fr-CA", "French (Canada)"),
    ("it-IT", "Italian"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("pt-PT", "Portuguese (Portugal)"),
    ("nl-NL", "Dutch"),
    ("de-DE", "German"),
    ("tr-TR", "Turkish"),
    ("ru-RU", "Russian"),
    ("ar-AR", "Arabic"),
    ("hi-IN", "Hindi"),
    ("ja-JP", "Japanese"),
    ("ko-KR", "Korean"),
    ("vi-VN", "Vietnamese"),
    ("uk-UA", "Ukrainian"),
    ("pl-PL", "Polish"),
    ("sv-SE", "Swedish"),
    ("cs-CZ", "Czech"),
    ("nb-NO", "Norwegian Bokmål"),
    ("da-DK", "Danish"),
    ("bg-BG", "Bulgarian"),
    ("fi-FI", "Finnish"),
    ("hr-HR", "Croatian"),
    ("sk-SK", "Slovak"),
    ("zh-CN", "Mandarin"),
    ("hu-HU", "Hungarian"),
    ("ro-RO", "Romanian"),
    ("et-EE", "Estonian"),
];

/// Whisper uses language codes without a regional suffix; Norwegian's code is `no`.
pub(crate) fn whisper_language(locale: &str) -> &str {
    match locale {
        "nb-NO" => "no",
        _ => locale
            .split_once('-')
            .map_or(locale, |(language, _)| language),
    }
}

/// Whisper language codes; actual coverage depends on the manually selected multilingual model.
pub(crate) const WHISPER_LANGUAGES: &[(&str, &str)] = &[
    ("auto", "Automatic"),
    ("af", "Afrikaans"),
    ("sq", "Albanian"),
    ("am", "Amharic"),
    ("ar", "Arabic"),
    ("hy", "Armenian"),
    ("as", "Assamese"),
    ("az", "Azerbaijani"),
    ("ba", "Bashkir"),
    ("eu", "Basque"),
    ("be", "Belarusian"),
    ("bn", "Bengali"),
    ("bs", "Bosnian"),
    ("br", "Breton"),
    ("bg", "Bulgarian"),
    ("yue", "Cantonese"),
    ("ca", "Catalan"),
    ("zh", "Chinese"),
    ("hr", "Croatian"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("nl", "Dutch"),
    ("en", "English"),
    ("et", "Estonian"),
    ("fo", "Faroese"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("gl", "Galician"),
    ("ka", "Georgian"),
    ("de", "German"),
    ("el", "Greek"),
    ("gu", "Gujarati"),
    ("ht", "Haitian Creole"),
    ("ha", "Hausa"),
    ("haw", "Hawaiian"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("hu", "Hungarian"),
    ("is", "Icelandic"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("jw", "Javanese"),
    ("kn", "Kannada"),
    ("kk", "Kazakh"),
    ("km", "Khmer"),
    ("ko", "Korean"),
    ("lo", "Lao"),
    ("la", "Latin"),
    ("lv", "Latvian"),
    ("ln", "Lingala"),
    ("lt", "Lithuanian"),
    ("lb", "Luxembourgish"),
    ("mk", "Macedonian"),
    ("mg", "Malagasy"),
    ("ms", "Malay"),
    ("ml", "Malayalam"),
    ("mt", "Maltese"),
    ("mi", "Maori"),
    ("mr", "Marathi"),
    ("mn", "Mongolian"),
    ("my", "Myanmar"),
    ("ne", "Nepali"),
    ("no", "Norwegian"),
    ("nn", "Nynorsk"),
    ("oc", "Occitan"),
    ("ps", "Pashto"),
    ("fa", "Persian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("pa", "Punjabi"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sa", "Sanskrit"),
    ("sr", "Serbian"),
    ("sn", "Shona"),
    ("sd", "Sindhi"),
    ("si", "Sinhala"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("so", "Somali"),
    ("es", "Spanish"),
    ("su", "Sundanese"),
    ("sw", "Swahili"),
    ("sv", "Swedish"),
    ("tl", "Tagalog"),
    ("tg", "Tajik"),
    ("ta", "Tamil"),
    ("tt", "Tatar"),
    ("te", "Telugu"),
    ("th", "Thai"),
    ("bo", "Tibetan"),
    ("tr", "Turkish"),
    ("tk", "Turkmen"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("uz", "Uzbek"),
    ("vi", "Vietnamese"),
    ("cy", "Welsh"),
    ("yi", "Yiddish"),
    ("yo", "Yoruba"),
];
