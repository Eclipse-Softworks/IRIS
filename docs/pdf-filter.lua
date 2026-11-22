-- Pandoc Lua filter for PDF builds of The IRIS Programming Language.
-- Keeps docs/BOOK.md clean and self-contained for GitHub Markdown while allowing
-- LaTeX to render a custom logo cover page (\maketitle) and page-numbered \tableofcontents.

local function is_horizontal_rule(block)
  return block.t == "HorizontalRule"
end

local function is_newpage(block)
  return block.t == "RawBlock" and block.format == "tex" and block.text:match("\\newpage") ~= nil
end

local function trim_leading_front_title(blocks)
  -- Strip the Markdown opening H1 ("The IRIS Programming Language"), its one-line
  -- bold subtitle, and immediate \newpage so the PDF starts directly with \maketitle.
  local out = {}
  local i = 1
  if i <= #blocks and blocks[i].t == "Header" and blocks[i].level == 1 then
    local title_text = pandoc.utils.stringify(blocks[i])
    if title_text == "The IRIS Programming Language" then
      i = i + 1
      if i <= #blocks and blocks[i].t == "Para" then
        i = i + 1
      end
      while i <= #blocks and is_newpage(blocks[i]) do
        i = i + 1
      end
    end
  end
  while i <= #blocks do
    table.insert(out, blocks[i])
    i = i + 1
  end
  return out
end

local function strip_markdown_toc(blocks)
  -- Strip "## Table of Contents" and the bullet list beneath it up to the
  -- next HorizontalRule or Header, since XeLaTeX emits \tableofcontents.
  local out = {}
  local skipping = false
  for _, block in ipairs(blocks) do
    if not skipping then
      if block.t == "Header" and block.level == 2 and pandoc.utils.stringify(block) == "Table of Contents" then
        skipping = true
      else
        table.insert(out, block)
      end
    else
      if is_horizontal_rule(block) then
        skipping = false
      elseif block.t == "Header" then
        skipping = false
        table.insert(out, block)
      end
    end
  end
  return out
end

function Pandoc(doc)
  local blocks = trim_leading_front_title(doc.blocks)
  blocks = strip_markdown_toc(blocks)
  return pandoc.Pandoc(blocks, doc.meta)
end
