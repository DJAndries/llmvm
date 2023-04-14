use std::{
    cmp::min,
    collections::{BTreeMap, HashMap},
};

use lsp_types::{Range, Url};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader},
};

#[derive(Debug)]
pub struct SnippetInfo {
    pub description: String,
    pub snippet: String,
}

struct PreSnippetInfo {
    description: String,
    lines: Vec<String>,
}

struct CharRangeInfo {
    range: Range,
    description: String,
    id: usize,
}

type StartLineToCharRangeMap = BTreeMap<u32, Vec<CharRangeInfo>>;

#[derive(Default)]
pub struct SnippetFetcher {
    uri_ranges: HashMap<Url, StartLineToCharRangeMap>,
    last_id: usize,
}

impl SnippetFetcher {
    pub fn add_snippet_request(&mut self, uri: Url, range: Range, description: String) {
        let ranges = self.uri_ranges.entry(uri).or_default();
        let start_line_ranges = ranges.entry(range.start.line).or_default();
        let id = self.last_id + 1;
        start_line_ranges.push(CharRangeInfo {
            range,
            description,
            id,
        });
        self.last_id = id;
    }

    pub async fn load(&self) -> std::io::Result<Vec<SnippetInfo>> {
        let mut pre_snippets = HashMap::new();
        for (uri, start_line_ranges) in &self.uri_ranges {
            let mut open_ranges: Vec<&CharRangeInfo> = Vec::new();
            let mut lines = BufReader::new(File::open(uri.path()).await?).lines();
            let mut line_num = 0;
            while let Some(line) = lines.next_line().await? {
                if let Some(char_ranges) = start_line_ranges.get(&line_num) {
                    open_ranges.extend(char_ranges);
                }
                for open_range in &open_ranges {
                    let pre_snippet =
                        pre_snippets
                            .entry(open_range.id)
                            .or_insert_with(|| PreSnippetInfo {
                                description: open_range.description.clone(),
                                lines: Vec::new(),
                            });
                    let start_index = match open_range.range.start.line == line_num {
                        true => min(line.len(), open_range.range.start.character as usize),
                        false => 0,
                    };
                    let end_index = match open_range.range.end.line == line_num {
                        true => min(line.len(), open_range.range.end.character as usize),
                        false => line.len(),
                    };
                    pre_snippet
                        .lines
                        .push(line[start_index..end_index].to_string());
                }
                open_ranges.retain(|open_range| open_range.range.end.line != line_num);
                line_num += 1;
            }
        }
        Ok(pre_snippets
            .into_values()
            .map(|p| SnippetInfo {
                description: p.description,
                snippet: p.lines.join("\n"),
            })
            .collect())
    }
}
