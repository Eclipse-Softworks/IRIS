use crate::formatter::doc::Doc;
use crate::formatter::syntax::{
    continues_a_name, eof_comments_trimmed_doc, DelimListStyle, Formatter,
};
use crate::parser::Token;

impl<'a> Formatter<'a> {
    pub fn fmt_block(&mut self) -> Result<Doc, String> {
        self.fmt_block_for_owner(None)
    }

    /// Formats `{ stmts... [tail] }`. When `owner_gid` is `Some(gid)`, a single-line
    /// pure tail expression breaks to multi-line together with that owning group (e.g.
    /// `if` / `while` / `for` / `with`) before the owner's header expression ever wraps.
    pub fn fmt_block_for_owner(&mut self, owner_gid: Option<u32>) -> Result<Doc, String> {
        self.with_stmt_scope(|f| f.fmt_block_for_owner_inner(owner_gid))
    }

    fn fmt_block_for_owner_inner(&mut self, owner_gid: Option<u32>) -> Result<Doc, String> {
        let lbrace = self.expect(&Token::LBrace)?;
        let had_nl_after_lbrace = self.had_newline_before_cur();

        let mut entries: Vec<(bool, Doc)> = Vec::new();
        let mut saw_stmt = false;
        let mut first_entry = true;

        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !first_entry && self.had_blank_before_cur();
            if first_entry && self.has_own_line_comments_before_cur() {
                saw_stmt = true;
            }
            let leading = self.take_leading_own_line_comments_cur(first_entry);
            first_entry = false;

            let (doc, is_stmt) = self.fmt_stmt_or_tail()?;
            if is_stmt {
                saw_stmt = true;
            }
            let entry = if leading.is_empty() {
                doc
            } else {
                Doc::concat(vec![leading, doc])
            };
            entries.push((had_blank, entry));
            if !is_stmt {
                break;
            }
        }

        if entries.is_empty() {
            if let Some(dangling) = self.take_dangling_comments(true) {
                let (rbrace, rbrace_eol) = self.expect_close_split_eol(&Token::RBrace)?;
                return Ok(Doc::concat(vec![
                    lbrace,
                    Doc::indent(Doc::concat(vec![
                        Doc::HardLine,
                        eof_comments_trimmed_doc(dangling),
                    ])),
                    Doc::HardLine,
                    rbrace,
                    rbrace_eol,
                ]));
            }
            let (rbrace, rbrace_eol) = self.expect_close_split_eol(&Token::RBrace)?;
            return Ok(Doc::concat(vec![lbrace, rbrace, rbrace_eol]));
        }

        let had_nl_before_rbrace = self.had_newline_before_cur();
        let dangling = self.take_dangling_comments(false);
        let (rbrace, rbrace_eol) = self.expect_close_split_eol(&Token::RBrace)?;

        // A single pure tail expression (no statements, no comments, single line in source)
        // can stay compact `{ expr }` if it fits within max_line_width.
        if entries.len() == 1
            && !saw_stmt
            && dangling.is_none()
            && !had_nl_after_lbrace
            && !had_nl_before_rbrace
        {
            let (_, tail_doc) = entries.into_iter().next().expect("len == 1");
            let block_doc = if let Some(gid) = owner_gid {
                let inner_break = Doc::if_group_break(gid, Doc::HardLine, Doc::Line);
                Doc::concat(vec![
                    lbrace,
                    Doc::indent(Doc::concat(vec![inner_break.clone(), tail_doc])),
                    inner_break,
                    rbrace,
                ])
            } else {
                Doc::group(Doc::concat(vec![
                    lbrace,
                    Doc::indent(Doc::concat(vec![Doc::Line, tail_doc])),
                    Doc::Line,
                    rbrace,
                ]))
            };
            return Ok(if rbrace_eol.is_empty() {
                block_doc
            } else {
                Doc::concat(vec![block_doc, rbrace_eol])
            });
        }

        let mut inner = Vec::new();
        for (i, (had_blank, item)) in entries.into_iter().enumerate() {
            if i == 0 {
                inner.push(Doc::HardLine);
            } else if had_blank {
                inner.push(Doc::BlankLine);
            } else {
                inner.push(Doc::HardLine);
            }
            inner.push(item);
        }
        if let Some(dangling_doc) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(dangling_doc));
        }

        Ok(Doc::concat(vec![
            lbrace,
            Doc::indent(Doc::concat(inner)),
            Doc::HardLine,
            rbrace,
            rbrace_eol,
        ]))
    }

    fn fmt_stmt_or_tail(&mut self) -> Result<(Doc, bool), String> {
        match self.tok().clone() {
            Token::Val | Token::Var | Token::Let => Ok((self.fmt_let_stmt()?, true)),
            Token::While => Ok((self.fmt_while_stmt()?, true)),
            Token::For => Ok((self.fmt_for_stmt()?, true)),
            Token::Par if matches!(self.peek(1), Token::For) => {
                Ok((self.fmt_par_for_stmt()?, true))
            }
            Token::Spawn if self.spawn_block_follows() => Ok((self.fmt_spawn_stmt()?, true)),
            Token::Ident(ref name)
                if name == "nursery" && matches!(self.peek(1), Token::LBrace) =>
            {
                let n = self.emit_tok();
                let b = self.fmt_block()?;
                let semi = self.opt_semi();
                Ok((Doc::concat(vec![n, Doc::text(" "), b, semi]), true))
            }
            Token::Loop
                if matches!(self.peek(1), Token::LBrace)
                    || (matches!(self.peek(1), Token::Ident(_))
                        && matches!(self.peek(2), Token::LBrace)) =>
            {
                Ok((self.fmt_loop_stmt()?, true))
            }
            Token::Defer if !continues_a_name(self.peek(1)) => {
                let kw = self.emit_tok();
                let e = self.fmt_expr()?;
                let semi = self.opt_semi();
                Ok((Doc::concat(vec![kw, Doc::text(" "), e, semi]), true))
            }
            Token::Select if matches!(self.peek(1), Token::LBrace | Token::Bang) => {
                Ok((self.fmt_select_stmt()?, true))
            }
            Token::Yield if !continues_a_name(self.peek(1)) => {
                let kw = self.emit_tok();
                let e = self.fmt_expr()?;
                let semi = self.opt_semi();
                Ok((Doc::concat(vec![kw, Doc::text(" "), e, semi]), true))
            }
            Token::Break | Token::Continue => {
                let kw = self.emit_tok();
                let label = if matches!(self.tok(), Token::Ident(_)) && !self.at_line_start() {
                    Doc::concat(vec![Doc::text(" "), self.emit_tok()])
                } else {
                    Doc::Empty
                };
                let semi = self.opt_semi();
                Ok((Doc::concat(vec![kw, label, semi]), true))
            }
            Token::Return => {
                let kw = self.emit_tok();
                let val = if matches!(self.tok(), Token::Semi | Token::RBrace | Token::Eof) {
                    Doc::Empty
                } else {
                    Doc::concat(vec![Doc::text(" "), self.fmt_expr()?])
                };
                let semi = self.opt_semi();
                Ok((Doc::concat(vec![kw, val, semi]), true))
            }
            _ => {
                let (expr_doc, is_block_expr) = self.fmt_expr_with_block_info()?;
                if matches!(
                    self.tok(),
                    Token::Eq
                        | Token::PlusEq
                        | Token::MinusEq
                        | Token::StarEq
                        | Token::SlashEq
                        | Token::PercentEq
                ) {
                    let op = self.emit_tok();
                    let rhs = self.fmt_expr()?;
                    let semi = self.opt_semi();
                    return Ok((
                        Doc::group(Doc::concat(vec![
                            expr_doc,
                            Doc::text(" "),
                            op,
                            Doc::text(" "),
                            rhs,
                            semi,
                        ])),
                        true,
                    ));
                }
                if matches!(self.tok(), Token::Semi) {
                    let semi = self.emit_tight_tok();
                    return Ok((Doc::concat(vec![expr_doc, semi]), true));
                }
                if !matches!(self.tok(), Token::RBrace | Token::Eof)
                    && (is_block_expr || self.at_line_start())
                {
                    return Ok((expr_doc, true));
                }
                Ok((expr_doc, false))
            }
        }
    }

    fn opt_semi(&mut self) -> Doc {
        if matches!(self.tok(), Token::Semi) {
            self.emit_tight_tok()
        } else {
            Doc::Empty
        }
    }

    fn spawn_block_follows(&self) -> bool {
        if matches!(self.peek(1), Token::LBrace) {
            return true;
        }
        if !matches!(self.peek(1), Token::LParen) {
            return false;
        }
        let mut depth = 1usize;
        let mut offset = 2usize;
        while depth > 0 {
            match self.peek(offset) {
                Token::Eof => return false,
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.peek(offset + 1), Token::LBrace);
                    }
                }
                _ => {}
            }
            offset += 1;
        }
        false
    }

    fn fmt_let_stmt(&mut self) -> Result<Doc, String> {
        let kw = self.emit_tok();
        let mut parts = vec![kw, Doc::text(" ")];

        if matches!(self.tok(), Token::LParen) {
            let names = self.fmt_delimited_list(
                &Token::LParen,
                &Token::RParen,
                false,
                DelimListStyle::Tuple,
                Self::expect_ident,
            )?;
            parts.push(names);
        } else if self.is_refutable_let_pattern_start() {
            parts.push(self.fmt_sub_pattern()?);
        } else {
            parts.push(self.expect_ident()?);
            if matches!(self.tok(), Token::Colon) {
                parts.push(self.emit_tight_tok());
                parts.push(Doc::text(" "));
                parts.push(self.fmt_type()?);
            }
        }

        let eq = self.expect(&Token::Eq)?;
        let init = self.fmt_expr()?;
        let semi = self.opt_semi();

        parts.push(Doc::text(" "));
        parts.push(eq);
        parts.push(Doc::text(" "));
        parts.push(init);
        parts.push(semi);

        Ok(Doc::group(Doc::concat(parts)))
    }

    fn is_refutable_let_pattern_start(&self) -> bool {
        match self.tok() {
            Token::Ident(name) if name == "none" => !matches!(self.peek(1), Token::Dot),
            Token::Ident(name) if name == "some" || name == "ok" || name == "err" => {
                matches!(self.peek(1), Token::LParen)
            }
            Token::Ident(_) => matches!(self.peek(1), Token::Dot | Token::At),
            _ => false,
        }
    }

    fn fmt_while_stmt(&mut self) -> Result<Doc, String> {
        let leading = self.take_leading_own_line_comments_cur(false);
        let gid = self.alloc_group_id();
        let mut parts = vec![self.expect(&Token::While)?, Doc::text(" ")];
        if matches!(self.tok(), Token::Ident(_)) && matches!(self.peek(1), Token::Ident(_)) {
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
        }
        if matches!(self.tok(), Token::Let) {
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
            parts.push(self.fmt_pattern()?);
            parts.push(Doc::text(" "));
            parts.push(self.expect(&Token::Eq)?);
            parts.push(Doc::text(" "));
            parts.push(self.fmt_expr()?);
        } else {
            parts.push(self.fmt_expr()?);
        }
        parts.push(Doc::text(" "));
        parts.push(self.fmt_block_for_owner(Some(gid))?);
        parts.push(self.opt_semi());
        let grouped = Doc::group_with_id(Doc::concat(parts), gid);
        Ok(if leading.is_empty() {
            grouped
        } else {
            Doc::concat(vec![leading, grouped])
        })
    }

    fn fmt_for_stmt(&mut self) -> Result<Doc, String> {
        let leading = self.take_leading_own_line_comments_cur(false);
        let gid = self.alloc_group_id();
        let mut parts = vec![self.expect(&Token::For)?, Doc::text(" ")];
        // Optional label: `for label var in ...`
        if matches!(self.tok(), Token::Ident(_))
            && !matches!(self.peek(1), Token::In)
            && matches!(self.peek(1), Token::Ident(_) | Token::LParen)
        {
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
        }
        if matches!(self.tok(), Token::LParen) {
            parts.push(self.fmt_delimited_list(
                &Token::LParen,
                &Token::RParen,
                false,
                DelimListStyle::Tuple,
                Self::expect_ident,
            )?);
        } else {
            parts.push(self.expect_ident()?);
        }
        parts.push(Doc::text(" "));
        parts.push(self.expect(&Token::In)?);
        parts.push(Doc::text(" "));
        parts.push(self.fmt_expr()?);
        if matches!(self.tok(), Token::DotDot | Token::DotDotEq) {
            parts.push(self.emit_tight_tok());
            parts.push(self.fmt_expr()?);
            if matches!(self.tok(), Token::By) {
                parts.push(Doc::text(" "));
                parts.push(self.emit_tok());
                parts.push(Doc::text(" "));
                parts.push(self.fmt_expr()?);
            }
        }
        parts.push(Doc::text(" "));
        parts.push(self.fmt_block_for_owner(Some(gid))?);
        parts.push(self.opt_semi());
        let grouped = Doc::group_with_id(Doc::concat(parts), gid);
        Ok(if leading.is_empty() {
            grouped
        } else {
            Doc::concat(vec![leading, grouped])
        })
    }

    fn fmt_par_for_stmt(&mut self) -> Result<Doc, String> {
        let leading = self.take_leading_own_line_comments_cur(false);
        let gid = self.alloc_group_id();
        let mut parts = vec![
            self.expect(&Token::Par)?,
            Doc::text(" "),
            self.expect(&Token::For)?,
            Doc::text(" "),
        ];
        if matches!(self.tok(), Token::Ident(_)) && matches!(self.peek(1), Token::Ident(_)) {
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
        }
        parts.push(self.expect_ident()?);
        parts.push(Doc::text(" "));
        parts.push(self.expect(&Token::In)?);
        parts.push(Doc::text(" "));
        parts.push(self.fmt_expr()?);
        if matches!(self.tok(), Token::DotDot | Token::DotDotEq) {
            parts.push(self.emit_tight_tok());
            parts.push(self.fmt_expr()?);
        }
        parts.push(Doc::text(" "));
        parts.push(self.fmt_block_for_owner(Some(gid))?);
        parts.push(self.opt_semi());
        let grouped = Doc::group_with_id(Doc::concat(parts), gid);
        Ok(if leading.is_empty() {
            grouped
        } else {
            Doc::concat(vec![leading, grouped])
        })
    }

    fn fmt_loop_stmt(&mut self) -> Result<Doc, String> {
        let mut parts = vec![self.expect(&Token::Loop)?, Doc::text(" ")];
        if matches!(self.tok(), Token::Ident(_)) {
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
        }
        parts.push(self.fmt_block()?);
        parts.push(self.opt_semi());
        Ok(Doc::concat(parts))
    }

    fn fmt_spawn_stmt(&mut self) -> Result<Doc, String> {
        let mut parts = vec![self.expect(&Token::Spawn)?];
        if matches!(self.tok(), Token::LParen) {
            parts.push(self.fmt_delimited_list(
                &Token::LParen,
                &Token::RParen,
                false,
                DelimListStyle::TrailingCommaOnBreak,
                Self::fmt_expr,
            )?);
        }
        parts.push(Doc::text(" "));
        parts.push(self.fmt_block()?);
        parts.push(self.opt_semi());
        Ok(Doc::concat(parts))
    }

    fn fmt_select_stmt(&mut self) -> Result<Doc, String> {
        let mut header = vec![self.expect(&Token::Select)?];
        if matches!(self.tok(), Token::Bang) {
            header.push(self.emit_tight_tok());
        }
        header.push(Doc::text(" "));
        let lbrace = self.expect(&Token::LBrace)?;

        let mut arms = Vec::new();
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !arms.is_empty() && self.had_blank_before_cur();
            let arm_doc = if matches!(self.tok(), Token::Ident(name) if name == "default") {
                let def_kw = self.emit_tok();
                let arrow = self.expect(&Token::FatArrow)?;
                let body = self.fmt_block()?;
                let comma = if matches!(self.tok(), Token::Comma) {
                    self.emit_tight_tok()
                } else {
                    Doc::Empty
                };
                Doc::concat(vec![
                    def_kw,
                    Doc::text(" "),
                    arrow,
                    Doc::text(" "),
                    body,
                    comma,
                ])
            } else {
                let binding = self.expect_ident()?;
                let eq = self.expect(&Token::Eq)?;
                let ch = self.fmt_expr()?;
                let arrow = self.expect(&Token::FatArrow)?;
                let body = self.fmt_block()?;
                let comma = if matches!(self.tok(), Token::Comma) {
                    self.emit_tight_tok()
                } else {
                    Doc::Empty
                };
                Doc::concat(vec![
                    binding,
                    Doc::text(" "),
                    eq,
                    Doc::text(" "),
                    ch,
                    Doc::text(" "),
                    arrow,
                    Doc::text(" "),
                    body,
                    comma,
                ])
            };
            arms.push((had_blank, arm_doc));
        }

        let dangling = self.take_dangling_comments(arms.is_empty());
        let rbrace = self.expect(&Token::RBrace)?;
        let semi = self.opt_semi();

        if arms.is_empty() && dangling.is_none() {
            return Ok(Doc::concat(vec![Doc::concat(header), lbrace, rbrace, semi]));
        }

        let mut inner = Vec::new();
        for (i, (had_blank, arm)) in arms.into_iter().enumerate() {
            if i == 0 {
                inner.push(Doc::HardLine);
            } else if had_blank {
                inner.push(Doc::BlankLine);
            } else {
                inner.push(Doc::HardLine);
            }
            inner.push(arm);
        }
        if let Some(dangling_doc) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(dangling_doc));
        }

        Ok(Doc::concat(vec![
            Doc::concat(header),
            lbrace,
            Doc::indent(Doc::concat(inner)),
            Doc::HardLine,
            rbrace,
            semi,
        ]))
    }

    pub fn fmt_pattern(&mut self) -> Result<Doc, String> {
        let mut parts = vec![self.fmt_sub_pattern()?];
        while matches!(self.tok(), Token::Pipe) {
            parts.push(Doc::Line);
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
            parts.push(self.fmt_sub_pattern()?);
        }
        Ok(Doc::group(Doc::concat(parts)))
    }

    pub fn fmt_sub_pattern(&mut self) -> Result<Doc, String> {
        match self.tok().clone() {
            Token::Minus if matches!(self.peek(1), Token::IntLit(_) | Token::FloatLit(_)) => {
                let minus = self.emit_tok();
                let lit = self.emit_tok();
                let mut parts = vec![Doc::concat(vec![minus, lit])];
                if matches!(self.tok(), Token::DotDotEq) {
                    parts.push(self.emit_tight_tok());
                    if matches!(self.tok(), Token::Minus) {
                        parts.push(self.emit_tok());
                    }
                    parts.push(self.emit_tok());
                }
                Ok(Doc::concat(parts))
            }
            Token::IntLit(_) | Token::FloatLit(_) => {
                let first = self.emit_tok();
                if matches!(self.tok(), Token::DotDotEq) {
                    let dd = self.emit_tight_tok();
                    let hi = self.emit_tok();
                    Ok(Doc::concat(vec![first, dd, hi]))
                } else {
                    Ok(first)
                }
            }
            Token::BoolLit(_) | Token::StringLit(_) | Token::CharLit(_) => Ok(self.emit_tok()),
            Token::LParen => self.fmt_delimited_list(
                &Token::LParen,
                &Token::RParen,
                false,
                DelimListStyle::Tuple,
                Self::fmt_pattern,
            ),
            Token::LBracket => self.fmt_delimited_list(
                &Token::LBracket,
                &Token::RBracket,
                false,
                DelimListStyle::TrailingCommaOnBreak,
                |f| {
                    if matches!(f.tok(), Token::DotDot) {
                        let dd = f.emit_tok();
                        if Self::is_ident_tok(f.tok()) {
                            Ok(Doc::concat(vec![dd, f.expect_ident()?]))
                        } else {
                            Ok(dd)
                        }
                    } else {
                        f.fmt_pattern()
                    }
                },
            ),
            _ if Self::is_ident_tok(self.tok()) => {
                let first = self.expect_ident()?;
                if matches!(self.tok(), Token::At) {
                    let at = self.emit_tok();
                    let sub = self.fmt_pattern()?;
                    return Ok(Doc::concat(vec![
                        first,
                        Doc::text(" "),
                        at,
                        Doc::text(" "),
                        sub,
                    ]));
                }
                if matches!(self.tok(), Token::LBrace) {
                    let fields = self.fmt_delimited_list(
                        &Token::LBrace,
                        &Token::RBrace,
                        true,
                        DelimListStyle::TrailingCommaOnBreak,
                        |f| {
                            let fname = f.expect_ident()?;
                            if matches!(f.tok(), Token::Colon) {
                                let colon = f.emit_tight_tok();
                                let fpat = f.fmt_pattern()?;
                                Ok(Doc::concat(vec![fname, colon, Doc::text(" "), fpat]))
                            } else {
                                Ok(fname)
                            }
                        },
                    )?;
                    return Ok(Doc::concat(vec![first, Doc::text(" "), fields]));
                }
                let mut parts = vec![first];
                while matches!(self.tok(), Token::Dot) {
                    parts.push(self.emit_tight_tok());
                    parts.push(self.expect_ident()?);
                }
                if matches!(self.tok(), Token::LParen) {
                    parts.push(self.fmt_delimited_list(
                        &Token::LParen,
                        &Token::RParen,
                        false,
                        DelimListStyle::NoTrailingComma,
                        Self::fmt_pattern,
                    )?);
                }
                Ok(Doc::concat(parts))
            }
            other => Err(format!(
                "formatter expected pattern, found `{other}` at token {}",
                self.pos
            )),
        }
    }
}
