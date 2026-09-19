use clap::Parser;
use ignore::WalkBuilder;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

#[derive(Parser, Debug)]
#[command(about = "Find code that matches a concept using TypeSafe Jev")]
struct Args {
    /// Concept to find, for example: "where does user JWT authentication happen?"
    concept: String,
    /// Directory to search (defaults to the current directory)
    #[arg(short, long, default_value = ".")]
    path: PathBuf,
    /// Minimum Jev probability to print a result
    #[arg(short, long, default_value_t = 0.55)]
    threshold: f64,
    /// Minimum path-only probability required before reading a file
    #[arg(long, default_value_t = 0.25)]
    file_threshold: f64,
    /// Jev model alias
    #[arg(long, default_value = "jev-latest")]
    model: String,
    /// Show a live colored file tree while scanning
    #[arg(long)]
    debug: bool,
    /// Include hidden files and directories
    #[arg(long)]
    hidden: bool,
    /// Maximum number of files scanned concurrently
    #[arg(long, default_value_t = 10)]
    parallelism: usize,
}

const WINDOW: usize = 100;
const OVERLAP: usize = 20;
const MAX_WINDOW_BYTES: usize = 12_000;
const MAX_REQUEST_SOURCE_BYTES: usize = 24_000;
const MAX_WINDOWS_PER_REQUEST: usize = 24;
const FILE_BATCH: usize = 200;
const MAX_RETRIES: usize = 3;
const API_URL: &str = "https://api.typesafe.ai/v1/systemone";
const RESET: &str = "\x1b[0m";
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".next",
    ".venv",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "out",
    "target",
    "vendor",
    "venv",
];

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if !(0.0..=1.0).contains(&args.threshold)
        || !(0.0..=1.0).contains(&args.file_threshold)
    {
        return Err("thresholds must be between 0 and 1".into());
    }
    if args.parallelism == 0 {
        return Err("parallelism must be at least 1".into());
    }
    let key = env::var("TYPESAFE_API_KEY").map_err(|_| "TYPESAFE_API_KEY is required")?;
    let client = Client::new();
    let candidates = code_files(&args.path, args.hidden)?;
    let mut debug = args
        .debug
        .then(|| DebugTree::new(args.path.clone(), &candidates));
    if let Some(tree) = debug.as_mut() {
        tree.start()?;
    }
    let files = relevant_files(&client, &key, &args, candidates, debug.as_mut())?;
    let mut found = false;
    let mut results = Vec::new();

    for batch in files.chunks(args.parallelism) {
        if let Some(tree) = debug.as_mut() {
            for (file, path_probability) in batch {
                tree.set(file, FileStatus::Scanning(*path_probability))?;
            }
        }
        let scans = thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|(file, path_probability)| {
                    scope.spawn(|| scan_file(&client, &key, &args, file, *path_probability))
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "file scan worker panicked".to_string())?
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .map_err(io::Error::other)?;

        for scan in scans {
            if scan.unreadable {
                if let Some(tree) = debug.as_mut() {
                    tree.set(&scan.file, FileStatus::Unreadable(scan.path_probability))?;
                }
                continue;
            }
            if !scan.matches.is_empty() {
                found = true;
                if args.debug {
                    results.extend(scan.matches);
                } else {
                    for result in scan.matches {
                        println!("{result}");
                    }
                }
            }
            if let Some(tree) = debug.as_mut() {
                let status = if scan.best_match >= args.threshold {
                    FileStatus::Found(scan.path_probability, scan.best_match)
                } else {
                    FileStatus::NoMatch(scan.path_probability)
                };
                tree.set(&scan.file, status)?;
            }
        }
    }

    if args.debug && !results.is_empty() {
        println!("\n\x1b[1;32mMatches\x1b[0m");
        for result in results {
            println!("{result}");
        }
    }

    if !found {
        println!("No matches found for: {}", args.concept);
    }

    Ok(())
}

struct FileScan {
    file: PathBuf,
    path_probability: f64,
    best_match: f64,
    matches: Vec<String>,
    unreadable: bool,
}

fn scan_file(
    client: &Client,
    key: &str,
    args: &Args,
    file: &Path,
    path_probability: f64,
) -> Result<FileScan, String> {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(_) => {
            return Ok(FileScan {
                file: file.to_path_buf(),
                path_probability,
                best_match: 0.0,
                matches: Vec::new(),
                unreadable: true,
            });
        }
    };
    let windows = windows(&text);
    if windows.is_empty() {
        return Ok(FileScan {
            file: file.to_path_buf(),
            path_probability,
            best_match: 0.0,
            matches: Vec::new(),
            unreadable: false,
        });
    }

    let mut best_match: f64 = 0.0;
    let mut matches = Vec::new();
    for batch in window_batches(&windows) {
        let mut questions = serde_json::Map::new();
        let state_windows: Vec<_> = batch
            .iter()
            .enumerate()
            .map(|(i, window)| {
                questions.insert(
                    format!("window_{i}"),
                    json!({
                        "type": "noul",
                        "instructions": format!(
                            "Does code window window_{i} contain, implement, configure, call, or enforce {}? Return yes only when it is materially relevant.",
                            args.concept
                        )
                    }),
                );
                json!({
                    "id": format!("window_{i}"),
                    "start_line": window.start,
                    "end_line": window.end,
                    "code": window.snippet,
                })
            })
            .collect();
        let body = json!({
            "model": args.model,
            "state": { "file": file.display().to_string(), "windows": state_windows },
            "questions": questions,
        });
        let response = send_jev(client, key, &body)
            .map_err(|error| format!("{}: {error}", file.display()))?;

        for (i, window) in batch.iter().enumerate() {
            let answer = response
                .get("answers")
                .and_then(|answers| answers.get(format!("window_{i}")))
                .unwrap_or(&Value::Null);
            let probability = noul_probability(answer);
            best_match = best_match.max(probability);
            if probability >= args.threshold {
                matches.push(format!(
                    "{}:{}-{} ({:.0}%)\n{}\n",
                    file.display(),
                    window.start,
                    window.end,
                    probability * 100.0,
                    window.snippet
                ));
            }
        }
    }
    Ok(FileScan {
        file: file.to_path_buf(),
        path_probability,
        best_match,
        matches,
        unreadable: false,
    })
}

fn relevant_files(
    client: &Client,
    key: &str,
    args: &Args,
    files: Vec<PathBuf>,
    mut debug: Option<&mut DebugTree>,
) -> Result<Vec<(PathBuf, f64)>, Box<dyn Error>> {
    let mut relevant = Vec::new();
    for batch in files.chunks(FILE_BATCH) {
        let mut questions = serde_json::Map::new();
        for (i, file) in batch.iter().enumerate() {
            questions.insert(
                format!("file_{i}"),
                json!({
                    "type": "noul",
                    "instructions": format!(
                        "Based only on its path, is file '{}' likely to contain code relevant to the requested concept?",
                        file.display()
                    )
                }),
            );
        }
        if let Some(tree) = debug.as_mut() {
            for file in batch {
                tree.set(file, FileStatus::PathScoring)?;
            }
        }

        let body = json!({
            "model": args.model,
            "state": { "requested_concept": args.concept },
            "questions": questions,
        });
        let response = send_jev(client, key, &body).map_err(io::Error::other)?;

        for (i, file) in batch.iter().enumerate() {
            let probability = response
                .get("answers")
                .and_then(|answers| answers.get(format!("file_{i}")))
                .map(noul_probability)
                .unwrap_or(0.0);
            if probability >= args.file_threshold {
                relevant.push((file.clone(), probability));
                if let Some(tree) = debug.as_mut() {
                    tree.set(file, FileStatus::Queued(probability))?;
                }
            } else if let Some(tree) = debug.as_mut() {
                tree.set(file, FileStatus::Skipped(probability))?;
            }
        }
    }
    Ok(relevant)
}

fn send_jev(client: &Client, key: &str, body: &Value) -> Result<Value, String> {
    let mut last_error = String::new();
    for retry in 0..=MAX_RETRIES {
        match client.post(API_URL).bearer_auth(key).json(body).send() {
            Ok(response) if response.status().is_success() => match response.json() {
                Ok(value) => return Ok(value),
                Err(error) => last_error = format!("invalid response: {error}"),
            },
            Ok(response) => {
                let status = response.status();
                let detail = response.text().unwrap_or_default();
                last_error = if detail.is_empty() {
                    format!("HTTP {status}")
                } else {
                    format!("HTTP {status}: {}", detail.chars().take(500).collect::<String>())
                };
                if status.as_u16() != 408 && status.as_u16() != 429 && !status.is_server_error() {
                    return Err(last_error);
                }
            }
            Err(error) => last_error = error.to_string(),
        }
        if retry < MAX_RETRIES {
            thread::sleep(retry_delay(retry));
        }
    }
    Err(format!(
        "request failed after {} attempts: {last_error}",
        MAX_RETRIES + 1
    ))
}

fn retry_delay(retry: usize) -> Duration {
    Duration::from_millis(250 * (1 << retry))
}

enum FileStatus {
    Pending,
    PathScoring,
    Skipped(f64),
    Queued(f64),
    Scanning(f64),
    NoMatch(f64),
    Found(f64, f64),
    Unreadable(f64),
}

struct DebugTree {
    root: PathBuf,
    files: BTreeMap<PathBuf, FileStatus>,
    rows: BTreeMap<PathBuf, (usize, String)>,
    line_count: usize,
}

impl DebugTree {
    fn new(root: PathBuf, files: &[PathBuf]) -> Self {
        Self {
            root,
            files: files
                .iter()
                .cloned()
                .map(|file| (file, FileStatus::Pending))
                .collect(),
            rows: BTreeMap::new(),
            line_count: 0,
        }
    }

    fn set(&mut self, file: &Path, status: FileStatus) -> io::Result<()> {
        self.files.insert(file.to_path_buf(), status);
        if let Some((row, prefix)) = self.rows.get(file) {
            let up = self.line_count - row;
            print!(
                "\x1b[{up}A\r\x1b[2K{prefix} {}\x1b[{up}B\r",
                self.files[file].label()
            );
            io::stdout().flush()?;
        }
        Ok(())
    }

    fn start(&mut self) -> io::Result<()> {
        println!(
            "\x1b[1;35mjev-code-finder\x1b[0m  {}",
            self.root.display()
        );
        self.line_count = 1;
        let mut previous_dirs = Vec::<String>::new();
        for (file, status) in &self.files {
            let relative = file
                .strip_prefix(&self.root)
                .ok()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(file);
            let parts: Vec<String> = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            let dirs = &parts[..parts.len().saturating_sub(1)];
            let shared = dirs
                .iter()
                .zip(&previous_dirs)
                .take_while(|(left, right)| left == right)
                .count();
            for (depth, dir) in dirs.iter().enumerate().skip(shared) {
                println!("{}\x1b[1;34m▾ {dir}/{RESET}", "│  ".repeat(depth));
                self.line_count += 1;
            }
            let name = parts.last().map(String::as_str).unwrap_or("?");
            let prefix = format!("{}├─ {:<34}", "│  ".repeat(dirs.len()), name);
            println!("{prefix} {}", status.label());
            self.rows
                .insert(file.clone(), (self.line_count, prefix));
            self.line_count += 1;
            previous_dirs = dirs.to_vec();
        }
        io::stdout().flush()
    }
}

impl FileStatus {
    fn label(&self) -> String {
        match self {
            Self::Pending => "\x1b[2m○ pending\x1b[0m".into(),
            Self::PathScoring => "\x1b[5;36m●\x1b[0m \x1b[36mscoring path\x1b[0m".into(),
            Self::Skipped(p) => format!("\x1b[31m× {:>3.0}% skipped\x1b[0m", p * 100.0),
            Self::Queued(p) => format!("\x1b[33m○ {:>3.0}% queued\x1b[0m", p * 100.0),
            Self::Scanning(p) => format!(
                "\x1b[33m{:>3.0}%\x1b[0m \x1b[5;36m●\x1b[0m \x1b[36mscanning\x1b[0m",
                p * 100.0
            ),
            Self::NoMatch(p) => format!("\x1b[2m· {:>3.0}% no match\x1b[0m", p * 100.0),
            Self::Found(path, matched) => format!(
                "\x1b[1;32m✓ {:>3.0}% path · {:>3.0}% match\x1b[0m",
                path * 100.0,
                matched * 100.0
            ),
            Self::Unreadable(p) => {
                format!("\x1b[31m! {:>3.0}% unreadable\x1b[0m", p * 100.0)
            }
        }
    }
}

fn code_files(root: &Path, include_hidden: bool) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut files = Vec::new();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!include_hidden)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .parents(true)
        .require_git(false)
        .filter_entry(|entry| !entry.path().is_dir() || !is_skipped_dir(entry.path()));
    for entry in builder.build() {
        let entry = entry?;
        if entry.file_type().is_some_and(|kind| kind.is_file())
            && entry.path().extension().is_some_and(is_code_extension)
        {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(files)
}

fn is_skipped_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SKIP_DIRS.contains(&name))
}

fn is_code_extension(extension: &std::ffi::OsStr) -> bool {
    matches!(
        extension.to_str(),
        Some(
            "rs" | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "py"
                | "go"
                | "java"
                | "kt"
                | "kts"
                | "rb"
                | "php"
                | "c"
                | "h"
                | "cpp"
                | "hpp"
                | "cs"
                | "swift"
                | "scala"
                | "sh"
                | "sql"
                | "yaml"
                | "yml"
                | "json"
                | "toml"
        )
    )
}

struct CodeWindow {
    start: usize,
    end: usize,
    snippet: String,
}

fn windows(text: &str) -> Vec<CodeWindow> {
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let end = (start + WINDOW).min(lines.len());
        push_bounded_windows(&mut result, &lines[start..end], start + 1);
        if end == lines.len() {
            break;
        }
        start = end.saturating_sub(OVERLAP);
    }
    result
}

fn push_bounded_windows(result: &mut Vec<CodeWindow>, lines: &[&str], first_line: usize) {
    let mut snippet = String::new();
    let mut start = first_line;
    let mut end = first_line;
    let mut has_lines = false;

    for (offset, line) in lines.iter().enumerate() {
        let line_number = first_line + offset;
        if line.len() > MAX_WINDOW_BYTES {
            if has_lines {
                result.push(CodeWindow {
                    start,
                    end,
                    snippet: std::mem::take(&mut snippet),
                });
                has_lines = false;
            }
            for part in utf8_chunks(line, MAX_WINDOW_BYTES) {
                result.push(CodeWindow {
                    start: line_number,
                    end: line_number,
                    snippet: part.to_owned(),
                });
            }
            continue;
        }

        let added = line.len() + usize::from(has_lines);
        if has_lines && snippet.len() + added > MAX_WINDOW_BYTES {
            result.push(CodeWindow {
                start,
                end,
                snippet: std::mem::take(&mut snippet),
            });
            has_lines = false;
        }
        if has_lines {
            snippet.push('\n');
        } else {
            start = line_number;
        }
        snippet.push_str(line);
        end = line_number;
        has_lines = true;
    }

    if has_lines {
        result.push(CodeWindow {
            start,
            end,
            snippet,
        });
    }
}

fn utf8_chunks(mut text: &str, max_bytes: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    while !text.is_empty() {
        let mut end = text.len().min(max_bytes);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[..end]);
        text = &text[end..];
    }
    chunks
}

fn window_batches(windows: &[CodeWindow]) -> Vec<&[CodeWindow]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (index, window) in windows.iter().enumerate() {
        if index > start
            && (index - start >= MAX_WINDOWS_PER_REQUEST
                || bytes + window.snippet.len() > MAX_REQUEST_SOURCE_BYTES)
        {
            batches.push(&windows[start..index]);
            start = index;
            bytes = 0;
        }
        bytes += window.snippet.len();
    }
    if start < windows.len() {
        batches.push(&windows[start..]);
    }
    batches
}

fn noul_probability(answer: &Value) -> f64 {
    answer
        .get("noul")
        .and_then(Value::as_f64)
        .or_else(|| answer.get("probability").and_then(Value::as_f64))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_have_one_based_ranges_and_overlap() {
        let text = (1..=180)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = windows(&text);
        assert_eq!((result[0].start, result[0].end), (1, 100));
        assert_eq!((result[1].start, result[1].end), (81, 180));
    }

    #[test]
    fn large_minified_files_are_split_into_bounded_requests() {
        let text = "é".repeat(MAX_WINDOW_BYTES * 8);
        let windows = windows(&text);
        assert!(windows.len() > 1);
        assert_eq!(
            windows
                .iter()
                .map(|window| window.snippet.as_str())
                .collect::<String>(),
            text
        );
        assert!(windows.iter().all(|window| {
            window.start == 1
                && window.end == 1
                && window.snippet.len() <= MAX_WINDOW_BYTES
                && window.snippet.is_char_boundary(window.snippet.len())
        }));
        assert!(window_batches(&windows).iter().all(|batch| {
            batch.len() <= MAX_WINDOWS_PER_REQUEST
                && batch.iter().map(|window| window.snippet.len()).sum::<usize>()
                    <= MAX_REQUEST_SOURCE_BYTES
        }));
    }

    #[test]
    fn generated_and_dependency_directories_are_skipped() {
        assert!(is_skipped_dir(Path::new("project/node_modules")));
        assert!(is_skipped_dir(Path::new("project/target")));
        assert!(!is_skipped_dir(Path::new("project/src")));
    }

    #[test]
    fn debug_status_shows_path_and_match_percentages() {
        let label = FileStatus::Found(0.42, 0.91).label();
        assert!(label.contains("42% path"));
        assert!(label.contains("91% match"));
    }

    #[test]
    fn gitignored_and_hidden_files_are_filtered() {
        let root = env::temp_dir().join(format!("jev-code-finder-{}", std::process::id()));
        fs::create_dir_all(root.join("ignored")).unwrap();
        fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        fs::write(root.join("visible.rs"), "fn visible() {}\n").unwrap();
        fs::write(root.join(".hidden.rs"), "fn hidden() {}\n").unwrap();
        fs::write(root.join("ignored/no.rs"), "fn ignored() {}\n").unwrap();

        let visible = code_files(&root, false).unwrap();
        assert_eq!(visible, vec![root.join("visible.rs")]);
        let with_hidden = code_files(&root, true).unwrap();
        assert_eq!(
            with_hidden,
            vec![root.join(".hidden.rs"), root.join("visible.rs")]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parallelism_defaults_to_ten() {
        let args = Args::try_parse_from(["jev-code-finder", "authentication"]).unwrap();
        assert_eq!(args.parallelism, 10);
    }

    #[test]
    fn retries_use_exponential_backoff() {
        assert_eq!(MAX_RETRIES, 3);
        assert_eq!(retry_delay(0), Duration::from_millis(250));
        assert_eq!(retry_delay(1), Duration::from_millis(500));
        assert_eq!(retry_delay(2), Duration::from_millis(1000));
    }
}
