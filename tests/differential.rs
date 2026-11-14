//! Differential testing across backends. A seeded generator writes programs
//! that combine features (records holding lists, lists of strings from
//! `split`, maps, options matched with `when`, capturing closures, fixed
//! arrays of numbers and strings, bounded loops, program state, calls between
//! generated functions) and every program runs in the interpreter and as a
//! native binary. The two must print the same thing, line for line.
//!
//! The `.iris` corpus tests one feature per file; the defects found while
//! building the RC2 projects lived in combinations of features, which is
//! what this generates.
//!
//! `IRIS_DIFF_SEEDS=from..to` runs other seeds (default 1..4);
//! `IRIS_DIFF_FUNCTIONS` sets functions per program (default 24). A failing
//! program is kept in target/difftest/ for reproduction.

use std::path::{Path, PathBuf};
use std::process::Command;

// ── A small deterministic generator ─────────────────────────────────────────

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    Int,
    Bool,
    Str,
    IntList,
    StrList,
    Rec,
    OptInt,
    Map,
    IntArr,
    StrArr,
    Fun,
    Float,
    RecList,
    Shape,
    Tuple,
}

#[derive(Clone)]
struct Var {
    name: String,
    ty: Ty,
    mutable: bool,
}

#[derive(Clone)]
struct Helper {
    name: String,
    params: Vec<Ty>,
    ret: Ty,
}

const WORDS: &[&str] = &[
    "",
    "a",
    "ab",
    "a,b",
    "x y",
    " pad ",
    "csv,row,3",
    "AbC",
    "zz,",
    "1,2,3",
    "hello world",
];

struct Gen {
    in_loop: bool,
    helper_ret: Option<Ty>,
    rng: Rng,
    scopes: Vec<Vec<Var>>,
    counter: usize,
    helpers: Vec<Helper>,
    state_ints: Vec<String>,
    state_strs: Vec<String>,
}

impl Gen {
    fn fresh(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{}", self.counter)
    }

    fn vars_of(&self, ty: Ty) -> Vec<Var> {
        self.scopes
            .iter()
            .flatten()
            .filter(|v| v.ty == ty)
            .cloned()
            .collect()
    }

    fn declare(&mut self, name: &str, ty: Ty, mutable: bool) {
        self.scopes.last_mut().unwrap().push(Var {
            name: name.to_owned(),
            ty,
            mutable,
        });
    }

    fn some_var(&mut self, ty: Ty) -> Option<String> {
        let vars = self.vars_of(ty);
        if vars.is_empty() {
            None
        } else {
            Some(self.rng.pick(&vars).name.clone())
        }
    }

    fn lit_str(&mut self) -> String {
        format!("\"{}\"", self.rng.pick(WORDS))
    }

    fn int(&mut self, depth: u32) -> String {
        if depth == 0 || self.rng.chance(25) {
            return match self.some_var(Ty::Int) {
                Some(v) if self.rng.chance(70) => v,
                _ => {
                    let n = self.rng.below(200) as i64 - 60;
                    if n < 0 {
                        format!("(0 - {})", -n)
                    } else {
                        n.to_string()
                    }
                }
            };
        }
        let d = depth - 1;
        match self.rng.below(23) {
            17 => {
                let op = *self.rng.pick(&["band", "bor", "bxor"]);
                format!("{op}({}, {})", self.int(d), self.int(d))
            }
            18 => format!("shr(abs({}), {})", self.int(d), self.rng.below(6)),
            19 => format!("(({} to i64) % 1000)", self.float(d)),
            20 => match self.some_var(Ty::RecList) {
                Some(l) => {
                    let i = self.int(d);
                    format!("(if list_len({l}) > 0 {{ list_get({l}, abs({i}) % list_len({l})).a }} else {{ 11 }})")
                }
                None => self.int(d),
            },
            21 => match self.some_var(Ty::Shape) {
                Some(sh) => {
                    let (r, l) = (self.fresh("rq"), self.fresh("lq"));
                    format!("(when {sh} {{ Shape.Dot => 1, Shape.Circle({r}) => {r}, Shape.Label({l}) => len({l}) }})")
                }
                None => self.int(d),
            },
            22 => match self.some_var(Ty::Tuple) {
                Some(t) => format!("{t}.0"),
                None => self.int(d),
            },
            0 => format!("(({} + {}) % 1000)", self.int(d), self.int(d)),
            1 => format!("(({} - {}) % 1000)", self.int(d), self.int(d)),
            2 => format!("(({} * {}) % 1000)", self.int(d), self.int(d)),
            3 => format!("({} / {})", self.int(d), 1 + self.rng.below(9)),
            4 => format!("len({})", self.string(d)),
            5 => {
                let list = self.int_list();
                let i = self.int(d);
                format!("(if list_len({list}) > 0 {{ list_get({list}, abs({i}) % list_len({list})) }} else {{ 7 }})")
            }
            6 => match self.some_var(Ty::Rec) {
                Some(r) => format!("{r}.a"),
                None => self.int(d),
            },
            7 => match self.some_var(Ty::OptInt) {
                Some(o) => {
                    let x = self.fresh("x");
                    format!("(when {o} {{ some({x}) => {x}, none => 3 }})")
                }
                None => self.int(d),
            },
            8 => match self.some_var(Ty::Map) {
                Some(m) => format!("map_get({m}, {}).unwrap_or(0)", self.string(d)),
                None => self.int(d),
            },
            9 => {
                let s = self.string(d);
                let i = self.int(d);
                format!("(if len({s}) > 0 {{ str_index({s}, abs({i}) % len({s})) }} else {{ 0 }})")
            }
            10 => {
                let calls: Vec<Helper> = self
                    .helpers
                    .iter()
                    .filter(|h| h.ret == Ty::Int)
                    .cloned()
                    .collect();
                if calls.is_empty() {
                    return self.int(d);
                }
                let h = self.rng.pick(&calls).clone();
                let args: Vec<String> = h.params.iter().map(|t| self.value(*t, d)).collect();
                format!("{}({})", h.name, args.join(", "))
            }
            11 => match self.some_var(Ty::Fun) {
                Some(f) => format!("{f}({})", self.int(d)),
                None => self.int(d),
            },
            12 => format!(
                "(if {} {{ {} }} else {{ {} }})",
                self.boolean(d),
                self.int(d),
                self.int(d)
            ),
            13 => match self.some_var(Ty::IntArr) {
                Some(a) => format!("{a}[abs({}) % 3]", self.int(d)),
                None => self.int(d),
            },
            14 => {
                let x = self.fresh("x");
                format!(
                    "(when parse_i64(to_str({})) {{ some({x}) => {x}, none => 0 }})",
                    self.int(d)
                )
            }
            15 => match self.some_var(Ty::StrList) {
                Some(l) => format!("list_len({l})"),
                None => self.int(d),
            },
            _ => match self.state_ints.is_empty() {
                false => self.rng.pick(&self.state_ints.clone()).clone(),
                true => self.int(d),
            },
        }
    }

    fn boolean(&mut self, depth: u32) -> String {
        let d = depth.saturating_sub(1);
        match self.rng.below(9) {
            8 => format!("({} < {})", self.float(d), self.float(d)),
            0 => format!("({} < {})", self.int(d), self.int(d)),
            1 => format!("({} == {})", self.int(d), self.int(d)),
            2 => format!("({} == {})", self.string(d), self.string(d)),
            3 => format!("contains({}, {})", self.string(d), self.lit_str()),
            4 => format!("starts_with({}, {})", self.string(d), self.lit_str()),
            5 => match self.some_var(Ty::OptInt) {
                Some(o) => format!("is_some({o})"),
                None => "true".into(),
            },
            6 => format!("({} && {})", self.boolean(d), self.boolean(d)),
            _ => format!("({} || {})", self.boolean(d), self.boolean(d)),
        }
    }

    fn string(&mut self, depth: u32) -> String {
        if depth == 0 || self.rng.chance(25) {
            return match self.some_var(Ty::Str) {
                Some(v) if self.rng.chance(70) => v,
                _ => self.lit_str(),
            };
        }
        let d = depth - 1;
        match self.rng.below(16) {
            13 => format!("to_str({})", self.float(d)),
            14 => match self.some_var(Ty::Tuple) {
                Some(t) => format!("{t}.1"),
                None => self.lit_str(),
            },
            15 => match self.some_var(Ty::Map) {
                Some(m) => format!("clip(join(map_keys({m}), \",\"))"),
                None => self.lit_str(),
            },
            0 => format!("clip({} + {})", self.string(d), self.string(d)),
            1 => format!("to_str({})", self.int(d)),
            2 => format!("to_upper({})", self.string(d)),
            3 => format!("trim({})", self.string(d)),
            4 => format!("str_replace({}, \",\", \";\")", self.string(d)),
            5 => format!("clip(repeat({}, abs({}) % 3))", self.string(d), self.int(d)),
            6 => match self.some_var(Ty::StrList) {
                Some(l) => format!("clip(join({l}, \"|\"))"),
                None => self.lit_str(),
            },
            7 => {
                let s = self.string(d);
                format!("(if len({s}) > 2 {{ slice({s}, 1, len({s}) - 1) }} else {{ {s} }})")
            }
            8 => match self.some_var(Ty::StrList) {
                Some(l) => {
                    let i = self.int(d);
                    format!("(if list_len({l}) > 0 {{ list_get({l}, abs({i}) % list_len({l})) }} else {{ \"none\" }})")
                }
                None => self.lit_str(),
            },
            9 => match self.some_var(Ty::Rec) {
                Some(r) => format!("{r}.b"),
                None => self.lit_str(),
            },
            10 => format!(
                "(if {} {{ {} }} else {{ {} }})",
                self.boolean(d),
                self.string(d),
                self.string(d)
            ),
            11 => match self.some_var(Ty::StrArr) {
                Some(a) => format!("{a}[abs({}) % 2]", self.int(d)),
                None => self.lit_str(),
            },
            _ => {
                let calls: Vec<Helper> = self
                    .helpers
                    .iter()
                    .filter(|h| h.ret == Ty::Str)
                    .cloned()
                    .collect();
                if calls.is_empty() {
                    return self.lit_str();
                }
                let h = self.rng.pick(&calls).clone();
                let args: Vec<String> = h.params.iter().map(|t| self.value(*t, d)).collect();
                format!("{}({})", h.name, args.join(", "))
            }
        }
    }

    fn float(&mut self, depth: u32) -> String {
        if depth == 0 || self.rng.chance(30) {
            return match self.some_var(Ty::Float) {
                Some(v) if self.rng.chance(60) => v,
                _ => {
                    let lit = *self.rng.pick(&[
                        "0.1",
                        "2.5",
                        "0.3333",
                        "1234.5678",
                        "0.0001",
                        "3.0",
                        "100.0",
                        "0.7",
                    ]);
                    if self.rng.chance(25) {
                        format!("(0.0 - {lit})")
                    } else {
                        lit.to_owned()
                    }
                }
            };
        }
        let d = depth - 1;
        match self.rng.below(10) {
            0 => format!("fclip({} + {})", self.float(d), self.float(d)),
            1 => format!("fclip({} - {})", self.float(d), self.float(d)),
            2 => format!("fclip({} * {})", self.float(d), self.float(d)),
            3 => format!(
                "({} / {})",
                self.float(d),
                self.rng.pick(&["2.0", "3.0", "7.0", "0.5"])
            ),
            4 => format!("sqrt(abs({}))", self.float(d)),
            5 => format!("floor({})", self.float(d)),
            6 => format!("({} to f64)", self.int(d)),
            7 => format!("sin({})", self.float(d)),
            8 => format!("cos({})", self.float(d)),
            _ => format!(
                "(if {} {{ {} }} else {{ {} }})",
                self.boolean(d),
                self.float(d),
                self.float(d)
            ),
        }
    }

    fn int_list(&mut self) -> String {
        if let Some(l) = self.some_var(Ty::IntList) {
            if self.rng.chance(70) {
                return l;
            }
        }
        match self.some_var(Ty::Rec) {
            Some(r) => format!("{r}.c"),
            None => self
                .some_var(Ty::IntList)
                .unwrap_or_else(|| "empty_ints()".into()),
        }
    }

    /// A value of `ty` for an argument.
    fn value(&mut self, ty: Ty, depth: u32) -> String {
        match ty {
            Ty::Int => self.int(depth),
            Ty::Str => self.string(depth),
            Ty::Bool => self.boolean(depth),
            Ty::IntList => self.int_list(),
            _ => unreachable!("helpers take i64, str, bool and list<i64>"),
        }
    }

    // ── Statements ───────────────────────────────────────────────────────────

    fn statement(&mut self, out: &mut String, indent: usize, depth: u32) {
        let pad = " ".repeat(indent);
        match self.rng.below(24) {
            18 => {
                let name = self.fresh("fl");
                let mutable = self.rng.chance(40);
                let e = self.float(2);
                out.push_str(&format!(
                    "{pad}{} {name} = {e};\n",
                    if mutable { "var" } else { "val" }
                ));
                self.declare(&name, Ty::Float, mutable);
            }
            19 => {
                let name = self.fresh("lr");
                out.push_str(&format!("{pad}val {name}: list<Rec> = list();\n"));
                for _ in 0..1 + self.rng.below(3) {
                    let (a, b, c) = (self.int(1), self.string(1), self.int_list());
                    out.push_str(&format!(
                        "{pad}push({name}, Rec {{ a: {a}, b: {b}, c: {c} }});\n"
                    ));
                }
                self.declare(&name, Ty::RecList, false);
            }
            20 => {
                let name = self.fresh("sh");
                let shape = match self.rng.below(3) {
                    0 => "Shape.Dot".to_owned(),
                    1 => format!("Shape.Circle({})", self.int(1)),
                    _ => format!("Shape.Label({})", self.string(1)),
                };
                out.push_str(&format!("{pad}val {name} = {shape};\n"));
                self.declare(&name, Ty::Shape, false);
            }
            21 => {
                let name = self.fresh("t");
                let (i, s) = (self.int(1), self.string(1));
                out.push_str(&format!("{pad}val {name} = ({i}, {s});\n"));
                self.declare(&name, Ty::Tuple, false);
            }
            22 if self.in_loop => {
                let b = self.boolean(1);
                out.push_str(&format!("{pad}if {b} {{ break }};\n"));
            }
            23 if self.helper_ret.is_some() && depth > 0 => {
                let b = self.boolean(1);
                let e = if self.helper_ret == Some(Ty::Str) {
                    format!("clip({})", self.string(1))
                } else {
                    self.int(1)
                };
                out.push_str(&format!("{pad}if {b} {{ return {e} }};\n"));
            }
            0 | 1 => {
                let name = self.fresh("v");
                let mutable = self.rng.chance(50);
                let e = self.int(2);
                out.push_str(&format!(
                    "{pad}{} {name} = {e};\n",
                    if mutable { "var" } else { "val" }
                ));
                self.declare(&name, Ty::Int, mutable);
            }
            2 => {
                let name = self.fresh("s");
                let mutable = self.rng.chance(50);
                let e = self.string(2);
                out.push_str(&format!(
                    "{pad}{} {name} = {e};\n",
                    if mutable { "var" } else { "val" }
                ));
                self.declare(&name, Ty::Str, mutable);
            }
            3 => {
                let vars: Vec<Var> = self
                    .scopes
                    .iter()
                    .flatten()
                    .filter(|v| v.mutable && matches!(v.ty, Ty::Int | Ty::Str | Ty::Float))
                    .cloned()
                    .collect();
                if let Some(v) = vars.get(self.rng.below(vars.len().max(1) as u64) as usize) {
                    let e = match v.ty {
                        Ty::Int => self.int(2),
                        Ty::Float => self.float(2),
                        _ => format!("clip({})", self.string(2)),
                    };
                    out.push_str(&format!("{pad}{} = {e};\n", v.name));
                }
            }
            4 => {
                let name = self.fresh("l");
                out.push_str(&format!("{pad}val {name}: list<i64> = list();\n"));
                for _ in 0..self.rng.below(4) {
                    let e = self.int(2);
                    out.push_str(&format!("{pad}push({name}, {e});\n"));
                }
                self.declare(&name, Ty::IntList, false);
            }
            5 => {
                let list = self.int_list();
                let i = self.int(1);
                let e = self.int(2);
                out.push_str(&format!("{pad}if list_len({list}) > 0 {{ list_set({list}, abs({i}) % list_len({list}), {e}) }};\n"));
            }
            6 => {
                let list = self.int_list();
                let e = self.int(2);
                out.push_str(&format!(
                    "{pad}if list_len({list}) < 12 {{ push({list}, {e}) }};\n"
                ));
            }
            7 => {
                let name = self.fresh("sl");
                let s = self.string(2);
                out.push_str(&format!("{pad}val {name} = split({s}, \",\");\n"));
                self.declare(&name, Ty::StrList, false);
            }
            8 => {
                let name = self.fresh("r");
                let (a, b, c) = (self.int(2), self.string(2), self.int_list());
                out.push_str(&format!(
                    "{pad}var {name} = Rec {{ a: {a}, b: {b}, c: {c} }};\n"
                ));
                self.declare(&name, Ty::Rec, true);
            }
            9 => {
                if let Some(r) = self.some_var(Ty::Rec) {
                    if self.rng.chance(50) {
                        let e = self.int(2);
                        out.push_str(&format!("{pad}{r}.a = {e};\n"));
                    } else {
                        let e = self.string(2);
                        out.push_str(&format!("{pad}{r}.b = clip({e});\n"));
                    }
                }
            }
            10 => {
                let name = self.fresh("o");
                let b = self.boolean(2);
                let e = self.int(2);
                out.push_str(&format!("{pad}var {name}: option<i64> = none();\n{pad}if {b} {{ {name} = some({e}) }};\n"));
                self.declare(&name, Ty::OptInt, true);
            }
            11 => {
                let name = self.fresh("m");
                out.push_str(&format!("{pad}val {name}: map<str, i64> = map();\n"));
                for _ in 0..1 + self.rng.below(3) {
                    let (k, v) = (self.string(1), self.int(2));
                    out.push_str(&format!("{pad}map_set({name}, {k}, {v});\n"));
                }
                self.declare(&name, Ty::Map, false);
            }
            12 if depth > 0 => {
                let counter = self.fresh("c");
                let bound = 1 + self.rng.below(4);
                out.push_str(&format!(
                    "{pad}var {counter} = 0;\n{pad}while {counter} < {bound} {{\n"
                ));
                self.scopes.push(Vec::new());
                self.declare(&counter, Ty::Int, false);
                let outer = self.in_loop;
                self.in_loop = true;
                // The counter steps first, so a `break` cannot skip it.
                out.push_str(&format!("{pad}    {counter} = {counter} + 1;\n"));
                for _ in 0..1 + self.rng.below(3) {
                    self.statement(out, indent + 4, depth - 1);
                }
                self.in_loop = outer;
                self.scopes.pop();
                out.push_str(&format!("{pad}}};\n"));
            }
            13 if depth > 0 => {
                let index = self.fresh("ix");
                let bound = 1 + self.rng.below(4);
                out.push_str(&format!("{pad}for {index} in 0..{bound} {{\n"));
                self.scopes.push(Vec::new());
                self.declare(&index, Ty::Int, false);
                let outer = self.in_loop;
                self.in_loop = true;
                for _ in 0..1 + self.rng.below(3) {
                    self.statement(out, indent + 4, depth - 1);
                }
                self.in_loop = outer;
                self.scopes.pop();
                out.push_str(&format!("{pad}}};\n"));
            }
            14 if depth > 0 => {
                let b = self.boolean(2);
                out.push_str(&format!("{pad}if {b} {{\n"));
                self.scopes.push(Vec::new());
                for _ in 0..1 + self.rng.below(2) {
                    self.statement(out, indent + 4, depth - 1);
                }
                self.scopes.pop();
                out.push_str(&format!("{pad}}};\n"));
            }
            15 => {
                let name = self.fresh("cl");
                let z = self.fresh("z");
                let captured = self.int(1);
                let k = 1 + self.rng.below(9);
                out.push_str(&format!(
                    "{pad}val {name} = |{z}: i64| ({z} * {k} + {captured}) % 1000;\n"
                ));
                self.declare(&name, Ty::Fun, false);
            }
            16 => {
                if self.rng.chance(50) {
                    let name = self.fresh("a");
                    let (x, y, z) = (self.int(1), self.int(1), self.int(1));
                    out.push_str(&format!("{pad}val {name} = [{x}, {y}, {z}];\n"));
                    self.declare(&name, Ty::IntArr, false);
                } else {
                    let name = self.fresh("b");
                    let (x, y) = (self.string(1), self.string(1));
                    out.push_str(&format!("{pad}val {name} = [{x}, {y}];\n"));
                    self.declare(&name, Ty::StrArr, false);
                }
            }
            _ => {
                if !self.state_ints.is_empty() && self.rng.chance(50) {
                    let g = self.rng.pick(&self.state_ints.clone()).clone();
                    let e = self.int(2);
                    out.push_str(&format!("{pad}{g} = ({g} + {e}) % 1000;\n"));
                } else if !self.state_strs.is_empty() {
                    let g = self.rng.pick(&self.state_strs.clone()).clone();
                    let e = self.string(1);
                    out.push_str(&format!("{pad}{g} = clip({g} + {e});\n"));
                }
            }
        }
    }

    /// One generated function; helpers take parameters and may be called later.
    fn function(&mut self, index: usize) -> String {
        let name = format!("g{index}");
        let helper = index > 0 && self.rng.chance(40);
        let mut params = Vec::new();
        let ret = if helper && self.rng.chance(40) {
            Ty::Str
        } else {
            Ty::Int
        };
        self.scopes = vec![Vec::new()];
        let mut signature = Vec::new();
        if helper {
            for p in 0..1 + self.rng.below(3) {
                let ty = *self.rng.pick(&[Ty::Int, Ty::Str, Ty::IntList, Ty::Bool]);
                let pname = format!("p{p}");
                signature.push(format!(
                    "{pname}: {}",
                    match ty {
                        Ty::Int => "i64",
                        Ty::Str => "str",
                        Ty::IntList => "list<i64>",
                        _ => "bool",
                    }
                ));
                self.declare(&pname, ty, false);
                params.push(ty);
            }
        }
        self.helper_ret = if helper { Some(ret) } else { None };
        self.in_loop = false;
        let mut body = String::new();
        for _ in 0..4 + self.rng.below(8) {
            self.statement(&mut body, 4, 2);
        }
        self.helper_ret = None;
        // Report every value in scope, and fold it into a checksum.
        let mut report = String::from("    var h = 0;\n");
        for var in self.scopes.iter().flatten().cloned().collect::<Vec<_>>() {
            let v = &var.name;
            match var.ty {
                Ty::Int => report.push_str(&format!("    h = (h * 31 + {v}) % 1000003;\n")),
                Ty::Bool => report.push_str(&format!("    h = (h * 31 + (if {v} {{ 1 }} else {{ 0 }})) % 1000003;\n")),
                Ty::Str => report.push_str(&format!("    println(\"{name} {v}=[\" + {v} + \"]\");\n")),
                Ty::IntList => report.push_str(&format!(
                    "    for e in 0..list_len({v}) {{ h = (h * 31 + list_get({v}, e)) % 1000003 }};\n"
                )),
                Ty::StrList => report.push_str(&format!("    println(\"{name} {v}=[\" + join({v}, \"|\") + \"]\");\n")),
                Ty::Rec => report.push_str(&format!(
                    "    println(\"{name} {v}=\" + to_str({v}.a) + \",\" + {v}.b + \",\" + to_str(list_len({v}.c)));\n"
                )),
                Ty::OptInt => report.push_str(&format!("    h = (h * 31 + (when {v} {{ some(q) => q, none => 99 }})) % 1000003;\n")),
                Ty::Map => report.push_str(&format!("    h = (h * 31 + map_len({v})) % 1000003;\n")),
                Ty::IntArr => report.push_str(&format!("    h = (h * 31 + {v}[0] + {v}[1] + {v}[2]) % 1000003;\n")),
                Ty::StrArr => report.push_str(&format!("    println(\"{name} {v}=\" + {v}[0] + \"/\" + {v}[1]);\n")),
                Ty::Fun => report.push_str(&format!("    h = (h * 31 + {v}(5)) % 1000003;\n")),
                Ty::Float => report.push_str(&format!("    println(\"{name} {v}=\" + to_str({v}));\n")),
                Ty::RecList => report.push_str(&format!(
                    "    for e in 0..list_len({v}) {{ println(\"{name} {v}=\" + to_str(list_get({v}, e).a) + list_get({v}, e).b) }};\n"
                )),
                Ty::Shape => report.push_str(&format!(
                    "    h = (h * 31 + (when {v} {{ Shape.Dot => 1, Shape.Circle(rr) => rr, Shape.Label(ll) => len(ll) + 2 }})) % 1000003;\n"
                )),
                Ty::Tuple => report.push_str(&format!("    println(\"{name} {v}=\" + to_str({v}.0) + {v}.1);\n")),
            }
        }
        let tail = if ret == Ty::Str {
            "    clip(to_str(h) + \":\" + \"done\")\n"
        } else {
            "    h\n"
        };
        let ret_name = if ret == Ty::Str { "str" } else { "i64" };
        if helper {
            self.helpers.push(Helper {
                name: name.clone(),
                params: params.clone(),
                ret,
            });
        }
        format!(
            "def {name}({}) -> {ret_name} {{\n{body}{report}{tail}}}\n\n",
            signature.join(", ")
        )
    }
}

fn program(seed: u64, functions: usize) -> String {
    let mut g = Gen {
        in_loop: false,
        helper_ret: None,
        rng: Rng::new(seed),
        scopes: Vec::new(),
        counter: 0,
        helpers: Vec::new(),
        state_ints: Vec::new(),
        state_strs: Vec::new(),
    };
    let mut out = format!("// Generated by tests/differential.rs, seed {seed}.\n\n");
    out.push_str("record Rec { a: i64, b: str, c: list<i64> }\n\n");
    out.push_str("choice Shape { Dot, Circle(i64), Label(str) }\n\n");
    for i in 0..1 + g.rng.below(2) {
        out.push_str(&format!("var gi{i}: i64 = {}\n", g.rng.below(50)));
        g.state_ints.push(format!("gi{i}"));
    }
    out.push_str("var gs0: str = \"state\"\n\n");
    g.state_strs.push("gs0".into());
    out.push_str(
        "def clip(s: str) -> str {\n    if len(s) > 40 { slice(s, 0, 40) } else { s }\n}\n\n",
    );
    out.push_str("def fclip(x: f64) -> f64 {\n    if x > 1000000.0 || x < 0.0 - 1000000.0 { 0.5 } else { x }\n}\n\n");
    out.push_str("def empty_ints() -> list<i64> {\n    val l: list<i64> = list();\n    l\n}\n\n");
    let mut calls = String::new();
    for i in 0..functions {
        out.push_str(&g.function(i));
        // Only the zero-argument functions are called from main; helpers are
        // reached through them.
        if !g.helpers.iter().any(|h| h.name == format!("g{i}")) {
            calls.push_str(&format!("    println(\"g{i} -> \" + to_str(g{i}()));\n"));
        }
    }
    out.push_str(&format!(
        "def main() -> i64 {{\n{calls}    println(\"state \" + to_str(gi0) + \" \" + gs0);\n    return 0\n}}\n"
    ));
    out
}

// ── Running ──────────────────────────────────────────────────────────────────

fn run_interpreter(path: &Path) -> Result<String, String> {
    let out = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "eval", "--max-steps", "100000000"])
        .arg(path)
        .env("IRIS_FORCE_INTERP", "1")
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    if !out.status.success() {
        return Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)));
    }
    // `--emit eval` ends with main's return value.
    Ok(stdout.strip_suffix("0\n").unwrap_or(&stdout).to_owned())
}

fn run_native(path: &Path, exe: &Path) -> Result<String, String> {
    let build = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "binary", "-o"])
        .arg(exe)
        .arg(path)
        .output()
        .map_err(|e| e.to_string())?;
    if !build.status.success() {
        return Err(format!("build: {}", String::from_utf8_lossy(&build.stderr)));
    }
    let out = Command::new(exe).output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    if !out.status.success() {
        return Err(format!(
            "exit {:?}: {stdout}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(stdout)
}

fn seeds() -> std::ops::Range<u64> {
    std::env::var("IRIS_DIFF_SEEDS")
        .ok()
        .and_then(|s| {
            let (a, b) = s.split_once("..")?;
            Some(a.parse().ok()?..b.parse().ok()?)
        })
        .unwrap_or(1..4)
}

#[test]
fn generated_programs_print_the_same_on_both_backends() {
    let functions: usize = std::env::var("IRIS_DIFF_FUNCTIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/difftest");
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    for seed in seeds() {
        let source = program(seed, functions);
        let path = dir.join(format!("seed{seed}.iris"));
        std::fs::write(&path, &source).unwrap();
        let exe = dir.join(format!("seed{seed}{}", std::env::consts::EXE_SUFFIX));
        let interpreted = run_interpreter(&path);
        let native = run_native(&path, &exe);
        let _ = std::fs::remove_file(&exe);
        match (interpreted, native) {
            (Ok(a), Ok(b)) if a == b => {
                let _ = std::fs::remove_file(&path);
            }
            (Ok(a), Ok(b)) => {
                let line = a
                    .lines()
                    .zip(b.lines())
                    .position(|(x, y)| x != y)
                    .unwrap_or(a.lines().count().min(b.lines().count()));
                failures.push(format!(
                    "seed {seed}: outputs differ at line {}:\n  interpreter: {:?}\n  native:      {:?}\n  program: {}",
                    line + 1,
                    a.lines().nth(line),
                    b.lines().nth(line),
                    path.display()
                ));
            }
            (a, b) => failures.push(format!(
                "seed {seed}: a backend failed\n  interpreter: {}\n  native: {}\n  program: {}",
                a.err()
                    .unwrap_or_else(|| "ok".into())
                    .lines()
                    .take(6)
                    .collect::<Vec<_>>()
                    .join("\n    "),
                b.err()
                    .unwrap_or_else(|| "ok".into())
                    .lines()
                    .take(6)
                    .collect::<Vec<_>>()
                    .join("\n    "),
                path.display()
            )),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
