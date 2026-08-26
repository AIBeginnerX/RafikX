use anyhow::{Result, anyhow};
use serde_json::{Value, json};

pub fn position(text: &str, line: u32, column: u32) -> Result<Value> {
    if line == 0 || column == 0 {
        return Err(anyhow!("line and column are 1-based"));
    }
    let row = text
        .lines()
        .nth((line - 1) as usize)
        .ok_or_else(|| anyhow!("line {line} is outside the document"))?;
    let prefix = row.chars().take((column - 1) as usize).collect::<String>();
    if prefix.chars().count() != (column - 1) as usize {
        return Err(anyhow!("column {column} is outside line {line}"));
    }
    Ok(json!({
        "line": line - 1,
        "character": prefix.encode_utf16().count(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_position_uses_utf16_columns() {
        assert_eq!(position("a😀b", 1, 3).expect("position")["character"], 3);
    }
}
