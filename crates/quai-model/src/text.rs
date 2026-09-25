//! Text from outside (an explorer, an indexer, a node's error) made safe to show.

/// `text` without control characters, bidirectional overrides, invisible and width-changing
/// marks, cut to `max_chars`.
pub fn clean(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c,
                    '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}'
                    // Width: variation selectors, the keycap mark, flags, skin tones, tags.
                    | '\u{FE00}'..='\u{FE0F}' | '\u{20E3}' | '\u{1F1E6}'..='\u{1F1FF}' | '\u{1F3FB}'..='\u{1F3FF}' | '\u{E0000}'..='\u{E007F}' | '\u{E0100}'..='\u{E01EF}')
        })
        .take(max_chars)
        .collect()
}

/// [`clean`] for explorer and indexer text (names, descriptions, messages).
pub fn clean_text(text: &str) -> String {
    clean(text, 4096)
}
