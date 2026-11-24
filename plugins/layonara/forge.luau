-- From the Forge: the builder's own Forge exports into the module (design section 11), and the
-- Forge source each area came from (module.ifo's LAYO_FORGE: {akey: {name, export}}).
local mg = require("@moonglow")
local api = require("api")
local res = require("res")

local forge = {}

forge.VAR = "LAYO_FORGE"
forge.LIBRARY = "ls_textures"

-- The Forge export an area of the module was fetched from ({name, export}), or nil.
function forge.source(ctx, key: string)
  local f = res.read_var(ctx, forge.VAR)[key]
  if type(f) == "table" and type(f.name) == "string" and type(f.export) == "string" then
    return { name = f.name, export = f.export }
  end
  return nil
end

function forge.record(ctx, key: string, name: string, export: string)
  local all, at = res.read_var(ctx, forge.VAR)
  all[key] = { name = name, export = export }
  res.write_var(ctx, forge.VAR, all, at)
end

-- Forgets an area's Forge source (its copy is live's now).
function forge.forget(ctx, key: string)
  local all, at = res.read_var(ctx, forge.VAR)
  if all[key] ~= nil then
    all[key] = nil
    res.write_var(ctx, forge.VAR, all, at)
  end
end

local function mb(n): string
  return string.format("%.1f MB", (tonumber(n) or 0) / 1048576)
end

local function when(t): string
  local n = tonumber(t)
  return if n then os.date("%Y-%m-%d %H:%M", math.floor(n)) else "an unknown date"
end

-- What a listed export is, on one line of the form's choice.
local function line(i): string
  local becomes = if i.new then "a new area" else `rework of {i.area}`
  local hak = if type(i.hak) == "table" then `, tiles {mb(i.hak.bytes)}` else ""
  local review = {}
  local r = if type(i.review) == "table" then i.review else {}
  if type(r.submitted) == "table" then
    table.insert(review, if r.submitted.export == i.export then "sent for review" else "an older export sent for review")
  end
  if type(r.published) == "table" then
    table.insert(review, "tiles published")
  end
  local state = if #review > 0 then `, {table.concat(review, ", ")}` else ""
  return `{i.name} → {i.area} ({becomes}), exported {when(i.exported_at)}{hak}{state}`
end

-- Objects in an area's GIT, for what a replace loses.
local function objects(ctx, key: string): number
  local n = 0
  if not ctx.module:has(key .. ".git") then
    return 0
  end
  for _, v in ctx.module:gff(key .. ".git") do
    if type(v) == "table" and #v > 0 then
      n += #v
    end
  end
  return n
end

-- Whether the module's resource is the blueprint given (GFF JSON as a table, a script as text).
local function same_blueprint(ctx, name: string, doc): boolean
  if type(doc) == "string" then
    return ctx.module:text(name) == doc
  end
  local ok, here = pcall(function()
    return mg.json.encode(mg.json.decode(ctx.module:json(name)))
  end)
  return ok and here == mg.json.encode(doc)
end

-- A blueprint's name the module can take: resref.ext, the resref 16 letters at most.
local function blueprint_ok(name: string): boolean
  local ref, ext = string.match(name, "^([%w_%-]+)%.(%a%w%w)$")
  return ref ~= nil and #ref <= 16 and ext ~= nil
end

-- What writing the export over the module loses: lines naming it (empty: nothing).
local function lost(ctx, e): { string }
  local out = {}
  if res.has(ctx, "area", e.area) and not res.same(ctx, "area", e.area, e.docs) then
    local n = objects(ctx, e.area)
    local from = forge.source(ctx, e.area)
    local was = if from then `your copy of {e.area} (from the Forge's {from.name})` else `your copy of {e.area}`
    table.insert(out, `{was}: its terrain, settings and {n} objects, with every change made to them in Moonglow`)
  end
  local changed = {}
  for name, doc in e.blueprints do
    if blueprint_ok(name) and ctx.module:has(name) and not same_blueprint(ctx, name, doc) then
      table.insert(changed, name)
    end
  end
  table.sort(changed)
  if #changed > 0 then
    table.insert(out, `your {table.concat(changed, ", ")}`)
  end
  return out
end

-- Whether the module lists the hak.
local function listed(ctx, hak: string): boolean
  local ifo = ctx.module:gff("module.ifo")
  for _, h in ifo.Mod_HakList or {} do
    if string.lower(h.Mod_Hak or "") == hak then
      return true
    end
  end
  return string.lower(ifo.Mod_Hak or "") == hak
end

local function download(ctx, hak: string, file, url: string)
  local got = ctx.hak:download(hak, {
    url = url,
    signin = api.SIGNIN,
    sha256 = file.sha256,
    bytes = file.bytes,
    -- (Q5 a: a rework of a released tileset shows the builder's tiles, not live NWSync's.)
    above_nwsync = true,
  })
  if got.downloaded then
    ctx.log:info(`{hak}.hak: downloaded ({mb(got.bytes)})`)
  else
    ctx.log:info(`{hak}.hak: yours is the Forge's already`)
  end
end

-- The builder picks one of their exports: its list entry, or nil (closed).
local function pick(ctx)
  local r, list = api.call(ctx, "GET", "/forge/areas")
  if not r.ok then
    if r.status == 404 and api.detail(r, list) ~= "Not Found" then
      error(`{api.detail(r, list)} (export an area in the Forge first)`, 0)
    end
    api.fail(ctx, r, list, "Your Forge exports")
  end
  local items = list.items or {}
  if #items == 0 then
    error("Your Forge has no exports: export an area in the Forge first", 0)
  end
  local choices, by = {}, {}
  for _, i in items do
    local text = line(i)
    ctx.log:info(text)
    table.insert(choices, text)
    by[text] = i
  end
  local a = ctx.ui:form({
    title = `From the Forge ({list.user or "you"})`,
    fields = { { id = "export", label = "Export", type = "choice", choices = choices, default = choices[1] } },
  })
  return a and by[a.export]
end

function forge.fetch(ctx)
  api.signed_in(ctx)
  local item = pick(ctx)
  if not item then
    return
  end
  local name = tostring(item.name)
  if not string.match(name, "^[a-z0-9_]+$") or #name > 16 then
    error(`The Forge's {name} is not a name a hak can have`, 0)
  end
  local r, e = api.call(ctx, "GET", `/forge/areas/{name}`)
  if not r.ok then
    api.fail(ctx, r, e, `The Forge's {name}`)
  end
  local key = tostring(e.area or "")
  if not string.match(key, "^[a-z0-9_]+$") or #key > 16 then
    error(`The Forge's {name} becomes {key}, which is no area's resref`, 0)
  end
  e.blueprints = if type(e.blueprints) == "table" then e.blueprints else {}
  if type(e.hak) ~= "table" then
    error(`The Forge lists no tile hak for {name}: export it again`, 0)
  end
  local gone = lost(ctx, e)
  if #gone > 0 then
    local text = `Replace your copy with the Forge's export of {when(item.exported_at)}?\n\nYou lose {table.concat(gone, "; and ")}.`
    if not ctx.ui:confirm(text) then
      ctx.log:info(`{key}: kept as it is`)
      return
    end
  end
  download(ctx, name, e.hak, `{api.BASE}/forge/areas/{name}/hak`)
  if type(e.library) == "table" then
    download(ctx, forge.LIBRARY, e.library, `{api.BASE}/forge/library`)
  end
  res.write(ctx, "area", key, e.docs)
  res.record(ctx, "area", key, tonumber(e.base_rev) or 0)
  forge.record(ctx, key, name, tostring(e.export))
  local names = {}
  for n in e.blueprints do
    table.insert(names, n)
  end
  table.sort(names)
  for _, n in names do
    local doc = e.blueprints[n]
    if not blueprint_ok(n) then
      ctx.log:warn(`{n}: not a resource name the module can have, left out`)
    elseif ctx.module:has(n) and same_blueprint(ctx, n, doc) then
      continue
    elseif type(doc) == "string" then
      ctx.edit:write(n, doc)
    else
      ctx.edit:write_json(n, doc)
    end
  end
  -- (The tiles over the library's textures: the tile hak listed last, at the top.)
  for _, hak in { forge.LIBRARY, name } do
    if (hak ~= forge.LIBRARY or type(e.library) == "table") and not listed(ctx, hak) then
      ctx.hak:attach(hak)
    end
  end
  for _, f in e.findings or {} do
    local text = `{key}: {f.what or ""} {f.msg or ""}`
    if f.level == "error" then
      ctx.log:error(text)
    elseif f.level == "warning" then
      ctx.log:warn(text)
    else
      ctx.log:info(text)
    end
  end
  local becomes = if e.new then "a new area" else `rework of live's revision {e.base_rev}`
  ctx.log:info(`{key}: fetched from the Forge's {name} ({becomes}), exported {when(item.exported_at)}`)
  return { label = `From the Forge {name}` }
end

-- A refused submit's "fetch again" (409 forge_changed), as an error; nothing if it is not one.
function forge.changed(r, data)
  if r.status ~= 409 or type(data) ~= "table" then
    return
  end
  for _, i in data.items or {} do
    if i.code == "forge_changed" then
      error(`{i.key}: {i.detail or "the Forge has a newer export"} (Layonara › From the Forge…)`, 0)
    end
  end
end

-- A refused trial of a Forge area whose tiles aren't on dev (Q4 a), as an error saying what to do.
function forge.not_on_dev(ctx, r, data)
  if r.status ~= 400 or type(data) ~= "table" then
    return
  end
  for _, i in data.items or {} do
    local from = i.key and forge.source(ctx, i.key)
    if from then
      for _, p in i.problems or {} do
        if string.find(tostring(p), `no tileset {from.name}`, 1, true) then
          api.log_problems(ctx, data)
          error(
            `{i.key}: its {from.name} tiles aren't on dev yet: press Try on dev in the Forge first, then Try on Dev here again`,
            0
          )
        end
      end
    end
  end
end

return forge
