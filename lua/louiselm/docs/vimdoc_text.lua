---Vimdoc text primitives shared by LouiseLM's generated help documents.
---
---Neovim's help syntax pays for column discipline: tags right-aligned to
---`MAX_WIDTH`, cross-references written as `|tag|`, and nothing wider than the
---window a reader opens `:help` in. These helpers keep both generators on the
---same rules instead of each rediscovering them.
local M = {}

---Column the closing `*` of a tag and the last character of a line sit on.
M.MAX_WIDTH = 78

M.SECTION_RULE = string.rep("=", M.MAX_WIDTH)
M.SUBSECTION_RULE = string.rep("-", M.MAX_WIDTH)

---Wrap `text` to the help width, prefixing the first and following lines.
---@param lines string[] Accumulator appended in place.
---@param text string Unwrapped paragraph; whitespace runs collapse.
---@param first_prefix string Prefix for the first emitted line.
---@param continuation_prefix string Prefix for every later line.
function M.append_wrapped(lines, text, first_prefix, continuation_prefix)
  local words = {}
  for word in text:gmatch("%S+") do
    words[#words + 1] = word
  end
  if #words == 0 then
    lines[#lines + 1] = first_prefix
    return
  end

  local current = first_prefix
  for _, word in ipairs(words) do
    local separator = current == first_prefix and "" or " "
    if #current + #separator + #word <= M.MAX_WIDTH then
      current = current .. separator .. word
    else
      lines[#lines + 1] = current
      current = continuation_prefix .. word
    end
  end
  lines[#lines + 1] = current
end

---Append a right-aligned `*tag*`, on its own line when the title leaves no room.
---@param lines string[] Accumulator appended in place.
---@param title string Human-readable heading.
---@param tag string Help tag, without surrounding asterisks.
function M.append_heading(lines, title, tag)
  local marked = "*" .. tag .. "*"
  if #marked > M.MAX_WIDTH then
    error("vimdoc tag does not fit the help width: " .. tag)
  end
  if #title + 1 + #marked <= M.MAX_WIDTH then
    lines[#lines + 1] = title .. string.rep(" ", M.MAX_WIDTH - #title - #marked) .. marked
    return
  end
  lines[#lines + 1] = string.rep(" ", M.MAX_WIDTH - #marked) .. marked
  lines[#lines + 1] = title
end

---Join a left and a right column into one right-aligned help-width line.
---@param left string Left column, already indented.
---@param right string Right column pushed to the help width.
---@param filler? string Single character used between the columns; defaults to a space.
---@return string line
function M.align_columns(left, right, filler)
  local gap = M.MAX_WIDTH - #left - #right
  if gap < 1 then
    return left .. " " .. right
  end
  return left .. string.rep(filler or " ", gap) .. right
end

---Link every `:Louiselm…` command named in prose that the document also tags.
---
---Commands are generated from Neovim's registered command table, so a
---hand-written link to one is a link that can rot. Rewriting them mechanically
---against the tags actually emitted keeps prose and document in step.
---@param text string Prose that may name commands.
---@param tags table<string, true> Tags emitted by this document.
---@return string linked
function M.link_commands(text, tags)
  return (
    text:gsub("(.?)(:Louiselm%w+)", function(prefix, command)
      if prefix == "|" or not tags[command] then
        return prefix .. command
      end
      return prefix .. "|" .. command .. "|"
    end)
  )
end

---Report every `|link|` in `document` that resolves to no known tag.
---@param document string Complete generated help document.
---@param tags table<string, true> Tags emitted by this document.
---@param external table<string, true> Tags owned by Neovim's own help files.
---@return string[] dangling Unresolved link targets, in order of appearance.
function M.dangling_links(document, tags, external)
  local dangling = {}
  for link in document:gmatch("|(%S-)|") do
    if not tags[link] and not external[link] then
      dangling[#dangling + 1] = link
    end
  end
  return dangling
end

return M
