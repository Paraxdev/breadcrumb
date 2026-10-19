use crate::note::{Note, yaml_display};

#[derive(Debug, Default, Clone)]
pub struct Query {
    pub terms: Vec<String>,
    pub phrases: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub note: Note,
    pub score: f64,
    pub snippet: String,
    /// How many query terms the note matched.
    pub matched: usize,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    /// Matches before any limit was applied.
    pub total: usize,
    /// True when no note matched every term and results are any-term matches.
    pub fell_back_to_or: bool,
}

pub fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| t.to_lowercase()).collect()
}

pub fn parse_query(q: &str) -> Query {
    let mut query = Query::default();
    let mut rest = String::new();
    let mut in_quote = false;
    let mut cur = String::new();
    for c in q.chars() {
        if c == '"' {
            if in_quote {
                let p = cur.trim().to_lowercase();
                if !p.is_empty() {
                    query.phrases.push(p);
                }
                cur.clear();
            }
            in_quote = !in_quote;
        } else if in_quote {
            cur.push(c);
        } else {
            rest.push(c);
        }
    }
    if in_quote {
        rest.push(' ');
        rest.push_str(&cur);
    }
    for t in tokenize(&rest) {
        if !query.terms.contains(&t) {
            query.terms.push(t);
        }
    }
    query
}

struct Prepared {
    title: Vec<String>,
    tags: Vec<String>,
    refs: Vec<String>,
    fields: Vec<String>,
    body: Vec<String>,
    title_lc: String,
    body_lc: String,
}

fn prepare(n: &Note) -> Prepared {
    let mut fields = vec![];
    for (_, v) in &n.extra {
        fields.extend(tokenize(&yaml_display(v)));
    }
    let mut tags = vec![];
    for t in &n.tags {
        tags.extend(tokenize(t));
    }
    Prepared {
        title: tokenize(&n.title),
        tags,
        refs: n.refs().iter().flat_map(|r| tokenize(r)).collect(),
        fields,
        body: tokenize(&n.body),
        title_lc: n.title.to_lowercase(),
        body_lc: n.body.to_lowercase(),
    }
}

/// Exact token hit counts fully, a prefix-only hit counts half.
fn token_weight(tokens: &[String], term: &str) -> f64 {
    if tokens.iter().any(|t| t == term) {
        1.0
    } else if tokens.iter().any(|t| t.starts_with(term)) {
        0.5
    } else {
        0.0
    }
}

fn term_score(n: &Note, p: &Prepared, term: &str) -> f64 {
    let mut s = 0.0;
    s += 6.0 * token_weight(&p.title, term);
    s += 4.0 * token_weight(&p.tags, term);
    s += 4.0 * token_weight(&p.refs, term);
    if term.len() >= crate::id::MIN_PREFIX && n.id.starts_with(term) {
        s += 10.0;
    }
    s += 2.0 * token_weight(&p.fields, term);
    let mut body: f64 = 0.0;
    for t in &p.body {
        if t == term {
            body += 1.0;
        } else if t.starts_with(term) {
            body += 0.5;
        }
        if body >= 3.0 {
            break;
        }
    }
    s + body.min(3.0)
}

fn snippet(n: &Note, q: &Query) -> String {
    let mut best: Option<(usize, &str)> = None;
    for line in n.body.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let lc = l.to_lowercase();
        let toks = tokenize(l);
        let mut hits = q.phrases.iter().filter(|p| lc.contains(p.as_str())).count() * 2;
        hits += q.terms.iter().filter(|t| toks.iter().any(|x| x.starts_with(t.as_str()))).count();
        if hits > 0 && best.is_none_or(|(b, _)| hits > b) {
            best = Some((hits, l));
        }
    }
    best.map(|(_, l)| crate::util::truncate_chars(l, 160)).unwrap_or_default()
}

/// Scores notes against a query. Sorting: score, then most recently updated.
pub fn search(notes: Vec<Note>, q: &str) -> SearchResult {
    let query = parse_query(q);
    if query.terms.is_empty() && query.phrases.is_empty() {
        return SearchResult { hits: vec![], total: 0, fell_back_to_or: false };
    }
    let mut and_hits = vec![];
    let mut or_hits = vec![];
    for n in notes {
        let p = prepare(&n);
        if !query.phrases.iter().all(|ph| p.title_lc.contains(ph.as_str()) || p.body_lc.contains(ph.as_str())) {
            continue;
        }
        let mut score = 0.0;
        for ph in &query.phrases {
            if p.title_lc.contains(ph.as_str()) {
                score += 6.0;
            }
            if p.body_lc.contains(ph.as_str()) {
                score += 2.0;
            }
        }
        let mut matched = 0;
        for t in &query.terms {
            let s = term_score(&n, &p, t);
            if s > 0.0 {
                matched += 1;
                score += s;
            }
        }
        if matched == query.terms.len() {
            let snip = snippet(&n, &query);
            and_hits.push(Hit { note: n, score, snippet: snip, matched });
        } else if matched > 0 {
            let snip = snippet(&n, &query);
            or_hits.push(Hit { note: n, score, snippet: snip, matched });
        }
    }
    let fell_back = and_hits.is_empty() && !or_hits.is_empty();
    let mut hits = if fell_back { or_hits } else { and_hits };
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then(b.note.updated.cmp(&a.note.updated)));
    SearchResult { total: hits.len(), hits, fell_back_to_or: fell_back }
}
