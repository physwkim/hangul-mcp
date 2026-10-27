# hangul-mcp

An MCP (Model Context Protocol) server for editing HWPX documents, built on
[rhwp](https://github.com/edwardkim/rhwp). It opens `.hwpx` (or `.hwp`) files,
edits them in memory, and saves them as HWPX or HWP.

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
3. Call editing tools (changes stay in memory; a failing call leaves the document unchanged, and `undo` reverts a successful one)
4. `save_document(doc_id, output_path?)` → write HWPX or HWP (chosen by the `.hwpx` / `.hwp` extension)

All coordinates are 0-based. A character position (`char_offset`, `start`, `end`) is an index
into the paragraph text as `get_text` returns it, and must not exceed the paragraph length.
rhwp's split-type calls count inline objects (pictures treated as characters, footnotes) as
extra positions; the server converts to that form, so the same `char_offset` means the same
place in every tool.

## Tools (55)

**Session**

| Tool | Description |
|------|-------------|
| `open_document(path)` | Open a document → `{doc_id, sections, paragraphs_per_section, page_count}` |
| `save_document(doc_id, output_path?)` | Save as HWPX or HWP 5.0, chosen by the path extension (`.hwpx` / `.hwp`). Overwrites the original when `output_path` is omitted |
| `render_page(doc_id, page, output_path)` | Lay out one page (0-based) of the current, possibly unsaved, state and write it as SVG or PDF (by `.svg` / `.pdf` extension) |
| `undo(doc_id)` | Revert the last successful editing tool call as one unit (up to 20 steps). Returning to the saved state clears `dirty` |
| `redo(doc_id)` | Re-apply an undone edit; a new edit after `undo` drops the redo history |
| `close_document(doc_id)` | Close the session. Unsaved changes are discarded, and the result says so |
| `list_documents()` | Open documents and their dirty state |
| `server_info()` | Build info: this crate's version, plus the rhwp version and source (git rev) it was built against |

**Reading**

| Tool | Description |
|------|-------------|
| `get_structure(doc_id)` | Section → paragraph indices, previews, table control positions, and `objects` (table/picture/shape/equation/footnote/endnote with `control`, `kind`, `char_offset`) |
| `get_text(doc_id, section, para_start?, para_end?)` | Full paragraph text |
| `get_table(doc_id, section, para, control)` | Table size plus per-cell `cell_idx`/`row`/`col`/text |

**Search and replace**

| Tool | Description |
|------|-------------|
| `search_text(doc_id, query, case_sensitive?, include_cells?)` | Positions of every match |
| `replace_text(doc_id, query, replacement, all?, case_sensitive?)` | Replace matches in body text and table cells — the same matches `search_text` finds. `all=true` (default) replaces all, `false` only the first in document order. No match is an error and leaves no undo step |

**Text and paragraph editing**

| Tool | Description |
|------|-------------|
| `insert_text(doc_id, section, para, char_offset, text, cell?)` | Insert text |
| `delete_text(doc_id, section, para, char_offset, count, cell?)` | Delete text |
| `insert_paragraph(doc_id, section, para)` | Insert an empty paragraph |
| `delete_paragraph(doc_id, section, para)` | Delete a paragraph |
| `split_paragraph(doc_id, section, para, char_offset, cell?)` | Split a paragraph |
| `merge_paragraph(doc_id, section, para, cell?)` | Merge with the previous paragraph (in a cell: `cell.cell_para` ≥ 1 into the one before it) |
| `insert_break(doc_id, section, para, char_offset, kind?)` | Split the paragraph and start the rest on a new page (`kind="page"`, default) or column (`"column"`) |

`cell = {control, cell, cell_para?}` targets a table cell paragraph instead of the body,
with `para` the paragraph holding the table (the same form the formatting tools take).

**Formatting**

`get_*` results use the same shape and units as the `format` input of the matching
`apply_*` tool, so a format read from one place can be applied elsewhere as is.

| Tool | Description |
|------|-------------|
| `get_char_format(doc_id, section, para, start?, end?, cell?)` | Character formatting of a paragraph range, split into runs of identical formatting → `{length, runs: [{start, end, format, per_language}]}` |
| `get_para_format(doc_id, section, para, cell?)` | Paragraph formatting → `{format, style, heading}` (`heading`: outline/number/bullet type and level 1–7) |
| `apply_para_format(doc_id, section, para, cell?, format)` | Apply paragraph formatting: `alignment` (`Justify`/`Left`/`Right`/`Center`/`Distribute`/`Split`), `line_spacing` (`{kind: Percent\|Fixed\|SpaceOnly\|Minimum, value}`; `%` for `Percent`, pt otherwise), `margin_left_pt`, `margin_right_pt`, `indent_pt` (negative = hanging), `spacing_before_pt`, `spacing_after_pt`, `keep_with_next`, `keep_lines`, `page_break_before`, `widow_orphan`, `korean_break_unit`, `english_break_unit`. Only the given fields change |
| `list_styles(doc_id)` | Styles defined in the document → `{styles: [{id, name, english_name, kind}]}` (`kind`: `para` or `char`) |
| `apply_style(doc_id, section, para, cell?, style)` | Apply a style by `id` (number) or name (string, e.g. `"개요 1"`). A paragraph that already has direct paragraph formatting keeps its paragraph shape |
| `apply_char_format(doc_id, section, para, start, end, cell?, format)` | Apply character formatting to the `[start, end)` range of a body paragraph, or of a table cell paragraph when `cell = {control, cell, cell_para?}` is given. `format` fields are all optional and only the given ones change: `bold`, `italic`, `underline`, `underline_type`, `underline_shape`, `underline_color`, `strikethrough`, `strike_shape`, `strike_color`, `font_size_pt`, `font_name`, `text_color`, `shade_color`, `outline_type`, `shadow_type`, `shadow_color`, `shadow_offset_x/y`, `superscript`, `subscript`, `emboss`, `engrave`, `emphasis_dot`, `kerning`, `ratio_percent`, `spacing_percent`, `relative_size_percent`, `char_offset_percent`. Colors are `"#RRGGBB"` |

**Page setup**

| Tool | Description |
|------|-------------|
| `get_page_setup(doc_id, section)` | Paper size, orientation, margins and binding of a section in mm — the same shape as the `setup` input of `set_page_setup` |
| `set_page_setup(doc_id, section, setup)` | Change `paper_width_mm`/`paper_height_mm` (portrait dimensions, A4 = 210×297), `orientation` (`portrait`/`landscape`), `margin_{left,right,top,bottom,header,footer,gutter}_mm`, `binding` (`single`/`duplex`/`top_flip`). Only the given fields change; margins that leave no body area are rejected |

**Table editing**

| Tool | Description |
|------|-------------|
| `create_table(doc_id, section, para, char_offset, rows, cols, column_widths_percent?, column_widths?, repeat_header?)` | Insert a new table; returns its `paraIdx`/`controlIdx` |
| `set_cell_text(doc_id, section, para, control, cell, cell_para?, text)` | Replace a cell paragraph's text |
| `insert_table_row(doc_id, section, para, control, row, below?)` | Insert a row |
| `insert_table_column(doc_id, section, para, control, col, right?)` | Insert a column |
| `delete_table_row(doc_id, section, para, control, row)` | Delete a row |
| `delete_table_column(doc_id, section, para, control, col)` | Delete a column |
| `fit_table_to_page(doc_id, section, para, control)` | Shrink the table to fit the body width |
| `set_table_column_widths(doc_id, section, para, control, widths)` | Set per-column widths directly |
| `merge_table_cells(doc_id, section, para, control, start_row, start_col, end_row, end_col)` | Merge a rectangular cell range (inclusive) |
| `split_table_cell(doc_id, section, para, control, row, col, rows?, cols?)` | Unmerge a merged cell, or split a cell into `rows`×`cols` |
| `get_table_format(doc_id, section, para, control)` | Table placement and layout → `inline`, `text_wrap` (`square`/`top_and_bottom`/`behind_text`/`in_front_of_text`), `horz_rel_to`/`horz_align`/`horz_offset_mm`, `vert_rel_to`/`vert_align`/`vert_offset_mm`, `margin_*_mm` (outer), `cell_padding_*_mm` (default inner margins for cells without their own), `cell_spacing_mm`, `repeat_header` — the same shape as the `format` input of `apply_table_format` |
| `apply_table_format(doc_id, section, para, control, format)` | Apply only the given fields of a table format. Cells without their own margins follow `cell_padding_*_mm` |
| `get_cell_format(doc_id, section, para, control, row, col)` | Cell formatting → `background`, `border_left/right/top/bottom` (`{style, width_mm, color}`), `vertical_align`, `padding_*_mm` (the margins in effect), `header` — the same shape as the `format` input of `apply_cell_format` |
| `apply_cell_format(doc_id, section, para, control, start_row, start_col, end_row?, end_col?, format)` | Apply cell formatting to every cell starting in the range: `background` (`"#RRGGBB"` / `"none"`), `border` (all four sides) or per-side borders, `vertical_align` (`top`/`center`/`bottom`), `padding_*_mm`, `header`. A partial border side keeps the side's other values; a side shared with a neighbouring cell is one line, as in Hangul, so the neighbour's side changes too |

**Objects**

| Tool | Description |
|------|-------------|
| `insert_picture(doc_id, section, para, char_offset, image_path, width_mm?, height_mm?, inline?, x_mm?, y_mm?, description?)` | Insert a PNG/JPEG/GIF/BMP/TIFF picture into a body paragraph. One size given keeps the aspect ratio; none uses the 96 DPI natural size. `inline=true` (default) treats it as a character; `inline=false` floats it at `x_mm`/`y_mm` from the paper corner (default: body top-left) |
| `delete_object(doc_id, section, para, control)` | Delete one object listed in `get_structure` `objects`: table, picture, shape, equation, footnote or endnote. Deleting a note renumbers the following ones |

**Notes, header and footer**

| Tool | Description |
|------|-------------|
| `insert_note(doc_id, section, para, char_offset, text, kind?)` | Insert a footnote (`kind="footnote"`, default) or endnote (`"endnote"`) with one line of text; numbering follows document order |
| `set_header_footer(doc_id, section, kind, pages?, text)` | Set (replace) the header (`kind="header"`) or footer (`"footer"`) for `pages` = `both` (default) / `even` / `odd`. `{page}`, `{total_pages}`, `{file_name}` in `text` become fields |
| `get_header_footer(doc_id, section, kind, pages?)` | Read a header/footer; fields are shown as `{page}` / `{total_pages}` / `{file_name}` |
| `delete_header_footer(doc_id, section, kind, pages?)` | Delete a header/footer |

**Fields (누름틀)** — filling in form documents

| Tool | Description |
|------|-------------|
| `list_fields(doc_id)` | Every field (name, kind, placeholder, current value, position) |
| `get_field_value(doc_id, name)` | Read a field value by name |
| `set_field_value(doc_id, name, value)` | Set a field value by name, including fields inside table cells |

**Form objects** — button, checkbox, radio, combo box, edit

| Tool | Description |
|------|-------------|
| `list_forms(doc_id)` | Every form object, including those in table cells (position `section/para/control`, plus `cell = {control, cell, cell_para}` inside a cell; kind, name, value, caption, text) |
| `get_form_value(doc_id, section, para, control, cell?)` | Read a form object's value (kind, value, text, caption, enabled) |
| `get_form_info(doc_id, section, para, control, cell?)` | Details: size, colors, attributes, combo box items (not available for a combo box inside a cell) |
| `set_form_value(doc_id, section, para, control, cell?, value?, text?, caption?)` | Set a value (at least one of the three) |

## Known limitations

- **The form object tools need rhwp's form object serializer (`serializer/hwpx/form.rs`).**
  Form objects are preserved on save only if the rev pinned in `Cargo.toml` includes that
  writer. Building against an rhwp version without it silently drops form objects when a
  document containing them is saved.
- **Combo box items inside table cells cannot be read.** rhwp extracts combo box items from
  the document script only for body-level form objects, so `get_form_info` rejects a combo box
  inside a cell. Its value is still readable and settable with `get_form_value`/`set_form_value`.
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
