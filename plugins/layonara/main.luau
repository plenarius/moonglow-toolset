-- The Layonara plugin: areas between the module and Layonara's database, through the
-- builder routes (fetch, merge, Try on Dev, Send for Review), signed in with Authentik.
local mg = require("@moonglow")
local api = require("api")
local res = require("res")

local KEEP_LIVE, TAKE_MINE = "Keep live's", "Take mine"
local UPDATE, ANYWAY, CANCEL =
  "Update first: merge live's changes into my copy", "Try anyway, on dev only", "Cancel"

local function path_of(kind: string, key: string): string
  return `/res/{kind}/{key}`
end

-- What a conflict says, short enough for a form's label.
local function said(c): string
  local what = tostring(c.what or c.id)
  if c.tag and c.tag ~= "" then
    what = `{what} ({c.tag})`
  end
  return if #what > 140 then string.sub(what, 1, 137) .. "…" else what
end

-- Asks how each conflict goes: the choices to send ("theirs" is the builder's), or nil (closed).
local function ask_conflicts(ctx, key: string, conflicts)
  local fields = {}
  local each = #conflicts <= 30
  if each then
    for i, c in conflicts do
      table.insert(fields, { id = `c{i}`, label = said(c), type = "choice", choices = { KEEP_LIVE, TAKE_MINE }, default = KEEP_LIVE })
    end
  else
    table.insert(fields, {
      id = "all",
      label = `{#conflicts} conflicts, all`,
      type = "choice",
      choices = { KEEP_LIVE, TAKE_MINE },
      default = KEEP_LIVE,
    })
  end
  local a = ctx.ui:form({ title = `{key}: your copy and live's both changed these`, fields = fields })
  if not a then
    return nil
  end
  local choices, mine = {}, 0
  for i, c in conflicts do
    local took = if each then a[`c{i}`] else a.all
    if took == TAKE_MINE then
      choices[c.id] = "theirs"
      mine += 1
    end
  end
  return choices, mine
end

-- Merges the module's copy with live's (3-way, from base): the answer, conflicts settled.
local function merge(ctx, kind: string, key: string, base: number)
  local body = { base_rev = base, docs = res.docs(ctx, kind, key) }
  local r, data = api.call(ctx, "POST", path_of(kind, key) .. "/merge", body)
  if not r.ok then
    api.fail(ctx, r, data, `{key}: the merge`)
  end
  local conflicts = data.merge and data.merge.conflicts or {}
  if #conflicts > 0 then
    local choices, mine = ask_conflicts(ctx, key, conflicts)
    if not choices then
      error(`{key}: the merge was canceled`, 0)
    end
    -- (Open conflicts come back as live has them: a second merge only for "take mine".)
    if mine > 0 then
      body.choices = choices
      r, data = api.call(ctx, "POST", path_of(kind, key) .. "/merge", body)
      if not r.ok then
        api.fail(ctx, r, data, `{key}: the merge`)
      end
    end
    ctx.log:info(`{key}: {#conflicts} conflicts, {mine} taken from your copy`)
  end
  for _, n in data.merge and data.merge.notes or {} do
    ctx.log:info(`{key}: {n}`)
  end
  return data
end

-- Writes a merge's (or a fetch's) docs into the module and records its revision.
local function take(ctx, kind: string, key: string, data)
  res.write(ctx, kind, key, data.docs)
  res.record(ctx, kind, key, data.rev)
end

mg.command("sign-in", function(ctx)
  local s = ctx.signin:status(api.SIGNIN)
  if not s.signed_in then
    s = ctx.signin:start(api.SIGNIN)
    if not s.signed_in then
      ctx.log:warn("Not signed in")
      return
    end
  end
  local r, me = api.call(ctx, "GET", "/me")
  if not r.ok then
    api.fail(ctx, r, me, "Signed in, but Layonara's builder service")
  end
  local groups = table.concat(me.groups or {}, ", ")
  ctx.log:info(`Signed in to Layonara as {me.name or me.username or s.name} ({groups})`)
  if not me.trials then
    ctx.log:warn("Try on Dev is off on the dev server now")
  end
end)

mg.command("sign-out", function(ctx)
  ctx.signin:stop(api.SIGNIN)
  ctx.log:info("Signed out of Layonara")
end)

-- The live areas whose key, tag or name has the text (an exact key alone if there is one).
local function matching(items, text: string)
  local want = string.lower(text)
  for _, i in items do
    if i.key == want then
      return { i }
    end
  end
  local out = {}
  for _, i in items do
    local hay = string.lower(`{i.key} {i.tag or ""} {i.name or ""}`)
    if string.find(hay, want, 1, true) then
      table.insert(out, i)
    end
  end
  return out
end

mg.command("fetch", function(ctx)
  api.signed_in(ctx)
  local kind = "area"
  local chosen = res.chosen(ctx)
  local a = ctx.ui:form({
    title = "Fetch an area from Layonara",
    fields = {
      { id = "area", label = "Area (its resref, tag or part of its name)", default = chosen[1] and chosen[1].key or "" },
    },
  })
  if not a or a.area == "" then
    return
  end
  local r, list = api.call(ctx, "GET", `/res/{kind}`)
  if not r.ok then
    api.fail(ctx, r, list, "The list of areas")
  end
  local found = matching(list.items or {}, a.area)
  if #found == 0 then
    error(`No live area has {a.area} in its resref, tag or name`, 0)
  end
  local item = found[1]
  if #found > 1 then
    local choices, by = {}, {}
    for i, f in found do
      if i > 2000 then
        break
      end
      local text = `{f.key}: {f.name or ""} (revision {f.rev})`
      table.insert(choices, text)
      by[text] = f
    end
    local pick = ctx.ui:form({
      title = `{#found} areas have {a.area}`,
      fields = { { id = "pick", label = "Area", type = "choice", choices = choices, default = choices[1] } },
    })
    if not pick then
      return
    end
    item = by[pick.pick]
    if not item then
      return
    end
  end
  local key = item.key
  local r2, live = api.call(ctx, "GET", path_of(kind, key))
  if not r2.ok then
    api.fail(ctx, r2, live, key)
  end
  local base = res.base(ctx, kind, key)
  if not res.has(ctx, kind, key) then
    take(ctx, kind, key, live)
    ctx.log:info(`{key}: fetched, live's revision {live.rev}`)
  elseif res.same(ctx, kind, key, live.docs) then
    if base ~= live.rev then
      res.record(ctx, kind, key, live.rev)
    end
    ctx.log:info(`{key}: your copy is live's revision {live.rev} already`)
  elseif base == live.rev or base == 0 then
    local from = if base == 0 then "with no record of the revision it came from" else `from revision {base}, as live's is`
    if not ctx.ui:confirm(`Your copy of {key} is {from}, and changed. Replace it with live's revision {live.rev}?`) then
      return
    end
    take(ctx, kind, key, live)
    ctx.log:info(`{key}: replaced with live's revision {live.rev}`)
  else
    take(ctx, kind, key, merge(ctx, kind, key, base))
    ctx.log:info(`{key}: your copy (from revision {base}) merged with live's revision {live.rev}`)
  end
  return { label = `Fetch Area {key}` }
end)

-- The items to send: each with its docs and the revision its copy came from.
local function items_of(ctx, chosen)
  local items = {}
  for _, c in chosen do
    if not res.has(ctx, c.kind, c.key) then
      error(`{c.key} is not in the module`, 0)
    end
    table.insert(items, {
      kind = c.kind,
      key = c.key,
      base_rev = res.base(ctx, c.kind, c.key),
      docs = res.docs(ctx, c.kind, c.key),
    })
  end
  return items
end

-- A new area's folder, asked for (from those live's areas are in) where it is not live.
local function folders_for(ctx, items)
  local new = {}
  for _, i in items do
    if i.base_rev == 0 then
      table.insert(new, i)
    end
  end
  if #new == 0 then
    return
  end
  local r, list = api.call(ctx, "GET", "/res/area")
  if not r.ok then
    api.fail(ctx, r, list, "The list of areas")
  end
  local live, folders, seen = {}, {}, {}
  for _, a in list.items or {} do
    live[a.key] = a
    if a.folder and not seen[a.folder] then
      seen[a.folder] = true
      table.insert(folders, a.folder)
    end
  end
  table.sort(folders)
  if seen.core then
    table.sort(folders, function(x, y)
      return x == "core" or (y ~= "core" and x < y)
    end)
  end
  for _, i in new do
    if live[i.key] then
      ctx.log:warn(`{i.key}: no revision recorded for your copy, and live has one: fetch it first to merge`)
    elseif #folders > 0 then
      local a = ctx.ui:form({
        title = `{i.key} is a new area`,
        fields = { { id = "folder", label = "Its folder", type = "choice", choices = folders, default = folders[1] } },
      })
      if not a then
        error("Canceled", 0)
      end
      i.folder = a.folder
    end
  end
end

-- After a 409 "behind": the items' base revisions updated (merged), or set to live's (anyway).
local function catch_up(ctx, items, data, may_try_anyway: boolean): boolean
  local lines = {}
  for _, b in data.behind or {} do
    local by = ""
    if type(b.published) == "table" then
      by = ` by {b.published.name or "someone"} {b.published.at or ""}`
    end
    table.insert(lines, `{b.key} is from revision {b.base_rev}, live's is {b.rev} (published{by})`)
  end
  for _, l in lines do
    ctx.log:warn(l)
  end
  local choices = if may_try_anyway then { UPDATE, ANYWAY, CANCEL } else { UPDATE, CANCEL }
  local a = ctx.ui:form({
    title = table.concat(lines, "\n"),
    fields = { { id = "next", label = "Your copy is behind live's", type = "choice", choices = choices, default = UPDATE } },
  })
  if not a or a.next == CANCEL then
    return false
  end
  for _, b in data.behind or {} do
    for _, i in items do
      if i.kind == b.kind and i.key == b.key then
        if a.next == ANYWAY then
          i.base_rev = b.rev
          ctx.log:warn(`{i.key}: tried over live's revision {b.rev}, whose changes your copy lacks`)
        elseif i.base_rev == 0 then
          error(`{i.key}: no revision is recorded for your copy to merge from: fetch it first`, 0)
        else
          local merged = merge(ctx, i.kind, i.key, i.base_rev)
          take(ctx, i.kind, i.key, merged)
          ctx.log:info(`{i.key}: merged with live's revision {merged.rev}`)
          i.base_rev = merged.rev
          i.docs = res.docs(ctx, i.kind, i.key)
        end
      end
    end
  end
  return true
end

-- Sends items to a route, catching up once if they are behind: the answer, or nil (canceled).
local function send(ctx, route: string, body, what: string, may_try_anyway: boolean)
  local merged = false
  for round = 1, 2 do
    local r, data = api.call(ctx, "POST", route, body)
    if r.ok then
      return data, merged
    end
    if r.status == 409 and type(data) == "table" and data.code == "behind" and round == 1 then
      if not catch_up(ctx, body.items, data, may_try_anyway) then
        ctx.log:info(`{what}: canceled`)
        return nil, merged
      end
      merged = true
    elseif merged then
      -- (The merge stays in the module, sent or not.)
      api.log_problems(ctx, data)
      ctx.log:error(`{what}: {api.detail(r, data)}`)
      return nil, merged
    else
      if r.status == 409 and type(data) == "table" and data.skipped then
        for _, s in data.skipped do
          ctx.log:warn(`{s.key}: the same as live's, nothing to try`)
        end
      end
      api.fail(ctx, r, data, what)
    end
  end
  return nil, merged
end

local function chosen_or_fail(ctx)
  local chosen = res.chosen(ctx)
  if #chosen == 0 then
    error("Select an area (or open one) first", 0)
  end
  if #chosen > 12 then
    error(`{#chosen} areas: at most 12 at once`, 0)
  end
  return chosen
end

local function names(chosen): string
  local keys = {}
  for _, c in chosen do
    table.insert(keys, c.key)
  end
  return table.concat(keys, ", ")
end

mg.command("try", function(ctx)
  local chosen = chosen_or_fail(ctx)
  api.signed_in(ctx)
  local items = items_of(ctx, chosen)
  folders_for(ctx, items)
  local body = { items = items, note = `Moonglow: {names(chosen)}`, mode = "add" }
  local data, merged = send(ctx, "/trial", body, "Try on Dev", true)
  if data then
    local commands = {}
    for _, i in data.items or {} do
      local dev = i.dev or {}
      local state = if dev.applied then "applied" elseif dev.waiting then "waiting for nobody to be in it (within 30 s)" else tostring(dev.message or "")
      ctx.log:info(`{i.key}: on dev as revision {i.rev} ({state})`)
      local ways = i.ways or {}
      for _, w in ways.new_broken or {} do
        ctx.log:warn(`{i.key}: a way out leads nowhere: {w}`)
      end
      if i.command then
        table.insert(commands, i.command)
      end
    end
    for _, e in data.errors or {} do
      ctx.log:error(`{e.key}: {e.detail}`)
    end
    for _, s in data.skipped or {} do
      ctx.log:info(`{s.key}: the same as live's, not tried`)
    end
    local minutes = math.floor((tonumber(data.expires_in) or 0) / 60)
    local way_in = data.way_in or "Connect to dev and type ;forge"
    ctx.log:info(way_in)
    local text = way_in
    if #commands > 0 then
      text ..= "\n\n" .. table.concat(commands, "\n")
    end
    ctx.ui:message(`{text}\n\nYour trial ends after {minutes} minutes without activity.`)
  end
  if merged then
    return { label = "Try on Dev (merged with live's)" }
  end
end)

mg.command("submit", function(ctx)
  local chosen = chosen_or_fail(ctx)
  api.signed_in(ctx)
  local a = ctx.ui:form({
    title = `Send {names(chosen)} for review`,
    fields = { { id = "note", label = "What you changed (for the reviewer)", default = "" } },
  })
  if not a then
    return
  end
  local note = string.gsub(a.note or "", "^%s+", "")
  if note == "" then
    error("Say what you changed: the reviewer reads it", 0)
  end
  if utf8.len(note) and utf8.len(note) > 200 then
    error("The note is over 200 characters", 0)
  end
  local items = items_of(ctx, chosen)
  folders_for(ctx, items)
  local data, merged = send(ctx, "/submit", { items = items, note = note }, "Send for Review", false)
  if data then
    for _, i in data.items or {} do
      ctx.log:info(`{i.key}: sent for review ({i.state or "waiting"}), {i.toolbox or ""}`)
      local check = i.check or {}
      for _, w in check.warnings or {} do
        ctx.log:warn(`{i.key}: {w}`)
      end
    end
  end
  if merged then
    return { label = "Send for Review (merged with live's)" }
  end
end)

-- This builder's trial (nil: none), as GET /trial gives it.
local function trial(ctx)
  local r, data = api.call(ctx, "GET", "/trial")
  if not r.ok then
    api.fail(ctx, r, data, "Your trial")
  end
  local t = data.trial
  if t == nil or t == mg.json.null then
    return nil
  end
  return t
end

mg.command("status", function(ctx)
  api.signed_in(ctx)
  local t = trial(ctx)
  if not t then
    ctx.log:info("You have nothing on trial on dev")
    ctx.ui:message("You have nothing on trial on dev.")
    return
  end
  local lines = {}
  for _, i in t.items or {} do
    local line = if i.state == "trial"
      then `{i.key}: your copy is on dev (revision {i.dev_rev})`
      elseif i.state == "live" then `{i.key}: {i.message ~= "" and i.message or "live's again"}`
      else `{i.key}: {i.state} {i.message or ""}`
    table.insert(lines, line)
    ctx.log:info(line)
  end
  local minutes = math.floor((tonumber(t.expires_in) or 0) / 60)
  local ends = `Your trial on dev ({t.user}) ends in {minutes} minutes without activity.`
  ctx.log:info(ends)
  ctx.ui:message(table.concat(lines, "\n") .. "\n\n" .. ends)
end)

mg.command("release", function(ctx)
  api.signed_in(ctx)
  local t = trial(ctx)
  if not t or #(t.items or {}) == 0 then
    ctx.log:info("You have nothing on trial on dev")
    return
  end
  local all = `All of it ({#t.items})`
  local choices = { all }
  for _, i in t.items do
    table.insert(choices, i.key)
  end
  local a = ctx.ui:form({
    title = "Put back to live's on dev",
    fields = { { id = "what", label = "Release", type = "choice", choices = choices, default = all } },
  })
  if not a then
    return
  end
  local body = {}
  if a.what ~= all then
    for _, i in t.items do
      if i.key == a.what then
        body = { items = { { kind = i.kind, key = i.key } } }
      end
    end
  end
  -- ({} is an object to the service: all of it.)
  local r, data = api.call(ctx, "POST", "/trial/release", if body.items then body else mg.json.raw("{}"))
  if not r.ok then
    if r.status == 409 and type(data) == "table" and data.pending then
      for _, p in data.pending do
        ctx.log:warn(`{p.key}: {p.message or "dev has not put it back yet"}`)
      end
      error("Dev has not put everything back yet: try again in a minute", 0)
    end
    api.fail(ctx, r, data, "Release")
  end
  for _, i in data.released or {} do
    ctx.log:info(`{i.key}: live's again on dev`)
  end
  local left = #(data.trial or {})
  ctx.log:info(if left == 0 then "Nothing of yours is on trial now" else `{left} still on trial`)
end)
