use crate::formatter::doc::Doc;
use crate::formatter::syntax::{eof_comments_trimmed_doc, DelimListStyle, Formatter, ItemKind};
use crate::parser::Token;

impl<'a> Formatter<'a> {
    pub fn classify_top_level_item(&self) -> ItemKind {
        match (self.tok(), self.peek(1)) {
            (Token::Bring, _) | (Token::Pub, Token::Bring) => ItemKind::Bring,
            _ => ItemKind::Other,
        }
    }

    pub fn fmt_top_level_item(&mut self, allow_pub_or_bring: bool) -> Result<Doc, String> {
        let mut prefix_parts = Vec::new();

        // Parse optional `@attr` annotations or `pub` before `extern`/`def`/`record`/etc.
        // Notice `pub` can appear before `@attr extern` OR before `def`, `record`, etc.,
        // and `@attr` can also appear before `async`/`def`/`extern`.
        let mut saw_pub = false;
        if allow_pub_or_bring && matches!(self.tok(), Token::Pub) {
            saw_pub = true;
            prefix_parts.push(self.emit_tok());
            prefix_parts.push(Doc::text(" "));
        }

        let mut attr_docs = Vec::new();
        while matches!(self.tok(), Token::At) {
            attr_docs.push(self.fmt_attribute()?);
        }

        if allow_pub_or_bring && !saw_pub && matches!(self.tok(), Token::Pub) {
            prefix_parts.push(self.emit_tok());
            prefix_parts.push(Doc::text(" "));
        }

        let attr_block = if attr_docs.is_empty() {
            Doc::Empty
        } else {
            let mut items = Vec::with_capacity(attr_docs.len() * 2);
            for a in attr_docs {
                items.push(a);
                items.push(Doc::HardLine);
            }
            Doc::concat(items)
        };

        let body_doc = match self.tok().clone() {
            Token::Bring if allow_pub_or_bring => self.fmt_bring()?,
            Token::Extern => self.fmt_extern_fn()?,
            Token::Record => self.fmt_record_def()?,
            Token::Choice => self.fmt_choice_def()?,
            Token::Trait => self.fmt_trait_def()?,
            Token::Impl => self.fmt_impl_def()?,
            Token::Effect => self.fmt_effect_def()?,
            Token::Model => self.fmt_model_def()?,
            Token::Mod => self.fmt_mod_def()?,
            Token::DefMacro => self.fmt_macro_def()?,
            Token::Type => self.fmt_type_alias()?,
            Token::Var => self.fmt_state_var()?,
            Token::Const => {
                if matches!(self.peek(1), Token::Def | Token::Async) {
                    self.fmt_fn_def()?
                } else {
                    self.fmt_const_decl()?
                }
            }
            Token::Async | Token::Def => self.fmt_fn_def()?,
            other => {
                return Err(format!(
                    "formatter unexpected top-level token `{other}` at token {}",
                    self.pos
                ));
            }
        };

        let prefix = Doc::concat(prefix_parts);
        let combined = if matches!(attr_block, Doc::Empty) {
            Doc::concat(vec![prefix, body_doc])
        } else {
            // Keep attributes above `pub def` or directly above `def`/`extern`.
            Doc::concat(vec![attr_block, prefix, body_doc])
        };

        Ok(combined)
    }

    fn fmt_attribute(&mut self) -> Result<Doc, String> {
        let at = self.expect(&Token::At)?;
        let name = self.expect_ident()?;
        if matches!(self.tok(), Token::LParen) {
            let args = self.fmt_delimited_list(
                &Token::LParen,
                &Token::RParen,
                false,
                DelimListStyle::TrailingCommaOnBreak,
                Self::fmt_call_arg,
            )?;
            Ok(Doc::concat(vec![at, name, args]))
        } else {
            Ok(Doc::concat(vec![at, name]))
        }
    }

    fn fmt_bring(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Bring)?;
        let mut parts = vec![kw, Doc::text(" ")];
        match self.tok() {
            Token::StringLit(_) => {
                parts.push(self.emit_tok());
            }
            _ => {
                let first = self.expect_ident_or_keyword()?;
                parts.push(first);
                // Handle `std.name` (or `mod.submod`), stopping before `.{` or `.*`.
                while matches!(self.tok(), Token::Dot)
                    && !matches!(self.peek(1), Token::LBrace | Token::Star)
                {
                    parts.push(self.emit_tight_tok());
                    parts.push(self.expect_ident_or_keyword()?);
                }
            }
        }
        if matches!(self.tok(), Token::Dot) {
            parts.push(self.emit_tight_tok());
            if matches!(self.tok(), Token::Star) {
                parts.push(self.emit_tight_tok());
            } else if matches!(self.tok(), Token::LBrace) {
                let items = self.fmt_delimited_list(
                    &Token::LBrace,
                    &Token::RBrace,
                    false,
                    DelimListStyle::TrailingCommaOnBreak,
                    Self::expect_ident,
                )?;
                parts.push(items);
            }
        }
        if matches!(self.tok(), Token::Semi) {
            parts.push(self.emit_tight_tok());
        }
        Ok(Doc::concat(parts))
    }

    fn fmt_type_alias(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Type)?;
        let name = self.expect_ident()?;
        let mut parts = vec![kw, Doc::text(" "), name];
        if matches!(self.tok(), Token::LBracket) {
            parts.push(self.fmt_generic_params()?);
        }
        let eq = self.expect(&Token::Eq)?;
        let ty = self.fmt_type()?;
        parts.push(Doc::text(" "));
        parts.push(eq);
        parts.push(Doc::text(" "));
        parts.push(ty);
        if matches!(self.tok(), Token::Semi) {
            parts.push(self.emit_tight_tok());
        }
        Ok(Doc::group(Doc::concat(parts)))
    }

    fn fmt_const_decl(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Const)?;
        let name = self.expect_ident()?;
        let mut parts = vec![kw, Doc::text(" "), name];
        if matches!(self.tok(), Token::Colon) {
            parts.push(self.emit_tight_tok());
            parts.push(Doc::text(" "));
            parts.push(self.fmt_type()?);
        }
        let eq = self.expect(&Token::Eq)?;
        let val = self.fmt_expr()?;
        parts.push(Doc::text(" "));
        parts.push(eq);
        parts.push(Doc::text(" "));
        parts.push(val);
        if matches!(self.tok(), Token::Semi) {
            parts.push(self.emit_tight_tok());
        }
        Ok(Doc::group(Doc::concat(parts)))
    }

    fn fmt_state_var(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Var)?;
        let name = self.expect_ident()?;
        let colon = self.expect(&Token::Colon)?;
        let ty = self.fmt_type()?;
        let eq = self.expect(&Token::Eq)?;
        let init = self.fmt_expr()?;
        let semi = if matches!(self.tok(), Token::Semi) {
            self.emit_tight_tok()
        } else {
            Doc::Empty
        };
        Ok(Doc::group(Doc::concat(vec![
            kw,
            Doc::text(" "),
            name,
            colon,
            Doc::text(" "),
            ty,
            Doc::text(" "),
            eq,
            Doc::text(" "),
            init,
            semi,
        ])))
    }

    fn fmt_macro_def(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::DefMacro)?;
        let name = self.expect_ident()?;
        let params = self.fmt_delimited_list(
            &Token::LParen,
            &Token::RParen,
            false,
            DelimListStyle::TrailingCommaOnBreak,
            Self::expect_ident,
        )?;
        let arrow = self.expect(&Token::FatArrow)?;
        let body = self.fmt_expr()?;
        let semi = if matches!(self.tok(), Token::Semi) {
            self.emit_tight_tok()
        } else {
            Doc::Empty
        };
        Ok(Doc::group(Doc::concat(vec![
            kw,
            Doc::text(" "),
            name,
            params,
            Doc::text(" "),
            arrow,
            Doc::text(" "),
            body,
            semi,
        ])))
    }

    fn fmt_extern_fn(&mut self) -> Result<Doc, String> {
        let mut parts = vec![self.expect(&Token::Extern)?];
        if matches!(self.tok(), Token::StringLit(_)) {
            parts.push(Doc::text(" "));
            parts.push(self.emit_tok());
        }
        parts.push(Doc::text(" "));
        parts.push(self.expect(&Token::Def)?);
        parts.push(Doc::text(" "));
        parts.push(self.expect_ident()?);
        parts.push(self.fmt_params_list()?);
        if matches!(self.tok(), Token::Arrow) {
            parts.push(Doc::text(" "));
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
            parts.push(self.fmt_type()?);
        }
        parts.push(self.fmt_effects_clause()?);
        if matches!(self.tok(), Token::Semi) {
            parts.push(self.emit_tight_tok());
        }
        Ok(Doc::group(Doc::concat(parts)))
    }

    fn fmt_fn_def(&mut self) -> Result<Doc, String> {
        let leading = self.take_leading_own_line_comments_cur(false);
        let mut sig = Vec::new();
        if matches!(self.tok(), Token::Const) {
            sig.push(self.emit_tok());
            sig.push(Doc::text(" "));
        }
        if matches!(self.tok(), Token::Async) {
            sig.push(self.emit_tok());
            sig.push(Doc::text(" "));
        }
        sig.push(self.expect(&Token::Def)?);
        sig.push(Doc::text(" "));
        sig.push(self.expect_ident()?);
        if matches!(self.tok(), Token::LBracket) {
            sig.push(self.fmt_generic_params()?);
        }
        sig.push(self.fmt_params_list()?);
        if matches!(self.tok(), Token::Arrow) {
            sig.push(Doc::text(" "));
            sig.push(self.emit_tok());
            sig.push(Doc::text(" "));
            sig.push(self.fmt_type()?);
        }
        sig.push(self.fmt_effects_clause()?);
        sig.push(Doc::text(" "));
        let body_doc = self.fmt_block()?;
        Ok(Doc::concat(vec![
            leading,
            Doc::group(Doc::concat(sig)),
            body_doc,
        ]))
    }

    pub fn fmt_generic_params(&mut self) -> Result<Doc, String> {
        self.fmt_delimited_list(
            &Token::LBracket,
            &Token::RBracket,
            false,
            DelimListStyle::NoTrailingComma,
            Self::fmt_one_generic_param,
        )
    }

    fn fmt_one_generic_param(&mut self) -> Result<Doc, String> {
        if matches!(self.tok(), Token::Const) {
            let kw = self.emit_tok();
            let name = self.expect_ident()?;
            let colon = self.expect(&Token::Colon)?;
            let ty = self.fmt_type()?;
            return Ok(Doc::concat(vec![
                kw,
                Doc::text(" "),
                name,
                colon,
                Doc::text(" "),
                ty,
            ]));
        }
        let mut parts = Vec::new();
        if matches!(self.tok(), Token::Plus | Token::Minus) {
            parts.push(self.emit_tok());
        }
        parts.push(self.expect_ident()?);
        if matches!(self.tok(), Token::LBracket) {
            parts.push(self.fmt_generic_params()?);
        }
        if matches!(self.tok(), Token::Ident(w) if w == "where") {
            parts.push(Doc::text(" "));
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
            if Self::is_ident_tok(self.tok()) {
                parts.push(self.expect_ident()?);
            }
            parts.push(self.expect(&Token::Colon)?);
            parts.push(Doc::text(" "));
            if Self::is_ident_tok(self.tok()) {
                parts.push(self.expect_ident()?);
                while matches!(self.tok(), Token::Comma) {
                    // Check if comma continues trait bounds on this param or starts next param:
                    // look ahead for `where` bound pattern vs param declaration.
                    // In `T where T: A, B` bounds stop when next param has `:` or `[` or `+`/`-`/`const`.
                    // Note: `parse_generic_param` consumes all idents separated by commas until non-ident
                    // unless there is a subsequent param. Actually `parse_generic_param` loops
                    // `while Ident(_) { ident; if Comma { advance } else { break } }`!
                    if !matches!(self.peek(1), Token::Ident(_))
                        || matches!(self.peek(2), Token::Colon | Token::LBracket)
                    {
                        break;
                    }
                    if matches!(self.peek(2), Token::Ident(w) if w == "where") {
                        break;
                    }
                    parts.push(self.emit_tight_tok());
                    parts.push(Doc::text(" "));
                    parts.push(self.expect_ident()?);
                }
            }
        }
        Ok(Doc::concat(parts))
    }

    pub fn fmt_params_list(&mut self) -> Result<Doc, String> {
        self.fmt_delimited_list(
            &Token::LParen,
            &Token::RParen,
            false,
            DelimListStyle::TrailingCommaOnBreak,
            Self::fmt_one_param,
        )
    }

    fn fmt_one_param(&mut self) -> Result<Doc, String> {
        let name = self.expect_ident()?;
        if !matches!(self.tok(), Token::Colon) {
            return Ok(name);
        }
        let colon = self.emit_tight_tok();
        let ty = self.fmt_type()?;
        let mut parts = vec![name, colon, Doc::text(" "), ty];
        if matches!(self.tok(), Token::Eq) {
            parts.push(Doc::text(" "));
            parts.push(self.emit_tok());
            parts.push(Doc::text(" "));
            parts.push(self.fmt_expr()?);
        }
        Ok(Doc::concat(parts))
    }

    pub fn fmt_effects_clause(&mut self) -> Result<Doc, String> {
        if !matches!(self.tok(), Token::Effect | Token::With) {
            return Ok(Doc::Empty);
        }
        let kw = self.emit_tok();
        let first = self.expect_ident()?;
        let mut parts = vec![Doc::text(" "), kw, Doc::text(" "), first];
        while matches!(self.tok(), Token::Comma) {
            parts.push(self.emit_tight_tok());
            parts.push(Doc::Line);
            parts.push(self.expect_ident()?);
        }
        Ok(Doc::group(Doc::concat(parts)))
    }

    fn fmt_record_def(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Record)?;
        let name = self.expect_ident()?;
        let generics = if matches!(self.tok(), Token::LBracket) {
            self.fmt_generic_params()?
        } else {
            Doc::Empty
        };

        let lbrace = self.expect(&Token::LBrace)?;
        let had_nl_after_lbrace = self.had_newline_before_cur();

        let mut fields = Vec::new();
        let mut trail_doc = Doc::Empty;
        let mut last_forbids_comma = false;
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !fields.is_empty() && self.had_blank_before_cur();
            let leading = self.take_leading_own_line_comments_cur(fields.is_empty());
            let field_name = self.expect_ident()?;
            let colon = self.expect(&Token::Colon)?;
            self.forbid_next_trailing_comma = false;
            let ty = self.fmt_type()?;
            let default = if matches!(self.tok(), Token::Eq) {
                let eq = self.emit_tok();
                let val = self.fmt_expr()?;
                Doc::concat(vec![Doc::text(" "), eq, Doc::text(" "), val])
            } else {
                Doc::Empty
            };
            last_forbids_comma = self.forbid_next_trailing_comma;
            let f_doc = Doc::concat(vec![
                leading,
                field_name,
                colon,
                Doc::text(" "),
                ty,
                default,
            ]);
            if matches!(self.tok(), Token::Comma) {
                if matches!(self.peek(1), Token::RBrace) {
                    let (_, doc) = self.consume_optional_trailing_comma(!last_forbids_comma);
                    trail_doc = doc;
                    fields.push((had_blank, f_doc, None));
                    break;
                } else {
                    let comma = self.emit_tight_tok();
                    fields.push((had_blank, f_doc, Some(comma)));
                }
            } else {
                fields.push((had_blank, f_doc, None));
            }
        }

        if fields.is_empty() {
            if let Some(dangling) = self.take_dangling_comments(true) {
                let (rbrace, rbrace_eol) = self.expect_close_split_eol(&Token::RBrace)?;
                return Ok(Doc::concat(vec![
                    kw,
                    Doc::text(" "),
                    name,
                    generics,
                    Doc::text(" "),
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
            return Ok(Doc::concat(vec![
                kw,
                Doc::text(" "),
                name,
                generics,
                Doc::text(" "),
                lbrace,
                rbrace,
                rbrace_eol,
            ]));
        }

        if matches!(trail_doc, Doc::Empty) && !last_forbids_comma {
            trail_doc = Doc::if_break(Doc::text(","), Doc::Empty);
        }

        let dangling = self.take_dangling_comments(false);
        let force_break = had_nl_after_lbrace
            || fields.len() > 2
            || dangling.is_some()
            || fields.iter().any(|(b, _, _)| *b);
        let (rbrace, rbrace_eol) = self.expect_close_split_eol(&Token::RBrace)?;

        let mut inner = vec![Doc::Line];
        let last = fields.len() - 1;
        for (i, (had_blank, f_doc, comma)) in fields.into_iter().enumerate() {
            if i > 0 {
                if had_blank {
                    inner.push(Doc::BlankLine);
                } else {
                    inner.push(Doc::Line);
                }
            }
            inner.push(f_doc);
            if i < last {
                if let Some(c) = comma {
                    inner.push(c);
                } else {
                    inner.push(Doc::text(","));
                }
            } else {
                inner.push(trail_doc.clone());
            }
        }
        if let Some(dangling_doc) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(dangling_doc));
        }

        let brace_doc = Doc::concat(vec![
            lbrace,
            Doc::indent(Doc::concat(inner)),
            Doc::Line,
            rbrace,
        ]);
        let wrapped = if force_break {
            Doc::group_breaking(brace_doc)
        } else {
            Doc::group(brace_doc)
        };
        Ok(Doc::concat(vec![
            kw,
            Doc::text(" "),
            name,
            generics,
            Doc::text(" "),
            wrapped,
            rbrace_eol,
        ]))
    }

    fn fmt_choice_def(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Choice)?;
        let name = self.expect_ident()?;
        let generics = if matches!(self.tok(), Token::LBracket) {
            self.fmt_generic_params()?
        } else {
            Doc::Empty
        };
        let lbrace = self.expect(&Token::LBrace)?;
        let had_nl_after_lbrace = self.had_newline_before_cur();

        let mut variants = Vec::new();
        let mut trail_doc = Doc::Empty;
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !variants.is_empty() && self.had_blank_before_cur();
            let leading = self.take_leading_own_line_comments_cur(variants.is_empty());
            let v_name = self.expect_ident()?;
            let payload = if matches!(self.tok(), Token::LParen) {
                self.fmt_delimited_list(
                    &Token::LParen,
                    &Token::RParen,
                    false,
                    DelimListStyle::TrailingCommaOnBreak,
                    Self::fmt_type,
                )?
            } else {
                Doc::Empty
            };
            let v_doc = Doc::concat(vec![leading, v_name, payload]);
            if matches!(self.tok(), Token::Comma) {
                if matches!(self.peek(1), Token::RBrace) {
                    let (_, doc) = self.consume_optional_trailing_comma(true);
                    trail_doc = doc;
                    variants.push((had_blank, v_doc, None));
                    break;
                } else {
                    variants.push((had_blank, v_doc, Some(self.emit_tight_tok())));
                }
            } else {
                variants.push((had_blank, v_doc, None));
            }
        }

        if variants.is_empty() {
            if let Some(dangling) = self.take_dangling_comments(true) {
                let (rbrace, rbrace_eol) = self.expect_close_split_eol(&Token::RBrace)?;
                return Ok(Doc::concat(vec![
                    kw,
                    Doc::text(" "),
                    name,
                    generics,
                    Doc::text(" "),
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
            return Ok(Doc::concat(vec![
                kw,
                Doc::text(" "),
                name,
                generics,
                Doc::text(" "),
                lbrace,
                rbrace,
                rbrace_eol,
            ]));
        }

        if matches!(trail_doc, Doc::Empty) {
            trail_doc = Doc::if_break(Doc::text(","), Doc::Empty);
        }

        let dangling = self.take_dangling_comments(false);
        let force_break = had_nl_after_lbrace
            || variants.len() > 3
            || dangling.is_some()
            || variants.iter().any(|(b, _, _)| *b);
        let (rbrace, rbrace_eol) = self.expect_close_split_eol(&Token::RBrace)?;

        let mut inner = vec![Doc::Line];
        let last = variants.len() - 1;
        for (i, (had_blank, v_doc, comma)) in variants.into_iter().enumerate() {
            if i > 0 {
                inner.push(if had_blank { Doc::BlankLine } else { Doc::Line });
            }
            inner.push(v_doc);
            if i < last {
                if let Some(c) = comma {
                    inner.push(c);
                } else {
                    inner.push(Doc::text(","));
                }
            } else {
                inner.push(trail_doc.clone());
            }
        }
        if let Some(dangling_doc) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(dangling_doc));
        }

        let brace_doc = Doc::concat(vec![
            lbrace,
            Doc::indent(Doc::concat(inner)),
            Doc::Line,
            rbrace,
        ]);
        let group_doc = if force_break {
            Doc::group_breaking(brace_doc)
        } else {
            Doc::group(brace_doc)
        };
        Ok(Doc::concat(vec![
            kw,
            Doc::text(" "),
            name,
            generics,
            Doc::text(" "),
            group_doc,
            rbrace_eol,
        ]))
    }

    fn fmt_trait_def(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Trait)?;
        let name = self.expect_ident()?;
        let generics = if matches!(self.tok(), Token::LBracket) {
            self.fmt_generic_params()?
        } else {
            Doc::Empty
        };
        let lbrace = self.expect(&Token::LBrace)?;

        let mut members = Vec::new();
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !members.is_empty() && self.had_blank_before_cur();
            let leading = self.take_leading_own_line_comments_cur(members.is_empty());
            let m_doc = if matches!(self.tok(), Token::Type) {
                let tk = self.emit_tok();
                let aname = self.expect_ident()?;
                let semi = if matches!(self.tok(), Token::Semi) {
                    self.emit_tight_tok()
                } else {
                    Doc::Empty
                };
                Doc::concat(vec![leading, tk, Doc::text(" "), aname, semi])
            } else {
                let dk = self.expect(&Token::Def)?;
                let mname = self.expect_ident()?;
                let tparams = if matches!(self.tok(), Token::LBracket) {
                    self.fmt_generic_params()?
                } else {
                    Doc::Empty
                };
                let params = self.fmt_params_list()?;
                let ret = if matches!(self.tok(), Token::Arrow) {
                    let arrow = self.emit_tok();
                    let rty = self.fmt_type()?;
                    Doc::concat(vec![Doc::text(" "), arrow, Doc::text(" "), rty])
                } else {
                    Doc::Empty
                };
                let eff = self.fmt_effects_clause()?;
                let body = if matches!(self.tok(), Token::LBrace) {
                    let b = self.fmt_block()?;
                    Doc::concat(vec![Doc::text(" "), b])
                } else if matches!(self.tok(), Token::Semi) {
                    self.emit_tight_tok()
                } else {
                    Doc::Empty
                };
                Doc::concat(vec![
                    leading,
                    Doc::group(Doc::concat(vec![
                        dk,
                        Doc::text(" "),
                        mname,
                        tparams,
                        params,
                        ret,
                        eff,
                        body,
                    ])),
                ])
            };
            members.push((had_blank, m_doc));
        }

        self.finish_braced_item(
            Doc::concat(vec![kw, Doc::text(" "), name, generics, Doc::text(" ")]),
            lbrace,
            members,
        )
    }

    fn fmt_impl_def(&mut self) -> Result<Doc, String> {
        let mut header = vec![self.expect(&Token::Impl)?];
        if matches!(self.tok(), Token::LBracket) {
            header.push(self.fmt_generic_params()?);
        }
        header.push(Doc::text(" "));

        // First target/trait type (note: `impl Container[i64] for IntBox` or `impl Trait for list<i64>` or `impl Foo`).
        header.push(self.fmt_impl_type_or_trait_head()?);
        if matches!(self.tok(), Token::For) {
            header.push(Doc::text(" "));
            header.push(self.emit_tok());
            header.push(Doc::text(" "));
            header.push(self.fmt_type()?);
        }
        header.push(Doc::text(" "));

        let lbrace = self.expect(&Token::LBrace)?;
        let mut members = Vec::new();
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !members.is_empty() && self.had_blank_before_cur();
            let leading = self.take_leading_own_line_comments_cur(members.is_empty());
            let m_body = if matches!(self.tok(), Token::Type) {
                self.fmt_type_alias()?
            } else {
                let mut mparts = Vec::new();
                while matches!(self.tok(), Token::At) {
                    mparts.push(self.fmt_attribute()?);
                    mparts.push(Doc::HardLine);
                }
                if matches!(self.tok(), Token::Pub) {
                    mparts.push(self.emit_tok());
                    mparts.push(Doc::text(" "));
                }
                let d = self.fmt_fn_def()?;
                mparts.push(d);
                Doc::concat(mparts)
            };
            members.push((had_blank, Doc::concat(vec![leading, m_body])));
        }

        self.finish_braced_item(Doc::group(Doc::concat(header)), lbrace, members)
    }

    fn fmt_impl_type_or_trait_head(&mut self) -> Result<Doc, String> {
        let first = self.fmt_type()?;
        if matches!(self.tok(), Token::LBracket) {
            let args = self.fmt_delimited_list(
                &Token::LBracket,
                &Token::RBracket,
                false,
                DelimListStyle::NoTrailingComma,
                Self::fmt_type,
            )?;
            Ok(Doc::concat(vec![first, args]))
        } else {
            Ok(first)
        }
    }

    fn fmt_effect_def(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Effect)?;
        let name = self.expect_ident()?;
        let lbrace = self.expect(&Token::LBrace)?;

        let mut ops = Vec::new();
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !ops.is_empty() && self.had_blank_before_cur();
            let leading = self.take_leading_own_line_comments_cur(ops.is_empty());
            let dk = self.expect(&Token::Def)?;
            let op_name = self.expect_ident()?;
            let params = self.fmt_params_list()?;
            let arrow = self.expect(&Token::Arrow)?;
            let ret = self.fmt_type()?;
            let semi = if matches!(self.tok(), Token::Semi) {
                self.emit_tight_tok()
            } else {
                Doc::Empty
            };
            let op_doc = Doc::concat(vec![
                leading,
                Doc::group(Doc::concat(vec![
                    dk,
                    Doc::text(" "),
                    op_name,
                    params,
                    Doc::text(" "),
                    arrow,
                    Doc::text(" "),
                    ret,
                    semi,
                ])),
            ]);
            ops.push((had_blank, op_doc));
        }

        self.finish_braced_item(
            Doc::concat(vec![kw, Doc::text(" "), name, Doc::text(" ")]),
            lbrace,
            ops,
        )
    }

    fn fmt_model_def(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Model)?;
        let name = self.expect_ident()?;
        let lbrace = self.expect(&Token::LBrace)?;

        let mut entries = Vec::new();
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !entries.is_empty() && self.had_blank_before_cur();
            let leading = self.take_leading_own_line_comments_cur(entries.is_empty());
            let entry = match self.tok() {
                Token::Ident(s) if s == "input" => {
                    let ikw = self.emit_tok();
                    let iname = self.expect_ident()?;
                    let colon = self.expect(&Token::Colon)?;
                    let ty = self.fmt_type()?;
                    Doc::concat(vec![ikw, Doc::text(" "), iname, colon, Doc::text(" "), ty])
                }
                Token::Ident(s) if s == "layer" => {
                    let lkw = self.emit_tok();
                    let lname = self.expect_ident()?;
                    let op = self.expect_ident()?;
                    let params = if matches!(self.tok(), Token::LParen) {
                        self.fmt_delimited_list(
                            &Token::LParen,
                            &Token::RParen,
                            false,
                            DelimListStyle::TrailingCommaOnBreak,
                            |f| {
                                if matches!(f.tok(), Token::Ident(_))
                                    && matches!(f.peek(1), Token::Eq)
                                {
                                    let k = f.emit_tok();
                                    let eq = f.emit_tok();
                                    let v = f.fmt_expr()?;
                                    Ok(Doc::concat(vec![k, Doc::text(" "), eq, Doc::text(" "), v]))
                                } else {
                                    f.expect_ident()
                                }
                            },
                        )?
                    } else {
                        Doc::Empty
                    };
                    Doc::group(Doc::concat(vec![
                        lkw,
                        Doc::text(" "),
                        lname,
                        Doc::text(" "),
                        op,
                        params,
                    ]))
                }
                Token::Ident(s) if s == "output" => {
                    let okw = self.emit_tok();
                    let oname = self.expect_ident()?;
                    Doc::concat(vec![okw, Doc::text(" "), oname])
                }
                other => {
                    return Err(format!(
                        "formatter unexpected token `{other}` inside model body"
                    ));
                }
            };
            entries.push((had_blank, Doc::concat(vec![leading, entry])));
        }

        self.finish_braced_item(
            Doc::concat(vec![kw, Doc::text(" "), name, Doc::text(" ")]),
            lbrace,
            entries,
        )
    }

    fn fmt_mod_def(&mut self) -> Result<Doc, String> {
        let kw = self.expect(&Token::Mod)?;
        let name = self.expect_ident()?;
        let lbrace = self.expect(&Token::LBrace)?;

        let mut items = Vec::new();
        let mut prev_single = false;
        while !matches!(self.tok(), Token::RBrace | Token::Eof) {
            let had_blank = !items.is_empty() && self.had_blank_before_cur();
            let had_comments = !items.is_empty() && self.has_own_line_comments_before_cur();
            let leading = self.take_leading_own_line_comments_cur(items.is_empty());
            let item_doc = self.fmt_top_level_item(true)?;
            let is_single = crate::formatter::doc::renders_single_line(
                &item_doc,
                self.indent_width,
                self.max_line_width,
                self.indent_width,
            );
            let want_blank =
                !items.is_empty() && (had_blank || had_comments || !prev_single || !is_single);
            items.push((want_blank, Doc::concat(vec![leading, item_doc])));
            prev_single = is_single;
        }

        self.finish_braced_item(
            Doc::concat(vec![kw, Doc::text(" "), name, Doc::text(" ")]),
            lbrace,
            items,
        )
    }

    fn finish_braced_item(
        &mut self,
        header: Doc,
        lbrace: Doc,
        entries: Vec<(bool, Doc)>,
    ) -> Result<Doc, String> {
        if entries.is_empty() {
            if let Some(dangling) = self.take_dangling_comments(true) {
                let (rbrace, rbrace_eol) = self.expect_close_split_eol(&Token::RBrace)?;
                return Ok(Doc::concat(vec![
                    header,
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
            return Ok(Doc::concat(vec![header, lbrace, rbrace, rbrace_eol]));
        }

        let dangling = self.take_dangling_comments(false);
        let (rbrace, rbrace_eol) = self.expect_close_split_eol(&Token::RBrace)?;
        let mut body = Vec::new();
        for (i, (had_blank, entry)) in entries.into_iter().enumerate() {
            if i == 0 {
                body.push(Doc::HardLine);
            } else if had_blank {
                body.push(Doc::BlankLine);
            } else {
                body.push(Doc::HardLine);
            }
            body.push(entry);
        }
        if let Some(dangling_doc) = dangling {
            body.push(Doc::HardLine);
            body.push(eof_comments_trimmed_doc(dangling_doc));
        }
        Ok(Doc::concat(vec![
            header,
            lbrace,
            Doc::indent(Doc::concat(body)),
            Doc::HardLine,
            rbrace,
            rbrace_eol,
        ]))
    }
}
