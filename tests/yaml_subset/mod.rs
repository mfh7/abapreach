//! A reader for the YAML subset the workflow files use: block mappings, block sequences and
//! `|` block scalars. Shared by the workflow guard tests.
#![allow(dead_code)]

/// A parsed node: a scalar, a sequence or a mapping that keeps its key order.
#[derive(Debug, PartialEq)]
pub enum Block {
    Value(String),
    List(Vec<Block>),
    Map(Vec<(String, Block)>),
}

pub fn parse(text: &str) -> Block {
    let mut lines = Lines::new(text);
    lines.block(0)
}

struct Lines {
    lines: Vec<(usize, String)>,
    pos: usize,
}

impl Lines {
    fn new(text: &str) -> Self {
        let lines = text
            .lines()
            .filter_map(|line| {
                let content = line.trim();
                if content.is_empty() || content.starts_with('#') {
                    return None;
                }
                Some((line.len() - line.trim_start().len(), content.to_owned()))
            })
            .collect();
        Self { lines, pos: 0 }
    }

    fn indent(&self) -> Option<usize> {
        self.lines.get(self.pos).map(|(indent, _)| *indent)
    }

    fn content(&self) -> String {
        self.lines[self.pos].1.clone()
    }

    fn block(&mut self, indent: usize) -> Block {
        if self.content().starts_with("- ") || self.content() == "-" {
            self.list(indent)
        } else {
            self.mapping(indent)
        }
    }

    fn list(&mut self, indent: usize) -> Block {
        let mut found = Vec::new();
        while self.indent() == Some(indent)
            && (self.content().starts_with("- ") || self.content() == "-")
        {
            let item = self.content()[1..].trim().to_owned();
            self.pos += 1;
            if item.is_empty() {
                let nested = self.indent().unwrap_or(indent + 2);
                found.push(self.block(nested));
                continue;
            }
            let Some((key, rest)) = split_entry(&item) else {
                found.push(Block::Value(unquote(&item)));
                continue;
            };
            // `- key: value` opens a mapping whose other keys sit two columns further right.
            let entry_indent = indent + 2;
            let mut entries = vec![(key, self.entry(&rest, entry_indent))];
            while self.indent() == Some(entry_indent) {
                let text = self.content();
                let Some((key, rest)) = split_entry(&text) else {
                    break;
                };
                self.pos += 1;
                entries.push((key, self.entry(&rest, entry_indent)));
            }
            found.push(Block::Map(entries));
        }
        Block::List(found)
    }

    fn mapping(&mut self, indent: usize) -> Block {
        let mut entries = Vec::new();
        while self.indent() == Some(indent) {
            let text = self.content();
            let Some((key, rest)) = split_entry(&text) else {
                break;
            };
            self.pos += 1;
            entries.push((key, self.entry(&rest, indent)));
        }
        Block::Map(entries)
    }

    /// What follows `key:` in the entry just at `self.pos`: an inline value, a block scalar, a
    /// nested block, or nothing at all.
    fn entry(&mut self, rest: &str, indent: usize) -> Block {
        if rest.starts_with('|') || rest.starts_with('>') {
            return Block::Value(self.block_scalar(indent));
        }
        if !rest.is_empty() {
            return Block::Value(unquote(rest));
        }
        match self.indent() {
            Some(nested) if nested > indent => self.block(nested),
            _ => Block::Map(Vec::new()),
        }
    }

    fn block_scalar(&mut self, indent: usize) -> String {
        let mut body = Vec::new();
        while let Some(nested) = self.indent() {
            if nested <= indent {
                break;
            }
            body.push(self.content());
            self.pos += 1;
        }
        body.join("\n")
    }
}

fn split_entry(text: &str) -> Option<(String, String)> {
    let at = text.find(':')?;
    let rest = &text[at + 1..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((text[..at].trim().to_owned(), rest.trim().to_owned()))
}

fn unquote(text: &str) -> String {
    for quote in ['"', '\''] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|inner| inner.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    text.to_owned()
}

pub fn map(block: &Block) -> &[(String, Block)] {
    match block {
        Block::Map(entries) => entries,
        other => panic!("expected a mapping, found {other:?}"),
    }
}

pub fn get<'a>(block: &'a Block, key: &str) -> &'a Block {
    find(block, key).unwrap_or_else(|| panic!("missing key `{key}` in {block:?}"))
}

pub fn find<'a>(block: &'a Block, key: &str) -> Option<&'a Block> {
    map(block)
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

pub fn keys(block: &Block) -> Vec<&str> {
    map(block)
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
}

pub fn items(block: &Block) -> &[Block] {
    match block {
        Block::List(items) => items.as_slice(),
        other => panic!("expected a list, found {other:?}"),
    }
}

pub fn scalar(block: &Block) -> &str {
    match block {
        Block::Value(text) => text,
        other => panic!("expected a value, found {other:?}"),
    }
}

pub fn scalars(block: &Block) -> Vec<&str> {
    items(block).iter().map(scalar).collect()
}

pub fn lines(block: &Block) -> Vec<&str> {
    scalar(block)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}
