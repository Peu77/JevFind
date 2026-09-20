---
name: jev-code-finder
description: Use the Jev Code Finder CLI to locate files and code windows relevant to a natural-language concept before inspecting or changing a codebase.
---

# Jev Code Finder

Use this skill when a user asks where a concept, behavior, feature, or flow is implemented in a repository—for example, “where does JWT authentication happen?” or “find the retry logic.” It is a repository-discovery step for code changes, not a replacement for inspecting or testing code.

## Mandatory pre-edit rule

Before editing code, run Jev Code Finder to locate the existing implementation and its likely callers. Do not guess filenames, add a parallel implementation, or patch only the first named path. Complete the search first, then inspect the strongest matches and trace the relevant flow before applying changes.

You may skip the search only when the task is not a code change, the user supplied the exact file and location to edit, or the requested change is limited to generated metadata such as a version bump.

## Workflow

1. Check that `jev-code-finder` is available with `command -v jev-code-finder`.
2. Confirm `TYPESAFE_API_KEY` is set before invoking it. Never print, paste, or commit the key.
3. Run it against the relevant repository path:

   ```sh
   jev-code-finder "<concept>" --path <repo> --file-threshold 0.25 --threshold 0.55 --parallelism 10
   ```

4. Read the returned file paths, line ranges, probabilities, and snippets. Treat probabilities as ranking signals, not proof.
5. Inspect the strongest results and nearby callers or definitions before making conclusions or edits.
6. After editing, run the smallest relevant test or check and verify that the changed flow still matches the discovered implementation.

Use `--debug` only when a user wants live scan progress; its ANSI output is noisy for agent parsing. Use `--file-threshold 0` when the path-based prefilter could hide a relevant file. Use `--hidden` only when hidden files are in scope.

If the binary is unavailable, stop before editing and tell the user to install it with Homebrew or build it from the JevFind repository rather than silently falling back to a different search tool. If the query returns no matches, report that clearly, try a broader concept or lower threshold when appropriate, and do not claim the code location is known without evidence.

The tool sends selected file paths and code windows to TypeSafe Jev. Warn the user before searching sensitive repositories when that matters.
