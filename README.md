# tree-map-base

`tree-map-base` is a read-only disk usage visualizer written in Rust.
It scans a selected directory, computes file/folder sizes, builds an in-memory tree, and renders a squarified treemap with hover details.

## Features

- Recursive directory scanning with `walkdir`
- In-memory tree using:

```rust
struct Node {
    name: String,
    path: PathBuf,
    size: u64,
    children: Vec<Node>,
}
```

- Squarified treemap rendering
- Hover details for each rectangle:
  - Name
  - Human-readable size
  - Full path
- Progress display while scanning
- Safety limits:
  - Max recursion depth
  - Optional max file count
- Graceful handling of permission and metadata errors
- Symbolic links are not followed (`follow_links(false)`) to avoid recursive link loops

## Safety Design (Visualization-Only)

This tool is intentionally **read-only**.

- No write APIs are used
- No delete/rename/move functionality exists
- No command execution is used for filesystem operations
- Scanner only reads directory entries and metadata
- UI exposes visualization controls only (directory selection, scan limits, and treemap display)

## Build

Requirements:

- Rust stable toolchain
- Cargo

Build command:

```bash
cargo build
```

## Run

```bash
cargo run
```

At launch, choose a root directory. The app scans it and displays the treemap.

## Project Structure

```text
tree-map-base/
¢u¢w¢w Cargo.toml
¢u¢w¢w README.md
¢|¢w¢w src/
    ¢u¢w¢w app.rs       # egui/eframe UI and interaction
    ¢u¢w¢w format.rs    # byte-size formatting helpers
    ¢u¢w¢w main.rs      # app entry point
    ¢u¢w¢w model.rs     # Node data model and tree construction utilities
    ¢u¢w¢w scanner.rs   # read-only recursive scanner using walkdir
    ¢|¢w¢w treemap.rs   # squarified treemap layout algorithm
```

## Notes on Large Directories

- Recursion depth is capped by `max_depth` for safety
- File count can be limited (`max_files`) to prevent unbounded memory growth
- If the file limit is reached, the result is marked as partial

## Future Extension Ideas

- Export treemap as image (still read-only)
- Search/filter by filename or extension
- Alternate color themes and legends
- Keyboard navigation and accessibility improvements
- Multi-threaded scan aggregation for very large trees