//! Copilot's documented inline-completion protocol and UTF-16 coordinate handling.
//! Kept independent of the UI so Unicode, replacement and sync rules are testable.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub insert_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Value>,
    // Keep opaque server fields intact when reporting shown/accepted items.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

pub fn completions_from_response(response: Value) -> Vec<Completion> {
    let items = match response {
        Value::Array(items) => items,
        Value::Object(mut result) => match result.remove("items") {
            Some(Value::Array(items)) => items,
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    // A server may add item kinds the editor cannot display. Skip only those
    // items, without logging model output or discarding valid alternatives.
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value(item).ok())
        .collect()
}

pub fn position_at(text: &str, offset: usize) -> Option<Position> {
    if !text.is_char_boundary(offset) {
        return None;
    }
    let prefix = text.get(..offset)?;
    let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
    Some(Position {
        line: u32::try_from(prefix.bytes().filter(|c| *c == b'\n').count()).ok()?,
        character: u32::try_from(prefix[line_start..].encode_utf16().count()).ok()?,
    })
}

pub fn offset_at(text: &str, position: Position) -> Option<usize> {
    let start = if position.line == 0 {
        0
    } else {
        text.match_indices('\n').nth(position.line as usize - 1)?.0 + 1
    };
    let line = text.get(start..)?.split('\n').next()?;
    let mut units = 0;
    for (offset, character) in line.char_indices() {
        if units == position.character {
            return Some(start + offset);
        }
        units += character.len_utf16() as u32;
        if units > position.character {
            return None; // Never split an emoji/surrogate pair.
        }
    }
    (units == position.character).then_some(start + line.len())
}

/// One minimal incremental LSP change, with positions relative to the old text.
pub fn incremental_change(old: &str, new: &str) -> Option<Value> {
    if old == new {
        return None;
    }
    let mut prefix = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = old[prefix..]
        .bytes()
        .rev()
        .zip(new[prefix..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    Some(json!({
        "range": {"start": position_at(old, prefix)?, "end": position_at(old, old.len() - suffix)?},
        "text": &new[prefix..new.len() - suffix],
    }))
}

/// Return only text that can be displayed as a non-destructive insertion at the
/// cursor. Changes to existing text belong to next-edit suggestions, not ghost text.
pub fn ghost_text(document: &str, cursor: usize, completion: &Completion) -> Option<String> {
    let position = position_at(document, cursor)?;
    let range = completion.range.clone().unwrap_or(Range {
        start: position,
        end: position,
    });
    let start = offset_at(document, range.start)?;
    let end = offset_at(document, range.end)?;
    if start > cursor || cursor > end {
        return None;
    }
    let replacement = completion.insert_text.replace("\r\n", "\n");
    let before = document.get(start..cursor)?;
    let after = document.get(cursor..end)?;
    let remaining = replacement.strip_prefix(before)?.strip_suffix(after)?;
    (!remaining.is_empty()).then(|| remaining.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, range: Option<Range>) -> Completion {
        Completion {
            insert_text: text.into(),
            range,
            command: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn utf16_positions_round_trip_and_reject_surrogate_halves() {
        let text = "привет 🦀\nlet α = 1;\n";
        for offset in (0..=text.len()).filter(|i| text.is_char_boundary(*i)) {
            assert_eq!(offset_at(text, position_at(text, offset).unwrap()), Some(offset));
        }
        assert_eq!(offset_at("a🦀b", Position { line: 0, character: 2 }), None);
        assert_eq!(offset_at("a", Position { line: 1, character: 0 }), None);
        assert_eq!(offset_at("a", Position { line: 0, character: 2 }), None);
    }

    #[test]
    fn incremental_changes_can_be_applied_to_unicode_documents() {
        for (old, new) in [
            ("", "fn hello()"),
            ("hello", ""),
            ("a🦀z", "a🐙z"),
            ("привет\nfoo\n", "привет\nbar🦀\n"),
            ("same", "same"),
            ("a\n", "a\n\n"),
        ] {
            if let Some(change) = incremental_change(old, new) {
                let range: Range = serde_json::from_value(change["range"].clone()).unwrap();
                let mut applied = old.to_owned();
                applied.replace_range(
                    offset_at(old, range.start).unwrap()..offset_at(old, range.end).unwrap(),
                    change["text"].as_str().unwrap(),
                );
                assert_eq!(applied, new);
            } else {
                assert_eq!(old, new);
            }
        }
    }

    #[test]
    fn ghost_text_preserves_existing_prefix_and_suffix() {
        let range = Some(Range {
            start: Position::default(),
            end: Position { line: 0, character: 3 },
        });
        assert_eq!(ghost_text("foo", 2, &item("foobar", range.clone())), None);
        assert_eq!(ghost_text("foo", 2, &item("foXXo", range.clone())), Some("XX".into()));
        assert_eq!(ghost_text("foo", 2, &item("different", range)), None);
        assert_eq!(
            ghost_text("fn x() {", 8, &item("\r\n    42\r\n}", None)),
            Some("\n    42\n}".into())
        );
        assert_eq!(ghost_text("abc", 3, &item("", None)), None);
    }

    #[test]
    fn opaque_completion_fields_survive_feedback_round_trip() {
        let value =
            json!({"insertText":"x", "command":{"command":"accepted", "arguments":["opaque"]}, "opaqueServerField":42});
        let completion: Completion = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(completion).unwrap(), value);
    }

    #[test]
    fn unsupported_items_do_not_hide_valid_alternatives() {
        let items = completions_from_response(json!({"items":[
            {"insertText":{"snippet":"unsupported item"}},
            {"insertText":"plain code"},
        ]}));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text, "plain code");
        assert!(completions_from_response(Value::Null).is_empty());
    }
}
