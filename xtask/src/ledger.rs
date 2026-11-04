//! Parsing of `e2e/LEDGER.md`. The e2e suite runs once a day in the Daily GitHub Actions
//! workflow (AGENTS.md §4); the "since last e2e run" counter is informational.

/// Returns the "Since last e2e run" value of the last row in the "Processed Step issues" table.
pub fn since_last_run(ledger: &str) -> Result<u32, String> {
    let mut in_table = false;
    let mut last: Option<u32> = None;
    for line in ledger.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            in_table = trimmed == "## Processed Step issues";
            continue;
        }
        if !in_table || !trimmed.starts_with('|') || trimmed.starts_with("| #") {
            continue;
        }
        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells
            .iter()
            .all(|c| c.chars().all(|ch| ch == '-' || ch == ':'))
        {
            continue; // separator row
        }
        let value = cells
            .last()
            .ok_or_else(|| format!("malformed ledger row: {trimmed}"))?;
        last = Some(
            value
                .parse()
                .map_err(|_| format!("ledger row has non-numeric counter `{value}`: {trimmed}"))?,
        );
    }
    last.ok_or_else(|| "e2e/LEDGER.md has no processed Step rows".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# E2E ledger
## Processed Step issues

| # | Step issue | Features added to e2e | Since last e2e run |
| --- | --- | --- | --- |
| 1 | #1 Step 01 | none | 1 |
| 2 | #2 Step 02 | placeholder CLIs | 2 |

## E2E runs

| Date | After Step | Command | Result |
| --- | --- | --- | --- |
| 2026-01-01 | — | — | 99 |
";

    #[test]
    fn reads_counter_from_last_processed_row_only() {
        assert_eq!(since_last_run(SAMPLE), Ok(2));
    }

    #[test]
    fn rejects_ledger_without_rows() {
        assert!(since_last_run("## Processed Step issues\n| # | a |\n| --- | --- |\n").is_err());
    }

    #[test]
    fn rejects_non_numeric_counter() {
        let bad = "## Processed Step issues\n| 1 | x | y | many |\n";
        assert!(since_last_run(bad).unwrap_err().contains("non-numeric"));
    }

    #[test]
    fn real_ledger_parses() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../e2e/LEDGER.md");
        let text = std::fs::read_to_string(path).expect("ledger exists");
        // The suite runs daily (AGENTS.md §4); the counter is informational.
        since_last_run(&text).expect("ledger parses");
    }
}
