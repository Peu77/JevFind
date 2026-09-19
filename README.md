# Jev Code Finder

Fast semantic code search powered by [TypeSafe Jev](https://typesafe.ai/). Describe a concept in plain English and get the relevant files, line ranges, confidence scores, and source snippets.

```text
$ jev-code-finder "where does user JWT authentication happen?"

src/auth/middleware.rs:81-180 (92%)
pub async fn authenticate(...)
```

Unlike text search, Jev Code Finder searches for meaning. A query such as `where are permissions enforced?` can find middleware, guards, policy checks, and configuration without requiring you to know their exact names.

## How it works

1. Walks the repository while respecting `.gitignore` and excluding hidden, dependency, cache, and build directories.
2. Asks Jev to score each source path for likely relevance to your query.
3. Opens only paths that meet `--file-threshold` and divides them into overlapping, size-bounded code windows.
4. Sends those windows in bounded request batches, then prints matches meeting `--threshold`.

The path filter keeps unnecessary source code out of later requests and makes large searches cheaper. Set `--file-threshold 0` when recall matters more than speed.

Transient network errors, HTTP 408/429 responses, server errors, and invalid responses are retried up to three times with exponential backoff: 250 ms, 500 ms, then 1 second. Permanent client errors fail immediately.

## Requirements

- A [TypeSafe](https://typesafe.ai/) API key
- Rust 1.88 or newer; the included `rust-toolchain.toml` selects the correct version when using `rustup`
- Network access to `https://api.typesafe.ai`

## Install

Clone the repository, then build and install the binary:

```sh
cargo install --path .
```

Set your API key in the environment:

```sh
export TYPESAFE_API_KEY="your-api-key"
```

Do not commit the key or pass it as a command-line argument.

## Usage

Search the current directory:

```sh
jev-code-finder "where does user JWT authentication happen?"
```

Search a specific directory:

```sh
jev-code-finder "where are database transactions committed?" --path ./backend
```

Require stronger confidence for both file selection and final matches:

```sh
jev-code-finder "rate limiting" \
  --file-threshold 0.40 \
  --threshold 0.75 \
  --parallelism 10
```

Run without installing:

```sh
cargo run -- "how are failed payments retried?" --path .
```

## Live debug tree

Use `--debug` to display a colored file tree that updates in place:

```sh
jev-code-finder "authorization checks" --debug
```

The status symbols are:

| Symbol | Meaning |
| --- | --- |
| `○` | Pending or queued |
| `●` | Jev is scoring or scanning the file |
| `×` | Path relevance was below `--file-threshold` |
| `·` | File was scanned but no window met `--threshold` |
| `✓` | At least one matching window was found |
| `!` | File could not be read as UTF-8 text |

Completed files show their path-relevance percentage. Matches additionally show the strongest matching-window percentage.

## Options

| Option | Default | Description |
| --- | --- | --- |
| `<CONCEPT>` | Required | Natural-language concept to locate |
| `-p, --path <PATH>` | `.` | File or directory to search |
| `--file-threshold <0..1>` | `0.25` | Minimum path relevance before opening a file |
| `-t, --threshold <0..1>` | `0.55` | Minimum probability for printing a code window |
| `--model <MODEL>` | `jev-latest` | TypeSafe model alias |
| `--debug` | Off | Show the live colored file tree |
| `--hidden` | Off | Include hidden files and directories |
| `--parallelism <N>` | `10` | Maximum concurrent file-scanning requests |
| `-h, --help` | — | Show command help |

Thresholds use decimal probabilities: `0.25` means 25% and `0.80` means 80%.

## Files considered

Supported extensions:

```text
rs  js  jsx  ts  tsx  py  go  java  kt  kts  rb  php
c   h   cpp  hpp  cs  swift  scala  sh  sql
yaml  yml  json  toml
```

The walker honors repository, nested, global, and `.git/info/exclude` Git ignore rules. Hidden paths are excluded unless `--hidden` is supplied.

These directories are always skipped:

```text
.git  .next  .venv  __pycache__  build  coverage
dist  node_modules  out  target  vendor  venv
```

## Output

Each match contains the path, one-based line range, probability, and complete matching window:

```text
./src/auth.rs:81-180 (92%)
fn verify_jwt(token: &str) -> Result<User, AuthError> {
    // ...
}
```

When no window meets the threshold:

```text
No matches found for: where does user JWT authentication happen?
```

## Accuracy and privacy

- File selection is probabilistic. A high `--file-threshold` is faster but can exclude unexpectedly named files; lower it or use `0` for a thorough scan.
- Results are overlapping code windows, not exact AST or function boundaries, so neighboring results may contain duplicate lines.
- Large and minified files are split across multiple requests instead of being sent as one oversized payload.
- Path-selection batches run sequentially; selected files are scanned concurrently up to `--parallelism`.
- Matching source code and file paths are sent to the TypeSafe API. Review TypeSafe's data policies before searching private or sensitive repositories.

## Development

Run the test suite:

```sh
cargo test
```

Build an optimized binary:

```sh
cargo build --release
```
