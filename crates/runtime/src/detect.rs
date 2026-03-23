//! Language auto-detection from code content.
//!
//! The system detects the language — no dropdown, no `%%` prefix, no kernel selector.
//! A subtle chip shows what was detected: `gremlin ▾` — one tap to override.
//!
//! Detection uses pattern matching on first tokens, ordered by specificity:
//! most-specific patterns first (graph queries), then general-purpose languages.

use crate::cell::CellLanguage;

/// Detect the language of a code string from its content.
///
/// Returns `None` if detection is ambiguous or the code is empty.
/// The caller can fall back to a default or prompt the user.
pub fn detect_language(code: &str) -> Option<CellLanguage> {
    let trimmed = code.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Check each language detector in order of specificity.
    // Graph query languages are most specific (fewest false positives).
    if is_gremlin(trimmed) {
        return Some(CellLanguage::Gremlin);
    }
    if is_cypher(trimmed) {
        return Some(CellLanguage::Cypher);
    }
    if is_sparql(trimmed) {
        return Some(CellLanguage::Sparql);
    }
    if is_nars(trimmed) {
        return Some(CellLanguage::Nars);
    }
    if is_markdown(trimmed) {
        return Some(CellLanguage::Markdown);
    }
    if is_r(trimmed) {
        return Some(CellLanguage::R);
    }
    if is_rust(trimmed) {
        return Some(CellLanguage::Rust);
    }
    if is_sql(trimmed) {
        return Some(CellLanguage::Sql);
    }
    if is_python(trimmed) {
        return Some(CellLanguage::Python);
    }

    // Fallback: if it looks like natural language, return None
    // (the notebook layer can route to NL→query via LLM).
    None
}

/// Gremlin traversals start with `g.` followed by a step.
fn is_gremlin(code: &str) -> bool {
    let lower = code.to_lowercase();
    // g.V(), g.E(), g.addV(, g.addE(, g.io(, g.tx()
    lower.starts_with("g.v(")
        || lower.starts_with("g.e(")
        || lower.starts_with("g.addv(")
        || lower.starts_with("g.adde(")
        || lower.starts_with("g.io(")
        || lower.starts_with("g.tx(")
        || lower.starts_with("g.inject(")
        // Multi-line: first token on any line starts a traversal
        || code.lines().any(|line| {
            let t = line.trim().to_lowercase();
            t.starts_with("g.v(") || t.starts_with("g.e(")
        })
}

/// Cypher queries start with MATCH, CREATE, MERGE, RETURN, OPTIONAL, CALL, UNWIND, WITH.
fn is_cypher(code: &str) -> bool {
    let first = first_token_upper(code);
    matches!(
        first.as_str(),
        "MATCH" | "CREATE" | "MERGE" | "RETURN" | "OPTIONAL" | "CALL" | "UNWIND" | "WITH" | "DETACH"
    )
    // Also detect `MATCH (` pattern specifically
    || code.trim().to_uppercase().starts_with("MATCH (")
    || code.trim().to_uppercase().starts_with("MATCH(")
}

/// SPARQL queries start with PREFIX, SELECT ?, BASE, CONSTRUCT, ASK, DESCRIBE.
fn is_sparql(code: &str) -> bool {
    let upper = code.trim().to_uppercase();
    upper.starts_with("PREFIX ")
        || upper.starts_with("BASE ")
        || upper.starts_with("ASK ")
        || upper.starts_with("ASK{")
        || upper.starts_with("DESCRIBE ")
        || upper.starts_with("CONSTRUCT ")
        // SELECT with ? variable binding is SPARQL, not SQL
        || (upper.starts_with("SELECT ") && code.contains('?'))
}

/// NARS/Narsese: statements with `<`, `>`, truth values `{`, `}`, copulas.
fn is_nars(code: &str) -> bool {
    let trimmed = code.trim();
    // Narsese statements: <term --> term>. or <term ==> term>.
    (trimmed.starts_with('<') && trimmed.contains("-->"))
        || (trimmed.starts_with('<') && trimmed.contains("==>"))
        || (trimmed.starts_with('<') && trimmed.contains("<->"))
        || (trimmed.starts_with('<') && trimmed.contains("=/>"))
        || trimmed.starts_with("//nars")
        || trimmed.starts_with("//NARS")
}

/// Markdown: starts with heading, list, horizontal rule, or frontmatter.
fn is_markdown(code: &str) -> bool {
    let trimmed = code.trim();
    trimmed.starts_with("# ")
        || trimmed.starts_with("## ")
        || trimmed.starts_with("### ")
        || trimmed.starts_with("---\n")
        || trimmed.starts_with("---\r\n")
        || trimmed.starts_with("> ")
        || (trimmed.starts_with("- ") && !trimmed.contains("fn ") && !trimmed.contains("<-"))
        || (trimmed.starts_with("* ") && !trimmed.contains("fn "))
}

/// R: pipe operators, assignment, common functions.
fn is_r(code: &str) -> bool {
    let trimmed = code.trim();
    // Pipe operator is definitive
    if trimmed.contains("%>%") || trimmed.contains("|>") {
        // |> could be Rust too, so check for R-specific context
        if trimmed.contains("%>%") {
            return true;
        }
    }
    // R assignment operator
    if trimmed.contains(" <- ") || trimmed.starts_with("library(") || trimmed.starts_with("require(") {
        return true;
    }
    // Common R patterns
    let first = first_token(code);
    matches!(
        first.as_str(),
        "library" | "require" | "data.frame" | "ggplot" | "tibble" | "dplyr" | "tidyr"
    ) || trimmed.starts_with("ggplot(")
        || trimmed.starts_with("plot(")
        || trimmed.starts_with("summary(")
        || trimmed.starts_with("str(")
        // function definition R style
        || trimmed.contains("function(")
            && trimmed.contains("<-")
}

/// Rust: keywords that are unique to Rust.
fn is_rust(code: &str) -> bool {
    let first = first_token(code);
    matches!(
        first.as_str(),
        "fn" | "let" | "use" | "mod" | "pub" | "struct" | "enum" | "impl" | "trait" | "const"
            | "static" | "extern" | "unsafe" | "async" | "match" | "macro_rules!"
    ) || code.trim().starts_with("#[")
        || code.trim().starts_with("//!")
        // `::` path separator is very Rust
        || (code.contains("::") && !code.contains("//") && first == "let" || first == "fn")
}

/// SQL: standard DML/DDL keywords (without ? variables, which would be SPARQL).
fn is_sql(code: &str) -> bool {
    let first = first_token_upper(code);
    matches!(
        first.as_str(),
        "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "ALTER" | "DROP" | "TRUNCATE" | "EXPLAIN"
    ) && !code.contains('?') // ? variables → SPARQL, not SQL
    || first == "CREATE" && code.to_uppercase().contains("TABLE")
}

/// Python: standard Python keywords and patterns.
fn is_python(code: &str) -> bool {
    let first = first_token(code);
    matches!(
        first.as_str(),
        "import" | "from" | "def" | "class" | "if" | "for" | "while" | "with" | "try"
            | "except" | "raise" | "assert" | "yield" | "return" | "print" | "async"
    ) || code.trim().starts_with("import ")
        || code.trim().starts_with("from ")
        || code.trim().starts_with("print(")
        || code.trim().starts_with("@")
        || code.contains("__name__")
}

/// Extract the first whitespace-delimited token from code.
fn first_token(code: &str) -> String {
    code.trim()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Extract the first token, uppercased.
fn first_token_upper(code: &str) -> String {
    first_token(code).to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_gremlin() {
        assert_eq!(detect_language("g.V().hasLabel('person')"), Some(CellLanguage::Gremlin));
        assert_eq!(detect_language("g.E().has('weight', gt(0.5))"), Some(CellLanguage::Gremlin));
        assert_eq!(detect_language("g.addV('person').property('name','Alice')"), Some(CellLanguage::Gremlin));
    }

    #[test]
    fn detect_cypher() {
        assert_eq!(detect_language("MATCH (n:Person) RETURN n"), Some(CellLanguage::Cypher));
        assert_eq!(detect_language("MATCH (a)-[:KNOWS]->(b) RETURN a, b"), Some(CellLanguage::Cypher));
        assert_eq!(detect_language("CREATE (n:Person {name: 'Alice'})"), Some(CellLanguage::Cypher));
        assert_eq!(detect_language("MERGE (n:Person {id: 1})"), Some(CellLanguage::Cypher));
    }

    #[test]
    fn detect_sparql() {
        assert_eq!(detect_language("PREFIX foaf: <http://xmlns.com/foaf/0.1/>\nSELECT ?name WHERE { ?s foaf:name ?name }"), Some(CellLanguage::Sparql));
        assert_eq!(detect_language("SELECT ?s ?p ?o WHERE { ?s ?p ?o }"), Some(CellLanguage::Sparql));
        assert_eq!(detect_language("ASK { <http://example.org/Alice> a foaf:Person }"), Some(CellLanguage::Sparql));
    }

    #[test]
    fn detect_r() {
        assert_eq!(detect_language("library(dplyr)"), Some(CellLanguage::R));
        assert_eq!(detect_language("paths %>% filter(weight > 0.8) %>% arrange(desc(weight))"), Some(CellLanguage::R));
        assert_eq!(detect_language("x <- 42"), Some(CellLanguage::R));
        assert_eq!(detect_language("ggplot(data, aes(x, y)) + geom_point()"), Some(CellLanguage::R));
    }

    #[test]
    fn detect_rust() {
        assert_eq!(detect_language("let x = 42;"), Some(CellLanguage::Rust));
        assert_eq!(detect_language("fn main() { println!(\"hello\"); }"), Some(CellLanguage::Rust));
        assert_eq!(detect_language("use std::collections::HashMap;"), Some(CellLanguage::Rust));
        assert_eq!(detect_language("struct Point { x: f64, y: f64 }"), Some(CellLanguage::Rust));
    }

    #[test]
    fn detect_python() {
        assert_eq!(detect_language("import pandas as pd"), Some(CellLanguage::Python));
        assert_eq!(detect_language("def hello():\n    print('hello')"), Some(CellLanguage::Python));
        assert_eq!(detect_language("from pathlib import Path"), Some(CellLanguage::Python));
        assert_eq!(detect_language("class Foo:\n    pass"), Some(CellLanguage::Python));
    }

    #[test]
    fn detect_sql() {
        assert_eq!(detect_language("SELECT * FROM users WHERE id = 1"), Some(CellLanguage::Sql));
        assert_eq!(detect_language("INSERT INTO users (name) VALUES ('Alice')"), Some(CellLanguage::Sql));
    }

    #[test]
    fn detect_nars() {
        assert_eq!(detect_language("<cat --> animal>."), Some(CellLanguage::Nars));
        assert_eq!(detect_language("<rain ==> wet>."), Some(CellLanguage::Nars));
    }

    #[test]
    fn detect_markdown() {
        assert_eq!(detect_language("# Hello World"), Some(CellLanguage::Markdown));
        assert_eq!(detect_language("## Section Two"), Some(CellLanguage::Markdown));
        assert_eq!(detect_language("> This is a quote"), Some(CellLanguage::Markdown));
    }

    #[test]
    fn detect_empty() {
        assert_eq!(detect_language(""), None);
        assert_eq!(detect_language("   "), None);
    }

    #[test]
    fn sparql_not_sql() {
        // SELECT with ? variables should be SPARQL, not SQL
        assert_eq!(detect_language("SELECT ?name WHERE { ?s foaf:name ?name }"), Some(CellLanguage::Sparql));
    }
}
