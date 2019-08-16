use failure::err_msg;
use failure::Error;
use std::iter::Peekable;
use std::slice::Iter;
use std::str::Chars;

type CharIter<'a> = Peekable<Chars<'a>>;
type TokenIter<'a> = Peekable<Iter<'a, Token>>;

#[derive(Debug, Clone)]
enum Token {
  And,
  Or,
  Not,
  Pal,
  Par,
  Wild,
  Name(String),
}

#[derive(Debug, Clone)]
pub enum Expr {
  And(Box<Expr>, Box<Expr>),
  Or(Box<Expr>, Box<Expr>),
  Not(Box<Expr>),
  Group(Box<Expr>),
  Atom(String),
  Wild,
}

impl Expr {
  pub fn apply(&self, to: &[String]) -> bool {
    match self {
      Expr::And(x, y) => x.apply(to) && y.apply(to),
      Expr::Or(x, y) => x.apply(to) || y.apply(to),
      Expr::Not(x) => !x.apply(to),
      Expr::Group(x) => x.apply(to),
      Expr::Atom(x) => to.contains(x),
      Expr::Wild => true,
    }
  }
}

pub fn compile(expr: &str) -> Result<Expr, Error> {
  let tokens = lex(expr);
  parse(&tokens)
}

fn parse(tokens: &[Token]) -> Result<Expr, Error> {
  let mut it: TokenIter = tokens.iter().peekable();
  let expr = parse_expr(&mut it)?;
  match it.next() {
    Some(_) => Err(err_msg("Parse error; expected end of expression")),
    None => Ok(expr),
  }
}

fn parse_expr(it: &mut TokenIter) -> Result<Expr, Error> {
  parse_or(it)
}

fn parse_or(it: &mut TokenIter) -> Result<Expr, Error> {
  let lhs = parse_and(it)?;

  if let Some(Token::Or) = it.peek() {
    it.next();
    let rhs = parse_expr(it)?;
    Ok(Expr::Or(lhs.into(), rhs.into()))
  } else {
    Ok(lhs)
  }
}

fn parse_and(it: &mut TokenIter) -> Result<Expr, Error> {
  let lhs = parse_not(it)?;

  if let Some(Token::And) = it.peek() {
    it.next();
    let rhs = parse_expr(it)?;
    Ok(Expr::And(lhs.into(), rhs.into()))
  } else {
    Ok(lhs)
  }
}

fn parse_not(it: &mut TokenIter) -> Result<Expr, Error> {
  if let Some(Token::Not) = it.peek() {
    it.next();
    let inner = parse_atom(it)?;
    Ok(Expr::Not(inner.into()))
  } else {
    parse_atom(it)
  }
}

fn parse_atom(it: &mut TokenIter) -> Result<Expr, Error> {
  match it.next() {
    Some(Token::Pal) => {
      let inner = parse_expr(it)?;
      if let Some(Token::Par) = it.next() {
        Ok(Expr::Group(inner.into()))
      } else {
        Err(err_msg("Parse error; expected close parenthesis"))
      }
    }
    Some(Token::Name(x)) => Ok(Expr::Atom(x.clone())),
    Some(Token::Wild) => Ok(Expr::Wild),
    Some(_) => Err(err_msg("Parse error; expected name or open parenthesis")),
    None => Err(err_msg("Parse error; unexpected end of expression")),
  }
}

fn lex(expr: &str) -> Vec<Token> {
  let mut tokens = vec![];
  let mut it: CharIter = expr.chars().peekable();

  while let Some(c) = it.next() {
    let x = match c {
      'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => Some(lex_name(c, &mut it)),
      '+' => Some(Token::And),
      '|' => Some(Token::Or),
      '*' | '?' => Some(Token::Wild),
      '~' => Some(Token::Not),
      '(' => Some(Token::Pal),
      ')' => Some(Token::Par),
      ' ' | '\t' | '\n' => None,
      _ => None,
    };
    if let Some(token) = x {
      tokens.push(token);
    }
  }

  tokens
}

fn lex_name(first: char, it: &mut CharIter) -> Token {
  let mut name = String::new();
  name.push(first);

  while let Some(&c) = it.peek() {
    match c {
      'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => {
        it.next();
        name.push(c);
      }
      _ => break,
    }
  }

  Token::Name(name)
}
