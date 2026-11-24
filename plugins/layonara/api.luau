-- The builder routes of Layonara's ops-api (docs/builder-api.md there).
local mg = require("@moonglow")

local api = {}

api.BASE = "https://builders.layonara.com/api/v1/builder"
api.SIGNIN = "layonara"

local SIGN_IN_AGAIN = "Your Layonara sign-in has ended: sign in again (Plugins › Layonara › Sign In)"

-- Stops the job with something to tell the builder (a refusal, a choice they must make), not a bug.
function api.stop(text: string)
  error({ stop = text }, 0)
end

-- What `pcall` caught, as text: a stop's own words.
function api.said(e): string
  return if type(e) == "table" and e.stop then tostring(e.stop) else tostring(e)
end

-- A command's handler, its stops shown in a dialog and the log; any other error (a bug) raised with its traceback.
function api.command(handler)
  return function(ctx)
    local ok, got = xpcall(handler, function(e)
      if type(e) == "table" and e.stop then
        return e
      end
      return if debug and debug.traceback then debug.traceback(tostring(e), 2) else tostring(e)
    end, ctx)
    if ok then
      return got
    end
    if type(got) == "table" and got.stop then
      local text = tostring(got.stop)
      ctx.log:error(text)
      ctx.ui:message(text)
      return nil
    end
    error(got, 0)
  end
end

-- An error from the host as a sentence: no traceback.
function api.plain(e): string
  local text = string.gsub(tostring(e), "\n*stack traceback:.*$", "")
  text = string.gsub(text, "^runtime error: ", "")
  return text
end

-- A failed download (refused, cut off) as a stop: what it said and what to do.
function api.download_failed(e)
  local text = api.plain(e)
  if string.find(text, "sign in again", 1, true) or string.find(text, "not signed in", 1, true) then
    api.stop(SIGN_IN_AGAIN)
  end
  api.stop(`{text}\n\nTry again in a few minutes: a download that stopped goes on from where it was.`)
end

-- A request with the sign-in's token: the answer, and its body decoded (nil if it is no JSON).
function api.call(ctx, method: string, path: string, body: any?)
  local sent, r = pcall(function()
    return ctx.net:request({ method = method, url = api.BASE .. path, json = body, signin = api.SIGNIN })
  end)
  if not sent then
    local e = tostring(r)
    if string.find(e, "sign in again", 1, true) or string.find(e, "not signed in", 1, true) then
      api.stop(SIGN_IN_AGAIN)
    end
    api.stop(`Moonglow couldn't reach Layonara's builder service ({api.plain(e)}).\n\nCheck your internet connection and try again.`)
  end
  local ok, data = pcall(mg.json.decode, r.body)
  return r, if ok then data else nil
end

-- What a refusal says, as a sentence to show.
function api.detail(r, data): string
  local d = if type(data) == "table" then data.detail else nil
  if type(d) == "string" and d ~= "" then
    return d
  end
  return `the service answered {r.status}`
end

-- A refusal's problems (a 400's items, a 409's problems), one line each, by area.
function api.problems(data): { string }
  local out = {}
  if type(data) ~= "table" then
    return out
  end
  for _, p in (if type(data.problems) == "table" then data.problems else {}) do
    table.insert(out, tostring(p))
  end
  for _, i in (if type(data.items) == "table" then data.items else {}) do
    local where = `{i.kind or "?"} {i.key or "?"}`
    if type(i.problems) == "table" and #i.problems > 0 then
      for _, p in i.problems do
        table.insert(out, `{where}: {p}`)
      end
    elseif i.detail then
      table.insert(out, `{where}: {i.detail}`)
    end
  end
  return out
end

-- Logs an answer's problems, each on its own line.
function api.log_problems(ctx, data)
  for _, p in api.problems(data) do
    ctx.log:error(p)
  end
end

-- What to do about a refusal of this status.
local function next_step(r, data): string?
  local s = r.status
  if s == 400 then
    return "Fix these in your copy, then try again."
  elseif s == 409 and type(data) == "table" then
    if type(data.problems) == "table" and #data.problems > 0 then
      return "Wait until that trial is released or ends (after an hour without activity), then try again."
    end
    local same = type(data.skipped) == "table" and #data.skipped > 0
    for _, i in (if type(data.items) == "table" then data.items else {}) do
      same = same or (i.status == 409 and i.code == nil)
    end
    return if same then "Your copy is the same as live's: change something first." else nil
  elseif s == 413 then
    return "Send fewer areas at once."
  elseif s == 422 then
    return "Moonglow sent something the service doesn't take: tell the staff, with the log."
  elseif s == 502 or s == 504 then
    return "Layonara's service couldn't reach a server behind it. Try again in a few minutes; if it keeps happening, tell the staff."
  elseif s >= 500 then
    local after = r.headers and r.headers["retry-after"]
    local wait = if after then `in {after} s` else "in a few minutes"
    return `Try again {wait}; if it keeps happening, tell the staff.`
  end
  return nil
end

local SHOWN = 15

-- Stops the job with what a refusal says, its problems by area and what to do next.
function api.fail(ctx, r, data, what: string)
  local d = api.detail(r, data)
  if r.status == 401 then
    api.stop(SIGN_IN_AGAIN)
  elseif r.status == 403 then
    api.stop(`{d} (Layonara's builder routes are for the Builders group)`)
  elseif r.status == 404 and d == "Not Found" then
    api.stop("Layonara's builder service is not open now")
  elseif r.status == 429 then
    local after = r.headers["retry-after"]
    api.stop(if after then `{d} (try again in {after} s)` else d)
  end
  if type(data) ~= "table" and r.status >= 500 then
    d = `Layonara's builder service answered {r.status}`
  end
  local problems = api.problems(data)
  for _, p in problems do
    ctx.log:error(p)
  end
  local lines = { `{what}: {d}` }
  if #problems > 0 then
    table.insert(lines, "")
    for n, p in problems do
      if n > SHOWN then
        table.insert(lines, `… and {#problems - SHOWN} more (in the log)`)
        break
      end
      table.insert(lines, `• {p}`)
    end
  end
  local step = next_step(r, data)
  if step then
    table.insert(lines, "")
    table.insert(lines, step)
  end
  api.stop(table.concat(lines, "\n"))
end

-- Signed in, or asked to sign in now: the sign-in's status.
function api.signed_in(ctx)
  local s = ctx.signin:status(api.SIGNIN)
  if s.signed_in then
    return s
  end
  if not ctx.ui:confirm("You are not signed in to Layonara. Sign in now?") then
    api.stop("Not signed in to Layonara (Plugins › Layonara › Sign In)")
  end
  s = ctx.signin:start(api.SIGNIN)
  if not s.signed_in then
    api.stop("Not signed in to Layonara")
  end
  return s
end

return api
