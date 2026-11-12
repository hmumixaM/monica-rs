//! Preserve the pinned HTMLParser text-node semantics, including whitespace.
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};
#[derive(Deserialize)]
struct Entities {
    entities: HashMap<String, String>,
    invalid_refs: HashMap<u32, String>,
    invalid_points: HashSet<u32>,
}
static ENTITIES: LazyLock<Entities> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../data/html-entities.json")).expect("HTML5 entity table")
});
static ENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"&(#(?:[xX][0-9a-fA-F]+|[0-9]+);?|[A-Za-z][A-Za-z0-9]{0,31};?)").unwrap()
});
static TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<!--.*?-->|<[!/?A-Za-z](?:[^'\">]|\"[^\"]*\"|'[^']*')*>"#).unwrap()
});
fn decode(text: &str) -> String {
    ENTITY
        .replace_all(text, |c: &regex::Captures<'_>| {
            let token = &c[1];
            if let Some(number) = token.strip_prefix('#') {
                let number = number.trim_end_matches(';');
                let parsed = if let Some(hex) = number.strip_prefix(['x', 'X']) {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    number.parse::<u32>().ok()
                };
                let Some(number) = parsed else {
                    return "\u{fffd}".into();
                };
                if let Some(value) = ENTITIES.invalid_refs.get(&number) {
                    return value.clone();
                }
                if ENTITIES.invalid_points.contains(&number) {
                    return String::new();
                }
                return char::from_u32(number).unwrap_or('\u{fffd}').to_string();
            }
            for length in (1..=token.len()).rev() {
                if let Some(value) = ENTITIES.entities.get(&token[..length]) {
                    return format!("{value}{}", &token[length..]);
                }
            }
            c[0].to_owned()
        })
        .into_owned()
}
pub(super) fn text(input: &str) -> String {
    let mut output = String::new();
    let mut end = 0;
    let mut hidden: Option<String> = None;
    for token in TAG.find_iter(input) {
        if hidden.is_none() {
            output.push_str(&decode(&input[end..token.start()]));
        }
        end = token.end();
        let tag = token.as_str();
        if tag.starts_with("<!") || tag.starts_with("<?") {
            continue;
        }
        let inner = tag.trim_start_matches('<');
        let closing = inner.starts_with('/');
        let name = inner
            .trim_start_matches('/')
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Some(hidden_tag) = &hidden {
            if closing && &name == hidden_tag {
                hidden = None;
            }
            continue;
        }
        if !closing {
            if ["script", "style"].contains(&name.as_str()) {
                hidden = Some(name.clone());
            }
            if ["br", "p", "div", "li"].contains(&name.as_str()) {
                output.push('\n');
            }
        }
    }
    if hidden.is_none() {
        output.push_str(&decode(&input[end..]));
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_nodes_and_hides_script() {
        assert_eq!(
            text(
                "<div>A  &amp; B</div><div>C<br>D</div><script>if(x<1){'<p>bad'}</script><style>bad</style><p>&NotEqualTilde; &#128;"
            ),
            "\nA  & B\nC\nD\n≂̸ €"
        );
    }
}
