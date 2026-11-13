//! Splitting text into sentences, so speech can be asked for one sentence
//! at a time.
//!
//! Servers such as Kokoro synthesize a large block of text before sending
//! any of it; asking for the first sentence on its own gets the first
//! sound out after that sentence alone. [`split`] keeps each piece
//! natural to say: it breaks after sentence-ending punctuation followed by
//! a space, never inside numbers ("3.14") or after common abbreviations
//! ("Dr.", "e.g.", initials), joins pieces too short to say well on their
//! own to the next, and breaks a sentence longer than [`MAX_PIECE_CHARS`]
//! at a comma or space.

/// Pieces shorter than this are joined to the next.
pub const MIN_PIECE_CHARS: usize = 24;
/// Longer sentences are broken at a comma, semicolon, or space.
pub const MAX_PIECE_CHARS: usize = 400;

/// Words that end in a full stop without ending a sentence (compared
/// without the stop, ignoring case).
const ABBREVIATIONS: [&str; 20] = [
    "mr", "mrs", "ms", "dr", "prof", "st", "sr", "jr", "vs", "etc", "e.g", "i.e", "cf", "no",
    "fig", "approx", "mt", "ca", "vol", "ed",
];

/// Whether the sentence-like ending at `end` (the punctuation's byte
/// index) is really an abbreviation or an initial.
fn is_abbreviation(text: &str, end: usize) -> bool {
    let word = text[..end]
        .rsplit(|c: char| c.is_whitespace() || c == '(' || c == '"')
        .next()
        .unwrap_or("");
    let lower = word.to_lowercase();
    // A single letter ("J." in "J. R. Tolkien").
    (word.chars().count() == 1 && word.chars().all(char::is_alphabetic))
        || ABBREVIATIONS.contains(&lower.as_str())
}

/// The text in sentence-sized pieces, in order, trimmed; joined with
/// spaces they say the same as the text.
pub fn split(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let after = index + c.len_utf8();
        let ends = match c {
            '.' | '!' | '?' | '…' | '。' | '！' | '？' => {
                // Closing quotes and brackets belong to the sentence.
                let mut end = after;
                while let Some(&(next, close)) = chars.peek() {
                    if matches!(close, '"' | '\'' | '”' | '’' | ')' | ']' | '»') {
                        end = next + close.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                let followed_by_space = chars.peek().is_none_or(|&(_, next)| next.is_whitespace());
                (followed_by_space && !(c == '.' && is_abbreviation(text, index))).then_some(end)
            }
            // A blank line ends a paragraph.
            '\n' if chars.peek().is_some_and(|&(_, next)| next == '\n') => Some(after),
            _ => None,
        };
        if let Some(end) = ends {
            sentences.push(&text[start..end]);
            start = end;
        }
    }
    sentences.push(&text[start..]);

    let mut pieces: Vec<String> = Vec::new();
    let mut pending = String::new();
    for sentence in sentences {
        let sentence = sentence.trim();
        if sentence.is_empty() {
            continue;
        }
        if !pending.is_empty() {
            pending.push(' ');
        }
        pending.push_str(sentence);
        if pending.chars().count() >= MIN_PIECE_CHARS {
            pieces.extend(break_long(&pending));
            pending.clear();
        }
    }
    if !pending.is_empty() {
        // A short tail joins the last piece rather than standing alone.
        match pieces.last_mut() {
            Some(last) if last.chars().count() + pending.chars().count() < MAX_PIECE_CHARS => {
                last.push(' ');
                last.push_str(&pending);
            }
            _ => pieces.push(pending),
        }
    }
    pieces
}

/// `sentence` in pieces of at most [`MAX_PIECE_CHARS`], broken after the
/// last comma or semicolon that fits, else the last space.
fn break_long(sentence: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = sentence.trim();
    while rest.chars().count() > MAX_PIECE_CHARS {
        let limit = rest
            .char_indices()
            .nth(MAX_PIECE_CHARS)
            .map_or(rest.len(), |(index, _)| index);
        let window = &rest[..limit];
        let cut = window
            .rfind([',', ';', ':'])
            .map(|index| index + 1)
            .or_else(|| window.rfind(char::is_whitespace))
            .filter(|&cut| cut > 0)
            .unwrap_or(limit);
        pieces.push(rest[..cut].trim().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() {
        pieces.push(rest.to_string());
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_sentence_ends() {
        let text = "Mnemosyne was the Titaness of memory. She was the mother of the nine Muses! \
                    Did the poets call on her? They did.";
        assert_eq!(
            split(text),
            vec![
                "Mnemosyne was the Titaness of memory.",
                "She was the mother of the nine Muses!",
                "Did the poets call on her? They did.",
            ]
        );
    }

    #[test]
    fn keeps_numbers_abbreviations_and_quotes_together() {
        let text = "Pi is about 3.14 and e is about 2.72, said Dr. Smith of St. Albans. \
                    \"It was J. R. R. Tolkien,\" she said (e.g. in letters). Then she left the room quietly.";
        assert_eq!(
            split(text),
            vec![
                "Pi is about 3.14 and e is about 2.72, said Dr. Smith of St. Albans.",
                "\"It was J. R. R. Tolkien,\" she said (e.g. in letters).",
                "Then she left the room quietly.",
            ]
        );
    }

    #[test]
    fn joins_short_pieces_and_breaks_long_ones() {
        assert_eq!(
            split("Yes. No. Maybe so, in the end, perhaps."),
            vec!["Yes. No. Maybe so, in the end, perhaps."]
        );
        assert_eq!(
            split("One long first sentence here. Ok."),
            vec!["One long first sentence here. Ok."]
        );
        let long = format!("{} and so on, {}", "word ".repeat(70), "more ".repeat(60));
        let pieces = split(&long);
        assert!(pieces.len() >= 2, "{pieces:?}");
        assert!(
            pieces
                .iter()
                .all(|piece| piece.chars().count() <= MAX_PIECE_CHARS)
        );
        assert_eq!(pieces.join(" "), long.trim());
        // Paragraphs end at blank lines, even without a stop.
        assert_eq!(
            split("A heading without a stop\n\nThen the first paragraph begins here."),
            vec![
                "A heading without a stop",
                "Then the first paragraph begins here."
            ]
        );
    }

    #[test]
    fn says_everything_once() {
        let text = "  Hello there, Mnemosyne!  How are the nine Muses today?\nAll well.  ";
        let joined = split(text).join(" ");
        let words = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(words(&joined), words(text));
        assert!(split("   ").is_empty());
        assert_eq!(split("No stop at the end"), vec!["No stop at the end"]);
    }
}
