use std::collections::HashMap;

/// Layout mode chosen when rendering a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Flat,
    Break,
}

/// Wadler-Lindjen / Prettier-style document intermediate representation.
#[derive(Debug, Clone)]
pub enum Doc {
    Empty,
    /// Single-line text chunk (must not contain `\n`).
    Text(String),
    /// Multi-line verbatim chunk (internal newlines do not apply extra indentation).
    Verbatim(String),
    Concat(Vec<Doc>),
    Group {
        contents: Box<Doc>,
        should_break: bool,
        id: Option<u32>,
    },
    Indent(Box<Doc>),
    /// Nothing in Flat mode; newline + current indent in Break mode.
    SoftLine,
    /// Single space in Flat mode; newline + current indent in Break mode.
    Line,
    /// Unconditional newline + current indent. Forces enclosing groups to break.
    HardLine,
    /// Unconditional blank line (forces an empty line with no trailing whitespace).
    BlankLine,
    /// Selects `broken` when the enclosing (or referenced) group breaks, else `flat`.
    IfBreak {
        broken: Box<Doc>,
        flat: Box<Doc>,
        group_id: Option<u32>,
    },
    /// Deferred line suffix (e.g. trailing `// comment`) flushed before the next newline or EOF.
    LineSuffix(Box<Doc>),
    /// Propagates a forced break to enclosing groups without emitting a newline itself.
    BreakParent,
}

impl Doc {
    pub fn text(s: impl Into<String>) -> Self {
        let s = s.into();
        if s.is_empty() {
            Self::Empty
        } else if s.contains('\n') {
            Self::Verbatim(s)
        } else {
            Self::Text(s)
        }
    }

    pub fn verbatim(s: impl Into<String>) -> Self {
        let s = s.into();
        if s.is_empty() {
            Self::Empty
        } else {
            Self::Verbatim(s)
        }
    }

    pub fn concat(items: Vec<Doc>) -> Self {
        let mut flat_items = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Doc::Empty => {}
                Doc::Concat(nested) => flat_items.extend(nested),
                other => flat_items.push(other),
            }
        }
        match flat_items.len() {
            0 => Doc::Empty,
            1 => flat_items.into_iter().next().unwrap_or(Doc::Empty),
            _ => Doc::Concat(flat_items),
        }
    }

    pub fn group(contents: Doc) -> Self {
        Self::Group {
            contents: Box::new(contents),
            should_break: false,
            id: None,
        }
    }

    pub fn group_with_id(contents: Doc, id: u32) -> Self {
        Self::Group {
            contents: Box::new(contents),
            should_break: false,
            id: Some(id),
        }
    }

    pub fn group_breaking(contents: Doc) -> Self {
        Self::Group {
            contents: Box::new(contents),
            should_break: true,
            id: None,
        }
    }

    pub fn group_breaking_with_id(contents: Doc, id: u32) -> Self {
        Self::Group {
            contents: Box::new(contents),
            should_break: true,
            id: Some(id),
        }
    }

    pub fn indent(contents: Doc) -> Self {
        match contents {
            Doc::Empty => Doc::Empty,
            other => Doc::Indent(Box::new(other)),
        }
    }

    pub fn if_break(broken: Doc, flat: Doc) -> Self {
        Doc::IfBreak {
            broken: Box::new(broken),
            flat: Box::new(flat),
            group_id: None,
        }
    }

    pub fn if_group_break(group_id: u32, broken: Doc, flat: Doc) -> Self {
        Doc::IfBreak {
            broken: Box::new(broken),
            flat: Box::new(flat),
            group_id: Some(group_id),
        }
    }

    pub fn line_suffix(contents: Doc) -> Self {
        Doc::LineSuffix(Box::new(contents))
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Doc::Empty => true,
            Doc::Concat(items) => items.iter().all(Self::is_empty),
            _ => false,
        }
    }
}

/// Propagates forced breaks (`HardLine`, `BlankLine`, `BreakParent`, multi-line
/// `Verbatim`, or pre-broken child groups) upwards into enclosing `Group` nodes.
pub fn propagate_breaks(doc: &mut Doc) -> bool {
    match doc {
        Doc::Empty | Doc::Text(_) | Doc::SoftLine | Doc::Line => false,
        Doc::Verbatim(s) => s.contains('\n'),
        Doc::HardLine | Doc::BlankLine | Doc::BreakParent => true,
        Doc::Indent(inner) => propagate_breaks(inner),
        Doc::LineSuffix(inner) => {
            propagate_breaks(inner);
            false
        }
        Doc::IfBreak { broken, flat, .. } => {
            let _ = propagate_breaks(broken);
            propagate_breaks(flat)
        }
        Doc::Concat(items) => {
            let mut breaks = false;
            for item in items {
                if propagate_breaks(item) {
                    breaks = true;
                }
            }
            breaks
        }
        Doc::Group {
            contents,
            should_break,
            ..
        } => {
            if propagate_breaks(contents) {
                *should_break = true;
            }
            *should_break
        }
    }
}

/// Renders a `Doc` tree to a formatted string using Prettier's stack algorithm.
pub fn render(doc: &Doc, indent_width: usize, max_line_width: usize) -> String {
    render_from_indent(doc, indent_width, max_line_width, 0)
}

/// Returns `true` if `doc` renders onto a single line when laid out starting at
/// `base_indent` columns with the given width budget.
pub fn renders_single_line(
    doc: &Doc,
    indent_width: usize,
    max_line_width: usize,
    base_indent: usize,
) -> bool {
    !render_from_indent(doc, indent_width, max_line_width, base_indent).contains('\n')
}

/// Renders a `Doc` tree starting at `base_indent` leading spaces.
pub fn render_from_indent(
    doc: &Doc,
    indent_width: usize,
    max_line_width: usize,
    base_indent: usize,
) -> String {
    let mut owned = doc.clone();
    propagate_breaks(&mut owned);

    let mut printer = Printer {
        out: String::new(),
        col: 0,
        pending_indent: if base_indent > 0 {
            Some(base_indent)
        } else {
            None
        },
        indent_width,
        max_line_width,
        group_modes: HashMap::new(),
        line_suffixes: Vec::new(),
    };

    let mut stack: Vec<(usize, Mode, &Doc)> = vec![(base_indent, Mode::Break, &owned)];
    while let Some((indent, mode, current)) = stack.pop() {
        match current {
            Doc::Empty | Doc::BreakParent => {}
            Doc::Text(s) => {
                printer.flush_indent();
                printer.out.push_str(s);
                printer.col += s.chars().count();
            }
            Doc::Verbatim(s) => {
                printer.flush_indent();
                printer.out.push_str(s);
                if let Some(last_nl) = s.rfind('\n') {
                    printer.col = s[last_nl + 1..].chars().count();
                } else {
                    printer.col += s.chars().count();
                }
            }
            Doc::Concat(items) => {
                for item in items.iter().rev() {
                    stack.push((indent, mode, item));
                }
            }
            Doc::Indent(inner) => {
                stack.push((indent + printer.indent_width, mode, inner));
            }
            Doc::IfBreak {
                broken,
                flat,
                group_id,
            } => {
                let effective_mode = group_id
                    .and_then(|id| printer.group_modes.get(&id).copied())
                    .unwrap_or(mode);
                match effective_mode {
                    Mode::Break => stack.push((indent, mode, broken)),
                    Mode::Flat => stack.push((indent, mode, flat)),
                }
            }
            Doc::LineSuffix(inner) => {
                printer.line_suffixes.push((indent, mode, inner));
            }
            Doc::Group {
                contents,
                should_break,
                id,
            } => {
                let chosen = if *should_break {
                    Mode::Break
                } else {
                    let remaining =
                        printer.max_line_width as isize - printer.current_col() as isize;
                    let mut probe = Vec::with_capacity(stack.len() + 1);
                    probe.extend_from_slice(&stack);
                    probe.push((indent, Mode::Flat, contents));
                    if fits(remaining, &mut probe, &printer.group_modes) {
                        Mode::Flat
                    } else {
                        Mode::Break
                    }
                };
                if let Some(gid) = id {
                    printer.group_modes.insert(*gid, chosen);
                }
                stack.push((indent, chosen, contents));
            }
            Doc::SoftLine => {
                if mode == Mode::Break {
                    printer.emit_newline(&mut stack, indent);
                }
            }
            Doc::Line => {
                if mode == Mode::Flat {
                    printer.flush_indent();
                    printer.out.push(' ');
                    printer.col += 1;
                } else {
                    printer.emit_newline(&mut stack, indent);
                }
            }
            Doc::HardLine => {
                printer.emit_newline(&mut stack, indent);
            }
            Doc::BlankLine => {
                printer.emit_blank_line(&mut stack, indent);
            }
        }

        if stack.is_empty() && !printer.line_suffixes.is_empty() {
            printer.drain_line_suffixes(&mut stack);
        }
    }

    while printer.out.ends_with(' ') {
        printer.out.pop();
    }
    printer.out
}

struct Printer<'a> {
    out: String,
    col: usize,
    pending_indent: Option<usize>,
    indent_width: usize,
    max_line_width: usize,
    group_modes: HashMap<u32, Mode>,
    line_suffixes: Vec<(usize, Mode, &'a Doc)>,
}

impl<'a> Printer<'a> {
    fn current_col(&self) -> usize {
        self.pending_indent.unwrap_or(self.col)
    }

    fn flush_indent(&mut self) {
        if let Some(spaces) = self.pending_indent.take() {
            for _ in 0..spaces {
                self.out.push(' ');
            }
            self.col = spaces;
        }
    }

    fn drain_line_suffixes(&mut self, stack: &mut Vec<(usize, Mode, &'a Doc)>) {
        if self.line_suffixes.is_empty() {
            return;
        }
        let drained = std::mem::take(&mut self.line_suffixes);
        for item in drained.into_iter().rev() {
            stack.push(item);
        }
    }

    fn emit_newline(&mut self, stack: &mut Vec<(usize, Mode, &'a Doc)>, next_indent: usize) {
        if !self.line_suffixes.is_empty() {
            // Re-run this HardLine after flushing the buffered EOL suffixes so
            // trailing comments appear before `\n` cleanly.
            stack.push((next_indent, Mode::Break, &Doc::HardLine));
            self.drain_line_suffixes(stack);
            return;
        }
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        self.out.push('\n');
        self.col = 0;
        self.pending_indent = Some(next_indent);
    }

    fn emit_blank_line(&mut self, stack: &mut Vec<(usize, Mode, &'a Doc)>, next_indent: usize) {
        if !self.line_suffixes.is_empty() {
            stack.push((next_indent, Mode::Break, &Doc::BlankLine));
            self.drain_line_suffixes(stack);
            return;
        }
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        if !self.out.is_empty() {
            if !self.out.ends_with('\n') {
                self.out.push('\n');
            }
            if !self.out.ends_with("\n\n") {
                self.out.push('\n');
            }
        }
        self.col = 0;
        self.pending_indent = Some(next_indent);
    }
}

fn fits(
    mut remaining: isize,
    stack: &mut Vec<(usize, Mode, &Doc)>,
    group_modes: &HashMap<u32, Mode>,
) -> bool {
    while remaining >= 0 {
        let Some((indent, mode, doc)) = stack.pop() else {
            return true;
        };
        match doc {
            Doc::Empty | Doc::BreakParent => {}
            Doc::Text(s) => {
                remaining -= s.chars().count() as isize;
            }
            Doc::Verbatim(s) => {
                if let Some(nl) = s.find('\n') {
                    if mode == Mode::Flat {
                        return false;
                    }
                    remaining -= s[..nl].chars().count() as isize;
                    return remaining >= 0;
                }
                remaining -= s.chars().count() as isize;
            }
            Doc::Concat(items) => {
                for item in items.iter().rev() {
                    stack.push((indent, mode, item));
                }
            }
            Doc::Indent(inner) => {
                stack.push((indent, mode, inner));
            }
            Doc::Group {
                contents,
                should_break,
                ..
            } => {
                if *should_break && mode == Mode::Flat {
                    return false;
                }
                let inner_mode = if *should_break {
                    Mode::Break
                } else {
                    Mode::Flat
                };
                stack.push((indent, inner_mode, contents));
            }
            Doc::IfBreak {
                broken,
                flat,
                group_id,
            } => {
                let effective = group_id
                    .and_then(|id| group_modes.get(&id).copied())
                    .unwrap_or(mode);
                match effective {
                    Mode::Break => stack.push((indent, mode, broken)),
                    Mode::Flat => stack.push((indent, mode, flat)),
                }
            }
            Doc::LineSuffix(_) => {
                // Deferred suffixes do not force an earlier break inside the current group
                // unless they themselves carried BreakParent.
            }
            Doc::SoftLine => {
                if mode == Mode::Break {
                    return true;
                }
            }
            Doc::Line => {
                if mode == Mode::Flat {
                    remaining -= 1;
                } else {
                    return true;
                }
            }
            Doc::HardLine | Doc::BlankLine => {
                return mode == Mode::Break;
            }
        }
    }
    false
}
