use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use pulldown_cmark::{Event, Parser, TagEnd};
use regex::Regex;
use notify_debouncer_mini::{DebounceEventResult, new_debouncer, notify::RecursiveMode};

use crate::config::{self, Config};
use crate::db::{Db, NoteHit};

pub struct IndexStats {
    pub updated: usize,
    pub skipped: usize,
    pub deleted: usize,
}

pub struct AskContext {
    pub block: String,
    pub sources: Vec<String>,
}

pub fn index_vault(cfg: &Config) -> Result<IndexStats> {
    let vault = vault_dir(cfg)?;
    let db = open_notes_db(cfg)?;
    let rebuilt = db.ensure_notes_fts(&cfg.file.obsidian.tokenizer)?;
    if rebuilt {
        eprintln!("tokenizer가 바뀌어 노트를 처음부터 다시 인덱싱합니다.");
    }

    let files = scan_md_files(&vault)?;
    let mut seen = HashSet::new();
    let mut updated = 0usize;
    let mut skipped = 0usize;

    for path in &files {
        let rel = rel_path(&vault, path);
        seen.insert(rel.clone());
        match upsert_file(&db, &vault, path, false)? {
            Upsert::Updated => updated += 1,
            Upsert::Skipped => skipped += 1,
            Upsert::Deleted => {}
        }
    }

    let mut deleted = 0usize;
    for old in db.all_note_paths()? {
        if !seen.contains(&old) {
            if db.delete_note(&old)? {
                deleted += 1;
            }
        }
    }

    Ok(IndexStats {
        updated,
        skipped,
        deleted,
    })
}

pub fn search_print(cfg: &Config, query: &str) -> Result<()> {
    let db = open_notes_db(cfg)?;
    db.ensure_notes_fts(&cfg.file.obsidian.tokenizer)?;
    let hits = db.search_notes(query, 10)?;
    if hits.is_empty() {
        println!("검색 결과 없음");
        return Ok(());
    }
    for (i, h) in hits.iter().enumerate() {
        println!("{}. {}  ({})", i + 1, h.title, h.path);
        if !h.tags.is_empty() {
            println!("   tags: {}", h.tags);
        }
        if !h.excerpt.trim().is_empty() {
            println!("   {}", h.excerpt.replace('\n', " "));
        }
    }
    Ok(())
}

pub fn format_tool_results(hits: &[NoteHit]) -> String {
    if hits.is_empty() {
        return "(검색 결과 없음)".into();
    }
    let mut out = String::new();
    for h in hits {
        out.push_str(&format!("제목: {}\n경로: {}\n", h.title, h.path));
        if !h.tags.is_empty() {
            out.push_str(&format!("태그: {}\n", h.tags));
        }
        out.push_str(&format!("발췌: {}\n\n", h.excerpt.replace('\n', " ")));
    }
    out
}

pub fn ask_context(cfg: &Config, query: &str) -> Result<AskContext> {
    let db = open_notes_db(cfg)?;
    db.ensure_notes_fts(&cfg.file.obsidian.tokenizer)?;
    let limit_chars = cfg.file.obsidian.context_limit_chars as usize;
    let hits = db.search_notes(query, 5)?;
    if hits.is_empty() {
        return Ok(AskContext {
            block: "[Obsidian 컨텍스트]\n인덱스가 비어 있거나 일치하는 노트가 없습니다. 먼저 `agent-harness index` 를 실행하세요.".into(),
            sources: Vec::new(),
        });
    }

    let mut sources: Vec<String> = Vec::new();
    let mut used_paths: HashSet<String> = HashSet::new();
    let mut body = String::from("[Obsidian 컨텍스트]\n다음 노트 발췌를 근거로 답하고, 출처 경로를 밝혀라.\n");
    let mut extra_slots = 3usize;

    for h in &hits {
        if !used_paths.insert(h.path.clone()) {
            continue;
        }
        let content = db
            .note_content(&h.path)?
            .unwrap_or_else(|| h.excerpt.clone());
        append_capped(&mut body, &h.path, &h.title, &content, limit_chars);
        sources.push(h.path.clone());

        if extra_slots == 0 {
            continue;
        }
        let targets = link_targets(&h.path, &h.title);
        let backs = db.backlinks(&targets, &h.path, extra_slots)?;
        for b in backs {
            if !used_paths.insert(b.path.clone()) {
                continue;
            }
            if extra_slots == 0 {
                break;
            }
            extra_slots -= 1;
            let content = db.note_content(&b.path)?.unwrap_or_default();
            append_capped(&mut body, &b.path, &b.title, &content, limit_chars);
            sources.push(b.path);
        }
        if body.chars().count() >= limit_chars {
            break;
        }
    }

    Ok(AskContext { block: body, sources })
}

pub async fn watch_vault(cfg: &Config) -> Result<()> {
    let vault = vault_dir(cfg)?;
    let db = open_notes_db(cfg)?;
    db.ensure_notes_fts(&cfg.file.obsidian.tokenizer)?;
    println!("Vault 감시 중: {}  (Ctrl+C 종료)", vault.display());

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_secs(1), move |res: DebounceEventResult| {
        let _ = tx.send(res);
    })
    .map_err(|e| anyhow!("파일 감시를 시작하지 못했습니다: {e}"))?;
    debouncer
        .watcher()
        .watch(&vault, RecursiveMode::Recursive)
        .map_err(|e| anyhow!("Vault를 감시할 수 없습니다: {e}"))?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("감시를 종료합니다.");
                break;
            }
            Some(res) = rx.recv() => {
                match res {
                    Ok(events) => {
                        for ev in events {
                            if let Err(e) = handle_watch_event(&db, &vault, &ev.path) {
                                eprintln!("감시 처리 오류 ({}): {e}", ev.path.display());
                            }
                        }
                    }
                    Err(e) => eprintln!("감시 오류: {e:?}"),
                }
            }
        }
    }
    Ok(())
}

fn handle_watch_event(db: &Db, vault: &Path, path: &Path) -> Result<()> {
    if is_skipped_path(vault, path) {
        return Ok(());
    }
    if path.is_dir() {
        return Ok(());
    }
    let is_md = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false);
    if path.exists() {
        if !is_md {
            return Ok(());
        }
        match upsert_file(db, vault, path, true)? {
            Upsert::Updated => println!("갱신: {}", rel_path(vault, path)),
            Upsert::Skipped => {}
            Upsert::Deleted => {}
        }
    } else if is_md || path.extension().is_none() {
        let rel = rel_path(vault, path);
        if !rel.ends_with(".md") {
            return Ok(());
        }
        if db.delete_note(&rel)? {
            println!("삭제: {rel}");
        }
    }
    Ok(())
}

enum Upsert {
    Updated,
    Skipped,
    Deleted,
}

fn upsert_file(db: &Db, vault: &Path, path: &Path, retry: bool) -> Result<Upsert> {
    if !path.exists() {
        let rel = rel_path(vault, path);
        if db.delete_note(&rel)? {
            return Ok(Upsert::Deleted);
        }
        return Ok(Upsert::Skipped);
    }
    let mtime = file_mtime(path)?;
    let rel = rel_path(vault, path);
    if db.note_mtime(&rel)? == Some(mtime) {
        return Ok(Upsert::Skipped);
    }
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) if retry => {
            std::thread::sleep(Duration::from_millis(200));
            fs::read_to_string(path).with_context(|| format!("{} 를 읽을 수 없습니다", path.display()))?
        }
        Err(e) => return Err(e).with_context(|| format!("{} 를 읽을 수 없습니다", path.display())),
    };
    let parsed = parse_note(&rel, &raw);
    db.upsert_note(
        &rel,
        &parsed.title,
        &parsed.tags,
        &parsed.links,
        &parsed.content,
        mtime,
    )?;
    Ok(Upsert::Updated)
}

pub struct ParsedNote {
    pub title: String,
    pub tags: String,
    pub links: String,
    pub content: String,
}

pub fn parse_note(rel_path: &str, raw: &str) -> ParsedNote {
    let (fm, body) = split_frontmatter(raw);
    let mut tags: Vec<String> = Vec::new();
    let mut fm_title = None;
    if let Some(fm) = fm {
        let (t, title) = parse_frontmatter(&fm);
        tags.extend(t);
        fm_title = title;
    }
    tags.extend(hash_tags(body));
    let links = wiki_links(body);
    let title = fm_title
        .or_else(|| first_heading(body))
        .unwrap_or_else(|| stem_title(rel_path));
    let content = md_to_text(body);
    tags.sort();
    tags.dedup();
    ParsedNote {
        title,
        tags: tags.join(","),
        links: links.join(","),
        content,
    }
}

fn split_frontmatter(raw: &str) -> (Option<String>, &str) {
    let s = raw.trim_start_matches('\u{feff}');
    let rest = if let Some(r) = s.strip_prefix("---\r\n") {
        r
    } else if let Some(r) = s.strip_prefix("---\n") {
        r
    } else {
        return (None, s);
    };
    let close = rest
        .find("\n---")
        .map(|i| (i, 4))
        .or_else(|| rest.find("\r\n---").map(|i| (i, 5)));
    let Some((idx, skip)) = close else {
        return (None, s);
    };
    let fm = rest[..idx].to_string();
    let after = &rest[idx + skip..];
    let body = after.trim_start_matches(['\r', '\n']);
    (Some(fm), body)
}

fn parse_frontmatter(fm: &str) -> (Vec<String>, Option<String>) {
    let mut tags = Vec::new();
    let mut title = None;
    let mut in_tags_list = false;
    for line in fm.lines() {
        let trimmed = line.trim();
        if in_tags_list {
            if let Some(item) = trimmed.strip_prefix("- ") {
                push_tag(&mut tags, item.trim());
                continue;
            }
            in_tags_list = false;
        }
        if let Some(rest) = stripped_key(trimmed, "title:") {
            if !rest.is_empty() {
                title = Some(unquote(rest));
            }
        } else if let Some(rest) = stripped_key(trimmed, "tags:") {
            if rest.is_empty() {
                in_tags_list = true;
            } else if rest.starts_with('[') {
                let inner = rest.trim_matches(|c| c == '[' || c == ']');
                for part in inner.split(',') {
                    push_tag(&mut tags, part.trim());
                }
            } else {
                push_tag(&mut tags, rest);
            }
        }
    }
    (tags, title)
}

fn stripped_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.strip_prefix(key).map(|s| s.trim())
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    if (t.starts_with('"') && t.ends_with('"') && t.len() >= 2)
        || (t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2)
    {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

fn push_tag(tags: &mut Vec<String>, raw: &str) {
    let t = unquote(raw).trim_start_matches('#').trim().to_string();
    if !t.is_empty() {
        tags.push(t);
    }
}

fn hash_tags(body: &str) -> Vec<String> {
    let re = Regex::new(r"(?:^|[\s(])#([^\s#\[\]]+)").expect("hash tag regex");
    re.captures_iter(body)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().trim_end_matches(|c: char| matches!(c, '.' | ',' | '!' | '?' | ':' | ';')))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn wiki_links(body: &str) -> Vec<String> {
    let re = Regex::new(r"\[\[([^\]|#]+)(?:[|#][^\]]*)?\]\]").expect("wikilink regex");
    re.captures_iter(body)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn first_heading(body: &str) -> Option<String> {
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("# ") {
            let title = rest.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

fn stem_title(rel: &str) -> String {
    Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(rel)
        .to_string()
}

fn md_to_text(md: &str) -> String {
    let mut out = String::new();
    for event in Parser::new(md) {
        match event {
            Event::Text(t) | Event::Code(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) => out.push('\n'),
            Event::Rule => out.push('\n'),
            _ => {}
        }
    }
    out
}

fn scan_md_files(vault: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(vault)
        .hidden(true)
        .git_ignore(false)
        .git_exclude(false)
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if is_skipped_path(vault, path) {
            continue;
        }
        let md = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("md"))
            .unwrap_or(false);
        if md {
            files.push(path.to_path_buf());
        }
    }
    Ok(files)
}

fn is_skipped_path(vault: &Path, path: &Path) -> bool {
    let Ok(stripped) = path.strip_prefix(vault) else {
        if is_temp_name(path) {
            return true;
        }
        return path.components().any(|c| {
            matches!(c.as_os_str().to_str(), Some(".obsidian" | ".trash"))
        });
    };
    if stripped.components().any(|c| {
        matches!(c.as_os_str().to_str(), Some(".obsidian" | ".trash"))
    }) {
        return true;
    }
    is_temp_name(path)
}

fn is_temp_name(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    name.starts_with('.')
        || name.ends_with('~')
        || name.starts_with("~$")
        || name.ends_with(".tmp")
        || name.ends_with(".temp")
        || name.ends_with(".swp")
        || name.ends_with(".swo")
        || (name.starts_with('#') && name.ends_with('#'))
}

fn rel_path(vault: &Path, path: &Path) -> String {
    path.strip_prefix(vault)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn file_mtime(path: &Path) -> Result<i64> {
    let m = fs::metadata(path)?.modified()?;
    Ok(m.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64)
}

fn vault_dir(cfg: &Config) -> Result<PathBuf> {
    let vault = config::expand_tilde(&cfg.file.obsidian.vault_path);
    if !vault.exists() {
        fs::create_dir_all(&vault)
            .with_context(|| format!("{} 폴더를 만들 수 없습니다", vault.display()))?;
        eprintln!("Vault 폴더를 만들었습니다: {}", vault.display());
    }
    Ok(vault)
}

pub fn open_notes_db(cfg: &Config) -> Result<Db> {
    let p = config::expand_tilde(&cfg.file.obsidian.db_path);
    Db::open(&p)
}

fn link_targets(path: &str, title: &str) -> Vec<String> {
    let mut v = vec![title.to_string()];
    let no_ext = path.trim_end_matches(".md");
    v.push(no_ext.to_string());
    if let Some(stem) = Path::new(path).file_stem().and_then(|s| s.to_str()) {
        v.push(stem.to_string());
    }
    v.sort();
    v.dedup();
    v
}

fn append_capped(body: &mut String, path: &str, title: &str, content: &str, limit: usize) {
    if body.chars().count() >= limit {
        return;
    }
    let mut chunk = format!("\n## {title}  ({path})\n{content}\n");
    let used = body.chars().count();
    if used + chunk.chars().count() > limit {
        let remain = limit.saturating_sub(used);
        chunk = chunk.chars().take(remain).collect();
    }
    body.push_str(&chunk);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_and_list_tags() {
        let inline = parse_note(
            "a.md",
            "---\ntitle: Hello\ntags: [alpha, beta]\n---\n# ignored\n#project [[WikiPage]]\n",
        );
        assert_eq!(inline.title, "Hello");
        assert!(inline.tags.contains("alpha"));
        assert!(inline.tags.contains("beta"));
        assert!(inline.tags.contains("project"));
        assert_eq!(inline.links, "WikiPage");

        let list = parse_note(
            "b.md",
            "---\ntags:\n  - one\n  - two\n---\nbody\n",
        );
        assert!(list.tags.contains("one"));
        assert!(list.tags.contains("two"));
        assert_eq!(list.title, "b");
    }

    #[test]
    fn fts_escape_never_raw() {
        let q = crate::db::build_fts_query("foo AND bar");
        assert!(q.contains('"'));
        assert_eq!(q, "\"foo\" \"AND\" \"bar\"");
    }
}
