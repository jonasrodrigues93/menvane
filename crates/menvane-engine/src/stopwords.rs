use std::collections::HashSet;
use std::sync::OnceLock;

const ENGLISH: &str = include_str!("../data/stopwords/english.stop");
const PORTUGUESE: &str = include_str!("../data/stopwords/portuguese.stop");

pub fn contains(token: &str) -> bool {
    words().contains(token)
}

fn words() -> &'static HashSet<String> {
    static WORDS: OnceLock<HashSet<String>> = OnceLock::new();
    WORDS.get_or_init(|| {
        ENGLISH
            .lines()
            .chain(PORTUGUESE.lines())
            .map(normalize)
            .collect()
    })
}

pub fn normalize(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .map(fold_latin_diacritic)
        .collect()
}

fn fold_latin_diacritic(character: char) -> char {
    match character {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'ç' => 'c',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => character,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_postgresql_stopwords_in_both_languages() {
        assert!(contains("com"));
        assert!(contains("que"));
        assert!(contains("with"));
        assert!(contains("what"));
    }

    #[test]
    fn normalizes_portuguese_diacritics() {
        assert_eq!(normalize("ESTÁ"), "esta");
        assert!(contains(&normalize("NÃO")));
    }
}
