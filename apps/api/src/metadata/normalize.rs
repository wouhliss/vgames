//! Text normalization for provider data (01-security §5 "Text from providers").

use time::{Date, Month};
use unicode_normalization::UnicodeNormalization;

/// Case-, punctuation- and trademark-insensitive form of a title used for matching.
pub fn normalize_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    // Marks go first: NFKC would turn `™` into the letters "TM".
    let unmarked = s.chars().filter(|c| !matches!(c, '™' | '®' | '©' | '℠'));
    for c in unmarked.nfkc().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_string()
}

/// Similarity of two titles after [`normalize_title`], in `0..=1`.
pub fn title_score(a: &str, b: &str) -> f32 {
    let (a, b) = (normalize_title(a), normalize_title(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    strsim::normalized_levenshtein(&a, &b) as f32
}

/// Trims `s` and shortens it to at most `max` characters, cutting at a word boundary
/// and ending with `…`. Returns `None` for empty text.
pub fn clamp(s: &str, max: usize) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || max == 0 {
        return None;
    }
    if s.chars().count() <= max {
        return Some(s.to_string());
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(i) if i > cut.len() / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    Some(format!("{}…", cut.trim_end()))
}

/// The first sentences of `s` that fit in `max` characters (falls back to [`clamp`]).
pub fn summarize(s: &str, max: usize) -> Option<String> {
    let s = s.trim();
    if s.chars().count() <= max {
        return clamp(s, max);
    }
    let mut end = None;
    for (count, (i, c)) in s.char_indices().enumerate() {
        if count >= max {
            break;
        }
        if matches!(c, '.' | '!' | '?') {
            end = Some(i + c.len_utf8());
        }
    }
    match end {
        Some(e) if e > max / 3 => clamp(&s[..e], max),
        _ => clamp(s, max),
    }
}

/// Converts provider HTML to plain text: tags removed, block elements become line breaks,
/// list items become `- ` lines, entities decoded, `script`/`style` contents dropped.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut skip_until: Option<&'static str> = None;
    while let Some(lt) = rest.find('<') {
        if skip_until.is_none() {
            push_text(&mut out, &rest[..lt]);
        }
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            if skip_until.is_none() {
                push_text(&mut out, &rest[lt..]);
            }
            rest = "";
            break;
        };
        let tag = &after[..gt];
        rest = &after[gt + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .flat_map(|c| c.to_lowercase())
            .collect();
        if let Some(end) = skip_until {
            if closing && name == end {
                skip_until = None;
            }
            continue;
        }
        match name.as_str() {
            "script" if !closing => skip_until = Some("script"),
            "style" if !closing => skip_until = Some("style"),
            "br" => out.push('\n'),
            "li" if !closing => out.push_str("\n- "),
            "p" | "div" | "ul" | "ol" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr"
            | "blockquote" => out.push_str("\n\n"),
            _ => {}
        }
    }
    if skip_until.is_none() {
        push_text(&mut out, rest);
    }
    tidy(&out)
}

fn push_text(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let end = after.find(';').filter(|&i| i <= 10);
        match end.and_then(|i| decode_entity(&after[..i]).map(|c| (i, c))) {
            Some((i, c)) => {
                out.push(c);
                rest = &after[i + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
}

fn decode_entity(name: &str) -> Option<char> {
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "rsquo" => '’',
        "lsquo" => '‘',
        "rdquo" => '”',
        "ldquo" => '“',
        "trade" => '™',
        "reg" => '®',
        "copy" => '©',
        _ => {
            let code =
                if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                    u32::from_str_radix(hex, 16).ok()?
                } else {
                    name.strip_prefix('#')?.parse().ok()?
                };
            return char::from_u32(code).filter(|c| !c.is_control() || *c == '\n');
        }
    };
    Some(c)
}

/// Collapses spaces inside lines and runs of blank lines; drops control characters.
fn tidy(s: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut blank = true;
    for line in s.lines() {
        let words: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || (c.is_control() && c != '\n'))
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() || words == ["-"] {
            if !blank {
                lines.push(String::new());
                blank = true;
            }
        } else {
            lines.push(words.join(" "));
            blank = false;
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// Parses Steam's localized-ish release dates (`"25 Jan, 2018"`, `"Jan 25, 2018"`).
pub fn parse_steam_date(s: &str) -> Option<Date> {
    let (mut day, mut month, mut year) = (None, None, None);
    for tok in s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        if let Ok(n) = tok.parse::<u16>() {
            if tok.len() == 4 {
                year = Some(i32::from(n));
            } else if n <= 31 {
                day = Some(u8::try_from(n).ok()?);
            }
        } else if month.is_none() {
            month = month_from_name(tok);
        }
    }
    Date::from_calendar_date(year?, month?, day?).ok()
}

fn month_from_name(s: &str) -> Option<Month> {
    let m = s.get(..3)?.to_ascii_lowercase();
    Some(match m.as_str() {
        "jan" => Month::January,
        "feb" => Month::February,
        "mar" => Month::March,
        "apr" => Month::April,
        "may" => Month::May,
        "jun" => Month::June,
        "jul" => Month::July,
        "aug" => Month::August,
        "sep" => Month::September,
        "oct" => Month::October,
        "nov" => Month::November,
        "dec" => Month::December,
        _ => return None,
    })
}

/// Deduplicated, trimmed genre names (at most 20, each 1-64 characters).
pub fn genres<I: IntoIterator<Item = String>>(names: I) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for g in names {
        let Some(g) = clamp(&g, 64) else { continue };
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&g)) {
            out.push(g);
        }
        if out.len() == 20 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_match_ignoring_marks_case_and_punctuation() {
        assert_eq!(normalize_title("  Half-Life™: ALYX® "), "half life alyx");
        assert!(title_score("Celeste", "CELESTE™") > 0.99);
        assert!(title_score("Half-Life: Alyx", "Half Life Alyx") > 0.99);
        assert!(title_score("Celeste", "Celeste Classic") < 0.95);
        assert_eq!(title_score("", "x"), 0.0);
        assert!(title_score("ＦＵＬＬＷＩＤＴＨ", "fullwidth") > 0.99);
    }

    #[test]
    fn clamps_at_word_boundaries() {
        assert_eq!(clamp("  short  ", 10).as_deref(), Some("short"));
        assert_eq!(clamp("   ", 10), None);
        let c = clamp("one two three four five", 12).unwrap();
        assert!(c.chars().count() <= 12, "{c}");
        assert_eq!(c, "one two…");
        assert_eq!(clamp(&"é".repeat(30), 10).unwrap().chars().count(), 10);
        let s = summarize(
            "First sentence here. Second one is much longer than allowed.",
            30,
        )
        .unwrap();
        assert_eq!(s, "First sentence here.");
    }

    #[test]
    fn html_becomes_plain_text() {
        let html = "<h2 class=\"bb_tag\">About</h2><p>Help Madeline survive&nbsp;her &amp; <b>inner</b> demons.</p>\
                    <ul><li>700+ screens</li><li>Hard &lt;but&gt; fair</li></ul><br><script>alert(1)</script>\
                    <img src=\"https://x\">End &#8212; &#x2764; &bogus; & more";
        assert_eq!(
            html_to_text(html),
            "About\n\nHelp Madeline survive her & inner demons.\n\n- 700+ screens\n- Hard <but> fair\n\nEnd — ❤ &bogus; & more"
        );
        assert_eq!(html_to_text("a < b"), "a < b");
        assert_eq!(html_to_text("<style>p{}</style>ok"), "ok");
        assert_eq!(html_to_text("&#0;&#x1b;x"), "&#0;&#x1b;x");
    }

    #[test]
    fn steam_dates() {
        let d = |y, m, day| Date::from_calendar_date(y, m, day).unwrap();
        assert_eq!(
            parse_steam_date("25 Jan, 2018"),
            Some(d(2018, Month::January, 25))
        );
        assert_eq!(
            parse_steam_date("Jan 25, 2018"),
            Some(d(2018, Month::January, 25))
        );
        assert_eq!(parse_steam_date("Coming soon"), None);
        assert_eq!(parse_steam_date("Q3 2027"), None);
        assert_eq!(parse_steam_date("31 Feb, 2020"), None);
    }

    #[test]
    fn genres_are_deduplicated_and_bounded() {
        let g = genres([
            "Indie".into(),
            " indie ".into(),
            "".into(),
            "Platform".into(),
        ]);
        assert_eq!(g, ["Indie", "Platform"]);
        assert_eq!(genres((0..50).map(|i| format!("g{i}"))).len(), 20);
    }
}
