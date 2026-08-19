//! Folding a file into blocks, without knowing what the language means.
//!
//! The reader is a lexer and nothing more. It hides comments and quoted text, then follows the
//! grammar's opening and closing delimiters to build a tree, recording for every block the line it
//! opened on and the text immediately before it. That text is all any later step gets: enough to
//! see a keyword or a call, never enough to evaluate an expression.
//!
//! Line numbers are preserved through hiding rather than recomputed, so a position the engine
//! reports is the position in the file the author will open.

use crate::grammar::Grammar;

/// What a block is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The file itself.
    File,
    /// Entered more than once: a loop, or a call that runs its argument per element.
    Repeat,
    /// Something callable.
    Define,
    /// A block that neither repeats nor defines: a branch, an object, a scope.
    Plain,
}

/// One block of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Index into [`File::blocks`].
    pub id: usize,
    /// The block this one opened inside.
    pub parent: Option<usize>,
    /// Blocks opened directly inside this one, in the order they were opened.
    pub children: Vec<usize>,
    /// What kind of block it is.
    pub kind: Kind,
    /// The line the opening delimiter is on, counting from one.
    pub line: usize,
    /// The line the closing delimiter is on, counting from one.
    pub end_line: usize,
    /// The text between the previous statement boundary and the opening delimiter.
    pub head: String,
    /// The name this block defines, when it defines one.
    pub name: Option<String>,
}

/// One call written inside a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// The name before the opening parenthesis.
    pub name: String,
    /// The text between the parentheses, with comments and quoted text already hidden.
    ///
    /// Kept as text rather than parsed. A rule may see which names an argument mentions, which is
    /// a lexical fact true in every language here; it may not see what the argument computes,
    /// which would be a claim about one language's semantics.
    pub args: String,
    /// The block the call is written in.
    pub within: usize,
    /// The line it is written on, counting from one.
    pub line: usize,
}

/// One file, folded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Path as the walk found it, relative to the root.
    pub path: String,
    /// The language it was read as.
    pub language: &'static str,
    /// Every block, with index zero the file itself.
    pub blocks: Vec<Block>,
    /// Every call, in the order they were read.
    pub calls: Vec<Call>,
    /// The file with comments and quoted text hidden, split by line, index zero being line one.
    ///
    /// Rules read this rather than the original so a keyword inside a comment or a message can
    /// never fire one, while the line numbers stay the file's own.
    pub text: Vec<String>,
    /// How many lines the file has.
    pub lines: usize,
}

/// Fold `text` into blocks under `grammar`.
///
/// Never fails. A file whose delimiters do not balance closes its open blocks at the last line,
/// which is the least it can be said to have contained.
#[must_use]
pub fn read(path: &str, text: &str, grammar: &Grammar) -> File {
    let hidden = hide(text, grammar);
    let mut file = File {
        path: path.to_owned(),
        language: grammar.name,
        blocks: vec![Block {
            id: 0,
            parent: None,
            children: Vec::new(),
            kind: Kind::File,
            line: 1,
            end_line: 1,
            head: String::new(),
            name: None,
        }],
        calls: Vec::new(),
        text: hidden.lines().map(std::borrow::ToOwned::to_owned).collect(),
        lines: 1,
    };
    let mut stack = vec![0_usize];
    let mut line = 1_usize;
    // Where each call's opening parenthesis stood, so its argument text can be cut out once the
    // whole file has been read.
    let mut openings: Vec<usize> = Vec::new();
    // Text since the last statement boundary. A head is what stands immediately before a block,
    // and a boundary is where the previous statement stopped being relevant to the next one.
    let mut head = String::new();
    for (position, character) in hidden.chars().enumerate() {
        if character == '\n' {
            line += 1;
            file.lines = line;
            head.push(' ');
            continue;
        }
        if character == grammar.open {
            let parent = *stack.last().unwrap_or(&0);
            let id = file.blocks.len();
            let (kind, name) = classify(&head, grammar);
            file.blocks.push(Block {
                id,
                parent: Some(parent),
                children: Vec::new(),
                kind,
                line,
                end_line: line,
                head: head.trim().to_owned(),
                name,
            });
            file.blocks[parent].children.push(id);
            stack.push(id);
            head.clear();
            continue;
        }
        if character == grammar.close {
            if let Some(id) = stack.pop() {
                file.blocks[id].end_line = line;
            }
            if stack.is_empty() {
                stack.push(0);
            }
            head.clear();
            continue;
        }
        if character == '(' {
            // A parameter list is not a call. Without this the definition of `total` reads as a
            // call to `total`, every routine reaches itself, and the call graph is one cycle.
            let opening_a_definition = classify(&head, grammar).0 == Kind::Define;
            if let Some(name) = trailing_name(&head) {
                if !opening_a_definition && !grammar.not_calls.contains(&name.as_str()) {
                    let within = *stack.last().unwrap_or(&0);
                    file.calls.push(Call { name, args: String::new(), within, line });
                    openings.push(position);
                }
            }
        }
        if character == ';' {
            head.clear();
            continue;
        }
        head.push(character);
    }
    while let Some(id) = stack.pop() {
        file.blocks[id].end_line = file.lines;
    }
    file.blocks[0].end_line = file.lines;
    let characters: Vec<char> = hidden.chars().collect();
    for (call, &opening) in file.calls.iter_mut().zip(openings.iter()) {
        call.args = arguments(&characters, opening);
    }
    if file.text.is_empty() {
        file.text.push(String::new());
    }
    file
}

/// The text between the parenthesis at `opening` and the one that closes it.
///
/// An unclosed parenthesis yields what is left of the file rather than nothing, which is the most
/// that can honestly be said to be inside it.
fn arguments(characters: &[char], opening: usize) -> String {
    let mut depth = 0_usize;
    let mut out = String::new();
    for &character in &characters[opening..] {
        if character == '(' {
            depth += 1;
            if depth == 1 {
                continue;
            }
        }
        if character == ')' {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
        out.push(character);
    }
    out
}

/// Replace comments and quoted text with spaces, keeping every newline where it was.
///
/// Hiding rather than removing is what lets a reported line be the line in the file. Quoted text
/// is hidden because a brace or a keyword inside a string is not structure, and a rule that
/// counted it would fire on a message about a loop rather than on a loop.
fn hide(text: &str, grammar: &Grammar) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(bytes.len());
    let mut at = 0;
    'outer: while at < bytes.len() {
        let rest: String = bytes[at..].iter().take(4).collect();
        for opener in grammar.line_comment {
            if rest.starts_with(opener) {
                while at < bytes.len() && bytes[at] != '\n' {
                    out.push(' ');
                    at += 1;
                }
                continue 'outer;
            }
        }
        for (opener, closer) in grammar.block_comment {
            if rest.starts_with(opener) {
                let closed = closer.chars().collect::<Vec<_>>();
                while at < bytes.len() {
                    if bytes[at..].starts_with(&closed[..]) {
                        for _ in 0..closed.len() {
                            out.push(' ');
                            at += 1;
                        }
                        continue 'outer;
                    }
                    out.push(if bytes[at] == '\n' { '\n' } else { ' ' });
                    at += 1;
                }
                continue 'outer;
            }
        }
        if grammar.quotes.contains(&bytes[at]) {
            let quote = bytes[at];
            out.push(' ');
            at += 1;
            while at < bytes.len() {
                if grammar.escapes && bytes[at] == '\\' {
                    out.push(' ');
                    at += 1;
                    if at < bytes.len() {
                        out.push(if bytes[at] == '\n' { '\n' } else { ' ' });
                        at += 1;
                    }
                    continue;
                }
                if bytes[at] == quote {
                    out.push(' ');
                    at += 1;
                    continue 'outer;
                }
                out.push(if bytes[at] == '\n' { '\n' } else { ' ' });
                at += 1;
            }
            continue 'outer;
        }
        out.push(bytes[at]);
        at += 1;
    }
    out
}

/// What the text before an opening delimiter says the block is.
fn classify(head: &str, grammar: &Grammar) -> (Kind, Option<String>) {
    if grammar.repeats.iter().any(|word| has_word(head, word)) {
        return (Kind::Repeat, None);
    }
    // A call that runs its argument once per element is a loop written as a call. Matched on the
    // method name rather than on a leading keyword, since that is where it appears.
    if grammar.iterating_calls.iter().any(|method| head.contains(&format!(".{method}("))) {
        return (Kind::Repeat, None);
    }
    if grammar.defines.iter().any(|word| has_word(head, word)) {
        return (Kind::Define, defined_name(head));
    }
    (Kind::Plain, None)
}

/// Whether `word` appears in `text` as a whole word.
fn has_word(text: &str, word: &str) -> bool {
    let bounds = |character: char| !character.is_alphanumeric() && character != '_';
    let mut at = 0;
    while let Some(found) = text[at..].find(word) {
        let start = at + found;
        let end = start + word.len();
        let before = text[..start].chars().next_back().map_or(true, bounds);
        let after = text[end..].chars().next().map_or(true, bounds);
        if before && after {
            return true;
        }
        at = end;
    }
    false
}

/// The name a definition gives, read as the identifier before the parameter list.
fn defined_name(head: &str) -> Option<String> {
    let up_to = head.find('(').map_or(head, |at| &head[..at]);
    up_to
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
        .rfind(|word| !word.is_empty())
        .map(std::borrow::ToOwned::to_owned)
}

/// The identifier immediately before an opening parenthesis, if there is one.
fn trailing_name(head: &str) -> Option<String> {
    let name: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if name.is_empty() || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::{read, Kind};
    use crate::grammar::for_extension;

    fn js(text: &str) -> super::File {
        read("a.js", text, for_extension("js").unwrap())
    }

    #[test]
    fn a_brace_inside_quoted_text_is_not_structure() {
        let file = js("const message = \"for (;;) { }\";\n");
        assert_eq!(file.blocks.len(), 1, "only the file itself");
    }

    #[test]
    fn a_loop_keyword_inside_a_comment_opens_nothing() {
        let file = js("// for (const x of xs) {\nconst a = 1;\n");
        assert_eq!(file.blocks.len(), 1);
    }

    #[test]
    fn a_reported_line_is_the_line_in_the_file() {
        let file = js("\n\n\nfor (const x of xs) {\n  work();\n}\n");
        let loops: Vec<_> = file.blocks.iter().filter(|b| b.kind == Kind::Repeat).collect();
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].line, 4, "the line the author will open");
        assert_eq!(loops[0].end_line, 6);
        let call = file.calls.iter().find(|c| c.name == "work").expect("the call is read");
        assert_eq!(call.line, 5);
    }

    #[test]
    fn a_call_that_runs_its_argument_per_element_is_a_repeat() {
        let file = js("items.map((item) => {\n  work();\n});\n");
        assert!(file.blocks.iter().any(|b| b.kind == Kind::Repeat), "a loop written as a call");
    }

    #[test]
    fn a_definition_carries_the_name_it_defines() {
        let file = js("function total(rows) {\n  return 1;\n}\n");
        let defined = file.blocks.iter().find(|b| b.kind == Kind::Define).expect("a definition");
        assert_eq!(defined.name.as_deref(), Some("total"));
    }

    #[test]
    fn unbalanced_delimiters_close_at_the_last_line_rather_than_panicking() {
        let file = js("function broken() {\n  if (a) {\n");
        assert!(file.blocks.iter().all(|b| b.end_line <= file.lines));
    }
}
