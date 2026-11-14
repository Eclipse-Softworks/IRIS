use crate::formatter::doc::Doc;
use crate::formatter::syntax::{eof_comments_trimmed_doc, DelimListStyle, Formatter};
use crate::parser::Token;

/// Binary operator precedence levels (strictly ordered low -> high).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinPrec {
    Or,
    And,
    Cmp,
    Add,
    Mul,
}

impl BinPrec {
    fn from_token(tok: &Token) -> Option<Self> {
        match tok {
            Token::PipePipe => Some(Self::Or),
            Token::AmpAmp => Some(Self::And),
            Token::EqEq
            | Token::NotEq
            | Token::LtGt
            | Token::LAngle
            | Token::LtEq
            | Token::RAngle
            | Token::GtEq => Some(Self::Cmp),
            Token::Plus | Token::Minus => Some(Self::Add),
            Token::Star | Token::Slash | Token::Percent => Some(Self::Mul),
            _ => None,
        }
    }

    fn next_higher(self) -> Option<Self> {
        match self {
            Self::Or => Some(Self::And),
            Self::And => Some(Self::Cmp),
            Self::Cmp => Some(Self::Add),
            Self::Add => Some(Self::Mul),
            Self::Mul => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    Regular,
    Hugged,
}

impl<'a> Formatter<'a> {
    pub fn fmt_expr(&mut self) -> Result<Doc, String> {
        let (doc, _) = self.fmt_expr_with_block_info()?;
        Ok(doc)
    }

    /// Formats an expression, returning `(doc, ends_as_statement_block)` where
    /// `ends_as_statement_block` matches `AstExpr::{If, When, Block, Mask}`.
    pub fn fmt_expr_with_block_info(&mut self) -> Result<(Doc, bool), String> {
        if self.tok() == &Token::If {
            let doc = self.fmt_if_expr()?;
            // Could continue into postfix `.foo()` / binary `+ ...`!
            return self.finish_expr_tail(doc, true);
        }
        if matches!(self.tok(), Token::When | Token::Match) {
            let doc = self.fmt_when_expr()?;
            return self.finish_expr_tail(doc, true);
        }
        if matches!(self.tok(), Token::With) {
            let doc = self.fmt_with_expr()?;
            return self.finish_expr_tail(doc, true);
        }
        if matches!(self.tok(), Token::LBrace) && !self.is_map_literal_start() {
            let doc = self.fmt_block()?;
            return self.finish_expr_tail(doc, true);
        }

        let doc = self.fmt_bin_level(BinPrec::Or)?;
        Ok((doc, false))
    }

    fn finish_expr_tail(
        &mut self,
        first_doc: Doc,
        was_block_primary: bool,
    ) -> Result<(Doc, bool), String> {
        // Postfix operations and binary operators may follow a block/if/when expression
        // (e.g. `if cond { 1 } else { 2 } + 3` or `when x { ... }.len()`).
        let starts_postfix_or_bin = matches!(
            self.tok(),
            Token::Dot
                | Token::Question
                | Token::QuestionQuestion
                | Token::To
                | Token::Plus
                | Token::Minus
                | Token::Star
                | Token::Slash
                | Token::Percent
                | Token::EqEq
                | Token::NotEq
                | Token::LtGt
                | Token::LAngle
                | Token::LtEq
                | Token::RAngle
                | Token::GtEq
                | Token::AmpAmp
                | Token::PipePipe
        ) || (matches!(self.tok(), Token::LBracket)
            && !self.at_line_start());

        if !starts_postfix_or_bin {
            return Ok((first_doc, was_block_primary));
        }
        let post = self.fmt_postfix_after(first_doc)?;
        let cast = self.fmt_cast_after(post)?;
        let mul = self.fmt_bin_tail(cast, BinPrec::Mul)?;
        let add = self.fmt_bin_tail(mul, BinPrec::Add)?;
        let cmp = self.fmt_bin_tail(add, BinPrec::Cmp)?;
        let and = self.fmt_bin_tail(cmp, BinPrec::And)?;
        let or = self.fmt_bin_tail(and, BinPrec::Or)?;
        Ok((or, false))
    }

    fn fmt_bin_level(&mut self, prec: BinPrec) -> Result<Doc, String> {
        let first = match prec.next_higher() {
            Some(higher) => self.fmt_bin_level(higher)?,
            None => self.fmt_cast_expr()?,
        };
        self.fmt_bin_tail(first, prec)
    }

    fn fmt_bin_tail(&mut self, first: Doc, prec: BinPrec) -> Result<Doc, String> {
        let stops_here = |f: &Self| {
            crate::parser::parse::stops_before_line_start_op(
                f.tokens,
                f.pos,
                f.expr_group_depth,
                f.tok(),
            )
        };
        if BinPrec::from_token(self.tok()) != Some(prec) || stops_here(self) {
            return Ok(first);
        }
        let mut ops = Vec::new();
        while BinPrec::from_token(self.tok()) == Some(prec) && !stops_here(self) {
            // Keep `*` and `-` on the same line as a preceding `}` outside grouping
            // delimiters so line-breaking cannot split them across a newline.
            let glue_to_prev = self.expr_group_depth == 0
                && self.prev_tok_is_rbrace()
                && matches!(self.tok(), Token::Star | Token::Minus);
            let op = self.emit_tok();
            let rhs = match prec.next_higher() {
                Some(higher) => self.fmt_bin_level(higher)?,
                None => self.fmt_cast_expr()?,
            };
            ops.push((glue_to_prev, op, rhs));
        }

        let mut parts = vec![first];
        let mut tail = Vec::with_capacity(ops.len() * 4);
        for (glue_to_prev, op, rhs) in ops {
            if glue_to_prev {
                tail.push(Doc::text(" "));
                tail.push(op);
                tail.push(Doc::Line);
                tail.push(rhs);
            } else {
                tail.push(Doc::Line);
                tail.push(op);
                tail.push(Doc::text(" "));
                tail.push(rhs);
            }
        }
        parts.push(Doc::indent(Doc::concat(tail)));
        Ok(Doc::group(Doc::concat(parts)))
    }

    fn fmt_cast_expr(&mut self) -> Result<Doc, String> {
        let first = self.fmt_unary_expr()?;
        self.fmt_cast_after(first)
    }

    fn fmt_cast_after(&mut self, mut expr: Doc) -> Result<Doc, String> {
        if !matches!(self.tok(), Token::To) {
            return Ok(expr);
        }
        let mut parts = vec![expr];
        while matches!(self.tok(), Token::To) {
            let kw = self.emit_tok();
            let ty = self.fmt_type()?;
            parts.push(Doc::Line);
            parts.push(kw);
            parts.push(Doc::text(" "));
            parts.push(ty);
        }
        expr = Doc::group(Doc::concat(parts));
        Ok(expr)
    }

    fn fmt_unary_expr(&mut self) -> Result<Doc, String> {
        match self.tok() {
            Token::Minus => {
                let op = self.emit_tok();
                let need_space = matches!(self.tok(), Token::Minus);
                let inner = self.fmt_unary_expr()?;
                if need_space {
                    Ok(Doc::concat(vec![op, Doc::text(" "), inner]))
                } else {
                    Ok(Doc::concat(vec![op, inner]))
                }
            }
            Token::Bang | Token::Star => {
                let op = self.emit_tok();
                let inner = self.fmt_unary_expr()?;
                Ok(Doc::concat(vec![op, inner]))
            }
            Token::Amp => {
                let op = self.emit_tok();
                if matches!(self.tok(), Token::Ident(s) if s == "mut") {
                    let m = self.emit_tok();
                    let inner = self.fmt_unary_expr()?;
                    Ok(Doc::concat(vec![op, m, Doc::text(" "), inner]))
                } else {
                    let inner = self.fmt_unary_expr()?;
                    Ok(Doc::concat(vec![op, inner]))
                }
            }
            Token::Move | Token::Await => {
                let op = self.emit_tok();
                let inner = self.fmt_unary_expr()?;
                Ok(Doc::concat(vec![op, Doc::text(" "), inner]))
            }
            Token::Unsafe => {
                let kw = self.emit_tok();
                let block = self.fmt_block()?;
                Ok(Doc::concat(vec![kw, Doc::text(" "), block]))
            }
            _ => {
                let (prim, _) = self.fmt_primary_expr()?;
                self.fmt_postfix_after(prim)
            }
        }
    }

    fn fmt_primary_expr(&mut self) -> Result<(Doc, ArgKind), String> {
        match self.tok().clone() {
            Token::Ident(ref name) if name == "handle" && self.is_handle_start() => {
                Ok((self.fmt_handle_expr()?, ArgKind::Hugged))
            }
            tok if crate::formatter::syntax::soft_keyword_name(&tok).is_some() => {
                let kw = self.emit_tok();
                if matches!(self.tok(), Token::LParen) && !self.at_line_start() {
                    let (args, _) = self.fmt_call_args_wrapped()?;
                    Ok((Doc::concat(vec![kw, args]), ArgKind::Regular))
                } else {
                    Ok((kw, ArgKind::Regular))
                }
            }
            Token::Ident(_) => self.fmt_ident_primary(),
            Token::IntLit(_)
            | Token::FloatLit(_)
            | Token::BoolLit(_)
            | Token::StringLit(_)
            | Token::CharLit(_)
            | Token::FStringLit(_) => Ok((self.emit_tok(), ArgKind::Regular)),
            Token::LParen => self.fmt_paren_or_tuple_expr(),
            Token::If => Ok((self.fmt_if_expr()?, ArgKind::Hugged)),
            Token::LBrace => {
                if self.is_map_literal_start() {
                    Ok((self.fmt_map_literal()?, ArgKind::Hugged))
                } else {
                    Ok((self.fmt_block()?, ArgKind::Hugged))
                }
            }
            Token::With => Ok((self.fmt_with_expr()?, ArgKind::Hugged)),
            Token::Try => Ok((self.fmt_try_expr()?, ArgKind::Hugged)),
            Token::Raise => Ok((self.fmt_raise_expr()?, ArgKind::Regular)),
            Token::LBracket => Ok((self.fmt_array_literal()?, ArgKind::Hugged)),
            Token::Pipe => self.fmt_pipe_lambda(),
            Token::PipePipe => self.fmt_zero_lambda(),
            Token::When | Token::Match => Ok((self.fmt_when_expr()?, ArgKind::Hugged)),
            ref tok if crate::formatter::syntax::reserved_noun(tok).is_some() => {
                Ok((self.emit_tok(), ArgKind::Regular))
            }
            other => Err(format!(
                "formatter expected expression, found `{other}` at token {}",
                self.pos
            )),
        }
    }

    fn fmt_ident_primary(&mut self) -> Result<(Doc, ArgKind), String> {
        let ident = self.emit_tok();
        if self.is_struct_lit_start() {
            let body = self.fmt_struct_lit_body()?;
            return Ok((
                Doc::concat(vec![ident, Doc::text(" "), body]),
                ArgKind::Hugged,
            ));
        }
        if matches!(self.tok(), Token::Bang) && matches!(self.peek(1), Token::LParen) {
            let bang = self.emit_tight_tok();
            let args = self.fmt_delimited_list(
                &Token::LParen,
                &Token::RParen,
                false,
                DelimListStyle::TrailingCommaOnBreak,
                Self::fmt_expr,
            )?;
            return Ok((Doc::concat(vec![ident, bang, args]), ArgKind::Regular));
        }
        if matches!(self.tok(), Token::LParen) && !self.at_line_start() {
            let (args, _) = self.fmt_call_args_wrapped()?;
            return Ok((Doc::concat(vec![ident, args]), ArgKind::Regular));
        }
        Ok((ident, ArgKind::Regular))
    }

    fn is_struct_lit_start(&self) -> bool {
        matches!(self.tok(), Token::LBrace)
            && (matches!(self.peek(1), Token::RBrace)
                || matches!(self.peek(1), Token::DotDot)
                || ((matches!(self.peek(1), Token::Ident(_)) || self.soft_keyword_at(1).is_some())
                    && matches!(self.peek(2), Token::Colon)))
    }

    fn is_map_literal_start(&self) -> bool {
        matches!(self.tok(), Token::LBrace)
            && matches!(
                self.peek(1),
                Token::StringLit(_) | Token::IntLit(_) | Token::Ident(_)
            )
            && matches!(self.peek(2), Token::Colon)
    }

    fn is_handle_start(&self) -> bool {
        // Look ahead for `with {` inside expression to distinguish `handle ... with {`
        // from an identifier named `handle`.
        let mut i = 1usize;
        let mut depth = 0usize;
        loop {
            match self.peek(i) {
                Token::Eof => return false,
                Token::LParen | Token::LBracket | Token::LBrace => depth += 1,
                Token::RParen | Token::RBracket | Token::RBrace => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                Token::With if depth == 0 && matches!(self.peek(i + 1), Token::LBrace) => {
                    return true;
                }
                Token::Semi if depth == 0 => return false,
                _ => {}
            }
            i += 1;
        }
    }

    fn fmt_postfix_after(&mut self, base: Doc) -> Result<Doc, String> {
        #[derive(Clone)]
        enum SegKind {
            Tight(Doc),
            Dot(Doc, bool), // (".field" or ".method(args)", is_call)
            NullCoal(Doc, Doc),
        }

        let mut segs: Vec<SegKind> = Vec::new();
        loop {
            if matches!(self.tok(), Token::LBracket) && !self.at_line_start() {
                let idx = self.fmt_delimited_list(
                    &Token::LBracket,
                    &Token::RBracket,
                    false,
                    DelimListStyle::TrailingCommaOnBreak,
                    Self::fmt_expr,
                )?;
                segs.push(SegKind::Tight(idx));
            } else if matches!(self.tok(), Token::Dot) {
                let dot = self.emit_tight_tok();
                if matches!(self.tok(), Token::IntLit(_)) {
                    let index = self.emit_tok();
                    segs.push(SegKind::Dot(Doc::concat(vec![dot, index]), false));
                } else {
                    let field = self.expect_ident()?;
                    if matches!(self.tok(), Token::LParen) {
                        let (args, _) = self.fmt_call_args_wrapped()?;
                        segs.push(SegKind::Dot(Doc::concat(vec![dot, field, args]), true));
                    } else if self.is_struct_lit_start() {
                        let body = self.fmt_struct_lit_body()?;
                        segs.push(SegKind::Dot(
                            Doc::concat(vec![dot, field, Doc::text(" "), body]),
                            false,
                        ));
                    } else {
                        segs.push(SegKind::Dot(Doc::concat(vec![dot, field]), false));
                    }
                }
            } else if matches!(self.tok(), Token::QuestionQuestion) {
                let op = self.emit_tok();
                let rhs = self.fmt_expr()?;
                segs.push(SegKind::NullCoal(op, rhs));
            } else if matches!(self.tok(), Token::Question) {
                segs.push(SegKind::Tight(self.emit_tight_tok()));
            } else {
                break;
            }
        }

        if segs.is_empty() {
            return Ok(base);
        }

        // Group `.` segments into a Prettier method chain ONLY when >= 2 method calls exist,
        // so `Point.new(...)` or `obj.foo(long1, long2)` keeps `obj.foo` attached and wraps
        // inside `(long1, long2)`.
        let method_call_count = segs
            .iter()
            .filter(|s| matches!(s, SegKind::Dot(_, true)))
            .count();
        let dot_count = segs
            .iter()
            .filter(|s| matches!(s, SegKind::Dot(_, _)))
            .count();
        let use_chain_wrap = method_call_count >= 2 || dot_count >= 3;

        if !use_chain_wrap {
            let mut out = vec![base];
            let mut null_coal_parts = Vec::new();
            for seg in segs {
                match seg {
                    SegKind::Tight(d) | SegKind::Dot(d, _) => out.push(d),
                    SegKind::NullCoal(op, rhs) => {
                        null_coal_parts.push(Doc::Line);
                        null_coal_parts.push(op);
                        null_coal_parts.push(Doc::text(" "));
                        null_coal_parts.push(rhs);
                    }
                }
            }
            if !null_coal_parts.is_empty() {
                out.push(Doc::indent(Doc::concat(null_coal_parts)));
                return Ok(Doc::group(Doc::concat(out)));
            }
            return Ok(Doc::concat(out));
        }

        let mut chain_parts = Vec::new();
        let mut non_chain_prefix = vec![base];
        let mut entered_chain = false;
        for seg in segs {
            match seg {
                SegKind::Tight(d) if !entered_chain => non_chain_prefix.push(d),
                SegKind::Tight(d) => chain_parts.push(d),
                SegKind::Dot(d, _) => {
                    entered_chain = true;
                    chain_parts.push(Doc::SoftLine);
                    chain_parts.push(d);
                }
                SegKind::NullCoal(op, rhs) => {
                    entered_chain = true;
                    chain_parts.push(Doc::Line);
                    chain_parts.push(op);
                    chain_parts.push(Doc::text(" "));
                    chain_parts.push(rhs);
                }
            }
        }
        let prefix = Doc::concat(non_chain_prefix);
        Ok(Doc::group(Doc::concat(vec![
            prefix,
            Doc::indent(Doc::concat(chain_parts)),
        ])))
    }

    pub fn fmt_call_arg(&mut self) -> Result<Doc, String> {
        let (doc, _) = self.fmt_call_arg_with_kind()?;
        Ok(doc)
    }

    fn fmt_call_arg_with_kind(&mut self) -> Result<(Doc, ArgKind), String> {
        if matches!(self.tok(), Token::DotDot) {
            let dd = self.emit_tok();
            let e = self.fmt_expr()?;
            return Ok((Doc::concat(vec![dd, e]), ArgKind::Regular));
        }
        if matches!(self.tok(), Token::Ident(_))
            && matches!(self.peek(1), Token::Eq)
            && !matches!(self.peek(2), Token::Eq)
        {
            let name = self.emit_tok();
            let eq = self.emit_tok();
            let val = self.fmt_expr()?;
            return Ok((
                Doc::concat(vec![name, Doc::text(" "), eq, Doc::text(" "), val]),
                ArgKind::Regular,
            ));
        }
        let doc = self.fmt_expr()?;
        let kind = match &doc {
            Doc::Group {
                should_break: true, ..
            } => ArgKind::Hugged,
            _ => ArgKind::Regular,
        };
        Ok((doc, kind))
    }

    fn fmt_call_args_wrapped(&mut self) -> Result<(Doc, bool), String> {
        self.with_expr_group(|f| f.fmt_call_args_wrapped_inner())
    }

    fn fmt_call_args_wrapped_inner(&mut self) -> Result<(Doc, bool), String> {
        let lparen = self.expect(&Token::LParen)?;
        if matches!(self.tok(), Token::RParen) {
            if let Some(dangling) = self.take_dangling_comments(true) {
                let (rparen, rparen_eol) = self.expect_close_split_eol(&Token::RParen)?;
                return Ok((
                    Doc::concat(vec![
                        lparen,
                        Doc::indent(Doc::concat(vec![
                            Doc::HardLine,
                            eof_comments_trimmed_doc(dangling),
                        ])),
                        Doc::HardLine,
                        rparen,
                        rparen_eol,
                    ]),
                    false,
                ));
            }
            let (rparen, rparen_eol) = self.expect_close_split_eol(&Token::RParen)?;
            return Ok((Doc::concat(vec![lparen, rparen, rparen_eol]), false));
        }

        let mut args: Vec<(Doc, Option<Doc>, ArgKind)> = Vec::new();
        let mut trail_doc = Doc::Empty;
        let mut trailing_seen = false;
        let mut arg_has_own_comment = self.has_own_line_comments_before_cur();

        loop {
            if matches!(self.tok(), Token::RParen | Token::Eof) {
                break;
            }
            arg_has_own_comment |= self.has_own_line_comments_before_cur();
            let leading = self.take_leading_own_line_comments_cur(args.is_empty());
            let (parsed_arg, kind) = self.fmt_call_arg_with_kind()?;
            let arg_doc = if leading.is_empty() {
                parsed_arg
            } else {
                Doc::concat(vec![leading, parsed_arg])
            };
            if matches!(self.tok(), Token::Comma) {
                if matches!(self.peek(1), Token::RParen) {
                    let (seen, doc) = self.consume_optional_trailing_comma(true);
                    trailing_seen = seen;
                    trail_doc = doc;
                    args.push((arg_doc, None, kind));
                    break;
                } else {
                    args.push((arg_doc, Some(self.emit_tight_tok()), kind));
                }
            } else {
                args.push((arg_doc, None, kind));
                break;
            }
        }

        let dangling = self.take_dangling_comments(false);
        let (rparen, rparen_eol) = self.expect_close_split_eol(&Token::RParen)?;

        // Prettier callback hugging: single callback/block/struct arg stays hugged inside `(...)`
        // (unless comments inside `(...)` or an explicit trailing comma require wrapping).
        if args.len() == 1
            && !trailing_seen
            && !arg_has_own_comment
            && dangling.is_none()
            && !self.trivia.has_any_comments(self.pos.saturating_sub(1))
        {
            let only = &args[0].0;
            if doc_has_hugged_shape(only) {
                let (only_doc, _, _) = args.into_iter().next().expect("len == 1");
                return Ok((
                    Doc::concat(vec![lparen, only_doc, rparen, rparen_eol]),
                    true,
                ));
            }
        }

        if matches!(trail_doc, Doc::Empty) && !trailing_seen {
            trail_doc = Doc::if_break(Doc::text(","), Doc::Empty);
        }

        let mut inner = vec![Doc::SoftLine];
        let last = args.len() - 1;
        for (i, (arg_doc, comma, _)) in args.into_iter().enumerate() {
            inner.push(arg_doc);
            if i < last {
                if let Some(c) = comma {
                    inner.push(c);
                } else {
                    inner.push(Doc::text(","));
                }
                inner.push(Doc::Line);
            } else {
                inner.push(trail_doc.clone());
            }
        }
        if let Some(dangling_doc) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(dangling_doc));
        }

        let group_doc = Doc::group(Doc::concat(vec![
            lparen,
            Doc::indent(Doc::concat(inner)),
            Doc::SoftLine,
            rparen,
        ]));
        Ok((
            if rparen_eol.is_empty() {
                group_doc
            } else {
                Doc::concat(vec![group_doc, rparen_eol])
            },
            false,
        ))
    }

    fn fmt_paren_or_tuple_expr(&mut self) -> Result<(Doc, ArgKind), String> {
        self.with_expr_group(|f| f.fmt_paren_or_tuple_expr_inner())
    }

    fn fmt_paren_or_tuple_expr_inner(&mut self) -> Result<(Doc, ArgKind), String> {
        let lp = self.expect(&Token::LParen)?;
        if matches!(self.tok(), Token::RParen) {
            let dangling = self.take_dangling_comments(true);
            let (rp, rp_eol) = self.expect_close_split_eol(&Token::RParen)?;
            return if let Some(d) = dangling {
                Ok((
                    Doc::concat(vec![
                        lp,
                        Doc::indent(Doc::concat(vec![
                            Doc::HardLine,
                            eof_comments_trimmed_doc(d),
                        ])),
                        Doc::HardLine,
                        rp,
                        rp_eol,
                    ]),
                    ArgKind::Regular,
                ))
            } else {
                Ok((Doc::concat(vec![lp, rp, rp_eol]), ArgKind::Regular))
            };
        }

        let first_leading = self.take_leading_own_line_comments_cur(true);
        let first_expr = self.fmt_expr()?;
        let first = if first_leading.is_empty() {
            first_expr
        } else {
            Doc::concat(vec![first_leading, first_expr])
        };
        if !matches!(self.tok(), Token::Comma) {
            let dangling = self.take_dangling_comments(false);
            let (rp, rp_eol) = self.expect_close_split_eol(&Token::RParen)?;
            let group_doc = if let Some(d) = dangling {
                Doc::group(Doc::concat(vec![
                    lp,
                    Doc::indent(Doc::concat(vec![
                        Doc::SoftLine,
                        first,
                        Doc::HardLine,
                        eof_comments_trimmed_doc(d),
                    ])),
                    Doc::SoftLine,
                    rp,
                ]))
            } else {
                Doc::group(Doc::concat(vec![
                    lp,
                    Doc::indent(Doc::concat(vec![Doc::SoftLine, first])),
                    Doc::SoftLine,
                    rp,
                ]))
            };
            return Ok((
                if rp_eol.is_empty() {
                    group_doc
                } else {
                    Doc::concat(vec![group_doc, rp_eol])
                },
                ArgKind::Regular,
            ));
        }

        let mut elems = vec![(first, self.emit_tight_tok())];
        let mut trail_after_first_comma_only = matches!(self.tok(), Token::RParen);
        while !matches!(self.tok(), Token::RParen | Token::Eof) {
            let leading = self.take_leading_own_line_comments_cur(false);
            let expr_doc = self.fmt_expr()?;
            let e = if leading.is_empty() {
                expr_doc
            } else {
                Doc::concat(vec![leading, expr_doc])
            };
            if matches!(self.tok(), Token::Comma) {
                if matches!(self.peek(1), Token::RParen) {
                    let (_, tdoc) = self.consume_optional_trailing_comma(true);
                    elems.push((e, tdoc));
                    trail_after_first_comma_only = false;
                    break;
                } else {
                    elems.push((e, self.emit_tight_tok()));
                }
            } else {
                elems.push((e, Doc::if_break(Doc::text(","), Doc::Empty)));
                trail_after_first_comma_only = false;
                break;
            }
        }
        let dangling = self.take_dangling_comments(false);
        let (rp, rp_eol) = self.expect_close_split_eol(&Token::RParen)?;

        let mut inner = vec![Doc::SoftLine];
        let last = elems.len() - 1;
        for (i, (e, sep)) in elems.into_iter().enumerate() {
            inner.push(e);
            inner.push(sep);
            if i < last {
                inner.push(Doc::Line);
            }
        }
        let _ = trail_after_first_comma_only;
        if let Some(d) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(d));
        }

        let group_doc = Doc::group(Doc::concat(vec![
            lp,
            Doc::indent(Doc::concat(inner)),
            Doc::SoftLine,
            rp,
        ]));
        Ok((
            if rp_eol.is_empty() {
                group_doc
            } else {
                Doc::concat(vec![group_doc, rp_eol])
            },
            ArgKind::Regular,
        ))
    }

    fn fmt_if_expr(&mut self) -> Result<Doc, String> {
        let leading = self.take_leading_own_line_comments_cur(false);
        let gid = self.alloc_group_id();
        let (chain_doc, force_break) = self.fmt_if_chain(gid)?;
        let grouped = if force_break {
            Doc::group_breaking_with_id(chain_doc, gid)
        } else {
            Doc::group_with_id(chain_doc, gid)
        };
        Ok(if leading.is_empty() {
            grouped
        } else {
            Doc::concat(vec![leading, grouped])
        })
    }

    fn fmt_if_chain(&mut self, gid: u32) -> Result<(Doc, bool), String> {
        let mut parts = vec![self.expect(&Token::If)?, Doc::text(" ")];
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
        let mut force_break = false;
        if matches!(self.tok(), Token::Else) {
            if self.had_newline_before_cur() {
                force_break = true;
            }
            parts.push(Doc::text(" "));
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
            if matches!(self.tok(), Token::If) {
                force_break = true;
                let (else_if_doc, _) = self.fmt_if_chain(gid)?;
                parts.push(else_if_doc);
            } else {
                parts.push(self.fmt_block_for_owner(Some(gid))?);
            }
        }
        Ok((Doc::concat(parts), force_break))
    }

    fn fmt_with_expr(&mut self) -> Result<Doc, String> {
        let leading = self.take_leading_own_line_comments_cur(false);
        let gid = self.alloc_group_id();
        let kw = self.expect(&Token::With)?;
        let first = self.expect_ident()?;
        let mut effs = vec![first];
        while matches!(self.tok(), Token::Comma) {
            effs.push(self.emit_tight_tok());
            effs.push(Doc::Line);
            effs.push(self.expect_ident()?);
        }
        let block = self.fmt_block_for_owner(Some(gid))?;
        let grouped = Doc::group_with_id(
            Doc::concat(vec![
                kw,
                Doc::text(" "),
                Doc::group(Doc::concat(effs)),
                Doc::text(" "),
                block,
            ]),
            gid,
        );
        Ok(if leading.is_empty() {
            grouped
        } else {
            Doc::concat(vec![leading, grouped])
        })
    }

    fn fmt_try_expr(&mut self) -> Result<Doc, String> {
        let try_kw = self.expect(&Token::Try)?;
        let body = self.fmt_block()?;
        let catch_kw = self.expect(&Token::Catch)?;
        let param = self.expect_ident()?;
        let catch_body = self.fmt_block()?;
        Ok(Doc::concat(vec![
            try_kw,
            Doc::text(" "),
            body,
            Doc::text(" "),
            catch_kw,
            Doc::text(" "),
            param,
            Doc::text(" "),
            catch_body,
        ]))
    }

    fn fmt_raise_expr(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Raise)?;
        let name = self.expect_ident()?;
        let args = if matches!(self.tok(), Token::LParen) {
            self.fmt_delimited_list(
                &Token::LParen,
                &Token::RParen,
                false,
                DelimListStyle::TrailingCommaOnBreak,
                Self::fmt_expr,
            )?
        } else {
            Doc::Empty
        };
        Ok(Doc::concat(vec![kw, Doc::text(" "), name, args]))
    }

    fn fmt_handle_expr(&mut self) -> Result<Doc, String> {
        let hkw = self.emit_tok();
        let inner_expr = self.fmt_expr()?;
        let wkw = self.expect(&Token::With)?;
        let lbrace = self.expect(&Token::LBrace)?;

        let mut arms = Vec::new();
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !arms.is_empty() && self.had_blank_before_cur();
            let eff = self.expect_ident()?;
            let params = if matches!(self.tok(), Token::LParen) {
                self.fmt_delimited_list(
                    &Token::LParen,
                    &Token::RParen,
                    false,
                    DelimListStyle::TrailingCommaOnBreak,
                    Self::expect_ident,
                )?
            } else {
                Doc::Empty
            };
            let mut arm_parts = vec![eff, params];
            if matches!(self.tok(), Token::Arrow) {
                arm_parts.push(Doc::text(" "));
                arm_parts.push(self.emit_tok());
                arm_parts.push(Doc::text(" "));
                arm_parts.push(self.expect(&Token::Resume)?);
                if matches!(self.tok(), Token::LParen) {
                    arm_parts.push(self.fmt_delimited_list(
                        &Token::LParen,
                        &Token::RParen,
                        false,
                        DelimListStyle::NoTrailingComma,
                        Self::expect_ident,
                    )?);
                }
            }
            arm_parts.push(Doc::text(" "));
            arm_parts.push(self.expect(&Token::FatArrow)?);
            arm_parts.push(Doc::text(" "));
            arm_parts.push(self.fmt_expr()?);
            if matches!(self.tok(), Token::Comma) {
                arm_parts.push(self.emit_tight_tok());
            }
            arms.push((had_blank, Doc::group(Doc::concat(arm_parts))));
        }

        let dangling = self.take_dangling_comments(arms.is_empty());
        let rbrace = self.expect(&Token::RBrace)?;

        if arms.is_empty() && dangling.is_none() {
            return Ok(Doc::concat(vec![
                hkw,
                Doc::text(" "),
                inner_expr,
                Doc::text(" "),
                wkw,
                Doc::text(" "),
                lbrace,
                rbrace,
            ]));
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
        if let Some(d) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(d));
        }

        Ok(Doc::concat(vec![
            hkw,
            Doc::text(" "),
            inner_expr,
            Doc::text(" "),
            wkw,
            Doc::text(" "),
            lbrace,
            Doc::indent(Doc::concat(inner)),
            Doc::HardLine,
            rbrace,
        ]))
    }

    fn fmt_when_expr(&mut self) -> Result<Doc, String> {
        let kw = self.emit_tok();
        let scrutinee = self.fmt_expr()?;
        let lbrace = self.expect(&Token::LBrace)?;

        let mut arms = Vec::new();
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !arms.is_empty() && self.had_blank_before_cur();
            let pat = self.fmt_pattern()?;
            let guard = if matches!(self.tok(), Token::If) {
                let g_if = self.emit_tok();
                let g_expr = self.fmt_expr()?;
                Doc::concat(vec![Doc::text(" "), g_if, Doc::text(" "), g_expr])
            } else {
                Doc::Empty
            };
            let arrow = self.expect(&Token::FatArrow)?;
            let body = self.fmt_expr()?;
            let comma = if matches!(self.tok(), Token::Comma) {
                self.emit_tight_tok()
            } else {
                Doc::Empty
            };
            arms.push((
                had_blank,
                Doc::group(Doc::concat(vec![
                    pat,
                    guard,
                    Doc::text(" "),
                    arrow,
                    Doc::text(" "),
                    body,
                    comma,
                ])),
            ));
        }

        let dangling = self.take_dangling_comments(arms.is_empty());
        let rbrace = self.expect(&Token::RBrace)?;

        if arms.is_empty() && dangling.is_none() {
            return Ok(Doc::concat(vec![
                kw,
                Doc::text(" "),
                scrutinee,
                Doc::text(" "),
                lbrace,
                rbrace,
            ]));
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
        if let Some(d) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(d));
        }

        Ok(Doc::concat(vec![
            kw,
            Doc::text(" "),
            scrutinee,
            Doc::text(" "),
            lbrace,
            Doc::indent(Doc::concat(inner)),
            Doc::HardLine,
            rbrace,
        ]))
    }

    fn fmt_array_literal(&mut self) -> Result<Doc, String> {
        self.fmt_delimited_list(
            &Token::LBracket,
            &Token::RBracket,
            false,
            DelimListStyle::TrailingCommaOnBreak,
            |f| {
                if matches!(f.tok(), Token::DotDot) {
                    let dd = f.emit_tok();
                    let e = f.fmt_expr()?;
                    Ok(Doc::concat(vec![dd, e]))
                } else {
                    f.fmt_expr()
                }
            },
        )
    }

    fn fmt_pipe_lambda(&mut self) -> Result<(Doc, ArgKind), String> {
        let params = self.fmt_delimited_list(
            &Token::Pipe,
            &Token::Pipe,
            false,
            DelimListStyle::NoTrailingComma,
            |f| {
                let name = f.expect_ident()?;
                let colon = f.expect(&Token::Colon)?;
                let ty = f.fmt_type()?;
                Ok(Doc::concat(vec![name, colon, Doc::text(" "), ty]))
            },
        )?;
        let body = self.fmt_expr()?;
        let is_block = matches!(body, Doc::Group { .. } | Doc::Concat(_));
        Ok((
            Doc::concat(vec![params, Doc::text(" "), body]),
            if is_block {
                ArgKind::Hugged
            } else {
                ArgKind::Regular
            },
        ))
    }

    fn fmt_zero_lambda(&mut self) -> Result<(Doc, ArgKind), String> {
        let pipes = self.expect(&Token::PipePipe)?;
        let body = self.fmt_expr()?;
        Ok((
            Doc::concat(vec![pipes, Doc::text(" "), body]),
            ArgKind::Hugged,
        ))
    }

    fn fmt_struct_lit_body(&mut self) -> Result<Doc, String> {
        let lbrace = self.expect(&Token::LBrace)?;
        let had_nl = self.had_newline_before_cur();

        let mut entries = Vec::new();
        let mut trail_doc = Doc::Empty;
        if matches!(self.tok(), Token::DotDot) {
            let dd = self.emit_tok();
            let sp = self.fmt_expr()?;
            let doc = Doc::concat(vec![dd, sp]);
            if matches!(self.tok(), Token::Comma) {
                if matches!(self.peek(1), Token::RBrace) {
                    let (_, t) = self.consume_optional_trailing_comma(true);
                    trail_doc = t;
                    entries.push((doc, None));
                } else {
                    entries.push((doc, Some(self.emit_tight_tok())));
                }
            } else {
                entries.push((doc, None));
            }
        }

        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let fname = self.expect_ident()?;
            let colon = self.expect(&Token::Colon)?;
            let fval = self.fmt_expr()?;
            let fdoc = Doc::concat(vec![fname, colon, Doc::text(" "), fval]);
            if matches!(self.tok(), Token::Comma) {
                if matches!(self.peek(1), Token::RBrace) {
                    let (_, t) = self.consume_optional_trailing_comma(true);
                    trail_doc = t;
                    entries.push((fdoc, None));
                    break;
                } else {
                    entries.push((fdoc, Some(self.emit_tight_tok())));
                }
            } else {
                entries.push((fdoc, None));
                break;
            }
        }

        self.finish_object_literal(lbrace, had_nl, entries, trail_doc)
    }

    fn fmt_map_literal(&mut self) -> Result<Doc, String> {
        let lbrace = self.expect(&Token::LBrace)?;
        let had_nl = self.had_newline_before_cur();

        let mut entries = Vec::new();
        let mut trail_doc = Doc::Empty;
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let k = self.fmt_expr()?;
            let colon = self.expect(&Token::Colon)?;
            let v = self.fmt_expr()?;
            let edoc = Doc::concat(vec![k, colon, Doc::text(" "), v]);
            if matches!(self.tok(), Token::Comma) {
                if matches!(self.peek(1), Token::RBrace) {
                    let (_, t) = self.consume_optional_trailing_comma(true);
                    trail_doc = t;
                    entries.push((edoc, None));
                    break;
                } else {
                    entries.push((edoc, Some(self.emit_tight_tok())));
                }
            } else {
                entries.push((edoc, None));
                break;
            }
        }

        self.finish_object_literal(lbrace, had_nl, entries, trail_doc)
    }

    fn finish_object_literal(
        &mut self,
        lbrace: Doc,
        had_nl_after_lbrace: bool,
        entries: Vec<(Doc, Option<Doc>)>,
        mut trail_doc: Doc,
    ) -> Result<Doc, String> {
        if entries.is_empty() {
            if let Some(dangling) = self.take_dangling_comments(true) {
                let rbrace = self.expect(&Token::RBrace)?;
                return Ok(Doc::concat(vec![
                    lbrace,
                    Doc::indent(Doc::concat(vec![
                        Doc::HardLine,
                        eof_comments_trimmed_doc(dangling),
                    ])),
                    Doc::HardLine,
                    rbrace,
                ]));
            }
            let rbrace = self.expect(&Token::RBrace)?;
            return Ok(Doc::concat(vec![lbrace, rbrace]));
        }

        if matches!(trail_doc, Doc::Empty) {
            trail_doc = Doc::if_break(Doc::text(","), Doc::Empty);
        }

        let dangling = self.take_dangling_comments(false);
        let force_break = had_nl_after_lbrace || dangling.is_some();
        let rbrace = self.expect(&Token::RBrace)?;

        let mut inner = vec![Doc::Line];
        let last = entries.len() - 1;
        for (i, (item, comma)) in entries.into_iter().enumerate() {
            inner.push(item);
            if i < last {
                if let Some(c) = comma {
                    inner.push(c);
                } else {
                    inner.push(Doc::text(","));
                }
                inner.push(Doc::Line);
            } else {
                inner.push(trail_doc.clone());
            }
        }
        if let Some(d) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(d));
        }

        let body = Doc::concat(vec![
            lbrace,
            Doc::indent(Doc::concat(inner)),
            Doc::Line,
            rbrace,
        ]);
        if force_break {
            Ok(Doc::group_breaking(body))
        } else {
            Ok(Doc::group(body))
        }
    }
}

/// Checks if a single argument doc contains a braced multi-line block/lambda/struct
/// so single-argument calls hug cleanly: `items.map(|x: i64| {\n    ...\n})`.
fn doc_has_hugged_shape(doc: &Doc) -> bool {
    fn scan(doc: &Doc, has_brace: &mut bool, has_nl: &mut bool) {
        match doc {
            Doc::Text(s) if s == "{" => *has_brace = true,
            Doc::HardLine | Doc::BlankLine => *has_nl = true,
            Doc::Concat(items) => {
                for item in items {
                    scan(item, has_brace, has_nl);
                }
            }
            Doc::Indent(inner)
            | Doc::Group {
                contents: inner, ..
            } => {
                scan(inner, has_brace, has_nl);
            }
            _ => {}
        }
    }
    let mut has_brace = false;
    let mut has_nl = false;
    scan(doc, &mut has_brace, &mut has_nl);
    has_brace && has_nl
}
