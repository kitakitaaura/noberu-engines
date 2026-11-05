//! Script expressions: C-like integer arithmetic over variables.
//!
//! `#N` is variable N, `#N[e]` is variable N+e. Operators seen in Little
//! Busters! EE: `|| && | & == != < <= > >= + - * / %`, unary `-` and `!`.

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(i32),
    Var(u16),
    Index(u16, Box<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(Bin, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bin {
    Or,
    And,
    BitOr,
    BitAnd,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl Bin {
    fn prec(self) -> u8 {
        use Bin::*;
        match self {
            Or => 1,
            And => 2,
            BitOr => 3,
            BitAnd => 4,
            Eq | Ne => 5,
            Lt | Le | Gt | Ge => 6,
            Add | Sub => 7,
            Mul | Div | Mod => 8,
        }
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(t.as_bytes()) {
            self.i += t.len();
            true
        } else {
            false
        }
    }
    fn num(&mut self) -> Result<i64, String> {
        self.ws();
        let st = self.i;
        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[st..self.i])
            .unwrap()
            .parse()
            .map_err(|_| format!("number expected at {st}"))
    }
    fn binop(&mut self) -> Option<(Bin, usize)> {
        self.ws();
        let r = &self.s[self.i..];
        let two = |a: &[u8]| r.starts_with(a);
        Some(if two(b"||") {
            (Bin::Or, 2)
        } else if two(b"&&") {
            (Bin::And, 2)
        } else if two(b"==") {
            (Bin::Eq, 2)
        } else if two(b"!=") {
            (Bin::Ne, 2)
        } else if two(b"<=") {
            (Bin::Le, 2)
        } else if two(b">=") {
            (Bin::Ge, 2)
        } else {
            match r.first()? {
                b'|' => (Bin::BitOr, 1),
                b'&' => (Bin::BitAnd, 1),
                b'<' => (Bin::Lt, 1),
                b'>' => (Bin::Gt, 1),
                b'+' => (Bin::Add, 1),
                b'-' => (Bin::Sub, 1),
                b'*' => (Bin::Mul, 1),
                b'/' => (Bin::Div, 1),
                b'%' => (Bin::Mod, 1),
                _ => return None,
            }
        })
    }
    fn expr(&mut self, min: u8) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        while let Some((op, len)) = self.binop() {
            if op.prec() < min {
                break;
            }
            self.i += len;
            let rhs = self.expr(op.prec() + 1)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat("-") {
            return Ok(match self.unary()? {
                Expr::Num(n) => Expr::Num(n.wrapping_neg()),
                e => Expr::Neg(Box::new(e)),
            });
        }
        if self.eat("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat("(") {
            let e = self.expr(0)?;
            if !self.eat(")") {
                return Err(format!("')' expected at {}", self.i));
            }
            return Ok(e);
        }
        if self.eat("#") {
            let n = self.num()?;
            let n = u16::try_from(n).map_err(|_| format!("variable {n} out of range"))?;
            if self.eat("[") {
                let i = self.expr(0)?;
                if !self.eat("]") {
                    return Err(format!("']' expected at {}", self.i));
                }
                return Ok(Expr::Index(n, Box::new(i)));
            }
            return Ok(Expr::Var(n));
        }
        match self.peek() {
            Some(c) if c.is_ascii_digit() => Ok(Expr::Num(self.num()? as i32)),
            _ => Err(format!("unexpected input at {}", self.i)),
        }
    }
}

pub fn parse(s: &str) -> Result<Expr, String> {
    let mut p = P { s: s.as_bytes(), i: 0 };
    let e = p.expr(0).map_err(|e| format!("{s:?}: {e}"))?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("{s:?}: trailing input at {}", p.i));
    }
    Ok(e)
}

/// Variable number for `#base[index]`.
pub fn index_var(base: u16, index: i32) -> u16 {
    (base as i32).wrapping_add(index) as u16
}

impl Expr {
    pub fn eval(&self, vars: &dyn Fn(u16) -> i32) -> i32 {
        match self {
            Expr::Num(n) => *n,
            Expr::Var(v) => vars(*v),
            Expr::Index(b, i) => vars(index_var(*b, i.eval(vars))),
            Expr::Neg(e) => e.eval(vars).wrapping_neg(),
            Expr::Not(e) => (e.eval(vars) == 0) as i32,
            Expr::Bin(op, a, b) => {
                let x = a.eval(vars);
                // short-circuit like C
                match op {
                    Bin::And if x == 0 => return 0,
                    Bin::Or if x != 0 => return 1,
                    _ => {}
                }
                let y = b.eval(vars);
                match op {
                    Bin::Or | Bin::And => (y != 0) as i32,
                    Bin::BitOr => x | y,
                    Bin::BitAnd => x & y,
                    Bin::Eq => (x == y) as i32,
                    Bin::Ne => (x != y) as i32,
                    Bin::Lt => (x < y) as i32,
                    Bin::Le => (x <= y) as i32,
                    Bin::Gt => (x > y) as i32,
                    Bin::Ge => (x >= y) as i32,
                    Bin::Add => x.wrapping_add(y),
                    Bin::Sub => x.wrapping_sub(y),
                    Bin::Mul => x.wrapping_mul(y),
                    Bin::Div => x.checked_div(y).unwrap_or(0),
                    Bin::Mod => x.checked_rem(y).unwrap_or(0),
                }
            }
        }
    }

    /// Every variable this expression reads directly (indexed reads give the base).
    pub fn vars(&self, out: &mut Vec<u16>) {
        match self {
            Expr::Num(_) => {}
            Expr::Var(v) => out.push(*v),
            Expr::Index(b, i) => {
                out.push(*b);
                i.vars(out);
            }
            Expr::Neg(e) | Expr::Not(e) => e.vars(out),
            Expr::Bin(_, a, b) => {
                a.vars(out);
                b.vars(out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str, f: &dyn Fn(u16) -> i32) -> i32 {
        parse(s).unwrap().eval(f)
    }

    #[test]
    fn precedence_and_vars() {
        let v = |n: u16| match n {
            1 => 3,
            2 => 5,
            12 => 7,
            _ => 0,
        };
        assert_eq!(ev("10*#1", &v), 30);
        assert_eq!(ev("(#1+#2)*2+1", &v), 17);
        assert_eq!(ev("((#1==3)&&(#2==5))", &v), 1);
        assert_eq!(ev("(#1==-3)", &v), 0);
        assert_eq!(ev("#10[#2-3]", &v), 7);
        assert_eq!(ev("(#1-#2)*10/#2*#1/2/1", &v), -6);
        assert_eq!(ev("((#20001)==0)", &v), 1);
        assert_eq!(ev("#1&2", &v), 2);
        assert_eq!(ev("(#1<#2-1)", &v), 1);
        assert_eq!(ev("5/0", &v), 0);
        assert!(parse("(#1==").is_err());
    }
}
