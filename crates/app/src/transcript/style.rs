use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use super::Fillers;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Style {
    #[default]
    Prose,
    Verbatim,
    Identifier(Case),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Case {
    Camel,
    Pascal,
    Snake,
    Kebab,
    Constant,
}

impl Style {
    pub(crate) const ALL: [Self; 7] = [
        Self::Prose,
        Self::Verbatim,
        Self::Identifier(Case::Camel),
        Self::Identifier(Case::Pascal),
        Self::Identifier(Case::Snake),
        Self::Identifier(Case::Kebab),
        Self::Identifier(Case::Constant),
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Prose => "prose",
            Self::Verbatim => "verbatim",
            Self::Identifier(Case::Camel) => "camel",
            Self::Identifier(Case::Pascal) => "pascal",
            Self::Identifier(Case::Snake) => "snake",
            Self::Identifier(Case::Kebab) => "kebab",
            Self::Identifier(Case::Constant) => "constant",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Prose => "Prose",
            Self::Verbatim => "Verbatim",
            Self::Identifier(Case::Camel) => "camelCase",
            Self::Identifier(Case::Pascal) => "PascalCase",
            Self::Identifier(Case::Snake) => "snake_case",
            Self::Identifier(Case::Kebab) => "kebab-case",
            Self::Identifier(Case::Constant) => "CONSTANT_CASE",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.name() == name)
    }

    pub(crate) const fn fillers(self, configured: Fillers) -> Fillers {
        match self {
            Self::Verbatim => Fillers::Preserve,
            Self::Prose | Self::Identifier(_) => configured,
        }
    }

    pub(crate) fn apply(self, text: String) -> String {
        match self {
            Self::Prose | Self::Verbatim => text,
            Self::Identifier(case) => case.apply(&text),
        }
    }
}

impl Serialize for Style {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Style {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name).ok_or_else(|| {
            D::Error::custom("Choose prose, verbatim, camel, pascal, snake, kebab or constant.")
        })
    }
}

impl Case {
    fn apply(self, text: &str) -> String {
        let mut output = String::with_capacity(text.len());
        for word in text.split(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '\'' | '’')
        }) {
            let mut characters = word.chars().filter(|character| character.is_alphanumeric());
            let Some(first) = characters.next() else {
                continue;
            };
            let follows_word = !output.is_empty();
            if follows_word {
                match self {
                    Self::Snake | Self::Constant => output.push('_'),
                    Self::Kebab => output.push('-'),
                    Self::Camel | Self::Pascal => {},
                }
            }
            let uppercase = self == Self::Constant;
            let capitalize = self == Self::Pascal || (self == Self::Camel && follows_word);
            push_cased(&mut output, first, uppercase || capitalize);
            for character in characters {
                push_cased(&mut output, character, uppercase);
            }
        }
        output
    }
}

fn push_cased(output: &mut String, character: char, uppercase: bool) {
    if uppercase {
        output.extend(
            character
                .to_uppercase()
                .filter(|character| character.is_alphanumeric()),
        );
    } else {
        output.extend(
            character
                .to_lowercase()
                .filter(|character| character.is_alphanumeric()),
        );
    }
}

#[cfg(test)]
mod tests;
