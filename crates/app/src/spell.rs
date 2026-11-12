//! Spelling, as the web's Code tab checks it and IntelliJ does: in
//! comments, in strings that read as prose, and through the whole of a
//! Markdown or text file but its code and links. A word is known if
//! English (Hunspell's en_US, its affixes expanded) or software (cspell's
//! lists, `dev/spell-words.py`) has it, or the person has saved it to the
//! dictionary their Code settings keep. Identifiers in prose are told in
//! their parts (camelCase, snake_case), and acronyms are passed over.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::OnceLock;

/// The words known: every form en_US's words take, and software's.
pub struct Dictionary {
    words: HashSet<String>,
}

static DICTIONARY: OnceLock<Dictionary> = OnceLock::new();

impl Dictionary {
    /// The bundled dictionary, read the first time it is asked for (a
    /// tenth of a second or so).
    pub fn get() -> &'static Dictionary {
        DICTIONARY.get_or_init(|| {
            Dictionary::new(
                include_str!("../assets/spell/en_US.aff"),
                include_str!("../assets/spell/en_US.dic"),
                include_str!("../assets/spell/software.txt"),
            )
        })
    }

    /// A dictionary of a Hunspell affix and word file's words and a list
    /// of more, a word a line.
    pub fn new(aff: &str, dic: &str, more: &str) -> Dictionary {
        let affixes = Affixes::read(aff);
        let mut words = HashSet::new();
        for line in dic.lines().skip(1) {
            let (stem, flags) = match line.split_once('/') {
                Some((s, f)) => (s, f.split_whitespace().next().unwrap_or("")),
                None => (line.split_whitespace().next().unwrap_or(""), ""),
            };
            if stem.is_empty() || stem.bytes().any(|b| b.is_ascii_digit()) {
                continue;
            }
            if flags.contains(affixes.only_in_compound) {
                continue;
            }
            for form in affixes.expand(stem, flags) {
                words.insert(form.to_lowercase());
            }
        }
        words.extend(
            more.lines()
                .map(|w| w.trim().to_lowercase())
                .filter(|w| !w.is_empty()),
        );
        Dictionary { words }
    }

    /// Whether a word is known, in any case, a possessive's "'s" aside.
    pub fn known(&self, word: &str) -> bool {
        let lower = word.to_lowercase();
        let lower = lower.trim_matches('\'');
        self.words.contains(lower)
            || lower
                .strip_suffix("'s")
                .is_some_and(|w| self.words.contains(w))
    }

    /// What a word may have been meant as: the known words one edit away
    /// (a letter dropped, added, changed, or two swapped), else two, in the
    /// word's own case, at most six.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let lower = word.to_lowercase();
        let mut found: Vec<String> = Vec::new();
        let take = |w: String, found: &mut Vec<String>| {
            if self.words.contains(&w) && !found.contains(&w) && w != lower {
                found.push(w);
            }
        };
        let near = edits(&lower);
        for w in near.iter().cloned() {
            take(w, &mut found);
        }
        if found.is_empty() {
            for w in &near {
                for w2 in edits(w) {
                    take(w2, &mut found);
                    if found.len() >= 12 {
                        break;
                    }
                }
            }
        }
        // Those keeping the first letter first, as a slip seldom changes it,
        // then those as long as the word: letters swapped or mistyped.
        let first = lower.chars().next();
        let len = lower.chars().count();
        found.sort_by_key(|w| (w.chars().next() != first, w.chars().count().abs_diff(len)));
        found
            .into_iter()
            .take(6)
            .map(|w| same_case(word, &w))
            .collect()
    }
}

/// Every string one edit from a word: a letter deleted, two swapped, one
/// changed or one added.
fn edits(word: &str) -> Vec<String> {
    const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz'";
    let chars: Vec<char> = word.chars().collect();
    let mut out = Vec::new();
    for i in 0..=chars.len() {
        let (a, b) = chars.split_at(i);
        let a: String = a.iter().collect();
        if !b.is_empty() {
            out.push(format!("{a}{}", b[1..].iter().collect::<String>()));
        }
        if b.len() > 1 {
            out.push(format!(
                "{a}{}{}{}",
                b[1],
                b[0],
                b[2..].iter().collect::<String>()
            ));
        }
        for c in LETTERS.chars() {
            if !b.is_empty() && b[0] != c {
                out.push(format!("{a}{c}{}", b[1..].iter().collect::<String>()));
            }
            out.push(format!("{a}{c}{}", b.iter().collect::<String>()));
        }
    }
    out
}

/// A suggestion in the case of the word it is for: capitalised, or all
/// capitals.
fn same_case(word: &str, suggestion: &str) -> String {
    let upper = word
        .chars()
        .filter(|c| c.is_alphabetic())
        .all(char::is_uppercase);
    if upper && word.chars().count() > 1 {
        return suggestion.to_uppercase();
    }
    if word.chars().next().is_some_and(char::is_uppercase) {
        let mut c = suggestion.chars();
        return c
            .next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default();
    }
    suggestion.to_string()
}

/// One prefix or suffix rule: what is taken off the word, what is put on,
/// and what the word must start or end with.
struct Rule {
    strip: String,
    add: String,
    condition: Vec<Class>,
}

/// One place of a rule's condition: any of some characters, or any but.
enum Class {
    Any,
    Of(Vec<char>, bool),
}

impl Class {
    fn matches(&self, c: char) -> bool {
        match self {
            Class::Any => true,
            Class::Of(cs, not) => cs.contains(&c) != *not,
        }
    }
}

/// An affix file's rules, by flag: whether a prefix, whether it combines
/// with the other kind, and its rules.
struct Affixes {
    rules: Vec<(char, bool, bool, Vec<Rule>)>,
    only_in_compound: char,
}

impl Affixes {
    fn read(aff: &str) -> Affixes {
        let mut rules: Vec<(char, bool, bool, Vec<Rule>)> = Vec::new();
        let mut only_in_compound = '\0';
        for line in aff.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            match f.as_slice() {
                ["ONLYINCOMPOUND", flag, ..] => {
                    only_in_compound = flag.chars().next().unwrap_or('\0')
                }
                [kind @ ("PFX" | "SFX"), flag, cross, _count] => {
                    let flag = flag.chars().next().unwrap_or('\0');
                    rules.push((flag, *kind == "PFX", *cross == "Y", Vec::new()));
                }
                [kind @ ("PFX" | "SFX"), flag, strip, add, condition, ..] => {
                    let flag = flag.chars().next().unwrap_or('\0');
                    let prefix = *kind == "PFX";
                    if let Some(r) = rules
                        .iter_mut()
                        .rev()
                        .find(|r| r.0 == flag && r.1 == prefix)
                    {
                        let none = |s: &str| {
                            if s == "0" {
                                String::new()
                            } else {
                                s.to_string()
                            }
                        };
                        let add = add.split('/').next().unwrap_or("");
                        r.3.push(Rule {
                            strip: none(strip),
                            add: none(add),
                            condition: condition_of(condition),
                        });
                    }
                }
                _ => {}
            }
        }
        Affixes {
            rules,
            only_in_compound,
        }
    }

    /// A stem and every form its flags give it: each suffix's, each
    /// prefix's, and a prefix on a suffixed form where both combine.
    fn expand(&self, stem: &str, flags: &str) -> Vec<String> {
        let mut out = vec![stem.to_string()];
        let mut crossed = vec![stem.to_string()];
        for (flag, prefix, cross, rules) in &self.rules {
            if *prefix || !flags.contains(*flag) {
                continue;
            }
            for r in rules {
                if let Some(w) = apply(stem, r, false) {
                    if *cross {
                        crossed.push(w.clone());
                    }
                    out.push(w);
                }
            }
        }
        for (flag, prefix, cross, rules) in &self.rules {
            if !*prefix || !flags.contains(*flag) {
                continue;
            }
            for r in rules {
                let bases: &[String] = if *cross { &crossed } else { &crossed[..1] };
                for base in bases {
                    if let Some(w) = apply(base, r, true) {
                        out.push(w);
                    }
                }
            }
        }
        out
    }
}

/// A rule's condition: "." any, "[^aeiou]y", "e".
fn condition_of(text: &str) -> Vec<Class> {
    if text == "." {
        return vec![Class::Any];
    }
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' => {
                let not = chars.peek() == Some(&'^');
                if not {
                    chars.next();
                }
                let set: Vec<char> = chars.by_ref().take_while(|c| *c != ']').collect();
                out.push(Class::Of(set, not));
            }
            '.' => out.push(Class::Any),
            c => out.push(Class::Of(vec![c], false)),
        }
    }
    out
}

/// A word with a rule applied, when its condition holds.
fn apply(word: &str, r: &Rule, prefix: bool) -> Option<String> {
    let chars: Vec<char> = word.chars().collect();
    let n = r.condition.len();
    if chars.len() < n {
        return None;
    }
    let part = if prefix {
        &chars[..n]
    } else {
        &chars[chars.len() - n..]
    };
    if !part
        .iter()
        .zip(&r.condition)
        .all(|(c, class)| class.matches(*c))
    {
        return None;
    }
    if prefix {
        let rest = word.strip_prefix(r.strip.as_str())?;
        Some(format!("{}{rest}", r.add))
    } else {
        let rest = word.strip_suffix(r.strip.as_str())?;
        Some(format!("{rest}{}", r.add))
    }
}

/// The words of prose in a stretch of text, by their byte ranges: runs of
/// letters (and apostrophes within), identifiers told in their parts, and
/// what reads as code (paths, URLs, dotted names, anything in backticks)
/// passed over; acronyms and words of fewer than three letters are too.
pub fn words(text: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let part = &text[range.clone()];
    let mut in_code = false;
    let mut start = 0;
    // Each run between spaces, as a whole: code-like runs are left out.
    for (i, c) in part.char_indices().chain([(part.len(), ' ')]) {
        if !c.is_whitespace() {
            continue;
        }
        let chunk = &part[start..i];
        let ticks = chunk.matches('`').count();
        let code = in_code
            || ticks > 0
            || [
                "://", "/", "\\", "::", "@", "=", "<", ">", "{", "}", "->", "()",
            ]
            .iter()
            .any(|s| chunk.contains(s))
            || dotted(chunk);
        if ticks % 2 == 1 {
            in_code = !in_code;
        }
        if !code {
            for r in letters(chunk) {
                for r in parts(&chunk[r.clone()])
                    .into_iter()
                    .map(|p| p.start + r.start..p.end + r.start)
                {
                    let w = &chunk[r.clone()];
                    let letters = w.chars().filter(|c| c.is_alphabetic()).count();
                    let acronym = w
                        .chars()
                        .filter(|c| c.is_alphabetic())
                        .all(char::is_uppercase);
                    if letters >= 3 && !acronym {
                        out.push(range.start + start + r.start..range.start + start + r.end);
                    }
                }
            }
        }
        start = i + c.len_utf8();
    }
    out
}

/// Whether a run reads as a dotted name: a letter or digit either side of
/// a dot ("main.rs", "os.path"), not a sentence's full stop.
fn dotted(chunk: &str) -> bool {
    let chars: Vec<char> = chunk.chars().collect();
    chars
        .windows(3)
        .any(|w| w[1] == '.' && w[0].is_alphanumeric() && w[2].is_alphanumeric())
}

/// The runs of letters in a chunk, apostrophes within them kept, a run
/// touching a digit or an underscore's other letters left whole.
fn letters(chunk: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut digit = false;
    let chars: Vec<(usize, char)> = chunk.char_indices().collect();
    for (k, &(i, c)) in chars.iter().enumerate() {
        let next_letter = chars.get(k + 1).is_some_and(|(_, n)| n.is_alphabetic());
        let inside = c.is_alphabetic() || (c == '\'' && start.is_some() && next_letter);
        if c.is_ascii_digit() {
            digit = true;
        }
        match (inside, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if !digit {
                    out.push(s..i);
                }
                start = None;
                digit = c.is_ascii_digit();
            }
            _ => {}
        }
    }
    if let Some(s) = start
        && !digit
    {
        out.push(s..chunk.len());
    }
    out
}

/// A run of letters cut where an identifier's words meet: "parseHTTPRequest"
/// is "parse", "HTTP" and "Request".
fn parts(word: &str) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut out = Vec::new();
    let mut start = 0;
    for k in 1..chars.len() {
        let (i, c) = chars[k];
        let prev = chars[k - 1].1;
        let next = chars.get(k + 1).map(|(_, n)| *n);
        let cut = (prev.is_lowercase() && c.is_uppercase())
            || (prev.is_uppercase() && c.is_uppercase() && next.is_some_and(char::is_lowercase));
        if cut {
            out.push(start..i);
            start = i;
        }
    }
    out.push(start..word.len());
    out
}

/// Where a Markdown or text file's prose is: all of it but fenced code
/// blocks, indented code, and link targets.
pub fn prose(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut fenced = false;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        let indented = line.starts_with("    ") || line.starts_with('\t');
        if fence {
            fenced = !fenced;
        } else if !fenced && !indented {
            // A link's target, "](...)", is not prose.
            let mut from = 0;
            while let Some(open) = line[from..].find("](") {
                let open = from + open;
                out.push(at + from..at + open + 1);
                from = match line[open..].find(')') {
                    Some(close) => open + close + 1,
                    None => line.len(),
                };
            }
            out.push(at + from..at + line.len());
        }
        at += line.len();
    }
    out
}

/// The misspelt words in the stretches of text given, by their byte
/// ranges, the person's own words known too.
pub fn misspelt(
    text: &str,
    stretches: &[Range<usize>],
    own: &HashSet<String>,
) -> Vec<Range<usize>> {
    let d = Dictionary::get();
    stretches
        .iter()
        .flat_map(|r| words(text, r.clone()))
        .filter(|r| {
            let w = &text[r.clone()];
            !d.known(w) && !own.contains(&w.to_lowercase())
        })
        .collect()
}

/// Where a file's text is checked: a Markdown or text file's prose, or a
/// program's comments and strings with a space in them, as its grammar
/// finds them.
pub fn stretches(text: &str, language: &str) -> Vec<Range<usize>> {
    match language {
        "markdown" | "text" => prose(text),
        _ => commented(text, language),
    }
}

/// The comments and the strings that read as prose, by the highlighting
/// grammar's names for them.
fn commented(text: &str, language: &str) -> Vec<Range<usize>> {
    use gpui::{HighlightStyle, hsla};
    struct Marks;
    impl gpui_component::input::HighlightStyleResolver for Marks {
        fn style(&self, name: &str) -> Option<HighlightStyle> {
            let mark = if name.starts_with("comment") {
                0.1
            } else if name.starts_with("string") && !name.starts_with("string.special") {
                0.2
            } else {
                return None;
            };
            Some(HighlightStyle {
                color: Some(hsla(mark, 1., 0.5, 1.)),
                ..Default::default()
            })
        }
    }
    if text.len() > 2 << 20 {
        return vec![];
    }
    let mut h = gpui_component::highlighter::SyntaxHighlighter::new(language);
    h.update(None, &gpui_component::Rope::from(text), None);
    h.styles(&(0..text.len()), &Marks)
        .into_iter()
        .filter_map(|(r, s)| {
            let hue = s.color?.h;
            let comment = (hue - 0.1).abs() < 0.01;
            let prose = (hue - 0.2).abs() < 0.01 && text[r.clone()].contains(' ');
            (comment || prose).then_some(r)
        })
        .collect()
}

/// The person's own words and whether spelling is checked at all, as their
/// Code settings say (`dictionary`, `spellcheck`), for every editor; each
/// change counts, so editors check again.
#[derive(Debug, Clone, PartialEq)]
pub struct Own {
    pub on: bool,
    pub words: HashSet<String>,
    pub version: u64,
}

impl Default for Own {
    fn default() -> Self {
        Own {
            on: true,
            words: HashSet::new(),
            version: 0,
        }
    }
}

impl gpui::Global for Own {}

/// What Code settings say of spelling: whether it is on, and the words
/// saved, lower-cased.
pub fn read_settings(settings: &serde_json::Value) -> (bool, HashSet<String>) {
    let on = settings["spellcheck"].as_bool() != Some(false);
    let words = settings["dictionary"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| w.as_str().map(str::to_lowercase))
        .collect();
    (on, words)
}

/// Takes the Code settings' spelling, when it differs from what is held.
pub fn take_settings(settings: &serde_json::Value, cx: &mut gpui::App) {
    let (on, words) = read_settings(settings);
    let own = cx.default_global::<Own>();
    if own.on != on || own.words != words {
        own.on = on;
        own.words = words;
        own.version += 1;
        cx.refresh_windows();
    }
}

/// Changes the Code settings: a word saved to the person's dictionary, or
/// spelling turned on or off. Taken at once here; the service tells the
/// other tabs.
pub fn change_settings(
    code: crate::code::Code,
    change: impl FnOnce(&mut serde_json::Value) + 'static,
    cx: &mut gpui::App,
) {
    let get = code.call("settings.get", serde_json::json!({}));
    cx.spawn(async move |cx| {
        let Ok(mut settings) = get.await else { return };
        change(&mut settings);
        cx.update(|cx| take_settings(&settings, cx));
        let _ = code
            .call("settings.set", serde_json::json!({ "settings": settings }))
            .await;
    })
    .detach();
}

/// Saves a word to the person's dictionary.
pub fn save_word(code: crate::code::Code, word: String, cx: &mut gpui::App) {
    change_settings(
        code,
        move |s| {
            let mut words: Vec<serde_json::Value> =
                s["dictionary"].as_array().cloned().unwrap_or_default();
            if !words.iter().any(|w| w.as_str() == Some(word.as_str())) {
                words.push(serde_json::Value::String(word));
            }
            s["dictionary"] = serde_json::Value::Array(words);
        },
        cx,
    );
}
