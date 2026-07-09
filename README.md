# hangul-mcp

An MCP (Model Context Protocol) server for editing HWPX documents, built on
[rhwp](https://github.com/physwkim/rhwp). It opens `.hwpx` (or `.hwp`) files,
edits them in memory, and saves them back as HWPX.

## Requirements

- Rust 1.93.1 (pinned in `rust-toolchain.toml`; `rustup` picks it up automatically)

rhwp is a git-rev-pinned dependency, so no separate checkout is needed — cargo fetches it.

## Installation

Build the server and register it with Claude Code.

```bash
git clone https://github.com/physwkim/hangul-mcp
cd hangul-mcp
cargo build --release
```

Register the resulting binary. The path must be absolute:

```bash
claude mcp add hangul-mcp -- "$(pwd)/target/release/hangul-mcp"
```

Alternatively, install it onto your `PATH` so the binary survives `cargo clean`:

```bash
cargo install --path . --locked
claude mcp add hangul-mcp -- "$HOME/.cargo/bin/hangul-mcp"
```

Verify the server is reachable:

```bash
claude mcp list          # hangul-mcp: ... - ✔ Connected
```

For MCP clients other than Claude Code, run the binary directly — it speaks the
MCP protocol over stdio and takes no arguments.

## Workflow

1. `open_document(path)` → returns a `doc_id`
2. `get_structure(doc_id)` → learn the editing coordinates (section/para/control)
3. Call editing tools (changes stay in memory)
4. `save_document(doc_id, output_path?)` → write HWPX

All coordinates are 0-based and character positions are counted in `char` units.
This is the same coordinate system rhwp's `DocumentCore` uses.

## Tools (29)

**Session**

| Tool | Description |
|------|-------------|
| `open_document(path)` | Open a document → `{doc_id, sections, paragraphs_per_section, page_count}` |
| `save_document(doc_id, output_path?)` | Save as HWPX. Overwrites the original when `output_path` is omitted |
| `close_document(doc_id)` | Close the session. Unsaved changes are discarded, and the result says so |
| `list_documents()` | Open documents and their dirty state |

**Reading**

| Tool | Description |
|------|-------------|
| `get_structure(doc_id)` | Section → paragraph indices, previews, table control positions |
| `get_text(doc_id, section, para_start?, para_end?)` | Full paragraph text |
| `get_table(doc_id, section, para, control)` | Table size plus per-cell `cell_idx`/`row`/`col`/text |

**Search and replace**

| Tool | Description |
|------|-------------|
| `search_text(doc_id, query, case_sensitive?, include_cells?)` | Positions of every match |
| `replace_text(doc_id, query, replacement, all?, case_sensitive?)` | Replace in body text (all matches or the first) |

**Text and paragraph editing**

| Tool | Description |
|------|-------------|
| `insert_text(doc_id, section, para, char_offset, text)` | Insert text |
| `delete_text(doc_id, section, para, char_offset, count)` | Delete text |
| `insert_paragraph(doc_id, section, para)` | Insert an empty paragraph |
| `delete_paragraph(doc_id, section, para)` | Delete a paragraph |
| `split_paragraph(doc_id, section, para, char_offset)` | Split a paragraph |
| `merge_paragraph(doc_id, section, para)` | Merge with the previous paragraph |

**Table editing**

| Tool | Description |
|------|-------------|
| `set_cell_text(doc_id, section, para, control, cell, cell_para?, text)` | Replace a cell paragraph's text |
| `insert_table_row(doc_id, section, para, control, row, below?)` | Insert a row |
| `insert_table_column(doc_id, section, para, control, col, right?)` | Insert a column |
| `delete_table_row(doc_id, section, para, control, row)` | Delete a row |
| `delete_table_column(doc_id, section, para, control, col)` | Delete a column |
| `fit_table_to_page(doc_id, section, para, control)` | Shrink the table to fit the body width |
| `set_table_column_widths(doc_id, section, para, control, widths)` | Set per-column widths directly |

**Fields (누름틀)** — filling in form documents

| Tool | Description |
|------|-------------|
| `list_fields(doc_id)` | Every field (name, kind, placeholder, current value, position) |
| `get_field_value(doc_id, name)` | Read a field value by name |
| `set_field_value(doc_id, name, value)` | Set a field value by name, including fields inside table cells |

**Form objects** — button, checkbox, radio, combo box, edit

| Tool | Description |
|------|-------------|
| `list_forms(doc_id)` | Body-level form objects (position `section/para/control`, kind, name, value, caption, text) |
| `get_form_value(doc_id, section, para, control)` | Read a form object's value (kind, value, text, caption, enabled) |
| `get_form_info(doc_id, section, para, control)` | Details: size, colors, attributes, combo box items |
| `set_form_value(doc_id, section, para, control, value?, text?, caption?)` | Set a value (at least one of the three) |

## Known limitations

- **The form object tools need rhwp's form object serializer (`serializer/hwpx/form.rs`).**
  Form objects are preserved on save only if the rev pinned in `Cargo.toml` includes that
  writer. Building against an rhwp version without it silently drops form objects when a
  document containing them is saved.
- **Form objects inside table cells are not exposed yet.** `list_forms`, `get_form_*`, and
  `set_form_value` only handle form objects at the body paragraph level. rhwp has a native
  API for setting form objects inside cells (reading is asymmetric — no native getter), but
  no tool surfaces it today. See the roadmap.
- Fields (`ClickHere`) and form objects both save and round-trip correctly.

## Testing

```bash
cargo nextest run
```

`tests/roundtrip.rs` asserts an open → edit → save → re-parse (`parse_hwpx`) round trip
against the fixtures in `tests/fixtures/` (copied from rhwp's samples).

## Out of scope (possible follow-ups)

- Formatting (char/para format, styles), headers and footers, footnotes, HTML import —
  rhwp's `DocumentCore` already has the APIs, so only the tools are missing
- Saving as HWP 5.0 (`export_hwp_with_adapter`), PDF export
- Batched edit optimization (`begin_batch_native` / `end_batch_native`)
