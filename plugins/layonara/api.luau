-- The builder routes of Layonara's ops-api (docs/builder-api.md there).
local mg = require("@moonglow")

local api = {}

api.BASE = "https://builders.layonara.com/api/v1/builder"
api.SIGNIN = "layonara"

-- A request with the sign-in's token: the answer, and its body decoded (nil if it is no JSON).
function api.call(ctx, method: string, path: string, body: any?)
  local sent, r = pcall(function()
    return ctx.net:request({ method = method, url = api.BASE .. path, json = body, signin = api.SIGNIN })
  end)
  if not sent then
    local e = tostring(r)
    if string.find(e, "sign in again", 1, true) or string.find(e, "not signed in", 1, true) then
      error("Your Layonara sign-in has ended: sign in again (Plugins › Layonara › Sign In)", 0)
    end
    error(e, 0)
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

-- Logs an answer's problems (a 400's items, a 409's problems), each on its own line.
function api.log_problems(ctx, data)
  if type(data) ~= "table" then
    return
  end
  for _, p in data.problems or {} do
    ctx.log:error(tostring(p))
  end
  for _, i in data.items or {} do
    local where = `{i.kind or "?"} {i.key or "?"}`
    if type(i.problems) == "table" and #i.problems > 0 then
      for _, p in i.problems do
        ctx.log:error(`{where}: {p}`)
      end
    elseif i.detail then
      ctx.log:error(`{where}: {i.detail}`)
    end
  end
end

-- Stops the job with what a refusal says (signing in again, the limits, the service being off).
function api.fail(ctx, r, data, what: string)
  local d = api.detail(r, data)
  if r.status == 401 then
    error("Your Layonara sign-in has ended: sign in again (Plugins › Layonara › Sign In)", 0)
  elseif r.status == 403 then
    error(`{d} (Layonara's builder routes are for the Builders group)`, 0)
  elseif r.status == 404 and d == "Not Found" then
    error("Layonara's builder service is not open now", 0)
  elseif r.status == 429 then
    local after = r.headers["retry-after"]
    error(if after then `{d} (try again in {after} s)` else d, 0)
  end
  api.log_problems(ctx, data)
  error(`{what}: {d}`, 0)
end

-- Signed in, or asked to sign in now: the sign-in's status.
function api.signed_in(ctx)
  local s = ctx.signin:status(api.SIGNIN)
  if s.signed_in then
    return s
  end
  if not ctx.ui:confirm("You are not signed in to Layonara. Sign in now?") then
    error("Not signed in to Layonara (Plugins › Layonara › Sign In)", 0)
  end
  s = ctx.signin:start(api.SIGNIN)
  if not s.signed_in then
    error("Not signed in to Layonara", 0)
  end
  return s
end

return api
