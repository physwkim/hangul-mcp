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

## Tools (74)

**Session**

| Tool | Description |
|------|-------------|
| `open_document(path)` | Open a document → `{doc_id, sections, paragraphs_per_section, page_count}` |
| `save_document(doc_id, output_path?)` | Save as HWPX or HWP 5.0, chosen by the path extension (`.hwpx` / `.hwp`). Overwrites the original when `output_path` is omitted. `warnings` lists what the output format could not hold: rhwp's content-loss report (emptied binary data, omitted controls, reduced metadata) and, for `.hwp`, how many objects with `tight`/`through` wrapping were stored as `square` (HWP 5.0 has no field value for either) |
| `render_page(doc_id, page, output_path)` | Lay out one page (0-based) of the current, possibly unsaved, state and write it as SVG or PDF (by `.svg` / `.pdf` extension) |
| `export_document(doc_id, output_path, page_start?, page_end?)` | Export the whole document (or a 0-based inclusive page range) as laid out: `.pdf` (multi-page), `.md` (tables as Markdown tables, pictures written to `<name>_assets/` and linked; a picture inside a table cell is linked right after that table), or `.txt` (lines in page order) |
| `undo(doc_id)` | Revert the last successful editing tool call as one unit (up to 20 steps). Returning to the saved state clears `dirty` |
| `redo(doc_id)` | Re-apply an undone edit; a new edit after `undo` drops the redo history |
| `close_document(doc_id)` | Close the session. Unsaved changes are discarded, and the result says so |
| `list_documents()` | Open documents and their dirty state |
| `server_info()` | Build info: this crate's version, plus the rhwp version and source (git rev) it was built against |

**Reading**

| Tool | Description |
|------|-------------|
| `get_structure(doc_id)` | Section → paragraph indices, previews, table control positions, and `objects` (table/picture/shape/equation/footnote/endnote with `control`, `kind`, `char_offset`; a textbox shape adds `textbox_paras`) |
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
A textbox shape is addressed the same way with `cell: 0`; its paragraphs also show up in
the list tools (`list_hyperlinks`, `list_bookmarks`, `list_forms`). Setting a form value or
an equation inside a textbox is rejected — rhwp's natives for those take table cells only.

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
| `get_columns(doc_id, section)` | Section column layout → `count`, `kind`, `spacing_mm` — the same shape as the `columns` input of `set_columns` |
| `set_columns(doc_id, section, columns)` | Change the section's first column layout: `count` (1–255; 1 turns multi-column off), `kind` (`normal`/`distribute`/`parallel`), `spacing_mm`. Only the given fields change; body text is rewrapped to the column width; a spacing that leaves no column width is rejected. Changing `count` makes the columns equal width |

**Table editing**

| Tool | Description |
|------|-------------|
| `create_table(doc_id, section, para, char_offset, rows, cols, column_widths_percent?, column_widths?, repeat_header?, cells?)` | Insert a new table; returns its `paraIdx`/`controlIdx`. `cells` fills it in the same call: a list of rows, each a list of cell texts in column order (at most `rows`×`cols`; missing cells stay empty; `\n` starts a new cell paragraph) |
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
| `get_picture_format(doc_id, section, para, control)` | Picture format → the table placement fields (`inline`, `text_wrap`, `horz/vert_rel_to`, `horz/vert_align`, `*_offset_mm`, outer `margin_*_mm`), `width_mm`/`height_mm`, `description`, `brightness`, `contrast`, `transparency`, `effect`, `rotation_deg`, `horz_flip`/`vert_flip` — the same shape as the `format` input of `apply_picture_format` |
| `apply_picture_format(doc_id, section, para, control, format)` | Apply only the given fields of a picture format. Pictures also take `text_wrap` `tight`/`through` (kept in HWPX; HWP 5.0 has no such wrap, so a `.hwp` save writes `square`) but not `inside`/`outside` alignment |
| `insert_shape(doc_id, section, para, char_offset, kind, width_mm, height_mm, inline?, x_mm?, y_mm?, text?)` | Insert a `rectangle`, `ellipse`, `line` (top-left to bottom-right of its box; a zero width or height draws a vertical or horizontal line) or `textbox`. `inline=false` (default) floats it in front of text at `x_mm`/`y_mm` from the paper corner (default: body top-left); `inline=true` treats it as a character. A textbox takes `text`, one paragraph per `\n`, and is edited later through `cell = {control, cell: 0}`. Returns `{para, control}` |
| `get_shape_format(doc_id, section, para, control)` | Drawing-object format (line, rectangle, ellipse, arc, polygon, curve, textbox) → the placement fields, `width_mm`/`height_mm`, `line_color`, `line_width_mm`, `fill_color` (`"#RRGGBB"` or `"none"`; omitted for lines and gradient/image fills), `description`, `rotation_deg`, `horz_flip`/`vert_flip` — the same shape as the `format` input of `apply_shape_format` |
| `apply_shape_format(doc_id, section, para, control, format)` | Apply only the given fields of a shape format. Offsets may be negative; `text_wrap` `tight`/`through` become `square` in a `.hwp` save; `inside`/`outside` alignment, a fill on a line, and shrinking a side to 0 are rejected |
| `insert_equation(doc_id, section, para, char_offset, script, font_size_pt?, color?)` | Insert an equation, treated as a character, from a Hangul equation script (`1 over 2`, `x^2`, `sqrt {a+b}`, `sum from {i=1} to n`). Size is computed from the script; default 10 pt black |
| `get_equation(doc_id, section, para, control, cell?)` / `set_equation(..., script?, font_size_pt?, color?)` | Read / change an equation's script, size and color; a new script or size recomputes its box. In a table cell only the first equation of the cell paragraph can be changed (rhwp addresses cell equations without a control index) |
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
| `get_form_info(doc_id, section, para, control, cell?)` | Details: size, colors, attributes, combo box items |
| `set_form_value(doc_id, section, para, control, cell?, value?, text?, caption?)` | Set a value (at least one of the three) |

**Bookmarks and hyperlinks**

| Tool | Description |
|------|-------------|
| `list_bookmarks(doc_id)` | Every bookmark in document order (`name`, `section/para/control`, `char_offset`; `cell` inside a table cell) |
| `add_bookmark(doc_id, section, para, char_offset, name)` | Add a bookmark in a body paragraph. Names are unique in the document |
| `rename_bookmark(doc_id, name, new_name)` / `delete_bookmark(doc_id, name)` | Rename / delete a body bookmark by name |
| `list_hyperlinks(doc_id)` | Every hyperlink in body and table-cell paragraphs (`field_id`, `start`/`end` char range, `text`, `url`) |
| `insert_hyperlink(doc_id, section, para, cell?, start, end, url, link_style?)` | Link the text `start..end` (end exclusive) to an `http(s)://` URL. The linked text turns blue (`#0000FF`) and underlined; `link_style=false` keeps its character format. Ranges overlapping another field are rejected |
| `update_hyperlink(doc_id, section, para, cell?, field_id, url?, text?)` | Change the URL and/or the displayed text |
| `remove_hyperlink(doc_id, section, para, cell?, field_id)` | Unlink; the text stays |

## Known limitations

- **The form object tools need rhwp's form object serializer (`serializer/hwpx/form.rs`).**
  Form objects are preserved on save only if the rev pinned in `Cargo.toml` includes that
  writer. Building against an rhwp version without it silently drops form objects when a
  document containing them is saved.
- **Bookmarks inside table cells are listed but cannot be renamed or deleted**, and bookmarks
  are added to body paragraphs only: rhwp's bookmark editing addresses body paragraphs only.
- Fields (`ClickHere`) and form objects both save and round-trip correctly.

## Testing

```bash
cargo nextest run
```

`tests/roundtrip.rs` asserts an open → edit → save → re-parse (`parse_hwpx` / `parse_hwp`) round
trip against the fixtures in `tests/fixtures/` (copied from rhwp's samples; `equation_in_cell.hwpx` is
rhwp's `issue7593/repro-equation-issue7593.hwpx` with a second equation (`z^2`) appended to the
first cell paragraph that holds one). `biz_plan.hwp` is a Hancom-written HWP 5.0 source, so the
`.hwp` reader is exercised on a real file and not only on files rhwp wrote itself.
`cell_bookmark.hwpx` is rhwp's `issue6284/child_policy_top_caption_charts.hwpx`, whose bookmarks
all sit in table cells. `columns_unequal.hwpx` is `ref_text.hwpx` with its `colPr` changed to two
columns of unequal width (`sameSz="0"`, two `colSz`), since no sample's first column definition has one. `combo_in_cell.hwpx` is `form_in_cells.hwpx` with its first cell checkbox replaced by the combo box from `form_fields.hwpx`, plus that file's `Scripts/`, so a combo box inside a cell has script items. `picture_in_cell.hwpx` is rhwp's
`issue3587/d-pi2-import.hwpx` (a picture inside a table cell).

## Out of scope (possible follow-ups)

- HTML import — rhwp's `DocumentCore` already has the API, so only the tool is missing
- Shape groups, charts and OLE objects (`get/apply_shape_format` take single drawing objects only)
- Batched edit optimization (`begin_batch_native` / `end_batch_native`)
