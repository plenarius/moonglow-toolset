-- Resources as the builder routes speak of them: (kind, key) and their docs, and the
-- revisions the module's copies came from (module.ifo's LAYO_EXPORT_REVS).
local mg = require("@moonglow")

local res = {}

-- Each kind: its files (by extension; the routes' docs keys) and those it may lack.
-- Blueprints and dialogs join as kinds of one file ({ parts = { "utc" } }).
res.KINDS = {
  area = { name = "area", parts = { "are", "git", "gic" }, optional = { gic = true }, listed = "Mod_Area_list" },
}

res.REVS = "LAYO_EXPORT_REVS"

function res.kind(kind: string)
  local k = res.KINDS[kind]
  if not k then
    error(`Layonara: no kind {kind} yet`, 0)
  end
  return k
end

-- What a selection's entry is as an item ({kind, key}), or nil.
local SELECTED = {
  area = function(s)
    return "area", s.area
  end,
}

-- The items a command runs on: what is selected, else what is open.
function res.chosen(ctx)
  local out, seen = {}, {}
  local function add(s)
    local of = SELECTED[s.kind]
    if not of then
      return
    end
    local kind, key = of(s)
    if key and not seen[kind .. ":" .. key] then
      seen[kind .. ":" .. key] = true
      table.insert(out, { kind = kind, key = key })
    end
  end
  for _, s in ctx.selection do
    add(s)
  end
  if #out == 0 and ctx.open then
    add(ctx.open)
  end
  return out
end

function res.has(ctx, kind: string, key: string): boolean
  local k = res.kind(kind)
  return ctx.module:has(key .. "." .. k.parts[1])
end

-- An item's docs as the module has them, as JSON values sent as they are.
function res.docs(ctx, kind: string, key: string)
  local docs = {}
  for _, part in res.kind(kind).parts do
    local name = key .. "." .. part
    if ctx.module:has(name) then
      docs[part] = mg.json.raw(ctx.module:json(name))
    elseif res.kind(kind).optional[part] then
      docs[part] = mg.json.null
    else
      error(`{name} is not in the module`, 0)
    end
  end
  return docs
end

-- Whether the module's copy is the docs given (as JSON, keys sorted).
function res.same(ctx, kind: string, key: string, docs): boolean
  for _, part in res.kind(kind).parts do
    local name = key .. "." .. part
    local theirs = docs[part]
    local here = ctx.module:has(name)
    if theirs == nil or theirs == mg.json.null then
      if here then
        return false
      end
    elseif not here or mg.json.encode(mg.json.decode(ctx.module:json(name))) ~= mg.json.encode(theirs) then
      return false
    end
  end
  return true
end

-- Writes docs into the module: each file, one the docs lack removed, the module's list kept.
function res.write(ctx, kind: string, key: string, docs)
  local k = res.kind(kind)
  for _, part in k.parts do
    local name = key .. "." .. part
    local doc = docs[part]
    if doc == nil or doc == mg.json.null then
      if ctx.module:has(name) then
        ctx.edit:delete(name)
      end
    else
      ctx.edit:write_json(name, doc)
    end
  end
  if k.listed then
    res.list(ctx, k.listed, key)
  end
end

-- Puts a resource in one of module.ifo's lists (Mod_Area_list) if it is not there.
function res.list(ctx, list: string, key: string)
  local ifo = ctx.module:gff("module.ifo")
  for _, e in ifo[list] or {} do
    if string.lower(e.Area_Name or "") == key then
      return
    end
  end
  local entry = { __struct_id = 6, Area_Name = mg.resref(key) }
  if ifo[list] then
    ctx.edit:insert("module.ifo", "/" .. list, entry)
  else
    ctx.edit:set("module.ifo", list, { type = "list", value = { entry } })
  end
end

-- The name an item's revision is kept under: an area's akey, as the website's export keeps it.
local function rev_name(kind: string, key: string): string
  return if kind == "area" then key else kind .. ":" .. key
end

-- The module's local LAYO_EXPORT_REVS: its revisions, and its place in VarTable (nil: none).
local function read_revs(ctx)
  local ifo = ctx.module:gff("module.ifo")
  for i, v in ifo.VarTable or {} do
    if v.Name == res.REVS then
      local ok, revs = pcall(mg.json.decode, v.Value or "")
      if ok and type(revs) == "table" then
        return revs, i - 1
      end
      ctx.log:warn(`module.ifo's {res.REVS} is no JSON: it is written anew`)
      return {}, i - 1
    end
  end
  return {}, nil
end

-- The live revision the module's copy came from: 0 where none is recorded.
function res.base(ctx, kind: string, key: string): number
  local revs = read_revs(ctx)
  return tonumber(revs[rev_name(kind, key)]) or 0
end

-- Records the revision the module's copy now comes from.
function res.record(ctx, kind: string, key: string, rev: number)
  local revs, at = read_revs(ctx)
  revs[rev_name(kind, key)] = rev
  local text = mg.json.encode(revs)
  if at then
    ctx.edit:set("module.ifo", `/VarTable[{at}]/Value`, text)
    return
  end
  local var = { __struct_id = 0, Name = mg.string(res.REVS), Type = mg.dword(3), Value = mg.string(text) }
  if ctx.module:gff("module.ifo").VarTable then
    ctx.edit:insert("module.ifo", "/VarTable", var)
  else
    ctx.edit:set("module.ifo", "VarTable", { type = "list", value = { var } })
  end
end

return res
