use lsp_types::{Range, Url};
use std::{
    cmp::min,
    collections::{BTreeMap, HashMap},
};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader},
};
use tokio_stream::{wrappers::LinesStream, StreamExt};

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
    content: HashMap<Url, Vec<String>>,
}

impl SnippetFetcher {
    pub async fn load_file(&mut self, uri: Url) -> std::io::Result<()> {
        if self.content.contains_key(&uri) {
            return Ok(());
        }
        let reader = BufReader::new(File::open(uri.path()).await?);
        self.content.insert(
            uri,
            LinesStream::new(reader.lines())
                .collect::<Result<Vec<String>, std::io::Error>>()
                .await?,
        );
        Ok(())
    }

    pub fn get_line(&self, uri: &Url, line: usize) -> Option<&str> {
        self.content
            .get(uri)
            .and_then(|lines| lines.get(line))
            .map(|v| v.as_str())
    }

    pub fn get_snippet(&self, uri: &Url, range: &Range) -> Option<String> {
        self.content
            .get(uri)
            .filter(|lines| !lines.is_empty())
            .map(|lines| {
                let start_line = min(range.start.line as usize, lines.len() - 1);
                let end_line = min(range.end.line as usize, lines.len() - 1);
                let range_lines = lines[start_line..=end_line]
                    .iter()
                    .zip(start_line..=end_line)
                    .map(|(line, line_index)| {
                        let start_char = match line_index == start_line {
                            true => min(
                                range.start.character as usize,
                                if line.is_empty() { 0 } else { line.len() - 1 },
                            ),
                            false => 0,
                        };
                        let end_char = match line_index == end_line {
                            true => min(range.end.character as usize, line.len()),
                            false => line.len(),
                        };
                        &line[start_char..end_char]
                    })
                    .collect::<Vec<&str>>();
                range_lines.join("\n")
            })
    }

    pub fn line_count(&self, uri: &Url) -> usize {
        self.content.get(uri).map(|v| v.len()).unwrap_or_default()
    }
}
