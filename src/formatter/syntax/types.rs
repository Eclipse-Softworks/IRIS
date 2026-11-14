use crate::formatter::doc::Doc;
use crate::formatter::syntax::{DelimListStyle, Formatter};
use crate::parser::Token;

impl<'a> Formatter<'a> {
    pub fn fmt_type(&mut self) -> Result<Doc, String> {
        match self.tok().clone() {
            Token::F32
            | Token::F64
            | Token::I32
            | Token::I64
            | Token::Bool
            | Token::I8
            | Token::U8
            | Token::U32
            | Token::U64
            | Token::Usize
            | Token::Str
            | Token::IntLit(_) => Ok(self.emit_tok()),
            Token::Tensor => self.fmt_tensor_type(),
            Token::LBracket => self.fmt_bracket_type(),
            Token::Dyn => {
                let d = self.emit_tok();
                let tr = self.expect_ident()?;
                Ok(Doc::concat(vec![d, Doc::text(" "), tr]))
            }
            Token::Amp => {
                let amp = self.emit_tok();
                if matches!(self.tok(), Token::Ident(s) if s == "mut") {
                    let mut_tok = self.emit_tok();
                    let inner = self.fmt_type()?;
                    Ok(Doc::concat(vec![amp, mut_tok, Doc::text(" "), inner]))
                } else {
                    let inner = self.fmt_type()?;
                    Ok(Doc::concat(vec![amp, inner]))
                }
            }
            Token::LParen => self.fmt_paren_or_fn_type(),
            Token::Pipe => self.fmt_pipe_closure_type(false),
            Token::PipePipe => self.fmt_pipe_closure_type(true),
            Token::With => {
                let w = self.emit_tok();
                let first = self.expect_ident()?;
                let mut parts = vec![w, Doc::text(" "), first];
                while matches!(self.tok(), Token::Comma) {
                    parts.push(self.emit_tight_tok());
                    parts.push(Doc::text(" "));
                    parts.push(self.expect_ident()?);
                }
                self.mark_forbid_trailing_comma();
                Ok(Doc::concat(parts))
            }
            _ if Self::is_ident_tok(self.tok()) => self.fmt_named_or_generic_type(),
            other => Err(format!(
                "formatter expected type, found `{other}` at token {}",
                self.pos
            )),
        }
    }

    fn fmt_tensor_type(&mut self) -> Result<Doc, String> {
        let tensor_kw = self.emit_tok();
        let langle = self.expect(&Token::LAngle)?;
        let dtype = self.emit_tok();
        let comma = self.expect(&Token::Comma)?;
        let dims = self.fmt_delimited_list(
            &Token::LBracket,
            &Token::RBracket,
            false,
            DelimListStyle::NoTrailingComma,
            |f| match f.tok() {
                Token::IntLit(_) => Ok(f.emit_tok()),
                _ => f.expect_ident(),
            },
        )?;
        let rangle = self.expect(&Token::RAngle)?;
        Ok(Doc::group(Doc::concat(vec![
            tensor_kw,
            langle,
            Doc::indent(Doc::concat(vec![
                Doc::SoftLine,
                dtype,
                comma,
                Doc::Line,
                dims,
            ])),
            Doc::SoftLine,
            rangle,
        ])))
    }

    fn fmt_bracket_type(&mut self) -> Result<Doc, String> {
        let lb = self.expect(&Token::LBracket)?;
        let elem = self.fmt_type()?;
        if matches!(self.tok(), Token::Semi) {
            let semi = self.emit_tight_tok();
            let len_expr = self.fmt_expr()?;
            let rb = self.expect(&Token::RBracket)?;
            Ok(Doc::group(Doc::concat(vec![
                lb,
                elem,
                semi,
                Doc::Line,
                len_expr,
                rb,
            ])))
        } else {
            let rb = self.expect(&Token::RBracket)?;
            Ok(Doc::concat(vec![lb, elem, rb]))
        }
    }

    fn fmt_paren_or_fn_type(&mut self) -> Result<Doc, String> {
        let params = self.fmt_delimited_list(
            &Token::LParen,
            &Token::RParen,
            false,
            DelimListStyle::Tuple,
            Self::fmt_type,
        )?;
        if matches!(self.tok(), Token::Arrow) {
            let arrow = self.emit_tok();
            let ret = self.fmt_type()?;
            let effects = self.fmt_type_effects()?;
            Ok(Doc::concat(vec![
                params,
                Doc::text(" "),
                arrow,
                Doc::text(" "),
                ret,
                effects,
            ]))
        } else {
            Ok(params)
        }
    }

    fn fmt_pipe_closure_type(&mut self, zero_params: bool) -> Result<Doc, String> {
        let sig = if zero_params {
            self.expect(&Token::PipePipe)?
        } else {
            self.fmt_delimited_list(
                &Token::Pipe,
                &Token::Pipe,
                false,
                DelimListStyle::NoTrailingComma,
                Self::fmt_type,
            )?
        };
        let arrow = self.expect(&Token::Arrow)?;
        let ret = self.fmt_type()?;
        let effects = self.fmt_type_effects()?;
        Ok(Doc::concat(vec![
            sig,
            Doc::text(" "),
            arrow,
            Doc::text(" "),
            ret,
            effects,
        ]))
    }

    fn fmt_type_effects(&mut self) -> Result<Doc, String> {
        if !matches!(self.tok(), Token::Effect) {
            return Ok(Doc::Empty);
        }
        let eff_kw = self.emit_tok();
        let first = self.expect_ident()?;
        let mut parts = vec![Doc::text(" "), eff_kw, Doc::text(" "), first];
        while matches!(self.tok(), Token::Comma) {
            parts.push(self.emit_tight_tok());
            parts.push(Doc::Line);
            parts.push(self.expect_ident()?);
        }
        self.mark_forbid_trailing_comma();
        Ok(Doc::group(Doc::concat(parts)))
    }

    fn fmt_named_or_generic_type(&mut self) -> Result<Doc, String> {
        let base = self.expect_ident()?;
        if matches!(self.tok(), Token::LAngle) {
            let args = self.fmt_delimited_list(
                &Token::LAngle,
                &Token::RAngle,
                false,
                DelimListStyle::NoTrailingComma,
                Self::fmt_type,
            )?;
            return Ok(Doc::concat(vec![base, args]));
        }
        if matches!(self.tok(), Token::DoubleColon) {
            let dc = self.emit_tight_tok();
            let assoc = self.expect_ident()?;
            return Ok(Doc::concat(vec![base, dc, assoc]));
        }
        let mut parts = vec![base];
        while matches!(self.tok(), Token::Dot) {
            parts.push(self.emit_tight_tok());
            parts.push(self.expect_ident()?);
        }
        Ok(Doc::concat(parts))
    }
}
