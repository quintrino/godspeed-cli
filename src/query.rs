//! The search query language, for example
//! `label:Low due<=3h "tax return" | !has:notes sort:due limit:20`.
//!
//! This follows Godspeed's search operators guide. A query is parsed into an expression that
//! picks tasks, plus presentation settings (`sort:`, `group:`, `limit:`). Relative dates
//! (`today`, `3h`) are turned into real moments when the query is parsed, so the same query
//! value always means the same thing.

use crate::dates::{self, Kind};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    Title,
    Notes,
    Attachment,
    List,
    Folder,
    /// `in:`: a list or a folder.
    In,
    Label,
    LabelGroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    Due,
    FirstDue,
    Snoozed,
    Start,
    Created,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateTest {
    Cmp(Cmp, NaiveDateTime),
    /// From the first moment to the last, both included.
    Within(NaiveDateTime, NaiveDateTime),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumField {
    Duration,
    SnoozeCount,
    IndentLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trait {
    Notes,
    Attachment,
    Label,
    Repeat,
    Due,
    Snooze,
    Start,
    DueTime,
    StartTime,
    Bookmark,
    Email,
    Duration,
    /// Every task.
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not in Godspeed's guide: completed tasks, for searching the local copy.
    Complete,
    Available,
    Uncleared,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Matches every task.
    All,
    /// A word, matched anywhere in the title, notes or attachment names.
    Text(String),
    Phrase(String),
    Field(TextField, String),
    Date(DateField, DateTest),
    Number(NumField, Cmp, i64),
    Has(Trait),
    Is(State),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Title,
    List,
    Label,
    Due,
    FirstDue,
    Snoozed,
    Start,
    Created,
    Modified,
    Duration,
    SnoozeCount,
    IndentLevel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortSpec {
    pub field: SortField,
    pub descending: bool,
    /// Names or ids to put first, in this order (`sort:label:High|Medium|Low`).
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupField {
    List,
    Label,
    LabelGroup,
    Due,
    FirstDue,
    Snoozed,
    Start,
    DueOrStart,
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSpec {
    pub field: GroupField,
    /// Names or ids of the groups to make, in order (`group:list:Work|Home`).
    pub targets: Vec<String>,
    /// `...rest` was among the targets: tasks matching none of them are grouped normally
    /// instead of going into "None".
    pub rest: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub expr: Expr,
    /// Whether completed tasks are part of the search. Set by `is:complete` and `is:uncleared`.
    pub include_completed: bool,
    pub sorts: Vec<SortSpec>,
    pub group: Option<GroupSpec>,
    pub limit: Option<usize>,
    /// Whether a task with several lists, labels or label groups shows up under each when
    /// grouping, instead of just the first.
    pub duplicates: bool,
}

const SORT_FIELDS: &str =
    "title, list, label, due, first-due, snoozed, start, created, modified, duration, snooze-count, indent-level";
const GROUP_FIELDS: &str = "list, label, label-group, due, first-due, snoozed, start, due-or-start, created";

pub fn parse(args: &[String], now: NaiveDateTime) -> Result<Query, String> {
    let tokens = tokenize(&args.join(" "))?;
    let mut query = Query {
        expr: Expr::All,
        include_completed: false,
        sorts: Vec::new(),
        group: None,
        limit: None,
        duplicates: false,
    };

    let mut predicate_tokens = Vec::new();
    for token in tokens {
        if let Token::Atom(atom) = &token {
            if apply_presentation(atom, &mut query)? {
                continue;
            }
            if atom == "is:complete" || atom == "is:uncleared" {
                query.include_completed = true;
            }
        }
        predicate_tokens.push(token);
    }

    if !predicate_tokens.is_empty() {
        query.expr = Parser { tokens: predicate_tokens, index: 0, now }.parse()?;
    }
    Ok(query)
}

/// Handles `sort:`, `sort-asc:`, `sort-desc:`, `group:`, `limit:`, `enable:` and `disable:`.
/// Returns whether the atom was one of those.
fn apply_presentation(atom: &str, query: &mut Query) -> Result<bool, String> {
    for (prefix, descending) in [("sort-desc:", true), ("sort-asc:", false), ("sort:", false)] {
        if let Some(rest) = atom.strip_prefix(prefix) {
            let (name, targets) = split_targets(rest);
            let field = sort_field(&name)
                .ok_or_else(|| format!("Unknown sort field '{name}' (use one of: {SORT_FIELDS})"))?;
            query.sorts.push(SortSpec { field, descending, targets });
            return Ok(true);
        }
    }
    if let Some(rest) = atom.strip_prefix("group:") {
        let (name, mut targets) = split_targets(rest);
        let field = group_field(&name)
            .ok_or_else(|| format!("Unknown group field '{name}' (use one of: {GROUP_FIELDS})"))?;
        let rest = targets.iter().any(|t| t == "...rest");
        targets.retain(|t| t != "...rest");
        query.group = Some(GroupSpec { field, targets, rest });
        return Ok(true);
    }
    if let Some(value) = atom.strip_prefix("limit:") {
        query.limit = Some(value.parse().map_err(|_| format!("Invalid limit '{value}' (use a whole number)"))?);
        return Ok(true);
    }
    for (prefix, enabled) in [("enable:", true), ("disable:", false)] {
        if let Some(flag) = atom.strip_prefix(prefix) {
            return match flag {
                "duplicates" => {
                    query.duplicates = enabled;
                    Ok(true)
                }
                // Results are always a flat list here, so there is nothing to nest or flatten.
                "nested" => Ok(true),
                other => Err(format!("Unknown option '{prefix}{other}' (use enable: or disable: with nested or duplicates)")),
            };
        }
    }
    Ok(false)
}

fn split_targets(rest: &str) -> (String, Vec<String>) {
    match rest.split_once(':') {
        Some((field, targets)) => (
            field.to_string(),
            targets.split('|').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect(),
        ),
        None => (rest.to_string(), Vec::new()),
    }
}

fn sort_field(name: &str) -> Option<SortField> {
    Some(match name {
        "title" => SortField::Title,
        "list" => SortField::List,
        "label" => SortField::Label,
        "due" => SortField::Due,
        "first-due" => SortField::FirstDue,
        "snoozed" => SortField::Snoozed,
        "start" => SortField::Start,
        "created" => SortField::Created,
        "modified" | "updated" => SortField::Modified,
        "duration" => SortField::Duration,
        "snooze-count" => SortField::SnoozeCount,
        "indent-level" => SortField::IndentLevel,
        _ => return None,
    })
}

fn group_field(name: &str) -> Option<GroupField> {
    Some(match name {
        "list" => GroupField::List,
        "label" => GroupField::Label,
        "label-group" => GroupField::LabelGroup,
        "due" => GroupField::Due,
        "first-due" => GroupField::FirstDue,
        "snoozed" => GroupField::Snoozed,
        "start" => GroupField::Start,
        "due-or-start" => GroupField::DueOrStart,
        "created" => GroupField::Created,
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A word or `operator:value`, lowercased, with any quotes removed.
    Atom(String),
    /// A quoted phrase on its own, lowercased.
    Phrase(String),
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn tokenize(query: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = query.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            _ if c.is_whitespace() => i += 1,
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '&' => {
                tokens.push(Token::And);
                i += 1;
            }
            '|' => {
                tokens.push(Token::Or);
                i += 1;
            }
            '!' => {
                tokens.push(Token::Not);
                i += 1;
            }
            '-' if chars.get(i + 1).is_some_and(|next| !next.is_whitespace()) => {
                tokens.push(Token::Not);
                i += 1;
            }
            '"' => {
                let start = i + 1;
                let end = chars[start..]
                    .iter()
                    .position(|&ch| ch == '"')
                    .ok_or("Unclosed quote in the search query")?;
                tokens.push(Token::Phrase(chars[start..start + end].iter().collect::<String>().to_lowercase()));
                i = start + end + 1;
            }
            _ => {
                let mut value = String::new();
                let mut in_quote = false;
                while i < chars.len() {
                    let ch = chars[i];
                    i += 1;
                    if ch == '"' {
                        in_quote = !in_quote;
                    } else if !in_quote && (ch.is_whitespace() || ch == ')') {
                        i -= 1;
                        break;
                    } else {
                        value.push(ch);
                    }
                }
                if in_quote {
                    return Err("Unclosed quote in the search query".to_string());
                }
                tokens.push(Token::Atom(value.to_lowercase()));
            }
        }
    }
    Ok(tokens)
}

/// `or` binds loosest, then `and` (written `&` or just a space), then `!`/`-`.
struct Parser {
    tokens: Vec<Token>,
    index: usize,
    now: NaiveDateTime,
}

impl Parser {
    fn parse(mut self) -> Result<Expr, String> {
        let expr = self.parse_or()?;
        match self.tokens.get(self.index) {
            None => Ok(expr),
            Some(Token::RParen) => Err("Unexpected ')' in the search query".to_string()),
            Some(other) => Err(format!("Unexpected {other:?} in the search query")),
        }
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_and()?;
        while self.consume(&Token::Or) {
            expr = Expr::Or(Box::new(expr), Box::new(self.parse_and()?));
        }
        Ok(expr)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_unary()?;
        while self.consume(&Token::And) || self.starts_term() {
            expr = Expr::And(Box::new(expr), Box::new(self.parse_unary()?));
        }
        Ok(expr)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.consume(&Token::Not) {
            return Ok(Expr::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        let token = self.tokens.get(self.index).cloned();
        self.index += 1;
        match token {
            Some(Token::Atom(atom)) => atom_to_expr(&atom, self.now),
            Some(Token::Phrase(phrase)) => Ok(Expr::Phrase(phrase)),
            Some(Token::LParen) => {
                let expr = self.parse_or()?;
                if !self.consume(&Token::RParen) {
                    return Err("Unclosed parenthesis in the search query".to_string());
                }
                Ok(expr)
            }
            Some(Token::RParen) => Err("Unexpected ')' in the search query".to_string()),
            Some(Token::And) => Err("'&' needs a search term on each side".to_string()),
            Some(Token::Or) => Err("'|' needs a search term on each side".to_string()),
            Some(Token::Not) => unreachable!("handled by parse_unary"),
            None => Err("The search query ends where a term was expected".to_string()),
        }
    }

    fn starts_term(&self) -> bool {
        matches!(
            self.tokens.get(self.index),
            Some(Token::Atom(_) | Token::Phrase(_) | Token::LParen | Token::Not)
        )
    }

    fn consume(&mut self, token: &Token) -> bool {
        let found = self.tokens.get(self.index) == Some(token);
        if found {
            self.index += 1;
        }
        found
    }
}

const TEXT_FIELDS: [(&str, TextField); 8] = [
    ("title", TextField::Title),
    ("notes", TextField::Notes),
    ("attachment", TextField::Attachment),
    ("list", TextField::List),
    ("folder", TextField::Folder),
    ("in", TextField::In),
    ("label-group", TextField::LabelGroup),
    ("label", TextField::Label),
];

const DATE_FIELDS: [(&str, DateField); 5] = [
    ("first-due", DateField::FirstDue),
    ("due", DateField::Due),
    ("snoozed", DateField::Snoozed),
    ("start", DateField::Start),
    ("created", DateField::Created),
];

const NUM_FIELDS: [(&str, NumField); 3] = [
    ("snooze-count", NumField::SnoozeCount),
    ("indent-level", NumField::IndentLevel),
    ("duration", NumField::Duration),
];

/// How an operator and its value are joined: `due:today` or `due<=3h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Joiner {
    Colon,
    Cmp(Cmp),
}

fn split_joiner(rest: &str) -> Option<(Joiner, &str)> {
    for (symbol, joiner) in [
        ("<=", Joiner::Cmp(Cmp::Lte)),
        (">=", Joiner::Cmp(Cmp::Gte)),
        ("<", Joiner::Cmp(Cmp::Lt)),
        (">", Joiner::Cmp(Cmp::Gt)),
        ("=", Joiner::Cmp(Cmp::Eq)),
        (":", Joiner::Colon),
    ] {
        if let Some(value) = rest.strip_prefix(symbol) {
            return Some((joiner, value));
        }
    }
    None
}

fn atom_to_expr(atom: &str, now: NaiveDateTime) -> Result<Expr, String> {
    if let Some(value) = atom.strip_prefix("has:") {
        return Ok(Expr::Has(parse_trait(value)?));
    }
    if let Some(value) = atom.strip_prefix("is:") {
        return Ok(Expr::Is(parse_state(value)?));
    }
    for (name, field) in TEXT_FIELDS {
        if let Some(value) = atom.strip_prefix(name).and_then(|rest| rest.strip_prefix(':')) {
            if value.is_empty() {
                return Err(format!("'{name}:' needs a value, such as {name}:something"));
            }
            return Ok(Expr::Field(field, value.to_string()));
        }
    }
    for (name, field) in DATE_FIELDS {
        if let Some((joiner, value)) = atom.strip_prefix(name).and_then(split_joiner) {
            return Ok(Expr::Date(field, parse_date_test(name, joiner, value, now)?));
        }
    }
    for (name, field) in NUM_FIELDS {
        if let Some((joiner, value)) = atom.strip_prefix(name).and_then(split_joiner) {
            let cmp = match joiner {
                Joiner::Colon => Cmp::Eq,
                Joiner::Cmp(cmp) => cmp,
            };
            let number = if field == NumField::Duration {
                dates::parse_minutes(value)
            } else {
                value.trim().parse().ok()
            };
            let number = number.ok_or_else(|| format!("Invalid value '{value}' for {name}"))?;
            return Ok(Expr::Number(field, cmp, number));
        }
    }
    Ok(Expr::Text(atom.to_string()))
}

fn parse_trait(value: &str) -> Result<Trait, String> {
    Ok(match value {
        "notes" => Trait::Notes,
        "attachment" => Trait::Attachment,
        "label" => Trait::Label,
        "repeat" => Trait::Repeat,
        "due" => Trait::Due,
        "snooze" => Trait::Snooze,
        "start" => Trait::Start,
        "due-time" => Trait::DueTime,
        "start-time" => Trait::StartTime,
        "bookmark" => Trait::Bookmark,
        "email" => Trait::Email,
        "duration" => Trait::Duration,
        "any" => Trait::Any,
        _ => {
            return Err(format!(
                "Unknown trait 'has:{value}' (use one of: notes, attachment, label, repeat, due, snooze, start, \
                 due-time, start-time, bookmark, email, duration, any)"
            ))
        }
    })
}

fn parse_state(value: &str) -> Result<State, String> {
    Ok(match value {
        "complete" => State::Complete,
        "available" => State::Available,
        "uncleared" => State::Uncleared,
        _ => return Err(format!("Unknown state 'is:{value}' (use one of: available, uncleared, complete)")),
    })
}

fn start_of(date: NaiveDate) -> NaiveDateTime {
    date.and_hms_opt(0, 0, 0).expect("midnight exists")
}

fn end_of(date: NaiveDate) -> NaiveDateTime {
    start_of(date) + Duration::days(1) - Duration::microseconds(1)
}

/// The first and last moments of the day, week, month or year the keyword names.
fn period(keyword: &str, today: NaiveDate) -> Option<(NaiveDateTime, NaiveDateTime)> {
    let week_of = |day: NaiveDate| {
        let monday = day - Duration::days(day.weekday().num_days_from_monday() as i64);
        (start_of(monday), end_of(monday + Duration::days(6)))
    };
    match keyword {
        "week" => Some(week_of(today)),
        "next-week" => Some(week_of(today + Duration::weeks(1))),
        "month" => {
            let first = today.with_day(1)?;
            let next_first = if today.month() == 12 {
                NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)?
            } else {
                NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1)?
            };
            Some((start_of(first), end_of(next_first - Duration::days(1))))
        }
        "year" => Some((
            start_of(NaiveDate::from_ymd_opt(today.year(), 1, 1)?),
            end_of(NaiveDate::from_ymd_opt(today.year(), 12, 31)?),
        )),
        _ => None,
    }
}

fn parse_date_test(name: &str, joiner: Joiner, value: &str, now: NaiveDateTime) -> Result<DateTest, String> {
    let invalid = || {
        format!(
            "Invalid date '{value}' for {name} (try today, tomorrow, a weekday, 3h, 2d, 2026-10-31 or \
             \"Oct 14th 3pm\")"
        )
    };
    match joiner {
        // `due:today`, `due:week`, ... and `due=...`: on that day, week, month or year.
        Joiner::Colon | Joiner::Cmp(Cmp::Eq) => {
            if let Some((from, to)) = period(value, now.date()) {
                return Ok(DateTest::Within(from, to));
            }
            let parsed = dates::parse(value, now).ok_or_else(invalid)?;
            Ok(match parsed.kind {
                Kind::Time => DateTest::Within(parsed.at, parsed.at + Duration::minutes(1) - Duration::microseconds(1)),
                Kind::Day | Kind::Instant => {
                    DateTest::Within(start_of(parsed.at.date()), end_of(parsed.at.date()))
                }
            })
        }
        Joiner::Cmp(cmp) => {
            if matches!(value, "week" | "next-week" | "month" | "year") {
                return Err(format!(
                    "'{value}' can only be used as {name}:{value}, not after <, <=, > or >=; compare with a date \
                     instead, such as today, 3d or 2026-10-31"
                ));
            }
            let parsed = dates::parse(value, now).ok_or_else(invalid)?;
            // A day with no time means the whole day: `<=` and `>` use its last moment.
            let at = match (parsed.kind, cmp) {
                (Kind::Day, Cmp::Lte | Cmp::Gt) => end_of(parsed.at.date()),
                _ => parsed.at,
            };
            Ok(DateTest::Cmp(cmp, at))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Thursday 15 October 2026, 10:30.
    fn now() -> NaiveDateTime {
        at(2026, 10, 15, 10, 30)
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap()
    }

    fn day_start(y: i32, m: u32, d: u32) -> NaiveDateTime {
        at(y, m, d, 0, 0)
    }

    fn day_end(y: i32, m: u32, d: u32) -> NaiveDateTime {
        end_of(NaiveDate::from_ymd_opt(y, m, d).unwrap())
    }

    fn q(query: &str) -> Query {
        parse(&[query.to_string()], now()).unwrap_or_else(|e| panic!("{query}: {e}"))
    }

    fn err(query: &str) -> String {
        parse(&[query.to_string()], now()).unwrap_err()
    }

    fn text(s: &str) -> Box<Expr> {
        Box::new(Expr::Text(s.to_string()))
    }

    fn field(f: TextField, v: &str) -> Expr {
        Expr::Field(f, v.to_string())
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert_eq!(q("").expr, Expr::All);
        assert_eq!(q("   ").expr, Expr::All);
    }

    #[test]
    fn words_are_anded_and_lowercased() {
        assert_eq!(q("Milk Eggs").expr, Expr::And(text("milk"), text("eggs")));
        assert_eq!(q("research PDF").expr, Expr::And(text("research"), text("pdf")));
    }

    #[test]
    fn quoted_phrases_stay_together() {
        assert_eq!(q("\"Tax Return\"").expr, Expr::Phrase("tax return".to_string()));
        assert_eq!(
            q("title:\"tax return\" list:admin").expr,
            Expr::And(Box::new(field(TextField::Title, "tax return")), Box::new(field(TextField::List, "admin")))
        );
        assert_eq!(
            q("research in:\"engineering q2\"").expr,
            Expr::And(text("research"), Box::new(field(TextField::In, "engineering q2")))
        );
        assert_eq!(
            q("research -\"budget meeting\"").expr,
            Expr::And(text("research"), Box::new(Expr::Not(Box::new(Expr::Phrase("budget meeting".to_string())))))
        );
    }

    #[test]
    fn an_unclosed_quote_is_an_error() {
        assert!(err("\"tax return").contains("Unclosed quote"));
        assert!(err("title:\"tax return").contains("Unclosed quote"));
    }

    #[test]
    fn boolean_operators_have_the_usual_precedence() {
        assert_eq!(q("a | b & c").expr, Expr::Or(text("a"), Box::new(Expr::And(text("b"), text("c")))));
        assert_eq!(q("a b | c").expr, Expr::Or(Box::new(Expr::And(text("a"), text("b"))), text("c")));
        assert_eq!(q("(a | b) c").expr, Expr::And(Box::new(Expr::Or(text("a"), text("b"))), text("c")));
        assert_eq!(
            q("feature & !bug").expr,
            Expr::And(text("feature"), Box::new(Expr::Not(text("bug"))))
        );
    }

    #[test]
    fn nested_parentheses_from_the_guide() {
        // in:features | (feature & !(bug | error))
        assert_eq!(
            q("in:features | (feature & !(bug | error))").expr,
            Expr::Or(
                Box::new(field(TextField::In, "features")),
                Box::new(Expr::And(text("feature"), Box::new(Expr::Not(Box::new(Expr::Or(text("bug"), text("error")))))))
            )
        );
    }

    #[test]
    fn negation_with_bang_or_dash() {
        assert_eq!(q("!a").expr, Expr::Not(text("a")));
        assert_eq!(q("-a").expr, Expr::Not(text("a")));
        assert_eq!(q("research -meeting").expr, Expr::And(text("research"), Box::new(Expr::Not(text("meeting")))));
        assert_eq!(q("!(a | b)").expr, Expr::Not(Box::new(Expr::Or(text("a"), text("b")))));
        assert_eq!(q("--a").expr, Expr::Not(Box::new(Expr::Not(text("a")))));
        assert_eq!(q("well-known").expr, *text("well-known"));
        // Every operator can be negated.
        assert!(matches!(q("!in:engineering").expr, Expr::Not(_)));
        assert!(matches!(q("!has:notes").expr, Expr::Not(_)));
        assert!(matches!(q("!due:today").expr, Expr::Not(_)));
        assert!(matches!(q("!label:v2").expr, Expr::Not(_)));
    }

    #[test]
    fn broken_boolean_syntax_is_reported() {
        assert!(err("(a | b").contains("Unclosed parenthesis"));
        assert!(err("a | b)").contains("Unexpected ')'"));
        assert!(err("a &").contains("ends where a term was expected"));
        assert!(err("| a").contains("'|' needs"));
        assert!(err("a & & b").contains("'&' needs"));
        assert!(err("!").contains("ends where a term was expected"));
    }

    #[test]
    fn text_operators() {
        assert_eq!(q("title:budget").expr, field(TextField::Title, "budget"));
        assert_eq!(q("notes:x").expr, field(TextField::Notes, "x"));
        assert_eq!(q("attachment:pdf").expr, field(TextField::Attachment, "pdf"));
        assert_eq!(q("list:Engineering").expr, field(TextField::List, "engineering"));
        assert_eq!(q("folder:Engineering").expr, field(TextField::Folder, "engineering"));
        assert_eq!(q("in:Engineering").expr, field(TextField::In, "engineering"));
        assert_eq!(q("label:Bug").expr, field(TextField::Label, "bug"));
        assert_eq!(q("label-group:Errands").expr, field(TextField::LabelGroup, "errands"));
        assert_eq!(
            q("research in:b345e415-d839-42f5-881b-14d1c053ea10").expr,
            Expr::And(text("research"), Box::new(field(TextField::In, "b345e415-d839-42f5-881b-14d1c053ea10")))
        );
        assert!(err("title:").contains("needs a value"));
    }

    #[test]
    fn unknown_operators_are_plain_text() {
        assert_eq!(q("10:30").expr, *text("10:30"));
        assert_eq!(q("foo:bar").expr, *text("foo:bar"));
    }

    fn within(field: DateField, from: NaiveDateTime, to: NaiveDateTime) -> Expr {
        Expr::Date(field, DateTest::Within(from, to))
    }

    #[test]
    fn date_shorthands_cover_the_whole_period() {
        let today = within(DateField::Due, day_start(2026, 10, 15), day_end(2026, 10, 15));
        assert_eq!(q("due:today").expr, today);
        assert_eq!(q("due:tomorrow").expr, within(DateField::Due, day_start(2026, 10, 16), day_end(2026, 10, 16)));
        // Thursday 15 October 2026: the week is Monday 12th to Sunday 18th.
        assert_eq!(q("due:week").expr, within(DateField::Due, day_start(2026, 10, 12), day_end(2026, 10, 18)));
        assert_eq!(q("due:next-week").expr, within(DateField::Due, day_start(2026, 10, 19), day_end(2026, 10, 25)));
        assert_eq!(q("due:month").expr, within(DateField::Due, day_start(2026, 10, 1), day_end(2026, 10, 31)));
        assert_eq!(q("due:year").expr, within(DateField::Due, day_start(2026, 1, 1), day_end(2026, 12, 31)));
    }

    #[test]
    fn every_date_field_takes_the_shorthands() {
        for (name, field) in [
            ("due", DateField::Due),
            ("first-due", DateField::FirstDue),
            ("snoozed", DateField::Snoozed),
            ("start", DateField::Start),
            ("created", DateField::Created),
        ] {
            assert_eq!(
                q(&format!("{name}:today")).expr,
                within(field, day_start(2026, 10, 15), day_end(2026, 10, 15)),
                "{name}"
            );
        }
    }

    #[test]
    fn december_month_range_rolls_the_year_correctly() {
        let query = parse(&["due:month".to_string()], at(2026, 12, 20, 9, 0)).unwrap();
        assert_eq!(query.expr, within(DateField::Due, day_start(2026, 12, 1), day_end(2026, 12, 31)));
    }

    #[test]
    fn equality_on_a_day_or_relative_amount_means_that_calendar_day() {
        let oct31 = within(DateField::Due, day_start(2026, 10, 31), day_end(2026, 10, 31));
        assert_eq!(q("due:20261031").expr, oct31);
        assert_eq!(q("due:2026-10-31").expr, oct31);
        assert_eq!(q("due=2026-10-31").expr, oct31);
        assert_eq!(q("due:3d").expr, within(DateField::Due, day_start(2026, 10, 18), day_end(2026, 10, 18)));
        assert_eq!(q("due:friday").expr, within(DateField::Due, day_start(2026, 10, 16), day_end(2026, 10, 16)));
    }

    #[test]
    fn equality_with_a_time_means_that_minute() {
        assert_eq!(
            q("due:\"oct 14th 3pm\"").expr,
            within(DateField::Due, at(2026, 10, 14, 15, 0), at(2026, 10, 14, 15, 0) + Duration::minutes(1) - Duration::microseconds(1))
        );
    }

    fn cmp(field: DateField, c: Cmp, at: NaiveDateTime) -> Expr {
        Expr::Date(field, DateTest::Cmp(c, at))
    }

    #[test]
    fn comparisons_with_a_day_use_the_start_or_the_end_of_that_day() {
        assert_eq!(q("due<today").expr, cmp(DateField::Due, Cmp::Lt, day_start(2026, 10, 15)));
        assert_eq!(q("due>=today").expr, cmp(DateField::Due, Cmp::Gte, day_start(2026, 10, 15)));
        assert_eq!(q("due<=2026-10-31").expr, cmp(DateField::Due, Cmp::Lte, day_end(2026, 10, 31)));
        assert_eq!(q("due>2026-10-31").expr, cmp(DateField::Due, Cmp::Gt, day_end(2026, 10, 31)));
        assert_eq!(q("snoozed>yesterday").expr, cmp(DateField::Snoozed, Cmp::Gt, day_end(2026, 10, 14)));
        assert_eq!(q("first-due<tomorrow").expr, cmp(DateField::FirstDue, Cmp::Lt, day_start(2026, 10, 16)));
    }

    #[test]
    fn comparisons_with_a_relative_amount_or_a_time_use_that_exact_moment() {
        // The guide's example: due or snoozed in the next 3 hours.
        assert_eq!(q("due<=3h").expr, cmp(DateField::Due, Cmp::Lte, at(2026, 10, 15, 13, 30)));
        assert_eq!(q("created>-7d").expr, cmp(DateField::Created, Cmp::Gt, at(2026, 10, 8, 10, 30)));
        assert_eq!(q("start>=2w").expr, cmp(DateField::Start, Cmp::Gte, at(2026, 10, 29, 10, 30)));
        assert_eq!(q("due<=\"in 3 days\"").expr, cmp(DateField::Due, Cmp::Lte, at(2026, 10, 18, 10, 30)));
    }

    #[test]
    fn the_guides_snoozed_example_with_quoted_dates() {
        let query = q("snoozed>=\"Oct 14th 12am\" & snoozed<=\"Oct 14th 1159pm\"");
        assert_eq!(
            query.expr,
            Expr::And(
                Box::new(cmp(DateField::Snoozed, Cmp::Gte, at(2026, 10, 14, 0, 0))),
                Box::new(cmp(DateField::Snoozed, Cmp::Lte, at(2026, 10, 14, 23, 59))),
            )
        );
    }

    #[test]
    fn bad_dates_are_reported_with_the_operator_name() {
        assert!(err("due:someday").contains("Invalid date 'someday' for due"));
        assert!(err("due:20261345").contains("Invalid date"));
        assert!(err("created<soon").contains("for created"));
        assert!(err("due<week").contains("can only be used as due:week"));
        assert!(err("due>=month").contains("can only be used as"));
        assert!(err("start<=\"oct 99\"").contains("Invalid date"));
    }

    #[test]
    fn number_and_duration_operators() {
        let num = |f, c, n| Expr::Number(f, c, n);
        assert_eq!(q("duration<15m").expr, num(NumField::Duration, Cmp::Lt, 15));
        assert_eq!(q("duration>=1.5h").expr, num(NumField::Duration, Cmp::Gte, 90));
        assert_eq!(q("duration>=\"1 hour and 30 minutes\"").expr, num(NumField::Duration, Cmp::Gte, 90));
        assert_eq!(q("duration=45").expr, num(NumField::Duration, Cmp::Eq, 45));
        assert_eq!(q("duration:45").expr, num(NumField::Duration, Cmp::Eq, 45));
        assert_eq!(q("snooze-count>=10").expr, num(NumField::SnoozeCount, Cmp::Gte, 10));
        assert_eq!(q("snooze-count=2").expr, num(NumField::SnoozeCount, Cmp::Eq, 2));
        assert_eq!(q("indent-level=0").expr, num(NumField::IndentLevel, Cmp::Eq, 0));
        assert_eq!(q("indent-level>0").expr, num(NumField::IndentLevel, Cmp::Gt, 0));
        assert!(err("duration<abc").contains("Invalid value 'abc' for duration"));
        assert!(err("snooze-count=x").contains("snooze-count"));
    }

    #[test]
    fn traits_and_states() {
        assert_eq!(q("has:notes").expr, Expr::Has(Trait::Notes));
        assert_eq!(q("has:due-time").expr, Expr::Has(Trait::DueTime));
        assert_eq!(q("has:any").expr, Expr::Has(Trait::Any));
        assert_eq!(q("is:available").expr, Expr::Is(State::Available));
        assert_eq!(q("is:uncleared").expr, Expr::Is(State::Uncleared));
        assert!(err("has:wings").contains("Unknown trait 'has:wings'"));
        assert!(err("is:shiny").contains("Unknown state 'is:shiny'"));
    }

    #[test]
    fn completed_tasks_join_the_search_for_is_complete_and_is_uncleared_only() {
        assert!(!q("milk").include_completed);
        assert!(!q("is:available").include_completed);
        let query = q("is:complete milk");
        assert!(query.include_completed);
        assert_eq!(query.expr, Expr::And(Box::new(Expr::Is(State::Complete)), text("milk")));
        assert!(q("is:uncleared").include_completed);
        assert!(q("!is:uncleared").include_completed);
    }

    #[test]
    fn sorting_grouping_and_limit_are_taken_out_of_the_expression() {
        let query = q("milk sort:due sort-desc:created group:list:Work|Home limit:20 enable:duplicates");
        assert_eq!(query.expr, *text("milk"));
        assert_eq!(
            query.sorts,
            vec![
                SortSpec { field: SortField::Due, descending: false, targets: vec![] },
                SortSpec { field: SortField::Created, descending: true, targets: vec![] },
            ]
        );
        assert_eq!(
            query.group,
            Some(GroupSpec {
                field: GroupField::List,
                targets: vec!["work".to_string(), "home".to_string()],
                rest: false
            })
        );
        assert_eq!(query.limit, Some(20));
        assert!(query.duplicates);
    }

    #[test]
    fn every_sort_field_in_the_guide() {
        for (name, field) in [
            ("due", SortField::Due),
            ("first-due", SortField::FirstDue),
            ("snoozed", SortField::Snoozed),
            ("start", SortField::Start),
            ("created", SortField::Created),
            ("modified", SortField::Modified),
            ("title", SortField::Title),
            ("label", SortField::Label),
            ("duration", SortField::Duration),
            ("snooze-count", SortField::SnoozeCount),
            ("list", SortField::List),
        ] {
            assert_eq!(q(&format!("sort:{name}")).sorts[0].field, field, "{name}");
        }
        assert_eq!(q("sort:updated").sorts[0].field, SortField::Modified);
    }

    #[test]
    fn sort_direction_and_priority_targets() {
        let query = q("sort-asc:title sort-desc:label:High|Medium|Low sort:due");
        assert_eq!(query.expr, Expr::All);
        assert_eq!(query.sorts[0], SortSpec { field: SortField::Title, descending: false, targets: vec![] });
        assert_eq!(query.sorts[1].field, SortField::Label);
        assert!(query.sorts[1].descending);
        assert_eq!(query.sorts[1].targets, vec!["high", "medium", "low"]);
        assert!(!query.sorts[2].descending, "the default order is ascending");
    }

    #[test]
    fn every_group_field_in_the_guide() {
        for (name, field) in [
            ("list", GroupField::List),
            ("label", GroupField::Label),
            ("label-group", GroupField::LabelGroup),
            ("due", GroupField::Due),
            ("start", GroupField::Start),
            ("due-or-start", GroupField::DueOrStart),
            ("created", GroupField::Created),
        ] {
            assert_eq!(q(&format!("group:{name}")).group.unwrap().field, field, "{name}");
        }
    }

    #[test]
    fn group_targets_may_be_quoted_and_may_end_with_rest() {
        let query = q("group:list:Morning|\"After lunch\"|\"Early evening\"");
        assert_eq!(query.group.unwrap().targets, vec!["morning", "after lunch", "early evening"]);

        let query = q("group:label:Bug|Feature|Improvement|Epic|...rest");
        let group = query.group.unwrap();
        assert_eq!(group.targets, vec!["bug", "feature", "improvement", "epic"]);
        assert!(group.rest);

        let query = q("group:label:p0|p1|p2");
        assert!(!query.group.unwrap().rest);
    }

    #[test]
    fn the_guides_today_smart_list() {
        let query = q("due:today group:list sort:due");
        assert_eq!(query.expr, within(DateField::Due, day_start(2026, 10, 15), day_end(2026, 10, 15)));
        assert_eq!(query.group.unwrap().field, GroupField::List);
        assert_eq!(query.sorts[0].field, SortField::Due);
    }

    #[test]
    fn bad_presentation_operators_are_reported() {
        assert!(err("sort:colour").contains("Unknown sort field 'colour'"));
        assert!(err("group:mood").contains("Unknown group field 'mood'"));
        assert!(err("limit:lots").contains("Invalid limit"));
        assert!(err("limit:-3").contains("Invalid limit"));
        assert!(err("enable:sparkles").contains("Unknown option"));
    }

    #[test]
    fn nested_and_duplicate_flags_are_accepted() {
        assert!(!q("enable:duplicates disable:duplicates").duplicates);
        assert!(q("disable:duplicates enable:duplicates").duplicates);
        // Results are always flat, so the nested flags change nothing but are valid.
        assert_eq!(q("enable:nested").expr, Expr::All);
        assert_eq!(q("milk disable:nested").expr, *text("milk"));
    }

    #[test]
    fn limit_takes_a_number() {
        assert_eq!(q("limit:10").limit, Some(10));
        assert_eq!(q("limit:0").limit, Some(0));
    }

    #[test]
    fn arguments_are_joined_with_spaces() {
        let query = parse(&["label:Low".to_string(), "due<=3d".to_string()], now()).unwrap();
        assert!(matches!(query.expr, Expr::And(..)));
    }
}
