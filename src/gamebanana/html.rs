//! Mod descriptions come as HTML. This makes plain text that reads right in
//! a terminal: block tags become line breaks, list items get a dash,
//! entities are decoded and every other tag is dropped.

pub fn to_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        push_text(&mut out, &rest[..start]);
        let Some(len) = rest[start..].find('>') else {
            push_text(&mut out, &rest[start..]);
            rest = "";
            break;
        };
        let tag = &rest[start + 1..start + len];
        rest = &rest[start + len + 1..];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match name.as_str() {
            "br" => out.push('\n'),
            "li" if !closing => {
                line_break(&mut out);
                out.push_str("- ");
            }
            "li" | "tr" => line_break(&mut out),
            "p" | "div" | "ul" | "ol" | "blockquote" | "table" | "hr" | "h1" | "h2" | "h3"
            | "h4" | "h5" | "h6" => {
                line_break(&mut out);
                out.push('\n');
            }
            "script" | "style" if !closing => {
                let end = format!("</{name}");
                let skip = rest.to_ascii_lowercase().find(&end).unwrap_or(rest.len());
                rest = &rest[skip..];
            }
            _ => {}
        }
    }
    push_text(&mut out, rest);
    tidy(&out)
}

/// Source newlines in HTML text are just spaces.
fn push_text(out: &mut String, text: &str) {
    out.push_str(&decode(text).replace(['\n', '\r', '\t'], " "));
}

fn line_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Collapses runs of spaces and blank lines, and trims both ends.
fn tidy(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() && lines.last().is_none_or(String::is_empty) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let entity = rest[1..]
            .find(';')
            .filter(|end| *end <= 8)
            .map(|end| (&rest[1..=end], end + 2));
        let decoded = entity.and_then(|(name, len)| {
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => ' ',
                _ => {
                    let code = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => name.strip_prefix('#')?.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, len))
        });
        if let Some((c, len)) = decoded {
            out.push(c);
            rest = &rest[len..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_become_lines() {
        let html = "<h2>Blue Haze</h2>A <b>recolor</b>\nof Haze.<br>Enjoy!<ul><li>one</li><li>two &amp; three</li></ul><p>Thanks &#8212; me</p>";
        assert_eq!(
            to_text(html),
            "Blue Haze\n\nA recolor of Haze.\nEnjoy!\n\n- one\n- two & three\n\nThanks \u{2014} me"
        );
    }

    #[test]
    fn junk_survives() {
        assert_eq!(to_text("a & b < c"), "a & b < c");
        assert_eq!(to_text("x<script>alert(1)</script>y"), "xy");
        assert_eq!(to_text("&bogus; &#xZZ;"), "&bogus; &#xZZ;");
    }
}
