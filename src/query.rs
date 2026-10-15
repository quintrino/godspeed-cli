//! The search query language, for example
//! `label:Low due<=3d "tax return" | !has:notes sort:due limit:20`.
//!
//! A query is parsed into an expression that picks tasks, plus presentation settings
//! (`sort:`, `group:`, `limit:`). Relative dates (`today`, `3d`) are turned into real dates
//! when the query is parsed, so the same query value always means the same thing.

use chrono::{Datelike, Duration, NaiveDate};

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
    /// `folder:` and `in:`: matches the list's path as well as its name.
    Folder,
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
    Cmp(Cmp, NaiveDate),
    /// On or between these two dates.
    Within(NaiveDate, NaiveDate),
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
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
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
    Updated,
    Duration,
    SnoozeCount,
    IndentLevel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortSpec {
    pub field: SortField,
    pub descending: bool,
    /// Names to put first, in this order (`sort:label:High|Medium|Low`).
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupField {
    List,
    Label,
    Due,
    FirstDue,
    Snoozed,
    Start,
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSpec {
    pub field: GroupField,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub expr: Expr,
    /// Whether completed tasks are part of the search. Set by `is:complete`.
    pub include_completed: bool,
    pub sorts: Vec<SortSpec>,
    pub group: Option<GroupSpec>,
    pub limit: Option<usize>,
    /// Whether a task with several labels shows up under each of them when grouping by label.
    pub duplicates: bool,
}

const SORT_FIELDS: &str = "title, list, label, due, first-due, snoozed, start, created, updated, duration, snooze-count, indent-level";
const GROUP_FIELDS: &str = "list, label, due, first-due, snoozed, start, created";

pub fn parse(args: &[String], today: NaiveDate) -> Result<Query, String> {
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
            if atom == "is:complete" {
                query.include_completed = true;
            }
        }
        predicate_tokens.push(token);
    }

    if !predicate_tokens.is_empty() {
        query.expr = Parser { tokens: predicate_tokens, index: 0, today }.parse()?;
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
        let (name, targets) = split_targets(rest);
        let field = group_field(&name)
            .ok_or_else(|| format!("Unknown group field '{name}' (use one of: {GROUP_FIELDS})"))?;
        query.group = Some(GroupSpec { field, targets });
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
                "nested" => Err(format!(
                    "{prefix}nested isn't supported: the local copy doesn't keep Godspeed's sub-task layout"
                )),
                other => Err(format!("Unknown option '{prefix}{other}' (only enable:/disable:duplicates is supported)")),
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
        "updated" => SortField::Updated,
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
        "due" => GroupField::Due,
        "first-due" => GroupField::FirstDue,
        "snoozed" => GroupField::Snoozed,
        "start" => GroupField::Start,
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
    today: NaiveDate,
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
            Some(Token::Atom(atom)) => atom_to_expr(&atom, self.today),
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
    ("in", TextField::Folder),
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

/// How an operator and its value are joined: `due:today` or `due<=3d`.
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

fn atom_to_expr(atom: &str, today: NaiveDate) -> Result<Expr, String> {
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
            return Ok(Expr::Date(field, parse_date_test(name, joiner, value, today)?));
        }
    }
    for (name, field) in NUM_FIELDS {
        if let Some((joiner, value)) = atom.strip_prefix(name).and_then(split_joiner) {
            let cmp = match joiner {
                Joiner::Colon => Cmp::Eq,
                Joiner::Cmp(cmp) => cmp,
            };
            let number = if field == NumField::Duration { parse_minutes(value) } else { value.parse().ok() };
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
        _ => return Err(format!("Unknown state 'is:{value}' (use one of: complete, available, uncleared)")),
    })
}

/// `30`, `30m`, `1.5h`: minutes.
pub fn parse_minutes(value: &str) -> Option<i64> {
    let (number, per_unit) = match value.strip_suffix('h') {
        Some(hours) => (hours, 60.0),
        None => (value.strip_suffix('m').unwrap_or(value), 1.0),
    };
    let minutes = (number.parse::<f64>().ok()? * per_unit).round();
    (number.chars().all(|c| c.is_ascii_digit() || c == '.') && minutes.is_finite()).then_some(minutes as i64)
}

fn parse_date_test(name: &str, joiner: Joiner, value: &str, today: NaiveDate) -> Result<DateTest, String> {
    match joiner {
        Joiner::Colon | Joiner::Cmp(Cmp::Eq) => {
            let (from, to) = date_range(name, value, today)?;
            Ok(DateTest::Within(from, to))
        }
        Joiner::Cmp(cmp) => {
            if matches!(value, "week" | "next-week" | "month" | "year") {
                return Err(format!(
                    "'{value}' can't follow <, <=, > or >= (as in {name}{}{value}); use a date such as today, 3d or 2026-10-31",
                    match cmp {
                        Cmp::Lt => "<",
                        Cmp::Lte => "<=",
                        Cmp::Gt => ">",
                        _ => ">=",
                    }
                ));
            }
            Ok(DateTest::Cmp(cmp, date_point(name, value, today)?))
        }
    }
}

/// A single date: `today`, `tomorrow`, `yesterday`, `20261031`, `2026-10-31`, or relative
/// `3d`, `-2d`, `2w`, `24h` (hours count as whole days, rounded down).
fn date_point(name: &str, value: &str, today: NaiveDate) -> Result<NaiveDate, String> {
    let invalid = || {
        format!(
            "Invalid date '{value}' for {name} (use today, tomorrow, yesterday, YYYYMMDD, YYYY-MM-DD, or a \
             relative amount like 3d, 2w or -1d)"
        )
    };
    match value {
        "today" => return Ok(today),
        "tomorrow" => return Ok(today + Duration::days(1)),
        "yesterday" => return Ok(today - Duration::days(1)),
        _ => {}
    }
    for (suffix, days_per_unit) in [("d", 1.0), ("w", 7.0), ("h", 1.0 / 24.0)] {
        if let Some(amount) = value.strip_suffix(suffix) {
            if let Ok(amount) = amount.parse::<i64>() {
                let days = (amount as f64 * days_per_unit).floor() as i64;
                return Ok(today + Duration::days(days));
            }
        }
    }
    if value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit()) {
        return NaiveDate::parse_from_str(value, "%Y%m%d").map_err(|_| invalid());
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| invalid())
}

/// A date or a span: the period keywords `week`, `next-week`, `month` and `year` cover a whole
/// calendar span (weeks run Monday to Sunday); anything else is a single day.
fn date_range(name: &str, value: &str, today: NaiveDate) -> Result<(NaiveDate, NaiveDate), String> {
    let week_of = |day: NaiveDate| {
        let monday = day - Duration::days(day.weekday().num_days_from_monday() as i64);
        (monday, monday + Duration::days(6))
    };
    match value {
        "week" => Ok(week_of(today)),
        "next-week" => Ok(week_of(today + Duration::weeks(1))),
        "month" => {
            let first = today.with_day(1).expect("day 1 exists");
            let next_month_first = if today.month() == 12 {
                NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1)
            }
            .expect("a valid first of the month");
            Ok((first, next_month_first - Duration::days(1)))
        }
        "year" => Ok((
            NaiveDate::from_ymd_opt(today.year(), 1, 1).expect("1 January exists"),
            NaiveDate::from_ymd_opt(today.year(), 12, 31).expect("31 December exists"),
        )),
        _ => date_point(name, value, today).map(|day| (day, day)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        // A Thursday.
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn q(query: &str) -> Query {
        parse(&[query.to_string()], today()).unwrap_or_else(|e| panic!("{query}: {e}"))
    }

    fn err(query: &str) -> String {
        parse(&[query.to_string()], today()).unwrap_err()
    }

    fn text(s: &str) -> Box<Expr> {
        Box::new(Expr::Text(s.to_string()))
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert_eq!(q("").expr, Expr::All);
        assert_eq!(q("   ").expr, Expr::All);
    }

    #[test]
    fn words_are_anded_and_lowercased() {
        assert_eq!(q("Milk Eggs").expr, Expr::And(text("milk"), text("eggs")));
    }

    #[test]
    fn quoted_phrases_stay_together() {
        assert_eq!(q("\"Tax Return\"").expr, Expr::Phrase("tax return".to_string()));
        assert_eq!(
            q("title:\"tax return\" list:admin").expr,
            Expr::And(
                Box::new(Expr::Field(TextField::Title, "tax return".to_string())),
                Box::new(Expr::Field(TextField::List, "admin".to_string()))
            )
        );
    }

    #[test]
    fn an_unclosed_quote_is_an_error() {
        assert!(err("\"tax return").contains("Unclosed quote"));
        assert!(err("title:\"tax return").contains("Unclosed quote"));
    }

    #[test]
    fn boolean_operators_have_the_usual_precedence() {
        // a | b & c  is  a | (b & c)
        assert_eq!(
            q("a | b & c").expr,
            Expr::Or(text("a"), Box::new(Expr::And(text("b"), text("c"))))
        );
        // A space is an implicit and, binding tighter than |.
        assert_eq!(
            q("a b | c").expr,
            Expr::Or(Box::new(Expr::And(text("a"), text("b"))), text("c"))
        );
        assert_eq!(
            q("(a | b) c").expr,
            Expr::And(Box::new(Expr::Or(text("a"), text("b"))), text("c"))
        );
    }

    #[test]
    fn negation_with_bang_or_dash() {
        assert_eq!(q("!a").expr, Expr::Not(text("a")));
        assert_eq!(q("-a").expr, Expr::Not(text("a")));
        assert_eq!(q("a -b").expr, Expr::And(text("a"), Box::new(Expr::Not(text("b")))));
        assert_eq!(q("!(a | b)").expr, Expr::Not(Box::new(Expr::Or(text("a"), text("b")))));
        assert_eq!(q("--a").expr, Expr::Not(Box::new(Expr::Not(text("a")))));
        // A dash inside a word is part of the word.
        assert_eq!(q("well-known").expr, *text("well-known"));
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
        assert_eq!(q("title:budget").expr, Expr::Field(TextField::Title, "budget".to_string()));
        assert_eq!(q("notes:x").expr, Expr::Field(TextField::Notes, "x".to_string()));
        assert_eq!(q("attachment:pdf").expr, Expr::Field(TextField::Attachment, "pdf".to_string()));
        assert_eq!(q("list:Work").expr, Expr::Field(TextField::List, "work".to_string()));
        assert_eq!(q("folder:Work").expr, Expr::Field(TextField::Folder, "work".to_string()));
        assert_eq!(q("in:Work").expr, Expr::Field(TextField::Folder, "work".to_string()));
        assert_eq!(q("label:Low").expr, Expr::Field(TextField::Label, "low".to_string()));
        assert_eq!(q("label-group:Priority").expr, Expr::Field(TextField::LabelGroup, "priority".to_string()));
        assert!(err("title:").contains("needs a value"));
    }

    #[test]
    fn unknown_operators_are_plain_text() {
        assert_eq!(q("10:30").expr, *text("10:30"));
        assert_eq!(q("foo:bar").expr, *text("foo:bar"));
    }

    #[test]
    fn date_shorthands_and_values() {
        let due = |t| Expr::Date(DateField::Due, t);
        assert_eq!(q("due:today").expr, due(DateTest::Within(today(), today())));
        assert_eq!(q("due:tomorrow").expr, due(DateTest::Within(day(2026, 10, 16), day(2026, 10, 16))));
        assert_eq!(q("due:20261031").expr, due(DateTest::Within(day(2026, 10, 31), day(2026, 10, 31))));
        assert_eq!(q("due:2026-10-31").expr, due(DateTest::Within(day(2026, 10, 31), day(2026, 10, 31))));
        assert_eq!(q("due:3d").expr, due(DateTest::Within(day(2026, 10, 18), day(2026, 10, 18))));
        assert_eq!(q("due:24h").expr, due(DateTest::Within(day(2026, 10, 16), day(2026, 10, 16))));
        assert_eq!(q("due=today").expr, due(DateTest::Within(today(), today())));
    }

    #[test]
    fn period_shorthands_span_the_calendar_period() {
        let due = |a, b| Expr::Date(DateField::Due, DateTest::Within(a, b));
        // Thursday 15 October 2026: the week is Monday 12th to Sunday 18th.
        assert_eq!(q("due:week").expr, due(day(2026, 10, 12), day(2026, 10, 18)));
        assert_eq!(q("due:next-week").expr, due(day(2026, 10, 19), day(2026, 10, 25)));
        assert_eq!(q("due:month").expr, due(day(2026, 10, 1), day(2026, 10, 31)));
        assert_eq!(q("due:year").expr, due(day(2026, 1, 1), day(2026, 12, 31)));
    }

    #[test]
    fn december_month_range_rolls_the_year_correctly() {
        let december = day(2026, 12, 20);
        let query = parse(&["due:month".to_string()], december).unwrap();
        assert_eq!(
            query.expr,
            Expr::Date(DateField::Due, DateTest::Within(day(2026, 12, 1), day(2026, 12, 31)))
        );
    }

    #[test]
    fn date_comparisons() {
        let cmp = |field, c, d| Expr::Date(field, DateTest::Cmp(c, d));
        assert_eq!(q("due<today").expr, cmp(DateField::Due, Cmp::Lt, today()));
        assert_eq!(q("due<=2026-10-31").expr, cmp(DateField::Due, Cmp::Lte, day(2026, 10, 31)));
        assert_eq!(q("created>-7d").expr, cmp(DateField::Created, Cmp::Gt, day(2026, 10, 8)));
        assert_eq!(q("start>=2w").expr, cmp(DateField::Start, Cmp::Gte, day(2026, 10, 29)));
        assert_eq!(q("snoozed>yesterday").expr, cmp(DateField::Snoozed, Cmp::Gt, day(2026, 10, 14)));
        assert_eq!(q("first-due<tomorrow").expr, cmp(DateField::FirstDue, Cmp::Lt, day(2026, 10, 16)));
    }

    #[test]
    fn bad_dates_are_reported_with_the_operator_name() {
        assert!(err("due:someday").contains("Invalid date 'someday' for due"));
        assert!(err("due:20261345").contains("Invalid date"));
        assert!(err("created<soon").contains("for created"));
        assert!(err("due<week").contains("can't follow"));
        assert!(err("due>=month").contains("can't follow"));
    }

    #[test]
    fn number_and_duration_operators() {
        let num = |f, c, n| Expr::Number(f, c, n);
        assert_eq!(q("duration<30m").expr, num(NumField::Duration, Cmp::Lt, 30));
        assert_eq!(q("duration>=1.5h").expr, num(NumField::Duration, Cmp::Gte, 90));
        assert_eq!(q("duration:45").expr, num(NumField::Duration, Cmp::Eq, 45));
        assert_eq!(q("snooze-count=2").expr, num(NumField::SnoozeCount, Cmp::Eq, 2));
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
    fn is_complete_adds_completed_tasks_to_the_search() {
        assert!(!q("milk").include_completed);
        let query = q("is:complete milk");
        assert!(query.include_completed);
        assert_eq!(query.expr, Expr::And(Box::new(Expr::Is(State::Complete)), text("milk")));
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
            Some(GroupSpec { field: GroupField::List, targets: vec!["work".to_string(), "home".to_string()] })
        );
        assert_eq!(query.limit, Some(20));
        assert!(query.duplicates);
    }

    #[test]
    fn sort_asc_and_priority_targets() {
        let query = q("sort-asc:title sort:label:High|Medium|Low");
        assert_eq!(query.expr, Expr::All);
        assert_eq!(query.sorts[0], SortSpec { field: SortField::Title, descending: false, targets: vec![] });
        assert_eq!(query.sorts[1].field, SortField::Label);
        assert_eq!(query.sorts[1].targets, vec!["high", "medium", "low"]);
    }

    #[test]
    fn bad_presentation_operators_are_reported() {
        assert!(err("sort:colour").contains("Unknown sort field 'colour'"));
        assert!(err("group:mood").contains("Unknown group field 'mood'"));
        assert!(err("limit:lots").contains("Invalid limit"));
        assert!(err("limit:-3").contains("Invalid limit"));
        assert!(err("enable:nested").contains("nested isn't supported"));
        assert!(err("disable:nested").contains("nested isn't supported"));
        assert!(err("enable:sparkles").contains("Unknown option"));
    }

    #[test]
    fn disable_duplicates_turns_it_back_off() {
        assert!(!q("enable:duplicates disable:duplicates").duplicates);
    }

    #[test]
    fn arguments_are_joined_with_spaces() {
        let query = parse(&["label:Low".to_string(), "due<=3d".to_string()], today()).unwrap();
        assert!(matches!(query.expr, Expr::And(..)));
    }

    #[test]
    fn minutes_parsing() {
        assert_eq!(parse_minutes("30"), Some(30));
        assert_eq!(parse_minutes("30m"), Some(30));
        assert_eq!(parse_minutes("1.5h"), Some(90));
        assert_eq!(parse_minutes("2h"), Some(120));
        assert_eq!(parse_minutes("x"), None);
        assert_eq!(parse_minutes(""), None);
        assert_eq!(parse_minutes("-5"), None);
    }
}
